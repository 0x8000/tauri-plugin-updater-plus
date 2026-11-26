use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use url::Url;

use super::DownloadRequest;
use crate::cache::PackageCache;
use crate::listener::{DownloadEvent, DownloadListener};
use crate::verifier::VerifyExpectation;

mod auth;
mod differential;
mod fallback;
mod hosting;

#[derive(Clone, Copy)]
pub(super) enum ServerRangeMode {
    MultipartAndSingle,
    SingleOnly501,
    SingleOnly200OnMulti,
    NoRangeSupport,
    NoRangeAcceptNone,
    Error500,
    AuthForbiddenRange,
    BlockmapForbidden,
    BlockmapUnauthorized,
}

#[derive(Clone)]
pub(super) struct MockServerState {
    pub(super) artifact: Arc<Vec<u8>>,
    pub(super) blockmap: Option<Arc<Vec<u8>>>,
    pub(super) mode: ServerRangeMode,
    pub(super) bytes_sent: Arc<AtomicUsize>,
    pub(super) requests: Arc<AtomicUsize>,
    pub(super) blockmap_requests: Arc<AtomicUsize>,
    pub(super) received_ranges: Arc<Mutex<Vec<Option<String>>>>,
}

impl MockServerState {
    pub(super) fn new(artifact: Vec<u8>, blockmap: Option<Vec<u8>>, mode: ServerRangeMode) -> Self {
        Self {
            artifact: Arc::new(artifact),
            blockmap: blockmap.map(Arc::new),
            mode,
            bytes_sent: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(AtomicUsize::new(0)),
            blockmap_requests: Arc::new(AtomicUsize::new(0)),
            received_ranges: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

async fn serve_artifact(State(state): State<MockServerState>, headers: HeaderMap) -> Response {
    state.requests.fetch_add(1, Ordering::SeqCst);
    let range_str = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    state
        .received_ranges
        .lock()
        .expect("lock")
        .push(range_str.clone());

    if matches!(state.mode, ServerRangeMode::Error500) {
        return (StatusCode::INTERNAL_SERVER_ERROR, "server failure").into_response();
    }
    if matches!(state.mode, ServerRangeMode::AuthForbiddenRange) && range_str.is_some() {
        return (StatusCode::FORBIDDEN, "artifact range forbidden").into_response();
    }

    let total = state.artifact.len();
    if matches!(state.mode, ServerRangeMode::NoRangeAcceptNone) {
        state.bytes_sent.fetch_add(total, Ordering::SeqCst);
        let mut resp_headers = HeaderMap::new();
        resp_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
        return (StatusCode::OK, resp_headers, (*state.artifact).clone()).into_response();
    }
    if matches!(state.mode, ServerRangeMode::NoRangeSupport) {
        state.bytes_sent.fetch_add(total, Ordering::SeqCst);
        return (StatusCode::OK, (*state.artifact).clone()).into_response();
    }

    let Some(range_header) = range_str else {
        state.bytes_sent.fetch_add(total, Ordering::SeqCst);
        return (StatusCode::OK, (*state.artifact).clone()).into_response();
    };

    let spec = range_header.trim_start_matches("bytes=");
    let parsed: Vec<(usize, usize)> = spec
        .split(',')
        .filter_map(|part| {
            let (s, e) = part.trim().split_once('-')?;
            Some((s.parse().ok()?, e.parse().ok()?))
        })
        .collect();

    if parsed.iter().any(|&(s, e)| s > e || e >= total) {
        return (StatusCode::RANGE_NOT_SATISFIABLE, "range out of bounds").into_response();
    }

    if parsed.len() > 1 && matches!(state.mode, ServerRangeMode::SingleOnly501) {
        return (StatusCode::NOT_IMPLEMENTED, "multi-range disabled").into_response();
    }

    if parsed.len() > 1 && matches!(state.mode, ServerRangeMode::SingleOnly200OnMulti) {
        let mut resp_headers = HeaderMap::new();
        resp_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        return (StatusCode::OK, resp_headers, (*state.artifact).clone()).into_response();
    }

    if parsed.len() == 1 {
        let (start, end) = parsed[0];
        let slice = state.artifact[start..=end].to_vec();
        state.bytes_sent.fetch_add(slice.len(), Ordering::SeqCst);
        let mut resp_headers = HeaderMap::new();
        resp_headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")).expect("header"),
        );
        return (StatusCode::PARTIAL_CONTENT, resp_headers, slice).into_response();
    }

    let boundary = "UPDATER_TEST_BOUNDARY";
    let mut body = Vec::new();
    for (start, end) in parsed {
        let slice = &state.artifact[start..=end];
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {start}-{end}/{total}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(slice);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    state.bytes_sent.fetch_add(body.len(), Ordering::SeqCst);

    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&format!("multipart/byteranges; boundary={boundary}")).expect("hdr"),
    );
    (StatusCode::PARTIAL_CONTENT, resp_headers, body).into_response()
}

async fn serve_blockmap(State(state): State<MockServerState>) -> Response {
    state.blockmap_requests.fetch_add(1, Ordering::SeqCst);
    if matches!(state.mode, ServerRangeMode::BlockmapForbidden) {
        return (StatusCode::FORBIDDEN, "missing object on private S3").into_response();
    }
    if matches!(state.mode, ServerRangeMode::BlockmapUnauthorized) {
        return (StatusCode::UNAUTHORIZED, "expired token").into_response();
    }
    match &state.blockmap {
        Some(bytes) => (StatusCode::OK, (**bytes).clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "missing blockmap").into_response(),
    }
}

pub(super) async fn spawn_server(state: MockServerState) -> Url {
    let app = Router::new()
        .route("/app.pkg", get(serve_artifact))
        .route("/app.pkg.blockmap", get(serve_blockmap))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Url::parse(&format!("http://{addr}/")).expect("url")
}

pub(super) fn build_v1_and_v2() -> (Vec<u8>, Vec<u8>) {
    let mut state = 0x1122_3344u64;
    let mut v1 = Vec::with_capacity(1_200_000);
    for _ in 0..1_200_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        v1.push((state >> 56) as u8);
    }
    let mut v2 = v1.clone();
    v2[100_000..104_096].fill(0x5a);
    v2[800_000..804_096].fill(0xa5);
    (v1, v2)
}

pub(super) fn seeded_cache(temp: &tempfile::TempDir, v1: &[u8]) -> PackageCache {
    let cache = PackageCache::with_appimage_fallback(temp.path().join("cache"), None);
    let v1_path = temp.path().join("v1.pkg");
    std::fs::write(&v1_path, v1).expect("write v1");
    cache.seed_from_file(&v1_path, "0.1.0").expect("seed v1");
    cache
}

pub(super) fn make_request(base_url: &Url, size: u64, sha512: Option<String>) -> DownloadRequest {
    DownloadRequest {
        version: "0.2.0".into(),
        artifact_url: base_url.join("app.pkg").expect("url"),
        blockmap_url: base_url.join("app.pkg.blockmap").expect("url"),
        expected_size: Some(size),
        verification: VerifyExpectation {
            sha512,
            announced_version: "0.2.0".into(),
            ..Default::default()
        },
        differential: true,
        full_download_threshold: 0.85,
    }
}

pub(super) fn ranges_cover_offset(received: &[Option<String>], target: usize) -> bool {
    received.iter().flatten().any(|hdr| {
        hdr.trim_start_matches("bytes=").split(',').any(|part| {
            let Some((s, e)) = part.trim().split_once('-') else {
                return false;
            };
            let (Ok(start), Ok(end)) = (s.parse::<usize>(), e.parse::<usize>()) else {
                return false;
            };
            start <= target && target <= end
        })
    })
}

pub(super) struct EventRecorder(pub(super) Mutex<Vec<DownloadEvent>>);

impl DownloadListener for EventRecorder {
    fn on_event(&self, event: DownloadEvent) {
        self.0.lock().expect("lock").push(event);
    }
}
