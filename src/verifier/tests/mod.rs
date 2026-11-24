use std::cell::Cell;
use std::io::{self, BufRead, Read};
use std::rc::Rc;

use super::{ArtifactVerifier, VerifyExpectation};
use crate::Error;

mod minisign;

const KNOWN_PAYLOAD: &[u8] = b"updater-plus blockmap payload";
// Standard base64 SHA-512 digests computed independently of ArtifactVerifier.
const KNOWN_SHA512: &str =
    "kJqFboBetws048YKgppTHnkpxks8fJhdJhyHc5L5NCIiz83KuvltG6/xqkH9ML5X5881hU/rN/8wvj33SD8wQQ==";
const EMPTY_SHA512: &str =
    "z4PhNX7vuL3xVChQ1m2AB9Yg5AULVxXcg/SpIdNs6c5H0NE8XYXysP+DGNKHfuwvY7kxvUdBeoGlODJ6+SfaPg==";
const TEST_PAYLOAD_SHA512: &str =
    "7iaw3Ur350mqGo7jwQrpkj9hiYB3Lkc/iBml1JQODbJ6wYX4oOHV+E+IvIh/1nsUNzLDBMxfqa2Ob1f1ACio/w==";
const TAMPERED_PAYLOAD_SHA512: &str =
    "cuAwXT/PythKA+fBkD6RIWKVBJHm0MfQ4jagTBgAgVVCy3Y8miUa0BwcTXLWq6npJgXi7Ze0Y/a5CNpY2Mt4cA==";

#[derive(Default)]
struct ReadStats {
    fill_buf_calls: Cell<usize>,
    read_calls: Cell<usize>,
    bytes_consumed: Cell<usize>,
    interrupted_hit: Cell<bool>,
}

struct ChunkedBufReader<'a> {
    data: &'a [u8],
    offset: usize,
    max_chunk: usize,
    interrupt_once: bool,
    fail_after: Option<usize>,
    stats: Rc<ReadStats>,
}

impl<'a> ChunkedBufReader<'a> {
    fn new(data: &'a [u8], stats: &Rc<ReadStats>) -> Self {
        Self {
            data,
            offset: 0,
            max_chunk: 1,
            interrupt_once: false,
            fail_after: None,
            stats: Rc::clone(stats),
        }
    }

    fn next_chunk(&mut self) -> io::Result<&'a [u8]> {
        if self.interrupt_once {
            self.interrupt_once = false;
            self.stats.interrupted_hit.set(true);
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "simulated eintr",
            ));
        }
        if self.fail_after.is_some_and(|limit| self.offset >= limit) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "simulated stream error",
            ));
        }
        let remaining = &self.data[self.offset..];
        Ok(&remaining[..remaining.len().min(self.max_chunk)])
    }
}

impl Read for ChunkedBufReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stats.read_calls.set(self.stats.read_calls.get() + 1);
        let chunk = self.next_chunk()?;
        let take = chunk.len().min(buf.len());
        buf[..take].copy_from_slice(&chunk[..take]);
        self.offset += take;
        self.stats
            .bytes_consumed
            .set(self.stats.bytes_consumed.get() + take);
        Ok(take)
    }
}

impl BufRead for ChunkedBufReader<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.stats
            .fill_buf_calls
            .set(self.stats.fill_buf_calls.get() + 1);
        self.next_chunk()
    }

    fn consume(&mut self, amt: usize) {
        self.offset += amt;
        self.stats
            .bytes_consumed
            .set(self.stats.bytes_consumed.get() + amt);
    }
}

#[test]
fn verifies_file_sha512_empty_large_and_noop_expectations() {
    let file = tempfile::NamedTempFile::new().expect("temp file");
    std::fs::write(file.path(), KNOWN_PAYLOAD).expect("write");

    assert_eq!(ArtifactVerifier::sha512_bytes(KNOWN_PAYLOAD), KNOWN_SHA512);
    ArtifactVerifier::verify_file(
        file.path(),
        &VerifyExpectation {
            sha512: Some(KNOWN_SHA512.into()),
            announced_version: "0.2.0".into(),
            ..Default::default()
        },
    )
    .expect("known SHA-512 matches");

    let err = ArtifactVerifier::verify_file(
        file.path(),
        &VerifyExpectation {
            sha512: Some("invalid-digest".into()),
            announced_version: "0.2.0".into(),
            ..Default::default()
        },
    )
    .expect_err("bad digest must fail");
    assert!(matches!(
        err,
        Error::ChecksumMismatch { ref expected, ref actual }
            if expected == "invalid-digest" && actual == KNOWN_SHA512
    ));

    // Empty file and multi-buffer (> 256 KiB) file streaming coverage.
    std::fs::write(file.path(), b"").expect("write empty");
    assert_eq!(
        ArtifactVerifier::sha512_file(file.path()).expect("empty hash"),
        EMPTY_SHA512
    );
    ArtifactVerifier::verify_file(
        file.path(),
        &VerifyExpectation {
            sha512: Some(EMPTY_SHA512.into()),
            ..Default::default()
        },
    )
    .expect("empty file SHA-512 verifies");

    let large: Vec<u8> = (0..600 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(file.path(), &large).expect("write large");
    assert_eq!(
        ArtifactVerifier::sha512_file(file.path()).expect("stream hash"),
        ArtifactVerifier::sha512_bytes(&large)
    );

    // No-op expectation skips touching both reader and filesystem.
    let stats = Rc::new(ReadStats::default());
    ArtifactVerifier::verify_reader(
        ChunkedBufReader::new(b"ignored", &stats),
        &VerifyExpectation::default(),
    )
    .expect("empty expectation succeeds without reading");
    assert_eq!(stats.fill_buf_calls.get(), 0);
    assert_eq!(stats.read_calls.get(), 0);
}

#[test]
fn extracts_and_validates_signed_version_comment() {
    let comment = "timestamp:1700000000\tfile:myapp.tar.gz\tversion:1.2.3";
    assert_eq!(
        ArtifactVerifier::extract_signed_version(comment),
        Some("1.2.3")
    );
    assert_eq!(
        ArtifactVerifier::extract_signed_version("version:2.0.0\tfile:app.zip"),
        Some("2.0.0")
    );
    assert_eq!(
        ArtifactVerifier::extract_signed_version(
            "timestamp:1\tversion:v1.2.3-beta.1\tfile:app.tar.gz"
        ),
        Some("v1.2.3-beta.1")
    );
    assert_eq!(ArtifactVerifier::extract_signed_version(""), None);
    assert_eq!(ArtifactVerifier::extract_signed_version("version:"), None);
    assert_eq!(
        ArtifactVerifier::extract_signed_version("version:\tfile:app.zip"),
        None
    );
    assert_eq!(
        ArtifactVerifier::extract_signed_version("timestamp:1\tfile:app-version:2.zip"),
        None
    );

    ArtifactVerifier::verify_signed_version(comment, "v1.2.3", true).expect("semver match");
    ArtifactVerifier::verify_signed_version("version:v1.2.3", "1.2.3", true)
        .expect("v-prefixed signed version matches bare semver");
    ArtifactVerifier::verify_signed_version("timestamp:1\tfile:app.zip", "1.0.0", false)
        .expect("optional signed version passes");
    ArtifactVerifier::verify_signed_version("version:nightly-2026", "nightly-2026", true)
        .expect("non-semver exact match");

    assert!(matches!(
        ArtifactVerifier::verify_signed_version(comment, "2.0.0", false),
        Err(Error::SignedVersionMismatch { ref signed, ref announced })
            if signed == "1.2.3" && announced == "2.0.0"
    ));
    assert!(matches!(
        ArtifactVerifier::verify_signed_version("version:nightly-2026", "nightly-2025", true),
        Err(Error::SignedVersionMismatch { ref signed, ref announced })
            if signed == "nightly-2026" && announced == "nightly-2025"
    ));
    assert!(matches!(
        ArtifactVerifier::verify_signed_version("timestamp:1\tfile:app.zip", "1.0.0", true),
        Err(Error::MissingSignedVersion)
    ));
}
