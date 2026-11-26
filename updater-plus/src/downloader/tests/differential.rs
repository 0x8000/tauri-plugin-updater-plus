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
    let downloader =
        DifferentialDownloader::http(HeaderMap::new(), cache, TransferOptions::default())
            .expect("downloader");
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
    let downloader =
        DifferentialDownloader::http(HeaderMap::new(), cache, TransferOptions::default())
            .expect("downloader");
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

    // Mutate 5 well-separated spans so `max_ranges_per_request: 2` produces two 2-range
    // `multipart/byteranges` batches followed by one trailing 1-range single request (`with_prior`).
    let mut v2_multi = v1.clone();
    for offset in [80_000usize, 320_000, 560_000, 800_000, 1_040_000] {
        v2_multi[offset..offset + 2048].fill((offset / 10_000) as u8);
    }
    let v2_multi_art =
        BlockMap::generate(v2_multi.as_slice(), "file", ChunkerConfig::default()).expect("map");
    let batch_state = MockServerState::new(
        v2_multi.clone(),
        Some(v2_multi_art.blockmap.to_gzip_bytes().expect("gz")),
        ServerRangeMode::MultipartAndSingle,
    );
    let batch_url = spawn_server(batch_state.clone()).await;
    let temp2 = tempfile::tempdir().expect("temp dir");
    let cache2 = seeded_cache(&temp2, &v1);
    let batched_dl = DifferentialDownloader::http(
        HeaderMap::new(),
        cache2,
        TransferOptions {
            max_ranges_per_request: 2,
            ..TransferOptions::default()
        },
    )
    .expect("downloader");
    let batch_req = make_request(&batch_url, v2_multi.len() as u64, Some(v2_multi_art.sha512));
    let res = batched_dl
        .download(&batch_req, &DummyListener)
        .await
        .expect("multi-batch multipart + trailing single-range download");
    assert_eq!(res.summary.mode, DownloadMode::Differential);
    assert_eq!(batch_state.requests.load(Ordering::SeqCst), 3);
    assert_eq!(res.summary.http_requests, 3);
    {
        let seen = batch_state.received_ranges.lock().expect("lock");
        let multi_count = seen.iter().flatten().filter(|h| h.contains(',')).count();
        let single_count = seen.iter().flatten().filter(|h| !h.contains(',')).count();
        assert_eq!(multi_count, 2, "expected 2 multipart batches, got {seen:?}");
        assert_eq!(
            single_count, 1,
            "expected 1 trailing single range, got {seen:?}"
        );
    }
    assert_eq!(std::fs::read(&res.path).expect("read"), v2_multi);
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

    let downloader =
        DifferentialDownloader::http(HeaderMap::new(), cache, TransferOptions::default())
            .expect("downloader");
    // Unverified request (no sha512 and no pubkey) must fail closed before downloading.
    let unverified_req = make_request(&base_url, v2.len() as u64, None);
    assert!(matches!(
        downloader.download(&unverified_req, &DummyListener).await,
        Err(crate::Error::InvalidConfig(ref msg)) if msg.contains("sha512 or minisign pubkey")
    ));
    assert_eq!(state.requests.load(Ordering::SeqCst), 0);

    let request = make_request(&base_url, v2.len() as u64, Some(v2_artifact.sha512));

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
