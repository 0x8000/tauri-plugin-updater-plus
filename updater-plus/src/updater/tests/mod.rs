use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use semver::Version;
use url::Url;

use super::UpdaterBuilder;
use crate::blockmap::BlockMap;
use crate::cache::PackageCache;
use crate::chunker::ChunkerConfig;
use crate::listener::{DownloadEvent, DownloadListener, DownloadMode};

mod failover;
mod installer;

#[derive(Clone, Default)]
pub(super) struct ReleaseServerState {
    pub(super) files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    pub(super) status_overrides: Arc<Mutex<HashMap<String, StatusCode>>>,
    pub(super) seen_custom_header: Arc<Mutex<Option<String>>>,
}

async fn serve_file(
    State(state): State<ReleaseServerState>,
    AxumPath(name): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(val) = headers.get("x-test-token").and_then(|v| v.to_str().ok()) {
        *state.seen_custom_header.lock().expect("lock") = Some(val.to_string());
    }
    if let Some(code) = state
        .status_overrides
        .lock()
        .expect("lock")
        .get(&name)
        .copied()
    {
        return (code, "").into_response();
    }

    let data = {
        let guard = state.files.lock().expect("lock");
        match guard.get(&name) {
            Some(bytes) => bytes.clone(),
            None => return (StatusCode::NOT_FOUND, "not found").into_response(),
        }
    };

    let Some(range_hdr) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
        return (StatusCode::OK, data).into_response();
    };
    let total = data.len();
    let ranges: Vec<(usize, usize)> = range_hdr
        .trim_start_matches("bytes=")
        .split(',')
        .filter_map(|part| {
            let (s, e) = part.trim().split_once('-')?;
            Some((s.parse().ok()?, e.parse().ok()?))
        })
        .collect();

    if ranges.iter().any(|&(s, e)| s > e || e >= total) {
        return (StatusCode::RANGE_NOT_SATISFIABLE, "range out of bounds").into_response();
    }

    if ranges.len() == 1 {
        let (start, end) = ranges[0];
        let mut resp_headers = HeaderMap::new();
        resp_headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")).expect("hdr"),
        );
        return (
            StatusCode::PARTIAL_CONTENT,
            resp_headers,
            data[start..=end].to_vec(),
        )
            .into_response();
    }

    let boundary = "CHAIN_TEST_BOUNDARY";
    let mut body = Vec::new();
    for (start, end) in ranges {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {start}-{end}/{total}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(&data[start..=end]);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&format!("multipart/byteranges; boundary={boundary}")).expect("hdr"),
    );
    (StatusCode::PARTIAL_CONTENT, resp_headers, body).into_response()
}

#[derive(Default)]
pub(super) struct RecordingListener {
    pub(super) events: Mutex<Vec<DownloadEvent>>,
}

impl DownloadListener for RecordingListener {
    fn on_event(&self, event: DownloadEvent) {
        self.events.lock().expect("lock").push(event);
    }
}

fn synthetic_bytes(seed: u64, size: usize) -> Vec<u8> {
    let mut state = seed;
    let mut buf = Vec::with_capacity(size);
    for _ in 0..size {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        buf.push((state >> 56) as u8);
    }
    buf
}

pub(super) async fn spawn_release_server(state: ReleaseServerState) -> Url {
    let app = Router::new()
        .route("/{name}", get(serve_file))
        .with_state(state);
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = tcp.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(tcp, app).await;
    });
    Url::parse(&format!("http://{addr}/")).expect("url")
}

#[tokio::test]
async fn chains_first_full_download_into_subsequent_differential_update_with_electron_yaml() {
    let state = ReleaseServerState::default();
    let base_url = spawn_release_server(state.clone()).await;

    let v2_bytes = synthetic_bytes(0x9988_7766, 1_500_000);
    let mut v3_bytes = v2_bytes.clone();
    v3_bytes[500_000..508_192].fill(0x7e);

    let v2_art =
        BlockMap::generate(v2_bytes.as_slice(), "file", ChunkerConfig::default()).expect("v2 map");
    let v3_art =
        BlockMap::generate(v3_bytes.as_slice(), "file", ChunkerConfig::default()).expect("v3 map");

    let v2_yaml = format!(
        "version: 0.2.0\nfiles:\n  - url: myapp-0.2.0.AppImage\n    sha512: {}\n    size: {}\npath: myapp-0.2.0.AppImage\nsha512: {}\nreleaseDate: '2026-11-24T00:00:00Z'\n",
        v2_art.sha512, v2_art.size, v2_art.sha512
    );
    {
        let mut files = state.files.lock().expect("lock");
        files.insert("latest-linux.yml".into(), v2_yaml.into_bytes());
        files.insert("myapp-0.2.0.AppImage".into(), v2_bytes.clone());
        files.insert(
            "myapp-0.2.0.AppImage.blockmap".into(),
            v2_art.blockmap.to_gzip_bytes().expect("gz"),
        );
    }

    let temp = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(temp.path().join("cache"), None);

    let updater_v1 = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(0, 1, 0),
        temp.path().join("myapp.AppImage"),
        cache.clone(),
    )
    .endpoints(vec![base_url.join("latest-linux.yml").expect("url")])
    .header("x-test-token", "secret-123")
    .expect("header")
    .build()
    .expect("updater");

    let update_v2 = updater_v1.check().await.expect("check").expect("update");
    assert_eq!(update_v2.version, "0.2.0");
    assert_eq!(
        state.seen_custom_header.lock().expect("lock").as_deref(),
        Some("secret-123")
    );
    let listener1 = RecordingListener {
        events: Mutex::new(Vec::new()),
    };
    let pkg_v2 = update_v2.download(&listener1).await.expect("download v2");
    assert_eq!(pkg_v2.summary.mode, DownloadMode::Full);
    assert_eq!(std::fs::read(&pkg_v2.path).expect("read"), v2_bytes);
    {
        let events1 = listener1.events.lock().expect("lock");
        assert!(matches!(
            events1.first(),
            Some(DownloadEvent::Started {
                differential: false,
                reused_bytes: 0,
                ..
            })
        ));
        assert!(events1.iter().any(
            |event| matches!(event, DownloadEvent::Progress { chunk_length } if *chunk_length > 0)
        ));
        assert!(matches!(
            events1.last(),
            Some(DownloadEvent::Finished { summary }) if summary == &pkg_v2.summary
        ));
    }

    let v3_json = serde_json::json!({
        "version": "0.3.0",
        "notes": "Small patch over 0.2.0",
        "pub_date": "2026-11-24T01:00:00Z",
        "platforms": {
            "linux-x86_64": {
                "url": base_url.join("myapp-0.3.0.AppImage").expect("url"),
                "sha512": v3_art.sha512,
                "size": v3_art.size,
            }
        }
    });
    {
        let mut files = state.files.lock().expect("lock");
        files.insert(
            "latest.json".into(),
            serde_json::to_vec(&v3_json).expect("json"),
        );
        files.insert("myapp-0.3.0.AppImage".into(), v3_bytes.clone());
        files.insert(
            "myapp-0.3.0.AppImage.blockmap".into(),
            v3_art.blockmap.to_gzip_bytes().expect("gz"),
        );
    }

    let updater_v2 = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(0, 2, 0),
        temp.path().join("myapp.AppImage"),
        cache,
    )
    .target("linux-x86_64")
    .endpoints(vec![base_url.join("latest.json").expect("url")])
    .build()
    .expect("updater");

    let update_v3 = updater_v2.check().await.expect("check").expect("update");
    assert_eq!(update_v3.version, "0.3.0");
    let listener2 = RecordingListener {
        events: Mutex::new(Vec::new()),
    };
    let pkg_v3 = update_v3.download(&listener2).await.expect("download v3");
    assert_eq!(pkg_v3.summary.mode, DownloadMode::Differential);
    assert!(pkg_v3.summary.saved_percent > 80.0);
    assert!(pkg_v3.summary.downloaded_bytes < (v3_bytes.len() as u64) / 5);
    assert_eq!(std::fs::read(&pkg_v3.path).expect("read"), v3_bytes);
    {
        let events2 = listener2.events.lock().expect("lock");
        assert!(matches!(
            events2.first(),
            Some(DownloadEvent::Started {
                differential: true,
                reused_bytes,
                ..
            }) if *reused_bytes == pkg_v3.summary.reused_bytes
        ));
        assert!(events2.iter().any(
            |event| matches!(event, DownloadEvent::Progress { chunk_length } if *chunk_length > 0)
        ));
        assert!(matches!(
            events2.last(),
            Some(DownloadEvent::Finished { summary }) if summary == &pkg_v3.summary
        ));
    }
}

#[test]
fn target_candidates_for_android_prefer_arch_then_universal_fallback() {
    assert_eq!(
        super::TargetResolver::target_candidates_for("android", "aarch64", None),
        vec![
            "android-aarch64-apk",
            "android-aarch64",
            "android-universal",
            "android",
        ]
    );
    assert_eq!(
        super::TargetResolver::target_candidates_for("linux", "x86_64", Some("deb")),
        vec!["linux-x86_64-deb", "linux-x86_64"]
    );
}
