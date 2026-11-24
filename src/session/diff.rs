use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::coverage::RangeCoverage;
use super::{DownloadedPackage, commit_verified_file, verify_reconstructed_file};
use crate::blockmap::BlockMap;
use crate::cache::PackageCache;
use crate::listener::{DownloadMode, DownloadSummary};
use crate::planner::{ByteRange, DownloadPlan, PlanOperation, PlanOptions};
use crate::verifier::{VerifyExpectation, VerifyPolicy};
use crate::{Error, Result};

/// Summary of local block reuse and remaining byte ranges computed when opening a [`DiffSession`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffPlanInfo {
    /// Total target artifact size in bytes.
    pub total_size: u64,
    /// Bytes copied and SHA-256 verified from the local base package.
    pub reused_bytes: u64,
    /// Remaining missing byte ranges that the caller must feed via [`DiffSession::write_range`].
    pub missing_ranges: Vec<ByteRange>,
    /// Total number of missing bytes across `missing_ranges`.
    pub expected_download_bytes: u64,
}

pub(crate) struct DiffSessionOpenParams {
    pub(crate) cache: PackageCache,
    pub(crate) version: String,
    pub(crate) remote_blockmap: BlockMap,
    pub(crate) expectation: VerifyExpectation,
}

/// Transport-agnostic differential staging session.
///
/// Designed for both Pull providers and Push/Pub-Sub transports (such as MQTT or DDS):
/// 1. [`DiffSession::open`] copies and SHA-256-verifies unchanged blocks from the local base
///    package and reports the exact missing [`ByteRange`] spans.
/// 2. The caller supplies missing bytes in any order via [`DiffSession::write_range`]; duplicate
///    or overlapping deliveries are handled idempotently.
/// 3. [`DiffSession::finish_and_verify`] asserts 100% byte coverage, validates the reconstructed
///    artifact against a fail-closed [`VerifyPolicy`], and commits it atomically into [`PackageCache`].
///
/// Methods perform synchronous filesystem I/O; wrap calls in `tokio::task::spawn_blocking` when
/// invoking from an async runtime.
pub struct DiffSession {
    cache: PackageCache,
    version: String,
    blockmap: BlockMap,
    expectation: VerifyExpectation,
    temp_file: tempfile::NamedTempFile,
    total_size: u64,
    reused_bytes: u64,
    coverage: RangeCoverage,
    covered_remote_bytes: u64,
}

impl DiffSession {
    /// Opens a fail-closed differential staging session against the locally cached base package.
    pub fn open(
        cache: PackageCache,
        version: impl Into<String>,
        remote_blockmap: BlockMap,
        policy: VerifyPolicy,
        max_range_gap: u64,
    ) -> Result<(Self, DiffPlanInfo)> {
        let base = cache.load_base().ok_or_else(|| {
            Error::DifferentialTransfer(
                "no local base package available for differential session".into(),
            )
        })?;
        let plan = DownloadPlan::build(
            &base.blockmap,
            &remote_blockmap,
            PlanOptions { max_range_gap },
        )?;
        let params = DiffSessionOpenParams {
            cache,
            version: version.into(),
            remote_blockmap,
            expectation: policy.to_expectation(),
        };
        Self::open_with_plan(params, &base.package_path, &plan)
    }

    pub(crate) fn open_with_plan(
        params: DiffSessionOpenParams,
        base_path: &Path,
        plan: &DownloadPlan,
    ) -> Result<(Self, DiffPlanInfo)> {
        std::fs::create_dir_all(params.cache.directory())?;
        let mut temp_file = tempfile::NamedTempFile::new_in(params.cache.directory())?;
        temp_file.as_file().set_len(plan.total_size)?;

        let (missing_ranges, reused_bytes) = copy_local_blocks(
            base_path,
            temp_file.as_file_mut(),
            &params.remote_blockmap,
            plan,
        )?;
        let coverage = RangeCoverage::new(missing_ranges);
        let expected_download_bytes = coverage.remaining_bytes();
        let info = DiffPlanInfo {
            total_size: plan.total_size,
            reused_bytes,
            missing_ranges: coverage.missing_ranges().to_vec(),
            expected_download_bytes,
        };

        let session = Self {
            cache: params.cache,
            version: params.version,
            blockmap: params.remote_blockmap,
            expectation: params.expectation,
            temp_file,
            total_size: plan.total_size,
            reused_bytes,
            coverage,
            covered_remote_bytes: 0,
        };
        Ok((session, info))
    }

    /// Writes `data` at `offset` in the staging file and returns newly covered byte count.
    ///
    /// Out-of-bounds writes are rejected. Duplicate or overlapping writes (e.g. MQTT QoS 1
    /// redeliveries) are written safely without double-counting progress.
    pub fn write_range(&mut self, offset: u64, data: &[u8]) -> Result<usize> {
        let end_exclusive = offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| Error::DifferentialTransfer("range write offset overflow".into()))?;
        if end_exclusive > self.total_size {
            return Err(Error::DifferentialTransfer(format!(
                "range write {offset}..{end_exclusive} exceeds target artifact size {}",
                self.total_size
            )));
        }
        if data.is_empty() {
            return Ok(0);
        }

        let file = self.temp_file.as_file_mut();
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)?;

        let newly_covered = self.coverage.record_write(offset, data.len() as u64);
        self.covered_remote_bytes = self.covered_remote_bytes.saturating_add(newly_covered);
        Ok(newly_covered as usize)
    }

    /// Returns the remaining uncovered byte ranges.
    pub fn missing_ranges(&self) -> &[ByteRange] {
        self.coverage.missing_ranges()
    }

    /// Returns the number of uncovered bytes still needed before verification can run.
    pub fn remaining_bytes(&self) -> u64 {
        self.coverage.remaining_bytes()
    }

    /// Returns true once all missing byte ranges have been covered.
    pub fn is_complete(&self) -> bool {
        self.coverage.is_complete()
    }

    /// Returns the total target artifact size in bytes.
    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    /// Returns the number of bytes reused from the local base package.
    pub fn reused_bytes(&self) -> u64 {
        self.reused_bytes
    }

    /// Verifies 100% coverage and cryptographic integrity, then commits the package into cache.
    pub fn finish_and_verify(self) -> Result<DownloadedPackage> {
        self.finish_with_stats(0)
    }

    /// Verifies and commits the staged package, recording `request_count` in [`DownloadSummary`].
    pub fn finish_with_stats(mut self, request_count: usize) -> Result<DownloadedPackage> {
        let remaining = self.coverage.remaining_bytes();
        if remaining > 0 {
            return Err(Error::IncompleteTransfer {
                missing_bytes: remaining,
            });
        }
        self.temp_file.as_file_mut().flush()?;
        // Release the file handle before verifying and persisting (required for atomic rename on Windows).
        let temp_path = self.temp_file.into_temp_path();

        verify_reconstructed_file(&temp_path, &self.blockmap, &self.expectation)?;
        let path = commit_verified_file(
            &self.cache,
            &self.version,
            &self.expectation,
            &temp_path,
            Some(&self.blockmap),
        )?;

        let summary = DownloadSummary::from_counts(
            DownloadMode::Differential,
            self.total_size,
            self.covered_remote_bytes,
            self.reused_bytes,
        )
        .with_details(request_count, None);

        Ok(DownloadedPackage { path, summary })
    }
}

fn copy_local_blocks(
    old_path: &Path,
    target: &mut File,
    new_blockmap: &BlockMap,
    plan: &DownloadPlan,
) -> Result<(Vec<ByteRange>, u64)> {
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

    let mut old_file = File::open(old_path)?;
    let mut ranges = Vec::new();
    let mut reused_bytes = 0u64;
    let blocks = flatten_blocks(new_blockmap);
    let mut block_idx = 0usize;
    let mut scratch = Vec::new();

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

                    scratch.resize(chunk_len as usize, 0);
                    old_file.seek(SeekFrom::Start(cursor_old))?;
                    old_file.read_exact(&mut scratch)?;

                    let valid = expected_hash
                        .map(|hash| BlockMap::verify_chunk(&scratch, hash))
                        .unwrap_or(true);
                    if valid {
                        target.seek(SeekFrom::Start(cursor_new))?;
                        target.write_all(&scratch)?;
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

    Ok((RangeCoverage::coalesce(ranges), reused_bytes))
}
