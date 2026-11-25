use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use clap::Parser;
use semver::Version;
use updater_plus::{
    ArtifactVerifier, BlockMap, ChunkerConfig, DummyListener, PackageCache, UpdaterBuilder,
};
use url::Url;

#[derive(Parser)]
#[command(about = "Run a real local HTTP blockmap differential update benchmark")]
struct Args {
    old_artifact: PathBuf,
    new_artifact: PathBuf,
}

#[derive(Clone)]
struct BenchServerState {
    files: Arc<HashMap<String, PathBuf>>,
    memory: Arc<HashMap<String, Vec<u8>>>,
    wire_bytes: Arc<AtomicUsize>,
    artifact_requests: Arc<AtomicUsize>,
}

async fn serve_bench_file(
    State(state): State<BenchServerState>,
    AxumPath(name): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(bytes) = state.memory.get(&name) {
        return (StatusCode::OK, bytes.clone()).into_response();
    }
    let Some(path) = state.files.get(&name) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    state.artifact_requests.fetch_add(1, Ordering::SeqCst);

    let total = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "stat failed").into_response(),
    };

    let Some(range_hdr) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
        let data = std::fs::read(path).unwrap_or_default();
        state.wire_bytes.fetch_add(data.len(), Ordering::SeqCst);
        return (StatusCode::OK, data).into_response();
    };

    let ranges: Vec<(u64, u64)> = range_hdr
        .trim_start_matches("bytes=")
        .split(',')
        .filter_map(|part| {
            let (s, e) = part.trim().split_once('-')?;
            Some((s.parse().ok()?, e.parse().ok()?))
        })
        .collect();

    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "open failed").into_response(),
    };

    if ranges.len() == 1 {
        let (start, end) = ranges[0];
        let mut buf = vec![0u8; (end - start + 1) as usize];
        if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut buf).is_err() {
            return (StatusCode::INTERNAL_SERVER_ERROR, "read failed").into_response();
        }
        state.wire_bytes.fetch_add(buf.len(), Ordering::SeqCst);
        let mut resp_headers = HeaderMap::new();
        if let Ok(val) = HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")) {
            resp_headers.insert(header::CONTENT_RANGE, val);
        }
        return (StatusCode::PARTIAL_CONTENT, resp_headers, buf).into_response();
    }

    let boundary = "UPDATER_BENCH_BOUNDARY";
    let mut body = Vec::new();
    for (start, end) in ranges {
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
    state.wire_bytes.fetch_add(body.len(), Ordering::SeqCst);

    let mut resp_headers = HeaderMap::new();
    if let Ok(val) = HeaderValue::from_str(&format!("multipart/byteranges; boundary={boundary}")) {
        resp_headers.insert(header::CONTENT_TYPE, val);
    }
    (StatusCode::PARTIAL_CONTENT, resp_headers, body).into_response()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let old_path = args.old_artifact;
    let new_path = args.new_artifact;

    let gen_start = Instant::now();
    let new_artifact = BlockMap::generate_from_path(&new_path, ChunkerConfig::default())?;
    let new_blockmap_gz = new_artifact.blockmap.to_gzip_bytes()?;
    let gen_ms = gen_start.elapsed().as_millis();

    let temp_dir = tempfile::tempdir()?;
    let cache = PackageCache::new(temp_dir.path().join("cache"));
    let seed_start = Instant::now();
    cache.seed_from_file(&old_path, "0.1.0")?;
    let seed_ms = seed_start.elapsed().as_millis();

    let wire_bytes = Arc::new(AtomicUsize::new(0));
    let artifact_requests = Arc::new(AtomicUsize::new(0));

    let mut files = HashMap::new();
    files.insert("myapp-0.2.0.pkg".to_string(), new_path.clone());

    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = tcp.local_addr()?;
    let base_url = Url::parse(&format!("http://{addr}/"))?;

    let manifest_json = serde_json::json!({
        "version": "0.2.0",
        "notes": "Real artifact blockmap differential update benchmark",
        "pub_date": "2026-11-24T00:00:00Z",
        "platforms": {
            "linux-x86_64": {
                "url": base_url.join("myapp-0.2.0.pkg")?,
                "sha512": new_artifact.sha512,
                "size": new_artifact.size,
                "blockmap_size": new_blockmap_gz.len()
            }
        }
    });

    let mut memory = HashMap::new();
    memory.insert(
        "latest.json".to_string(),
        serde_json::to_vec(&manifest_json)?,
    );
    memory.insert(
        "myapp-0.2.0.pkg.blockmap".to_string(),
        new_blockmap_gz.clone(),
    );

    let state = BenchServerState {
        files: Arc::new(files),
        memory: Arc::new(memory),
        wire_bytes: wire_bytes.clone(),
        artifact_requests: artifact_requests.clone(),
    };
    let app = Router::new()
        .route("/{name}", get(serve_bench_file))
        .with_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(tcp, app).await;
    });

    let updater = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(0, 1, 0),
        temp_dir.path().join("MyApp.AppImage"),
        cache,
    )
    .target("linux-x86_64")
    .endpoints(vec![base_url.join("latest.json")?])
    .build()?;

    let update_start = Instant::now();
    let update = updater
        .check()
        .await?
        .ok_or("expected update 0.2.0 to be available")?;
    let downloaded = update.download(&DummyListener).await?;
    let update_ms = update_start.elapsed().as_millis();

    let rebuilt_sha512 = ArtifactVerifier::sha512_file(&downloaded.path)?;
    assert_eq!(rebuilt_sha512, new_artifact.sha512);

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "mode": downloaded.summary.mode,
            "totalBytes": downloaded.summary.total_bytes,
            "reusedBytes": downloaded.summary.reused_bytes,
            "downloadedPayloadBytes": downloaded.summary.downloaded_bytes,
            "httpWireBytes": wire_bytes.load(Ordering::SeqCst),
            "blockmapBytes": new_blockmap_gz.len(),
            "blockCount": new_artifact.blockmap.block_count(),
            "httpRequests": artifact_requests.load(Ordering::SeqCst),
            "savedPercent": (downloaded.summary.saved_percent * 100.0).round() / 100.0,
            "blockmapGenMs": gen_ms,
            "seedOldMs": seed_ms,
            "updateWallMs": update_ms,
            "sha512Verified": true,
        }))?
    );
    Ok(())
}
