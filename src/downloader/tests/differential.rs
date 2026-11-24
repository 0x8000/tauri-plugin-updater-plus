use std::sync::Mutex;
use std::sync::atomic::Ordering;

use axum::http::HeaderMap;

use super::super::{DifferentialDownloader, TransferOptions};
use super::{
    EventRecorder, MockServerState, ServerRangeMode, build_v1_and_v2, make_request,
    ranges_cover_offset, seeded_cache, spawn_server,
};
use crate::blockmap::BlockMap;
use crate::chunker::ChunkerConfig;
use crate::listener::{DownloadEvent, DownloadMode, DummyListener};

#[tokio::test]
async fn differential_multipart_download_reconstructs_exact_artifact_with_huge_savings() {
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
        TransferOptions::default(),
    );
    let request = make_request(&base_url, v2.len() as u64, Some(v2_artifact.sha512));
    let recorder = EventRecorder(Mutex::new(Vec::new()));

    let result = downloader
        .download(&request, &recorder)
        .await
        .expect("differential download");
    assert_eq!(result.summary.mode, DownloadMode::Differential);
    assert!(result.summary.saved_percent > 75.0);
    assert_eq!(state.requests.load(Ordering::SeqCst), 1);
    assert!(state.bytes_sent.load(Ordering::SeqCst) < v2.len() / 3);
    assert_eq!(std::fs::read(&result.path).expect("read"), v2);

    let events = recorder.0.lock().expect("lock");
    assert!(matches!(
        events.first(),
        Some(DownloadEvent::Started {
            differential: true,
            reused_bytes,
            ..
        }) if *reused_bytes == result.summary.reused_bytes
    ));
    assert!(matches!(
        events.last(),
        Some(DownloadEvent::Finished { summary }) if summary == &result.summary
    ));
}

#[tokio::test]
async fn degrades_gracefully_from_multipart_to_single_range_and_splits_batches() {
    let (v1, v2) = build_v1_and_v2();
    let v2_artifact =
        BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("v2 map");
    let state = MockServerState::new(
        v2.clone(),
        Some(v2_artifact.blockmap.to_gzip_bytes().expect("gz")),
        ServerRangeMode::SingleOnly501,
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
        .expect("single-range fallback");
    assert_eq!(result.summary.mode, DownloadMode::Differential);
    assert!(
        state.requests.load(Ordering::SeqCst) >= 3,
        "expected 1 rejected multipart request followed by >=2 single-range requests"
    );
    assert_eq!(
        result.summary.http_requests,
        state.requests.load(Ordering::SeqCst)
    );
    assert!(state.bytes_sent.load(Ordering::SeqCst) < v2.len() / 3);
    assert_eq!(std::fs::read(&result.path).expect("read"), v2);

    let batch_state = MockServerState::new(
        v2.clone(),
        Some(v2_artifact.blockmap.to_gzip_bytes().expect("gz")),
        ServerRangeMode::MultipartAndSingle,
    );
    let batch_url = spawn_server(batch_state.clone()).await;
    let temp2 = tempfile::tempdir().expect("temp dir");
    let cache2 = seeded_cache(&temp2, &v1);
    let batched_dl = DifferentialDownloader::new(
        reqwest::Client::new(),
        HeaderMap::new(),
        cache2,
        TransferOptions {
            max_ranges_per_request: 1,
            ..TransferOptions::default()
        },
    );
    let batch_req = make_request(&batch_url, v2.len() as u64, Some(v2_artifact.sha512));
    let res = batched_dl
        .download(&batch_req, &DummyListener)
        .await
        .expect("1-range-per-batch download");
    assert_eq!(res.summary.mode, DownloadMode::Differential);
    assert!(batch_state.requests.load(Ordering::SeqCst) >= 2);
    assert_eq!(std::fs::read(&res.path).expect("read"), v2);
}

#[tokio::test]
async fn recovers_when_cached_local_blocks_are_corrupted_on_disk() {
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

    let cached_pkg = temp.path().join("cache").join("current.pkg");
    let mut corrupted = std::fs::read(&cached_pkg).expect("read cached");
    corrupted[300_000] ^= 0xff;
    std::fs::write(&cached_pkg, &corrupted).expect("write corrupted");

    let downloader = DifferentialDownloader::new(
        reqwest::Client::new(),
        HeaderMap::new(),
        cache,
        TransferOptions::default(),
    );
    let request = make_request(&base_url, v2.len() as u64, None);

    let result = downloader
        .download(&request, &DummyListener)
        .await
        .expect("corrupted block re-fetched cleanly");
    assert_eq!(result.summary.mode, DownloadMode::Differential);
    assert!(
        ranges_cover_offset(&state.received_ranges.lock().expect("lock"), 300_000),
        "corrupted local offset 300_000 must be re-fetched over HTTP"
    );
    assert_eq!(std::fs::read(&result.path).expect("read"), v2);
}
