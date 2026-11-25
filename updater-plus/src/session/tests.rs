use super::{DiffSession, FullSession};
use crate::Error;
use crate::blockmap::BlockMap;
use crate::cache::PackageCache;
use crate::chunker::ChunkerConfig;
use crate::listener::DownloadMode;
use crate::verifier::{ArtifactVerifier, VerifyExpectation, VerifyPolicy};

fn synthetic_pair() -> (Vec<u8>, Vec<u8>) {
    let mut state = 0x1234_5678u64;
    let mut v1 = Vec::with_capacity(600_000);
    for _ in 0..600_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        v1.push((state >> 56) as u8);
    }
    let mut v2 = v1.clone();
    v2[100_000..102_048].fill(0x5a);
    v2[400_000..402_048].fill(0xa5);
    (v1, v2)
}

#[test]
fn diff_session_handles_out_of_order_duplicate_overlapping_and_interior_split_writes() {
    let (v1, v2) = synthetic_pair();
    let v2_art = BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("map");
    let temp = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(temp.path().join("cache"), None);

    // Opening a diff session with an empty cache fails cleanly.
    assert!(matches!(
        DiffSession::open(
            cache.clone(),
            "0.2.0",
            v2_art.blockmap.clone(),
            VerifyPolicy::Sha512 {
                expected: v2_art.sha512.clone(),
            },
            0,
        ),
        Err(Error::DifferentialTransfer(_))
    ));

    let v1_path = temp.path().join("v1.pkg");
    std::fs::write(&v1_path, &v1).expect("write v1");
    cache.seed_from_file(&v1_path, "0.1.0").expect("seed");

    let policy = VerifyPolicy::Sha512 {
        expected: v2_art.sha512.clone(),
    };
    let (mut session, plan) =
        DiffSession::open(cache.clone(), "0.2.0", v2_art.blockmap, policy, 0).expect("open diff");
    assert!(plan.reused_bytes > 0);
    assert!(plan.missing_ranges.len() >= 2);
    assert!(!session.is_complete());

    // Boundary checks: empty write, exact-end overflow, partial overflow, and u64::MAX overflow.
    assert_eq!(session.write_range(0, &[]).expect("empty write"), 0);
    assert!(matches!(
        session.write_range(plan.total_size, b"overflow"),
        Err(Error::DifferentialTransfer(_))
    ));
    assert!(matches!(
        session.write_range(plan.total_size - 2, b"1234"),
        Err(Error::DifferentialTransfer(_))
    ));
    assert!(matches!(
        session.write_range(u64::MAX, b"x"),
        Err(Error::DifferentialTransfer(_))
    ));

    // Exercise interior 2-way split on the first missing range (middle written first, splitting
    // one missing span into two disjoint sub-spans) plus QoS-1 duplicate and overlapping writes.
    let mut covered_total = 0usize;
    let first = plan.missing_ranges[0];
    let f_start = first.start as usize;
    let f_end = first.end as usize;
    let f_len = f_end - f_start + 1;
    let mid_start = f_start + f_len / 3;
    let mid_end = f_start + (2 * f_len) / 3;
    let initial_range_count = session.missing_ranges().len();
    let mid_cov = session
        .write_range(mid_start as u64, &v2[mid_start..mid_end])
        .expect("interior write");
    assert_eq!(mid_cov, mid_end - mid_start);
    assert_eq!(
        session.missing_ranges().len(),
        initial_range_count + 1,
        "interior write must split one missing range into two"
    );
    // Exact duplicate of the interior write must yield 0 newly covered bytes.
    assert_eq!(
        session
            .write_range(mid_start as u64, &v2[mid_start..mid_end])
            .expect("duplicate interior"),
        0
    );
    // Finish left and right halves with overlap over the already-written interior.
    covered_total += mid_cov;
    covered_total += session
        .write_range(
            f_start as u64,
            &v2[f_start..(mid_start + 16).min(f_end + 1)],
        )
        .expect("left half");
    covered_total += session
        .write_range(
            (mid_end.saturating_sub(16)) as u64,
            &v2[mid_end.saturating_sub(16)..=f_end],
        )
        .expect("right half");

    // Deliver remaining missing ranges in reverse order with duplicate QoS-1 delivery.
    for range in plan.missing_ranges[1..].iter().rev() {
        let start = range.start as usize;
        let end = range.end as usize;
        let mid = start + (end - start).div_ceil(2);
        let overlap_end = (mid + 32).min(end + 1);

        let c1 = session
            .write_range(start as u64, &v2[start..overlap_end])
            .expect("write part 1");
        let dup = session
            .write_range(start as u64, &v2[start..overlap_end])
            .expect("duplicate delivery must be idempotent");
        assert_eq!(dup, 0, "duplicate chunk must not double-count progress");

        let c2 = session
            .write_range(mid as u64, &v2[mid..=end])
            .expect("write overlapping part 2");
        covered_total += c1 + c2;
    }

    assert_eq!(covered_total as u64, plan.expected_download_bytes);
    assert!(session.is_complete());
    assert_eq!(session.remaining_bytes(), 0);

    let pkg = session.finish_and_verify().expect("verify and commit");
    assert_eq!(pkg.summary.mode, DownloadMode::Differential);
    assert_eq!(pkg.summary.downloaded_bytes, plan.expected_download_bytes);
    assert_eq!(std::fs::read(&pkg.path).expect("read"), v2);
    assert_eq!(cache.state().expect("state").version, "0.2.0");
}

#[test]
fn diff_session_rejects_incomplete_or_tampered_transfer_and_preserves_cache() {
    let (v1, v2) = synthetic_pair();
    let v2_art = BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("map");
    let temp = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(temp.path().join("cache"), None);
    let v1_path = temp.path().join("v1.pkg");
    std::fs::write(&v1_path, &v1).expect("write v1");
    cache.seed_from_file(&v1_path, "0.1.0").expect("seed");

    let policy = VerifyPolicy::Sha512 {
        expected: v2_art.sha512.clone(),
    };
    let (mut session, plan) = DiffSession::open(
        cache.clone(),
        "0.2.0",
        v2_art.blockmap.clone(),
        policy.clone(),
        0,
    )
    .expect("open diff");

    // Write only the first missing range; compute expected_missing independently from plan.
    let first = plan.missing_ranges[0];
    session
        .write_range(first.start, &v2[first.start as usize..=first.end as usize])
        .expect("write first");
    let expected_missing: u64 = plan.missing_ranges[1..].iter().map(|r| r.len()).sum();
    assert!(expected_missing > 0);
    assert_eq!(session.remaining_bytes(), expected_missing);

    let err = session
        .finish_and_verify()
        .expect_err("incomplete session must fail");
    assert!(matches!(
        err,
        Error::IncompleteTransfer { missing_bytes } if missing_bytes == expected_missing
    ));
    assert_eq!(cache.state().expect("state").version, "0.1.0");
    assert_eq!(
        std::fs::read(temp.path().join("cache/current.pkg")).expect("read"),
        v1
    );

    // Tampered complete transfer must fail SHA-512 verification without corrupting v1 cache.
    let (mut tampered_session, plan2) =
        DiffSession::open(cache.clone(), "0.2.0", v2_art.blockmap, policy, 0).expect("open diff 2");
    for (idx, range) in plan2.missing_ranges.iter().enumerate() {
        let mut slice = v2[range.start as usize..=range.end as usize].to_vec();
        if idx == 0 {
            slice[0] ^= 0xff;
        }
        tampered_session
            .write_range(range.start, &slice)
            .expect("write");
    }
    assert!(matches!(
        tampered_session.finish_and_verify(),
        Err(Error::ChecksumMismatch { .. })
    ));
    assert_eq!(cache.state().expect("state").version, "0.1.0");
    assert_eq!(
        std::fs::read(temp.path().join("cache/current.pkg")).expect("read"),
        v1
    );
}

#[test]
fn full_session_verifies_streams_rejects_tampering_and_unverified_policy() {
    assert!(matches!(
        VerifyPolicy::from_expectation(&VerifyExpectation::default()),
        Err(Error::InvalidConfig(_))
    ));

    let payload = b"full push payload delivered over arbitrary bus".to_vec();
    let sha512 = ArtifactVerifier::sha512_bytes(&payload);
    let temp = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(temp.path().join("cache"), None);

    let mut session = FullSession::open(
        cache.clone(),
        "1.0.0",
        VerifyPolicy::Sha512 {
            expected: sha512.clone(),
        },
    )
    .expect("open full");
    for chunk in payload.chunks(7) {
        session.write_chunk(chunk).expect("write chunk");
    }
    let pkg = session.finish_and_verify(None).expect("finish full");
    assert_eq!(pkg.summary.mode, DownloadMode::Full);
    assert_eq!(pkg.summary.downloaded_bytes, payload.len() as u64);
    assert_eq!(std::fs::read(&pkg.path).expect("read"), payload);
    assert_eq!(cache.state().expect("state").version, "1.0.0");

    // Tampered full push must fail verification and leave 1.0.0 cache untouched.
    let mut bad_session = FullSession::open(
        cache.clone(),
        "2.0.0",
        VerifyPolicy::Sha512 { expected: sha512 },
    )
    .expect("open bad full");
    bad_session.write_chunk(b"corrupted bytes").expect("write");
    assert!(matches!(
        bad_session.finish_and_verify(None),
        Err(Error::ChecksumMismatch { .. })
    ));
    assert_eq!(cache.state().expect("state").version, "1.0.0");
    assert_eq!(
        std::fs::read(temp.path().join("cache/current.pkg")).expect("read"),
        payload
    );
}
