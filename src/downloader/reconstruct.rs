use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::blockmap::BlockMap;
use crate::listener::{DownloadEvent, DownloadListener};
use crate::multipart::ByteRangePart;
use crate::planner::{DownloadPlan, PlanOperation};
use crate::verifier::{ArtifactVerifier, VerifyExpectation};
use crate::{Error, Result};

use super::transfer::ByteRange;

pub(crate) struct LocalCopyOutcome {
    pub(crate) missing_ranges: Vec<ByteRange>,
    pub(crate) reused_bytes: u64,
}

pub(crate) struct PackageReconstructor;

impl PackageReconstructor {
    pub(crate) fn apply_local_copies(
        old_path: &Path,
        target: &mut File,
        new_blockmap: &BlockMap,
        plan: &DownloadPlan,
    ) -> Result<LocalCopyOutcome> {
        fn flatten_blocks(blockmap: &BlockMap) -> Vec<(u64, u64, &str)> {
            let mut blocks = Vec::with_capacity(blockmap.block_count());
            for file in &blockmap.files {
                let mut cursor = file.offset;
                for (checksum, &size) in file.checksums.iter().zip(&file.sizes) {
                    blocks.push((cursor, u64::from(size), checksum.as_str()));
                    cursor = cursor.saturating_add(u64::from(size));
                }
            }
            blocks
        }

        fn coalesce_ranges(mut ranges: Vec<ByteRange>) -> Vec<ByteRange> {
            if ranges.is_empty() {
                return ranges;
            }
            ranges.sort_by_key(|range| range.start);
            let mut merged: Vec<ByteRange> = Vec::with_capacity(ranges.len());
            for range in ranges {
                if let Some(last) = merged.last_mut()
                    && last.end.saturating_add(1) >= range.start
                {
                    last.end = last.end.max(range.end);
                    continue;
                }
                merged.push(range);
            }
            merged
        }

        let mut old_file = File::open(old_path)?;
        let mut ranges = Vec::new();
        let mut reused_bytes = 0u64;
        let blocks = flatten_blocks(new_blockmap);
        let mut block_idx = 0usize;

        for operation in &plan.operations {
            match *operation {
                PlanOperation::Download { offset, length } => {
                    ranges.push(ByteRange::from_offset_len(offset, length)?);
                }
                PlanOperation::Copy {
                    old_offset,
                    new_offset,
                    length,
                } => {
                    let span_end = new_offset.saturating_add(length);
                    while block_idx < blocks.len() && blocks[block_idx].0 < new_offset {
                        block_idx += 1;
                    }
                    let mut cursor_new = new_offset;
                    let mut cursor_old = old_offset;
                    while cursor_new < span_end {
                        let (chunk_len, expected_hash) =
                            if block_idx < blocks.len() && blocks[block_idx].0 == cursor_new {
                                let entry = blocks[block_idx];
                                block_idx += 1;
                                (entry.1.min(span_end - cursor_new), Some(entry.2))
                            } else {
                                ((span_end - cursor_new).min(256 * 1024), None)
                            };

                        let mut buf = vec![0u8; chunk_len as usize];
                        old_file.seek(SeekFrom::Start(cursor_old))?;
                        old_file.read_exact(&mut buf)?;

                        let valid = expected_hash
                            .map(|hash| BlockMap::verify_chunk(&buf, hash))
                            .unwrap_or(true);
                        if valid {
                            target.seek(SeekFrom::Start(cursor_new))?;
                            target.write_all(&buf)?;
                            reused_bytes = reused_bytes.saturating_add(chunk_len);
                        } else {
                            ranges.push(ByteRange::from_offset_len(cursor_new, chunk_len)?);
                        }
                        cursor_new = cursor_new.saturating_add(chunk_len);
                        cursor_old = cursor_old.saturating_add(chunk_len);
                    }
                }
            }
        }

        Ok(LocalCopyOutcome {
            missing_ranges: coalesce_ranges(ranges),
            reused_bytes,
        })
    }

    pub(crate) fn write_part<L: DownloadListener>(
        target: &mut File,
        part: &ByteRangePart,
        listener: &L,
    ) -> Result<()> {
        target.seek(SeekFrom::Start(part.start))?;
        target.write_all(&part.data)?;
        listener.on_event(DownloadEvent::Progress {
            chunk_length: part.data.len(),
        });
        Ok(())
    }

    pub(crate) fn verify_reconstructed(
        path: &Path,
        blockmap: &BlockMap,
        expectation: &VerifyExpectation,
    ) -> Result<()> {
        ArtifactVerifier::verify_file(path, expectation)?;
        if expectation.sha512.is_none() {
            let mut file = File::open(path)?;
            for entry in &blockmap.files {
                file.seek(SeekFrom::Start(entry.offset))?;
                for (checksum, &size) in entry.checksums.iter().zip(&entry.sizes) {
                    let mut buf = vec![0u8; size as usize];
                    file.read_exact(&mut buf)?;
                    if !BlockMap::verify_chunk(&buf, checksum) {
                        return Err(Error::DifferentialTransfer(
                            "reconstructed chunk failed blockmap SHA-256 verification".into(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}
