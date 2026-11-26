use std::sync::atomic::Ordering;

use axum::http::HeaderMap;

use super::super::{DifferentialDownloader, TransferOptions};
use super::{
    MockServerState, ServerRangeMode, build_v1_and_v2, make_request, seeded_cache, spawn_server,
};
use crate::Error;
use crate::blockmap::BlockMap;
use crate::chunker::ChunkerConfig;
use crate::listener::{DownloadMode, DummyListener};

#[tokio::test]
async fn surfaces_http_status_error_and_handles_auth_status_boundaries() {
    let (v1, v2) = build_v1_and_v2();
    let v2_artifact =
        BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("v2 map");
    let gz_map = v2_artifact.blockmap.to_gzip_bytes().expect("gz");

    let err_state = MockServerState::new(v2.clone(), None, ServerRangeMode::Error500);
    let err_url = spawn_server(err_state).await;
    let temp = tempfile::tempdir().expect("temp dir");
    let downloader = DifferentialDownloader::http(
        HeaderMap::new(),
        seeded_cache(&temp, &v1),
        TransferOptions::default(),
    )
    .expect("downloader");
    let err_req = make_request(&err_url, v2.len() as u64, Some(v2_artifact.sha512.clone()));
    assert!(matches!(
        downloader.download(&err_req, &DummyListener).await,
        Err(Error::HttpStatus { status: 500, .. })
    ));

    // 403 on `.blockmap` (private S3 without s3:ListBucket or presigned URL) falls back to full.
    let bm403_state = MockServerState::new(
        v2.clone(),
        Some(gz_map.clone()),
        ServerRangeMode::BlockmapForbidden,
    );
    let bm403_url = spawn_server(bm403_state).await;
    let temp403 = tempfile::tempdir().expect("temp dir");
    let dl403 = DifferentialDownloader::http(
        HeaderMap::new(),
        seeded_cache(&temp403, &v1),
        TransferOptions::default(),
    )
    .expect("downloader");
    let req403 = make_request(
        &bm403_url,
        v2.len() as u64,
        Some(v2_artifact.sha512.clone()),
    );
    let res403 = dl403
        .download(&req403, &DummyListener)
        .await
        .expect("403 on blockmap must fall back cleanly to verified full download");
    assert_eq!(res403.summary.mode, DownloadMode::Full);
    assert!(
        res403
            .summary
            .fallback_reason
            .as_deref()
            .is_some_and(|r| r.contains("403")),
        "got {:?}",
        res403.summary.fallback_reason
    );
    assert_eq!(std::fs::read(&res403.path).expect("read"), v2);

    // 401 on `.blockmap` fails fast without invoking full download.
    let bm401_state = MockServerState::new(
        v2.clone(),
        Some(gz_map.clone()),
        ServerRangeMode::BlockmapUnauthorized,
    );
    let bm401_url = spawn_server(bm401_state.clone()).await;
    let temp401 = tempfile::tempdir().expect("temp dir");
    let dl401 = DifferentialDownloader::http(
        HeaderMap::new(),
        seeded_cache(&temp401, &v1),
        TransferOptions::default(),
    )
    .expect("downloader");
    let req401 = make_request(
        &bm401_url,
        v2.len() as u64,
        Some(v2_artifact.sha512.clone()),
    );
    assert!(matches!(
        dl401.download(&req401, &DummyListener).await,
        Err(Error::HttpStatus { status: 401, .. })
    ));
    assert_eq!(bm401_state.requests.load(Ordering::SeqCst), 0);

    // 403 on artifact range request fails fast (no fallback to fetch_full).
    let range403_state = MockServerState::new(
        v2.clone(),
        Some(gz_map),
        ServerRangeMode::AuthForbiddenRange,
    );
    let range403_url = spawn_server(range403_state.clone()).await;
    let temp_r403 = tempfile::tempdir().expect("temp dir");
    let dl_r403 = DifferentialDownloader::http(
        HeaderMap::new(),
        seeded_cache(&temp_r403, &v1),
        TransferOptions::default(),
    )
    .expect("downloader");
    let req_r403 = make_request(&range403_url, v2.len() as u64, Some(v2_artifact.sha512));
    assert!(matches!(
        dl_r403.download(&req_r403, &DummyListener).await,
        Err(Error::HttpStatus { status: 403, .. })
    ));
    assert_eq!(
        range403_state.requests.load(Ordering::SeqCst),
        1,
        "403 on artifact range must fail fast without falling back to full download"
    );
}
