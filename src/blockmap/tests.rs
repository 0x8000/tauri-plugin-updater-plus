use std::io::Read;

use aws_lc_rs::digest;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::{BlockMap, BlockMapFile};
use crate::Error;
use crate::chunker::ChunkerConfig;

struct ShortReader<'a> {
    data: &'a [u8],
    max_step: usize,
}

impl Read for ShortReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = buf.len().min(self.max_step).min(self.data.len());
        buf[..len].copy_from_slice(&self.data[..len]);
        self.data = &self.data[len..];
        Ok(len)
    }
}

#[test]
fn generates_and_roundtrips_gzipped_and_raw_json_v2_blockmap() {
    let payload: Vec<u8> = (0..180_000u32).map(|i| ((i * 31) % 251) as u8).collect();
    let config = ChunkerConfig {
        min_size: 4 * 1024,
        avg_size: 16 * 1024,
        max_size: 64 * 1024,
    };

    let artifact = BlockMap::generate(payload.as_slice(), "file", config).expect("generate map");
    let streamed = BlockMap::generate(
        ShortReader {
            data: &payload,
            max_step: 1337,
        },
        "file",
        config,
    )
    .expect("generate via short reads");
    assert_eq!(streamed, artifact);

    assert_eq!(artifact.size, payload.len() as u64);
    assert_eq!(artifact.blockmap.total_size(), payload.len() as u64);
    assert_eq!(artifact.blockmap.version, "2");

    let expected_sha512 = STANDARD.encode(digest::digest(&digest::SHA512, &payload).as_ref());
    assert_eq!(artifact.sha512, expected_sha512);

    let gzipped = artifact.blockmap.to_gzip_bytes().expect("gzip encode");
    assert!(gzipped.starts_with(&[0x1f, 0x8b]));
    let decoded = BlockMap::from_bytes(&gzipped).expect("gzip decode");
    assert_eq!(decoded, artifact.blockmap);

    let raw_json = serde_json::to_vec(&artifact.blockmap).expect("raw json");
    assert_eq!(
        BlockMap::from_bytes(&raw_json).expect("raw decode"),
        artifact.blockmap
    );

    let mut offset = 0usize;
    let file = &decoded.files[0];
    for (checksum, &size) in file.checksums.iter().zip(&file.sizes) {
        let slice = &payload[offset..offset + size as usize];
        assert!(BlockMap::verify_chunk(slice, checksum));
        offset += size as usize;
    }
    assert_eq!(offset, payload.len());
}

#[test]
fn verifies_electron_truncated_sha256_and_rejects_short_or_invalid_hashes() {
    let chunk = b"electron blockmap compatibility payload";
    let full = digest::digest(&digest::SHA256, chunk);
    let truncated = STANDARD.encode(&full.as_ref()[..24]);
    assert!(BlockMap::verify_chunk(chunk, &truncated));
    assert!(!BlockMap::verify_chunk(b"tampered", &truncated));
    assert!(!BlockMap::verify_chunk(chunk, ""));
    assert!(!BlockMap::verify_chunk(
        chunk,
        &STANDARD.encode(&full.as_ref()[..15])
    ));
    assert!(!BlockMap::verify_chunk(chunk, &STANDARD.encode([0u8; 33])));
    assert!(!BlockMap::verify_chunk(chunk, "not valid base64!!!"));
}

#[test]
fn rejects_corrupt_or_unsupported_blockmaps_with_typed_errors() {
    let bad_version = BlockMap {
        version: "1".into(),
        files: vec![BlockMapFile {
            name: "file".into(),
            offset: 0,
            checksums: vec!["abc".into()],
            sizes: vec![10],
        }],
    };
    assert!(matches!(
        bad_version.validate(),
        Err(Error::InvalidBlockMap(ref msg)) if msg.contains("unsupported blockmap version")
    ));

    let empty_files = BlockMap {
        version: "2".into(),
        files: vec![],
    };
    assert!(matches!(
        empty_files.validate(),
        Err(Error::InvalidBlockMap(ref msg)) if msg.contains("no file entries")
    ));

    let mismatch = BlockMap {
        version: "2".into(),
        files: vec![BlockMapFile {
            name: "file".into(),
            offset: 0,
            checksums: vec!["abc".into()],
            sizes: vec![10, 20],
        }],
    };
    assert!(matches!(
        mismatch.validate(),
        Err(Error::InvalidBlockMap(ref msg)) if msg.contains("length mismatch")
    ));

    let zero_block = BlockMap {
        version: "2".into(),
        files: vec![BlockMapFile {
            name: "file".into(),
            offset: 0,
            checksums: vec!["abc".into()],
            sizes: vec![0],
        }],
    };
    assert!(matches!(
        zero_block.validate(),
        Err(Error::InvalidBlockMap(ref msg)) if msg.contains("zero-sized block")
    ));

    let overflow = BlockMap {
        version: "2".into(),
        files: vec![BlockMapFile {
            name: "file".into(),
            offset: u64::MAX - 5,
            checksums: vec!["abc".into()],
            sizes: vec![10],
        }],
    };
    assert!(matches!(
        overflow.validate(),
        Err(Error::InvalidBlockMap(ref msg)) if msg.contains("overflow")
    ));

    assert!(matches!(
        BlockMap::from_bytes(b"\x1f\x8bnot-valid-gzip"),
        Err(Error::Json(_))
    ));
}
