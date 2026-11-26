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
    let downloader =
        DifferentialDownloader::http(HeaderMap::new(), cache, TransferOptions::default())
            .expect("downloader");
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
        assert_eq!(seen.len(), 2);
        assert!(
            seen[0].as_deref().is_some_and(|h| h.contains(',')),
            "probe 1 must be multi-range: {seen:?}"
        );
        assert!(
            seen[1].as_deref().is_some_and(|h| !h.contains(',')),
            "probe 2 must be single-range: {seen:?}"
        );
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
    let dl2 = DifferentialDownloader::http(HeaderMap::new(), cache2, TransferOptions::default())
        .expect("downloader");
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
    let downloader =
        DifferentialDownloader::http(HeaderMap::new(), cache, TransferOptions::default())
            .expect("downloader");
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
    let dl2 = DifferentialDownloader::http(HeaderMap::new(), cache2, TransferOptions::default())
        .expect("downloader");
    let mut disabled_req = make_request(&disabled_url, v2.len() as u64, Some(v2_artifact.sha512));
    disabled_req.differential = false;
    let res2 = dl2
        .download(&disabled_req, &DummyListener)
        .await
        .expect("disabled differential full download");
    assert_eq!(res2.summary.mode, DownloadMode::Full);
    assert_eq!(res2.summary.downloaded_bytes, v2.len() as u64);
    assert_eq!(res2.summary.reused_bytes, 0);
    assert_eq!(
        res2.summary.fallback_reason.as_deref(),
        Some("differential updates disabled")
    );
    assert_eq!(state_disabled.blockmap_requests.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&res2.path).expect("read"), v2);
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
    let downloader =
        DifferentialDownloader::http(HeaderMap::new(), cache.clone(), TransferOptions::default())
            .expect("downloader");
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
    let mut remaining: Vec<String> = std::fs::read_dir(cache.directory())
        .expect("read cache dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    remaining.sort();
    assert_eq!(
        remaining,
        vec!["current.blockmap", "current.pkg", "state.json"],
        "failed verification must not leave staging files behind"
    );
}

#[tokio::test]
async fn falls_back_on_size_mismatch_threshold_exceeded_and_unhashed_chunk_tampering() {
    let (v1, v2) = build_v1_and_v2();
    let v2_artifact =
        BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("v2 map");
    let gz_map = v2_artifact.blockmap.to_gzip_bytes().expect("gz");

    let mismatch_state = MockServerState::new(
        v2.clone(),
        Some(gz_map.clone()),
        ServerRangeMode::MultipartAndSingle,
    );
    let mismatch_url = spawn_server(mismatch_state).await;
    let temp1 = tempfile::tempdir().expect("temp dir");
    let dl1 = DifferentialDownloader::http(
        HeaderMap::new(),
        seeded_cache(&temp1, &v1),
        TransferOptions::default(),
    )
    .expect("downloader");
    let bad_size_req = make_request(
        &mismatch_url,
        (v2.len() as u64) + 64,
        Some(v2_artifact.sha512.clone()),
    );
    let res1 = dl1
        .download(&bad_size_req, &DummyListener)
        .await
        .expect("size mismatch fallback");
    assert_eq!(res1.summary.mode, DownloadMode::Full);
    assert!(
        res1.summary
            .fallback_reason
            .as_deref()
            .is_some_and(|r| r.contains("size disagrees")),
        "got {:?}",
        res1.summary.fallback_reason
    );
    assert_eq!(std::fs::read(&res1.path).expect("read"), v2);

    let threshold_state = MockServerState::new(
        v2.clone(),
        Some(gz_map.clone()),
        ServerRangeMode::MultipartAndSingle,
    );
    let threshold_url = spawn_server(threshold_state).await;
    let temp2 = tempfile::tempdir().expect("temp dir");
    let dl2 = DifferentialDownloader::http(
        HeaderMap::new(),
        seeded_cache(&temp2, &v1),
        TransferOptions::default(),
    )
    .expect("downloader");
    let mut low_thresh_req =
        make_request(&threshold_url, v2.len() as u64, Some(v2_artifact.sha512));
    low_thresh_req.full_download_threshold = 0.01;
    let res2 = dl2
        .download(&low_thresh_req, &DummyListener)
        .await
        .expect("threshold fallback");
    assert_eq!(res2.summary.mode, DownloadMode::Full);
    assert!(
        res2.summary
            .fallback_reason
            .as_deref()
            .is_some_and(|r| r.contains("delta requires")),
        "got {:?}",
        res2.summary.fallback_reason
    );
    assert_eq!(std::fs::read(&res2.path).expect("read"), v2);

    // Stale remote blockmap with same size: differential reconstruction fails verification,
    // falls back to verified full download, and regenerates clean cached blockmap (no poisoning).
    let mut updated_remote = v2.clone();
    updated_remote[450_000] ^= 0xff;
    let updated_sha512 = crate::verifier::ArtifactVerifier::sha512_bytes(&updated_remote);
    let stale_map_state = MockServerState::new(
        updated_remote.clone(),
        Some(gz_map),
        ServerRangeMode::MultipartAndSingle,
    );
    let stale_map_url = spawn_server(stale_map_state).await;
    let temp3 = tempfile::tempdir().expect("temp dir");
    let cache3 = seeded_cache(&temp3, &v1);
    let dl3 =
        DifferentialDownloader::http(HeaderMap::new(), cache3.clone(), TransferOptions::default())
            .expect("downloader");
    let stale_map_req = make_request(
        &stale_map_url,
        updated_remote.len() as u64,
        Some(updated_sha512),
    );
    let res3 = dl3
        .download(&stale_map_req, &DummyListener)
        .await
        .expect("stale remote blockmap falls back to verified full download");
    assert_eq!(res3.summary.mode, DownloadMode::Full);
    assert!(
        res3.summary
            .fallback_reason
            .as_deref()
            .is_some_and(|r| r.contains("reconstructed package verification failed")),
        "got {:?}",
        res3.summary.fallback_reason
    );
    assert_eq!(std::fs::read(&res3.path).expect("read"), updated_remote);
    let committed = cache3.load_base().expect("committed cache");
    assert_ne!(
        committed.blockmap, v2_artifact.blockmap,
        "stale remote blockmap must not poison PackageCache"
    );
    let expected_regenerated =
        BlockMap::generate(updated_remote.as_slice(), "file", ChunkerConfig::default())
            .expect("regen")
            .blockmap;
    assert_eq!(committed.blockmap, expected_regenerated);
}
