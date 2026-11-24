use std::io::Write;

use super::{DownloadedPackage, commit_verified_file};
use crate::Result;
use crate::blockmap::BlockMap;
use crate::cache::PackageCache;
use crate::listener::{DownloadMode, DownloadSummary};
use crate::verifier::{ArtifactVerifier, VerifyExpectation, VerifyPolicy};

/// Transport-agnostic full-artifact staging session for Push or Pull full downloads.
pub struct FullSession {
    cache: PackageCache,
    version: String,
    expectation: VerifyExpectation,
    temp_file: tempfile::NamedTempFile,
    written_bytes: u64,
}

impl FullSession {
    /// Opens a fail-closed full-artifact staging session backed by `cache`.
    pub fn open(
        cache: PackageCache,
        version: impl Into<String>,
        policy: VerifyPolicy,
    ) -> Result<Self> {
        Self::open_with_expectation(cache, version.into(), policy.to_expectation())
    }

    pub(crate) fn open_with_expectation(
        cache: PackageCache,
        version: String,
        expectation: VerifyExpectation,
    ) -> Result<Self> {
        std::fs::create_dir_all(cache.directory())?;
        let temp_file = tempfile::NamedTempFile::new_in(cache.directory())?;
        Ok(Self {
            cache,
            version,
            expectation,
            temp_file,
            written_bytes: 0,
        })
    }

    /// Appends a sequential chunk of artifact bytes to the staging file.
    pub fn write_chunk(&mut self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.temp_file.as_file_mut().write_all(data)?;
        self.written_bytes = self.written_bytes.saturating_add(data.len() as u64);
        Ok(())
    }

    /// Returns the total number of bytes written to this staging session so far.
    pub fn written_bytes(&self) -> u64 {
        self.written_bytes
    }

    /// Verifies the staged artifact against its [`VerifyPolicy`] and commits it atomically into cache.
    pub fn finish_and_verify(self, known_blockmap: Option<&BlockMap>) -> Result<DownloadedPackage> {
        self.finish_with_stats(known_blockmap, 1, None)
    }

    pub(crate) fn finish_with_stats(
        mut self,
        known_blockmap: Option<&BlockMap>,
        request_count: usize,
        fallback_reason: Option<String>,
    ) -> Result<DownloadedPackage> {
        self.temp_file.as_file_mut().flush()?;
        let temp_path = self.temp_file.into_temp_path();

        ArtifactVerifier::verify_file(&temp_path, &self.expectation)?;
        let path = commit_verified_file(
            &self.cache,
            &self.version,
            &self.expectation,
            &temp_path,
            known_blockmap,
        )?;

        let summary = DownloadSummary::from_counts(
            DownloadMode::Full,
            self.written_bytes,
            self.written_bytes,
            0,
        )
        .with_details(request_count, fallback_reason);
        Ok(DownloadedPackage { path, summary })
    }
}
