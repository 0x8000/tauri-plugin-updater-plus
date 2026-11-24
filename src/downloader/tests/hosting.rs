use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use url::Url;

use super::super::transfer::TransferExecutor;
use super::super::{DifferentialDownloader, TransferOptions};
use super::{
    MockServerState, ServerRangeMode, build_v1_and_v2, make_request, seeded_cache, spawn_server,
};
use crate::blockmap::BlockMap;
use crate::chunker::ChunkerConfig;
use crate::listener::{DownloadMode, DummyListener};

#[test]
fn detects_known_single_range_hosts_for_github_and_object_stores() {
    for url in [
        "https://github.com/example/myapp/releases/latest/download/myapp.AppImage",
        "https://api.github.com/repos/example/myapp/releases/assets/123",
        "https://objects.githubusercontent.com/github-production-release-asset/foo",
        "https://release-assets.githubusercontent.com/github-production-release-asset/foo",
        "https://myapp-updates.s3.us-east-1.amazonaws.com/myapp.zip",
        "https://abc123.r2.cloudflarestorage.com/myapp.zip",
        "https://myapp.oss-cn-hangzhou.aliyuncs.com/myapp.zip",
        "https://myapp.blob.core.windows.net/releases/myapp.zip",
        "https://storage.googleapis.com/myapp/myapp.zip",
    ] {
        let parsed = Url::parse(url).expect("url");
        assert!(
            TransferExecutor::is_known_single_range_host(&parsed),
            "expected {url} to prefer single-range"
        );
    }

    for url in [
        "https://updates.myapp.example/myapp.AppImage",
        "http://127.0.0.1:8080/myapp.zip",
        "https://cdn.my-static-host.org/releases/app.zip",
    ] {
        let parsed = Url::parse(url).expect("url");
        assert!(
            !TransferExecutor::is_known_single_range_host(&parsed),
            "expected {url} to allow multi-range probing"
        );
    }
}

#[tokio::test]
async fn degrades_s3_style_200_on_multi_range_into_single_range_differential_download() {
    let (v1, v2) = build_v1_and_v2();
    let v2_artifact =
        BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("v2 map");
    let state = MockServerState::new(
        v2.clone(),
        Some(v2_artifact.blockmap.to_gzip_bytes().expect("gz")),
        ServerRangeMode::SingleOnly200OnMulti,
    );
    let base_url = spawn_server(state.clone()).await;

    let temp = tempfile::tempdir().expect("temp dir");
    let cache = seeded_cache(&temp, &v1);
    let downloader = DifferentialDownloader::new(
        reqwest::Client::new(),
        HeaderMap::new(),
        cache,
        TransferOptions::default(),
    );
    let request = make_request(&base_url, v2.len() as u64, Some(v2_artifact.sha512));

    let result = downloader
        .download(&request, &DummyListener)
        .await
        .expect("S3/GitHub 200-on-multi must degrade to single-range differential");
    assert_eq!(result.summary.mode, DownloadMode::Differential);
    assert!(result.summary.saved_percent > 75.0);
    assert!(state.bytes_sent.load(Ordering::SeqCst) < v2.len() / 3);
    assert_eq!(std::fs::read(&result.path).expect("read"), v2);
}

#[tokio::test]
async fn explicit_disable_multi_range_skips_multipart_probe() {
    let (v1, v2) = build_v1_and_v2();
    let v2_artifact =
        BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("v2 map");
    let state = MockServerState::new(
        v2.clone(),
        Some(v2_artifact.blockmap.to_gzip_bytes().expect("gz")),
        ServerRangeMode::MultipartAndSingle,
    );
    let base_url = spawn_server(state.clone()).await;

    let temp = tempfile::tempdir().expect("temp dir");
    let cache = seeded_cache(&temp, &v1);
    let downloader = DifferentialDownloader::new(
        reqwest::Client::new(),
        HeaderMap::new(),
        cache,
        TransferOptions {
            use_multiple_range_request: Some(false),
            max_concurrent_ranges: 4,
            ..TransferOptions::default()
        },
    );
    let request = make_request(&base_url, v2.len() as u64, Some(v2_artifact.sha512));

    let result = downloader
        .download(&request, &DummyListener)
        .await
        .expect("forced single-range differential");
    assert_eq!(result.summary.mode, DownloadMode::Differential);
    let seen = state.received_ranges.lock().expect("lock");
    assert!(seen.len() >= 2);
    assert!(
        seen.iter().flatten().all(|hdr| !hdr.contains(',')),
        "no multi-range header should be sent when disabled: {seen:?}"
    );
    assert_eq!(std::fs::read(&result.path).expect("read"), v2);
}

#[derive(Clone)]
struct CdnState {
    artifact: Arc<Vec<u8>>,
    v1_hits: Arc<AtomicUsize>,
    v2_hits: Arc<AtomicUsize>,
    leaked_auth_headers: Arc<AtomicUsize>,
}

async fn serve_cdn_asset(
    State(state): State<CdnState>,
    AxumPath(token): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if headers.contains_key(header::AUTHORIZATION) {
        state.leaked_auth_headers.fetch_add(1, Ordering::SeqCst);
        return (StatusCode::BAD_REQUEST, "foreign auth header rejected").into_response();
    }
    if token == "signed-v1" {
        let hit = state.v1_hits.fetch_add(1, Ordering::SeqCst);
        if hit >= 1 {
            return (StatusCode::FORBIDDEN, "signed URL expired").into_response();
        }
    } else if token == "signed-v2" {
        state.v2_hits.fetch_add(1, Ordering::SeqCst);
    } else {
        return (StatusCode::NOT_FOUND, "unknown token").into_response();
    }

    let Some(range_hdr) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
        return (StatusCode::OK, (*state.artifact).clone()).into_response();
    };
    let Some((s, e)) = range_hdr.trim_start_matches("bytes=").split_once('-') else {
        return (StatusCode::BAD_REQUEST, "invalid single range").into_response();
    };
    let (Ok(start), Ok(end)) = (s.parse::<usize>(), e.parse::<usize>()) else {
        return (StatusCode::BAD_REQUEST, "non-numeric range").into_response();
    };
    let total = state.artifact.len();
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")).expect("hdr"),
    );
    (
        StatusCode::PARTIAL_CONTENT,
        resp_headers,
        state.artifact[start..=end].to_vec(),
    )
        .into_response()
}

#[derive(Clone)]
struct OriginState {
    cdn_base: Url,
    blockmap: Arc<Vec<u8>>,
    redirect_hits: Arc<AtomicUsize>,
    origin_auth_hits: Arc<AtomicUsize>,
}

async fn serve_origin_release(State(state): State<OriginState>, headers: HeaderMap) -> Response {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        == Some("Bearer gh-secret")
    {
        state.origin_auth_hits.fetch_add(1, Ordering::SeqCst);
    }
    let hit = state.redirect_hits.fetch_add(1, Ordering::SeqCst);
    let token = if hit == 0 { "signed-v1" } else { "signed-v2" };
    let target = state
        .cdn_base
        .join(&format!("cdn/{token}/app.pkg"))
        .expect("cdn url");
    Redirect::temporary(target.as_str()).into_response()
}

async fn serve_origin_blockmap(State(state): State<OriginState>) -> Response {
    (StatusCode::OK, (*state.blockmap).clone()).into_response()
}

#[tokio::test]
async fn reuses_github_redirect_url_recovers_on_expiry_and_strips_cross_origin_auth() {
    let mut state_rng = 0x5566_7788u64;
    let mut v1 = Vec::with_capacity(2_000_000);
    for _ in 0..2_000_000 {
        state_rng = state_rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        v1.push((state_rng >> 56) as u8);
    }
    let mut v2 = v1.clone();
    // Create 4 well-separated mutated ranges to verify redirect reuse + expiry refresh.
    v2[100_000..102_000].fill(0x11);
    v2[600_000..602_000].fill(0x22);
    v2[1_100_000..1_102_000].fill(0x33);
    v2[1_600_000..1_602_000].fill(0x44);

    let v2_artifact =
        BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("v2 map");
    let cdn_state = CdnState {
        artifact: Arc::new(v2.clone()),
        v1_hits: Arc::new(AtomicUsize::new(0)),
        v2_hits: Arc::new(AtomicUsize::new(0)),
        leaked_auth_headers: Arc::new(AtomicUsize::new(0)),
    };
    let cdn_app = Router::new()
        .route("/cdn/{token}/app.pkg", get(serve_cdn_asset))
        .with_state(cdn_state.clone());
    let cdn_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind cdn");
    let cdn_base = Url::parse(&format!(
        "http://{}/",
        cdn_listener.local_addr().expect("addr")
    ))
    .expect("cdn url");
    tokio::spawn(async move {
        let _ = axum::serve(cdn_listener, cdn_app).await;
    });

    let origin_state = OriginState {
        cdn_base,
        blockmap: Arc::new(v2_artifact.blockmap.to_gzip_bytes().expect("gz")),
        redirect_hits: Arc::new(AtomicUsize::new(0)),
        origin_auth_hits: Arc::new(AtomicUsize::new(0)),
    };
    let origin_app = Router::new()
        .route("/app.pkg", get(serve_origin_release))
        .route("/app.pkg.blockmap", get(serve_origin_blockmap))
        .with_state(origin_state.clone());
    let origin_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind origin");
    let origin_base = Url::parse(&format!(
        "http://{}/",
        origin_listener.local_addr().expect("addr")
    ))
    .expect("origin url");
    tokio::spawn(async move {
        let _ = axum::serve(origin_listener, origin_app).await;
    });

    let temp = tempfile::tempdir().expect("temp dir");
    let cache = seeded_cache(&temp, &v1);
    let mut auth_headers = HeaderMap::new();
    auth_headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer gh-secret"),
    );

    let downloader = DifferentialDownloader::new(
        reqwest::Client::new(),
        auth_headers,
        cache,
        TransferOptions {
            use_multiple_range_request: Some(false),
            max_concurrent_ranges: 1,
            ..TransferOptions::default()
        },
    );
    let request = make_request(&origin_base, v2.len() as u64, Some(v2_artifact.sha512));

    let result = downloader
        .download(&request, &DummyListener)
        .await
        .expect("redirect reuse + expiry refresh differential download");
    assert_eq!(result.summary.mode, DownloadMode::Differential);
    assert!(result.summary.saved_percent > 70.0);
    assert_eq!(std::fs::read(&result.path).expect("read"), v2);

    // Auth reached origin on redirects, and NEVER leaked to the cross-origin CDN.
    assert_eq!(cdn_state.leaked_auth_headers.load(Ordering::SeqCst), 0);
    assert_eq!(origin_state.origin_auth_hits.load(Ordering::SeqCst), 2);
    // Only 2 redirects total across 4 ranges: initial -> signed-v1, expiry -> signed-v2, then reused!
    assert_eq!(origin_state.redirect_hits.load(Ordering::SeqCst), 2);
    assert!(cdn_state.v2_hits.load(Ordering::SeqCst) >= 3);
}
