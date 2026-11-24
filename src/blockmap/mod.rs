use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use aws_lc_rs::digest;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};

use crate::chunker::{ChunkerConfig, FastCdc};
use crate::{Error, Result};

#[cfg(test)]
mod tests;

const BLOCKMAP_VERSION: &str = "2";
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const MAX_BLOCKMAP_JSON_BYTES: u64 = 64 * 1024 * 1024;

struct CappedReader<'a, R> {
    inner: R,
    remaining: u64,
    exceeded: &'a mut bool,
}

impl<R: Read> Read for CappedReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(buf)?;
        if count as u64 > self.remaining {
            *self.exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "decompressed blockmap exceeds maximum size limit",
            ));
        }
        self.remaining -= count as u64;
        Ok(count)
    }
}

/// Single file entry inside an Electron-compatible v2 blockmap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockMapFile {
    /// Logical file name inside the package (conventionally `"file"` for whole-artifact maps).
    pub name: String,
    /// Starting byte offset of this file inside the distribution artifact.
    pub offset: u64,
    /// Base64-encoded SHA-256 digests for each sequential chunk.
    pub checksums: Vec<String>,
    /// Byte length of each sequential chunk.
    pub sizes: Vec<u32>,
}

/// Metadata produced alongside a freshly generated [`BlockMap`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockMapArtifact {
    /// Generated Electron v2 blockmap.
    pub blockmap: BlockMap,
    /// Total artifact size in bytes.
    pub size: u64,
    /// Base64-encoded SHA-512 digest of the full artifact.
    pub sha512: String,
}

/// Electron-compatible v2 blockmap describing content-defined chunks of an update artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockMap {
    /// Format version (`"2"`).
    pub version: String,
    /// Chunked file entries covered by this blockmap.
    pub files: Vec<BlockMapFile>,
}

impl BlockMap {
    /// Builds a blockmap and full-file SHA-512 digest in a single streaming pass over `reader`.
    pub fn generate<R: Read>(
        mut reader: R,
        file_name: impl Into<String>,
        config: ChunkerConfig,
    ) -> Result<BlockMapArtifact> {
        let chunker = FastCdc::new(config)?;
        let mut checksums = Vec::new();
        let mut sizes = Vec::new();
        let mut file_hasher = digest::Context::new(&digest::SHA512);
        let mut total_size = 0u64;
        let buffer_cap = config.max_size.saturating_mul(2);
        let mut buffer = Vec::with_capacity(buffer_cap);
        let mut read_scratch = vec![0u8; config.max_size];
        let mut eof = false;

        loop {
            while !eof && buffer.len() < config.max_size {
                let read_len = reader.read(&mut read_scratch)?;
                if read_len == 0 {
                    eof = true;
                } else {
                    file_hasher.update(&read_scratch[..read_len]);
                    buffer.extend_from_slice(&read_scratch[..read_len]);
                    total_size = total_size.checked_add(read_len as u64).ok_or_else(|| {
                        Error::InvalidBlockMap("artifact size exceeds u64 range".into())
                    })?;
                }
            }
            if buffer.is_empty() {
                break;
            }
            let chunk_len = if eof && buffer.len() <= config.min_size {
                buffer.len()
            } else {
                chunker.next_chunk_len(&buffer)
            };
            let chunk_digest = digest::digest(&digest::SHA256, &buffer[..chunk_len]);
            checksums.push(STANDARD.encode(chunk_digest.as_ref()));
            let size = u32::try_from(chunk_len)
                .map_err(|_| Error::InvalidBlockMap("chunk size exceeds u32".into()))?;
            sizes.push(size);
            buffer.drain(..chunk_len);
        }

        let blockmap = Self {
            version: BLOCKMAP_VERSION.to_string(),
            files: vec![BlockMapFile {
                name: file_name.into(),
                offset: 0,
                checksums,
                sizes,
            }],
        };
        blockmap.validate()?;
        Ok(BlockMapArtifact {
            blockmap,
            size: total_size,
            sha512: STANDARD.encode(file_hasher.finish().as_ref()),
        })
    }

    /// Streams a file from disk to build its blockmap and SHA-512 checksum.
    pub fn generate_from_path(path: &Path, config: ChunkerConfig) -> Result<BlockMapArtifact> {
        let file = File::open(path)?;
        let reader = BufReader::with_capacity(config.max_size, file);
        Self::generate(reader, "file", config)
    }

    /// Encodes the blockmap as gzip-compressed JSON matching Electron `.blockmap` files.
    pub fn to_gzip_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let json = serde_json::to_vec(self)?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&json)?;
        Ok(encoder.finish()?)
    }

    /// Decodes a gzip-compressed or raw JSON Electron v2 blockmap.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let parsed: Self = if bytes.starts_with(&GZIP_MAGIC) {
            let mut exceeded = false;
            let reader = CappedReader {
                inner: GzDecoder::new(bytes),
                remaining: MAX_BLOCKMAP_JSON_BYTES,
                exceeded: &mut exceeded,
            };
            match serde_json::from_reader(reader) {
                Ok(map) => map,
                Err(_) if exceeded => {
                    return Err(Error::InvalidBlockMap(
                        "decompressed blockmap exceeds 64 MiB limit".into(),
                    ));
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            if bytes.len() as u64 > MAX_BLOCKMAP_JSON_BYTES {
                return Err(Error::InvalidBlockMap(
                    "blockmap exceeds 64 MiB limit".into(),
                ));
            }
            serde_json::from_slice(bytes)?
        };
        parsed.validate()?;
        Ok(parsed)
    }

    /// Validates blockmap schema invariants.
    pub fn validate(&self) -> Result<()> {
        if self.version != BLOCKMAP_VERSION {
            return Err(Error::InvalidBlockMap(format!(
                "unsupported blockmap version '{}'",
                self.version
            )));
        }
        if self.files.is_empty() {
            return Err(Error::InvalidBlockMap(
                "blockmap contains no file entries".into(),
            ));
        }
        let mut min_next_offset = 0u64;
        for file in &self.files {
            if file.offset < min_next_offset {
                return Err(Error::InvalidBlockMap(format!(
                    "blockmap file '{}' offset {} overlaps previous entry end {}",
                    file.name, file.offset, min_next_offset
                )));
            }
            if file.checksums.len() != file.sizes.len() {
                return Err(Error::InvalidBlockMap(format!(
                    "blockmap file '{}' checksum/size length mismatch",
                    file.name
                )));
            }
            let mut cursor = file.offset;
            for &size in &file.sizes {
                if size == 0 {
                    return Err(Error::InvalidBlockMap(format!(
                        "blockmap file '{}' contains zero-sized block",
                        file.name
                    )));
                }
                cursor = cursor.checked_add(u64::from(size)).ok_or_else(|| {
                    Error::InvalidBlockMap("blockmap block offset overflow".into())
                })?;
            }
            min_next_offset = cursor;
        }
        Ok(())
    }

    /// Returns the maximum byte end offset across all mapped files.
    pub fn total_size(&self) -> u64 {
        self.files
            .iter()
            .map(|file| {
                let len: u64 = file.sizes.iter().map(|&s| u64::from(s)).sum();
                file.offset.saturating_add(len)
            })
            .max()
            .unwrap_or(0)
    }

    /// Returns the total number of blocks across all file entries.
    pub fn block_count(&self) -> usize {
        self.files.iter().map(|file| file.sizes.len()).sum()
    }

    /// Verifies a chunk's bytes against an expected base64 SHA-256 checksum (full or 24-byte Electron v2 prefix).
    pub fn verify_chunk(chunk: &[u8], expected_base64: &str) -> bool {
        let Ok(expected) = STANDARD.decode(expected_base64) else {
            return false;
        };
        if expected.len() < 16 || expected.len() > 32 {
            return false;
        }
        let actual = digest::digest(&digest::SHA256, chunk);
        actual.as_ref().starts_with(&expected)
    }
}
