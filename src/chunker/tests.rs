use std::collections::HashSet;

use super::{ChunkerConfig, FastCdc};
use crate::Error;

fn pseudo_random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut output = Vec::with_capacity(len);
    for _ in 0..len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        output.push((state >> 56) as u8);
    }
    output
}

fn split_all(chunker: &FastCdc, data: &[u8]) -> Vec<Vec<u8>> {
    let mut offset = 0;
    let mut chunks = Vec::new();
    while offset < data.len() {
        let len = chunker.next_chunk_len(&data[offset..]);
        assert!(len > 0, "chunker must always advance on non-empty input");
        chunks.push(data[offset..offset + len].to_vec());
        offset += len;
    }
    chunks
}

#[test]
fn enforces_size_bounds_and_reconstructs_exact_input() {
    let config = ChunkerConfig {
        min_size: 4 * 1024,
        avg_size: 16 * 1024,
        max_size: 64 * 1024,
    };
    let chunker = FastCdc::new(config).expect("valid chunker config");
    let data = pseudo_random_bytes(512 * 1024, 0x1234_5678);
    let chunks = split_all(&chunker, &data);

    assert!(chunks.len() > 8, "expected multiple content-defined chunks");
    assert_eq!(chunks.concat(), data);
    for chunk in &chunks[..chunks.len() - 1] {
        assert!(chunk.len() >= config.min_size);
        assert!(chunk.len() <= config.max_size);
    }
    assert!(chunks.last().expect("at least one chunk").len() <= config.max_size);
}

#[test]
fn handles_empty_small_and_uniform_inputs_at_boundaries() {
    let config = ChunkerConfig {
        min_size: 4 * 1024,
        avg_size: 16 * 1024,
        max_size: 64 * 1024,
    };
    let chunker = FastCdc::new(config).expect("valid chunker config");

    assert_eq!(chunker.next_chunk_len(&[]), 0);
    assert_eq!(chunker.next_chunk_len(&[1u8; 512]), 512);
    assert_eq!(chunker.next_chunk_len(&[1u8; 4 * 1024]), 4 * 1024);
    assert_eq!(
        chunker.next_chunk_len(&vec![0u8; config.max_size + 1024]),
        config.max_size
    );
}

#[test]
fn resists_byte_insertions_with_high_chunk_reuse() {
    let config = ChunkerConfig {
        min_size: 4 * 1024,
        avg_size: 16 * 1024,
        max_size: 64 * 1024,
    };
    let chunker = FastCdc::new(config).expect("valid chunker config");
    let original = pseudo_random_bytes(1024 * 1024, 0xdead_beef);
    let mut shifted = vec![0xaa; 137];
    shifted.extend_from_slice(&original);

    let old_set: HashSet<Vec<u8>> = split_all(&chunker, &original).into_iter().collect();
    let new_chunks = split_all(&chunker, &shifted);
    let reused_bytes: usize = new_chunks
        .iter()
        .filter(|chunk| old_set.contains(*chunk))
        .map(Vec::len)
        .sum();

    let reuse_ratio = reused_bytes as f64 / shifted.len() as f64;
    assert!(
        reuse_ratio > 0.90,
        "expected >90% chunk reuse after prefix insertion, got {:.2}%",
        reuse_ratio * 100.0
    );
}

#[test]
fn rejects_invalid_config_with_typed_error() {
    let too_small = ChunkerConfig {
        min_size: 512,
        avg_size: 4096,
        max_size: 8192,
    };
    assert!(matches!(
        FastCdc::new(too_small),
        Err(Error::InvalidBlockMap(ref msg)) if msg.contains("min_size")
    ));

    let inverted_min = ChunkerConfig {
        min_size: 32 * 1024,
        avg_size: 16 * 1024,
        max_size: 64 * 1024,
    };
    assert!(matches!(
        FastCdc::new(inverted_min),
        Err(Error::InvalidBlockMap(ref msg)) if msg.contains("min_size <= avg_size <= max_size")
    ));

    let inverted_max = ChunkerConfig {
        min_size: 4 * 1024,
        avg_size: 64 * 1024,
        max_size: 32 * 1024,
    };
    assert!(matches!(
        FastCdc::new(inverted_max),
        Err(Error::InvalidBlockMap(ref msg)) if msg.contains("min_size <= avg_size <= max_size")
    ));
}
