use std::io::{self, BufReader};
use std::rc::Rc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::{
    ChunkedBufReader, KNOWN_SHA512, ReadStats, TAMPERED_PAYLOAD_SHA512, TEST_PAYLOAD_SHA512,
};
use crate::Error;
use crate::verifier::{ArtifactVerifier, VerifyExpectation};

fn is_minisign_verify_failure(err: &Error) -> bool {
    matches!(
        err,
        Error::Signature(msg)
            if msg
                .strip_prefix("minisign verification failed:")
                .is_some_and(|detail| {
                    let lower = detail.to_ascii_lowercase();
                    lower.contains("verification failed") && !lower.contains("different key")
                })
    )
}

const TEST_PUBKEY: &str = "untrusted comment: minisign public key E7620F1842B4E81F\n\
RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
const OTHER_PUBKEY: &str = "untrusted comment: minisign public key: 232648C165D818AB\n\
RWSrGNhlwUgmI7VPPVyHcyljRanp9uPdy+yNWvffG3hSzYefpMvfT4WR";
const PREHASHED_TEST_SIG: &str = "untrusted comment: signature from minisign secret key\n\
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\n\
trusted comment: timestamp:1556193335\tfile:test\n\
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";
const LEGACY_TEST_SIG: &str = "untrusted comment: signature from minisign secret key\n\
RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=\n\
trusted comment: timestamp:1555779966\tfile:test\n\
QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==";

#[test]
fn verifies_prehashed_minisign_and_sha512_in_single_streaming_pass() {
    let pubkey_b64 = STANDARD.encode(TEST_PUBKEY);
    let sig_b64 = STANDARD.encode(PREHASHED_TEST_SIG);
    let stats = Rc::new(ReadStats::default());
    let mut reader = ChunkedBufReader::new(b"test", &stats);
    reader.interrupt_once = true;

    ArtifactVerifier::verify_reader(
        reader,
        &VerifyExpectation {
            sha512: Some(TEST_PAYLOAD_SHA512.into()),
            pubkey: Some(pubkey_b64.clone()),
            signature: Some(sig_b64.clone()),
            announced_version: "1.0.0".into(),
            require_signed_version: false,
        },
    )
    .expect("combined SHA-512 and streaming minisign passes");
    assert!(stats.interrupted_hit.get());
    assert_eq!(stats.bytes_consumed.get(), b"test".len());
    assert!(stats.fill_buf_calls.get() > 1);
    assert_eq!(stats.read_calls.get(), 0);

    // Both SHA-512 and signature invalid => ChecksumMismatch takes precedence.
    let both_bad = ArtifactVerifier::verify_reader(
        BufReader::new(b"tampered".as_slice()),
        &VerifyExpectation {
            sha512: Some(TEST_PAYLOAD_SHA512.into()),
            pubkey: Some(pubkey_b64.clone()),
            signature: Some(sig_b64.clone()),
            announced_version: "1.0.0".into(),
            require_signed_version: false,
        },
    )
    .expect_err("checksum mismatch must take precedence over invalid signature");
    assert!(matches!(
        both_bad,
        Error::ChecksumMismatch { ref expected, ref actual }
            if expected == TEST_PAYLOAD_SHA512 && actual == TAMPERED_PAYLOAD_SHA512
    ));

    let bad_sig = ArtifactVerifier::verify_reader(
        BufReader::new(b"tampered".as_slice()),
        &VerifyExpectation {
            sha512: Some(TAMPERED_PAYLOAD_SHA512.into()),
            pubkey: Some(pubkey_b64),
            signature: Some(sig_b64),
            announced_version: "1.0.0".into(),
            require_signed_version: false,
        },
    )
    .expect_err("matching SHA-512 with invalid signature must fail");
    assert!(is_minisign_verify_failure(&bad_sig), "got {bad_sig:?}");
}

#[test]
fn verifies_file_minisign_and_rejects_tampered_wrong_key_or_unversioned() {
    let pubkey_b64 = STANDARD.encode(TEST_PUBKEY);
    let other_pubkey_b64 = STANDARD.encode(OTHER_PUBKEY);
    let sig_b64 = STANDARD.encode(PREHASHED_TEST_SIG);

    let file = tempfile::NamedTempFile::new().expect("temp file");
    std::fs::write(file.path(), b"test").expect("write");

    ArtifactVerifier::verify_file(
        file.path(),
        &VerifyExpectation {
            sha512: Some(TEST_PAYLOAD_SHA512.into()),
            pubkey: Some(format!("  {pubkey_b64}\n")),
            signature: Some(format!("\t{sig_b64}\r\n")),
            announced_version: "1.0.0".into(),
            require_signed_version: false,
        },
    )
    .expect("trimmed base64 pubkey and prehashed signature verify on disk");

    assert!(matches!(
        ArtifactVerifier::verify_file(
            file.path(),
            &VerifyExpectation {
                pubkey: Some(pubkey_b64.clone()),
                signature: Some(sig_b64.clone()),
                announced_version: "1.0.0".into(),
                require_signed_version: true,
                ..Default::default()
            },
        ),
        Err(Error::MissingSignedVersion)
    ));

    let stats = Rc::new(ReadStats::default());
    let wrong_key = ArtifactVerifier::verify_reader(
        ChunkedBufReader::new(b"test", &stats),
        &VerifyExpectation {
            pubkey: Some(other_pubkey_b64),
            signature: Some(sig_b64.clone()),
            announced_version: "1.0.0".into(),
            ..Default::default()
        },
    )
    .expect_err("wrong public key must fail before streaming");
    assert!(matches!(wrong_key, Error::Signature(ref msg) if msg.contains("different key")));
    assert_eq!(stats.fill_buf_calls.get(), 0);
    assert_eq!(stats.read_calls.get(), 0);

    std::fs::write(file.path(), b"tampered").expect("tamper");
    let tampered = ArtifactVerifier::verify_file(
        file.path(),
        &VerifyExpectation {
            pubkey: Some(pubkey_b64),
            signature: Some(sig_b64),
            announced_version: "1.0.0".into(),
            ..Default::default()
        },
    )
    .expect_err("tampered file must fail minisign verification");
    assert!(is_minisign_verify_failure(&tampered), "got {tampered:?}");
}

#[test]
fn verifies_legacy_non_prehashed_minisign_via_fallback_and_enforces_checks() {
    let pubkey_b64 = STANDARD.encode(TEST_PUBKEY);
    let legacy_sig_b64 = STANDARD.encode(LEGACY_TEST_SIG);

    let stats = Rc::new(ReadStats::default());
    ArtifactVerifier::verify_reader(
        ChunkedBufReader::new(b"test", &stats),
        &VerifyExpectation {
            sha512: Some(TEST_PAYLOAD_SHA512.into()),
            pubkey: Some(pubkey_b64.clone()),
            signature: Some(legacy_sig_b64.clone()),
            announced_version: "1.0.0".into(),
            require_signed_version: false,
        },
    )
    .expect("legacy non-prehashed signature verifies via buffered fallback");
    assert!(stats.read_calls.get() > 0);

    let file = tempfile::NamedTempFile::new().expect("temp file");
    std::fs::write(file.path(), b"test").expect("write");

    assert!(matches!(
        ArtifactVerifier::verify_file(
            file.path(),
            &VerifyExpectation {
                pubkey: Some(pubkey_b64.clone()),
                signature: Some(legacy_sig_b64.clone()),
                announced_version: "1.0.0".into(),
                require_signed_version: true,
                ..Default::default()
            },
        ),
        Err(Error::MissingSignedVersion)
    ));

    assert!(matches!(
        ArtifactVerifier::verify_file(
            file.path(),
            &VerifyExpectation {
                sha512: Some("wrong-digest".into()),
                pubkey: Some(pubkey_b64.clone()),
                signature: Some(legacy_sig_b64.clone()),
                announced_version: "1.0.0".into(),
                require_signed_version: false,
            },
        ),
        Err(Error::ChecksumMismatch { ref expected, ref actual })
            if expected == "wrong-digest" && actual == TEST_PAYLOAD_SHA512
    ));

    std::fs::write(file.path(), b"Test").expect("tamper");
    let tampered = ArtifactVerifier::verify_file(
        file.path(),
        &VerifyExpectation {
            pubkey: Some(pubkey_b64),
            signature: Some(legacy_sig_b64),
            announced_version: "1.0.0".into(),
            ..Default::default()
        },
    )
    .expect_err("tampered payload must fail legacy minisign verification");
    assert!(is_minisign_verify_failure(&tampered), "got {tampered:?}");
}

#[test]
fn rejects_malformed_minisign_inputs_and_propagates_io_errors() {
    let valid_pubkey = STANDARD.encode(TEST_PUBKEY);
    let valid_sig = STANDARD.encode(PREHASHED_TEST_SIG);
    let legacy_sig = STANDARD.encode(LEGACY_TEST_SIG);
    let non_utf8 = STANDARD.encode([0xff_u8, 0xfe]);
    let invalid_text = STANDARD.encode("not a minisign block");

    let cases = [
        (Some(valid_pubkey.clone()), None, "missing signature"),
        (
            Some("not-valid-base64!!!".into()),
            Some(valid_sig.clone()),
            "invalid base64 pubkey",
        ),
        (
            Some(non_utf8.clone()),
            Some(valid_sig.clone()),
            "non-utf8 pubkey",
        ),
        (
            Some(valid_pubkey.clone()),
            Some("not-valid-base64!!!".into()),
            "invalid base64 signature",
        ),
        (
            Some(valid_pubkey.clone()),
            Some(non_utf8),
            "non-utf8 signature",
        ),
        (
            Some(invalid_text.clone()),
            Some(valid_sig.clone()),
            "invalid minisign pubkey",
        ),
        (
            Some(valid_pubkey.clone()),
            Some(invalid_text),
            "invalid minisign signature",
        ),
    ];

    for (pubkey, signature, fragment) in cases {
        let stats = Rc::new(ReadStats::default());
        let err = ArtifactVerifier::verify_reader(
            ChunkedBufReader::new(b"test", &stats),
            &VerifyExpectation {
                pubkey,
                signature,
                ..Default::default()
            },
        )
        .expect_err("malformed minisign input must fail");
        assert!(
            matches!(err, Error::Signature(ref msg) if msg.contains(fragment)),
            "expected '{fragment}', got: {err:?}"
        );
        assert_eq!(stats.fill_buf_calls.get(), 0);
        assert_eq!(stats.read_calls.get(), 0);
    }

    for sig in [&valid_sig, &legacy_sig] {
        let stats = Rc::new(ReadStats::default());
        let mut reader = ChunkedBufReader::new(b"test", &stats);
        reader.fail_after = Some(2);
        let err = ArtifactVerifier::verify_reader(
            reader,
            &VerifyExpectation {
                sha512: Some(TEST_PAYLOAD_SHA512.into()),
                pubkey: Some(valid_pubkey.clone()),
                signature: Some(sig.clone()),
                announced_version: "1.0.0".into(),
                require_signed_version: false,
            },
        )
        .expect_err("non-EINTR stream error must abort verification");
        assert!(matches!(err, Error::Io(ref io_err) if io_err.kind() == io::ErrorKind::BrokenPipe));
    }

    let missing = tempfile::tempdir()
        .expect("temp dir")
        .path()
        .join("missing-artifact.bin");
    assert!(matches!(
        ArtifactVerifier::sha512_file(&missing),
        Err(Error::Io(ref err)) if err.kind() == io::ErrorKind::NotFound
    ));
    assert!(matches!(
        ArtifactVerifier::verify_file(
            &missing,
            &VerifyExpectation {
                sha512: Some(KNOWN_SHA512.into()),
                ..Default::default()
            },
        ),
        Err(Error::Io(ref err)) if err.kind() == io::ErrorKind::NotFound
    ));
}
