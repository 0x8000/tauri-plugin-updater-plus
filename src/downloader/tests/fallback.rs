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
async fn falls_back_to_full_when_server_ignores_range_headers() {
    let (v1, v2) = build_v1_and_v2();
    let v2_artifact =
        BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("v2 map");
    let state = MockServerState::new(
        v2.clone(),
        Some(v2_artifact.blockmap.to_gzip_bytes().expect("gz")),
        ServerRangeMode::NoRangeSupport,
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
    let request = make_request(&base_url, v2.len() as u64, Some(v2_artifact.sha512.clone()));

    let result = downloader
        .download(&request, &DummyListener)
        .await
        .expect("200 OK fallback");
    assert_eq!(result.summary.mode, DownloadMode::Full);
    assert!(
        result
            .summary
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("Range")),
        "unexpected fallback reason: {:?}",
        result.summary.fallback_reason
    );
    // Probed multi-range (got 200), then confirmed single-range also returned 200 and streamed it.
    assert_eq!(state.requests.load(Ordering::SeqCst), 2);
    {
        let seen = state.received_ranges.lock().expect("lock");
        assert!(seen.iter().all(Option::is_some));
    }
    assert_eq!(std::fs::read(&result.path).expect("read"), v2);

    // When a server declares `Accept-Ranges: none`, fallback reuses the first 200 OK directly.
    let none_state = MockServerState::new(
        v2.clone(),
        Some(v2_artifact.blockmap.to_gzip_bytes().expect("gz")),
        ServerRangeMode::NoRangeAcceptNone,
    );
    let none_url = spawn_server(none_state.clone()).await;
    let temp2 = tempfile::tempdir().expect("temp dir");
    let cache2 = seeded_cache(&temp2, &v1);
    let dl2 = DifferentialDownloader::new(
        reqwest::Client::new(),
        HeaderMap::new(),
        cache2,
        TransferOptions::default(),
    );
    let req2 = make_request(&none_url, v2.len() as u64, Some(v2_artifact.sha512));
    let res2 = dl2
        .download(&req2, &DummyListener)
        .await
        .expect("Accept-Ranges: none fast fallback");
    assert_eq!(res2.summary.mode, DownloadMode::Full);
    assert_eq!(none_state.requests.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(&res2.path).expect("read"), v2);
}

#[tokio::test]
async fn falls_back_to_full_when_remote_blockmap_returns_404_or_differential_disabled() {
    let (v1, v2) = build_v1_and_v2();
    let v2_artifact =
        BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("v2 map");
    let state = MockServerState::new(v2.clone(), None, ServerRangeMode::MultipartAndSingle);
    let base_url = spawn_server(state).await;

    let temp = tempfile::tempdir().expect("temp dir");
    let cache = seeded_cache(&temp, &v1);
    let downloader = DifferentialDownloader::new(
        reqwest::Client::new(),
        HeaderMap::new(),
        cache,
        TransferOptions::default(),
    );
    let request = make_request(&base_url, v2.len() as u64, Some(v2_artifact.sha512.clone()));

    let result = downloader
        .download(&request, &DummyListener)
        .await
        .expect("404 blockmap fallback");
    assert_eq!(result.summary.mode, DownloadMode::Full);
    assert_eq!(result.summary.reused_bytes, 0);
    assert_eq!(result.summary.downloaded_bytes, v2.len() as u64);
    assert!(
        result
            .summary
            .fallback_reason
            .as_deref()
            .is_some_and(|r| r.contains("404")),
        "expected 404 fallback reason, got {:?}",
        result.summary.fallback_reason
    );
    assert_eq!(std::fs::read(&result.path).expect("read"), v2);

    let state_disabled = MockServerState::new(
        v2.clone(),
        Some(v2_artifact.blockmap.to_gzip_bytes().expect("gz")),
        ServerRangeMode::MultipartAndSingle,
    );
    let disabled_url = spawn_server(state_disabled.clone()).await;
    let temp2 = tempfile::tempdir().expect("temp dir");
    let cache2 = seeded_cache(&temp2, &v1);
    let dl2 = DifferentialDownloader::new(
        reqwest::Client::new(),
        HeaderMap::new(),
        cache2,
        TransferOptions::default(),
    );
    let mut disabled_req = make_request(&disabled_url, v2.len() as u64, Some(v2_artifact.sha512));
    disabled_req.differential = false;
    let res2 = dl2
        .download(&disabled_req, &DummyListener)
        .await
        .expect("disabled differential full download");
    assert_eq!(res2.summary.mode, DownloadMode::Full);
    assert_eq!(state_disabled.blockmap_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rejects_tampered_artifact_without_overwriting_cached_base_package() {
    let (v1, v2) = build_v1_and_v2();
    let v2_artifact =
        BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("v2 map");
    let mut tampered = v2.clone();
    tampered[100_000] ^= 0xff;
    let state = MockServerState::new(
        tampered,
        Some(v2_artifact.blockmap.to_gzip_bytes().expect("gz")),
        ServerRangeMode::MultipartAndSingle,
    );
    let base_url = spawn_server(state).await;

    let temp = tempfile::tempdir().expect("temp dir");
    let cache = seeded_cache(&temp, &v1);
    let downloader = DifferentialDownloader::new(
        reqwest::Client::new(),
        HeaderMap::new(),
        cache.clone(),
        TransferOptions::default(),
    );
    let request = make_request(&base_url, v2.len() as u64, Some(v2_artifact.sha512));

    let err = downloader
        .download(&request, &DummyListener)
        .await
        .expect_err("tampered artifact must fail SHA-512 verification");
    assert!(matches!(err, Error::ChecksumMismatch { .. }), "got {err:?}");

    assert_eq!(cache.state().expect("state").version, "0.1.0");
    assert_eq!(
        std::fs::read(temp.path().join("cache/current.pkg")).expect("read preserved v1"),
        v1
    );
}

#[tokio::test]
async fn surfaces_http_status_error_when_artifact_endpoint_fails() {
    let (v1, v2) = build_v1_and_v2();
    let v2_artifact =
        BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("v2 map");
    let err_state = MockServerState::new(v2.clone(), None, ServerRangeMode::Error500);
    let err_url = spawn_server(err_state).await;

    let temp = tempfile::tempdir().expect("temp dir");
    let cache = seeded_cache(&temp, &v1);
    let downloader = DifferentialDownloader::new(
        reqwest::Client::new(),
        HeaderMap::new(),
        cache,
        TransferOptions {
            max_ranges_per_request: 1,
            ..TransferOptions::default()
        },
    );
    let err_req = make_request(&err_url, v2.len() as u64, Some(v2_artifact.sha512));
    assert!(matches!(
        downloader.download(&err_req, &DummyListener).await,
        Err(Error::HttpStatus { status: 500, .. })
    ));
}
