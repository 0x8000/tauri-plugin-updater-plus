use crate::{Error, Result};

#[cfg(test)]
mod tests;

const GEAR_TABLE: [u64; 256] = build_gear_table();
const WINDOW_BYTES: usize = 64;

const fn build_gear_table() -> [u64; 256] {
    let mut table = [0u64; 256];
    let mut state = 0x5550_4441_5445_5231u64;
    let mut index = 0;
    while index < 256 {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        table[index] = z ^ (z >> 31);
        index += 1;
    }
    table
}

/// Configuration for FastCDC content-defined chunking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkerConfig {
    /// Minimum chunk size in bytes.
    pub min_size: usize,
    /// Target average chunk size in bytes.
    pub avg_size: usize,
    /// Maximum chunk size in bytes.
    pub max_size: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            min_size: 16 * 1024,
            avg_size: 64 * 1024,
            max_size: 256 * 1024,
        }
    }
}

impl ChunkerConfig {
    /// Validates chunker size invariants.
    pub fn validate(&self) -> Result<()> {
        if self.min_size < 1024 {
            return Err(Error::InvalidBlockMap(
                "chunker min_size must be at least 1024 bytes".into(),
            ));
        }
        if self.min_size > self.avg_size || self.avg_size > self.max_size {
            return Err(Error::InvalidBlockMap(
                "chunker sizes must satisfy min_size <= avg_size <= max_size".into(),
            ));
        }
        if u32::try_from(self.max_size).is_err() {
            return Err(Error::InvalidBlockMap(
                "chunker max_size exceeds u32 range".into(),
            ));
        }
        Ok(())
    }
}

/// Deterministic normalized FastCDC chunker using a 64-byte Gear rolling hash.
#[derive(Debug, Clone, Copy)]
pub struct FastCdc {
    config: ChunkerConfig,
    mask_strict: u64,
    mask_relaxed: u64,
}

impl FastCdc {
    /// Creates a validated FastCDC chunker.
    pub fn new(config: ChunkerConfig) -> Result<Self> {
        config.validate()?;
        let bits = config.avg_size.ilog2().clamp(4, 30);
        let strict_bits = (bits + 1).min(31);
        let relaxed_bits = bits.saturating_sub(1).max(1);
        let mask_strict = ((1u64 << strict_bits) - 1) << (64 - strict_bits);
        let mask_relaxed = ((1u64 << relaxed_bits) - 1) << (64 - relaxed_bits);
        Ok(Self {
            config,
            mask_strict,
            mask_relaxed,
        })
    }

    /// Returns the active chunker configuration.
    pub fn config(&self) -> ChunkerConfig {
        self.config
    }

    /// Finds the next cutpoint length within `data`, bounded by `[min_size, max_size]`.
    pub fn next_chunk_len(&self, data: &[u8]) -> usize {
        let len = data.len();
        if len <= self.config.min_size {
            return len;
        }
        let max_len = len.min(self.config.max_size);
        let normal_len = max_len.min(self.config.avg_size);
        let seed_start = self.config.min_size.saturating_sub(WINDOW_BYTES);
        let mut fingerprint = 0u64;

        for &byte in &data[seed_start..self.config.min_size] {
            fingerprint = (fingerprint << 1).wrapping_add(GEAR_TABLE[byte as usize]);
        }
        for (offset, &byte) in data[self.config.min_size..normal_len].iter().enumerate() {
            fingerprint = (fingerprint << 1).wrapping_add(GEAR_TABLE[byte as usize]);
            if (fingerprint & self.mask_strict) == 0 {
                return self.config.min_size + offset + 1;
            }
        }
        for (offset, &byte) in data[normal_len..max_len].iter().enumerate() {
            fingerprint = (fingerprint << 1).wrapping_add(GEAR_TABLE[byte as usize]);
            if (fingerprint & self.mask_relaxed) == 0 {
                return normal_len + offset + 1;
            }
        }
        max_len
    }
}
