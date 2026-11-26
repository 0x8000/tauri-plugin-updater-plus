use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use url::Url;

pub(crate) struct PublishedHop {
    pub(crate) pkg_name: String,
    pub(crate) pkg_path: PathBuf,
    pub(crate) blockmap_name: String,
    pub(crate) blockmap_bytes: Vec<u8>,
    pub(crate) manifest_bytes: Vec<u8>,
}

#[derive(Default)]
struct ActiveHop {
    files: HashMap<String, PathBuf>,
    memory: HashMap<String, Vec<u8>>,
}

#[derive(Clone)]
struct ServerState {
    active: Arc<RwLock<ActiveHop>>,
    wire_bytes: Arc<AtomicUsize>,
    artifact_requests: Arc<AtomicUsize>,
}

pub(crate) struct BenchServer {
    base_url: Url,
    state: ServerState,
}

impl BenchServer {
    pub(crate) async fn start() -> Result<Self, Box<dyn std::error::Error>> {
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = tcp.local_addr()?;
        let base_url = Url::parse(&format!("http://{addr}/"))?;
        let state = ServerState {
            active: Arc::new(RwLock::new(ActiveHop::default())),
            wire_bytes: Arc::new(AtomicUsize::new(0)),
            artifact_requests: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/{name}", get(serve_bench_file))
            .with_state(state.clone());
        tokio::spawn(async move {
            let _ = axum::serve(tcp, app).await;
        });
        Ok(Self { base_url, state })
    }

    pub(crate) fn base_url(&self) -> &Url {
        &self.base_url
    }

    pub(crate) fn publish_hop(&self, published: PublishedHop) {
        self.state.wire_bytes.store(0, Ordering::SeqCst);
        self.state.artifact_requests.store(0, Ordering::SeqCst);
        let mut active = self.state.active.write().expect("RwLock poisoned");
        active.files.clear();
        active.files.insert(published.pkg_name, published.pkg_path);
        active.memory.clear();
        active
            .memory
            .insert("latest.json".to_string(), published.manifest_bytes);
        active
            .memory
            .insert(published.blockmap_name, published.blockmap_bytes);
    }

    pub(crate) fn wire_bytes(&self) -> u64 {
        self.state.wire_bytes.load(Ordering::SeqCst) as u64
    }

    pub(crate) fn artifact_requests(&self) -> usize {
        self.state.artifact_requests.load(Ordering::SeqCst)
    }
}

async fn serve_bench_file(
    State(state): State<ServerState>,
    AxumPath(name): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    let (mem_hit, file_path) = {
        let active = state.active.read().expect("RwLock poisoned");
        (
            active.memory.get(&name).cloned(),
            active.files.get(&name).cloned(),
        )
    };
    if let Some(bytes) = mem_hit {
        return (StatusCode::OK, bytes).into_response();
    }
    let Some(path) = file_path else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    state.artifact_requests.fetch_add(1, Ordering::SeqCst);

    let total = match std::fs::metadata(&path) {
        Ok(meta) => meta.len(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "stat failed").into_response(),
    };

    let Some(range_hdr) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
        let data = std::fs::read(&path).unwrap_or_default();
        state.wire_bytes.fetch_add(data.len(), Ordering::SeqCst);
        return (StatusCode::OK, data).into_response();
    };

    let ranges = parse_byte_ranges(range_hdr);
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "open failed").into_response(),
    };

    if ranges.len() == 1 {
        return serve_single_range(&mut file, ranges[0], total, &state.wire_bytes);
    }
    serve_multipart_ranges(&mut file, &ranges, total, &state.wire_bytes)
}

fn parse_byte_ranges(range_hdr: &str) -> Vec<(u64, u64)> {
    range_hdr
        .trim_start_matches("bytes=")
        .split(',')
        .filter_map(|part| {
            let (s, e) = part.trim().split_once('-')?;
            Some((s.parse().ok()?, e.parse().ok()?))
        })
        .collect()
}

fn serve_single_range(
    file: &mut File,
    range: (u64, u64),
    total: u64,
    wire_bytes: &AtomicUsize,
) -> Response {
    let (start, end) = range;
    let mut buf = vec![0u8; (end - start + 1) as usize];
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut buf).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "read failed").into_response();
    }
    wire_bytes.fetch_add(buf.len(), Ordering::SeqCst);
    let mut resp_headers = HeaderMap::new();
    if let Ok(val) = HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")) {
        resp_headers.insert(header::CONTENT_RANGE, val);
    }
    (StatusCode::PARTIAL_CONTENT, resp_headers, buf).into_response()
}

fn serve_multipart_ranges(
    file: &mut File,
    ranges: &[(u64, u64)],
    total: u64,
    wire_bytes: &AtomicUsize,
) -> Response {
    let boundary = "UPDATER_BENCH_BOUNDARY";
    let mut body = Vec::new();
    for &(start, end) in ranges {
        let mut slice = vec![0u8; (end - start + 1) as usize];
        if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut slice).is_err() {
            return (StatusCode::INTERNAL_SERVER_ERROR, "read failed").into_response();
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {start}-{end}/{total}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(&slice);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    wire_bytes.fetch_add(body.len(), Ordering::SeqCst);

    let mut resp_headers = HeaderMap::new();
    if let Ok(val) = HeaderValue::from_str(&format!("multipart/byteranges; boundary={boundary}")) {
        resp_headers.insert(header::CONTENT_TYPE, val);
    }
    (StatusCode::PARTIAL_CONTENT, resp_headers, body).into_response()
}
