use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::blockmap::BlockMap;
use crate::cache::{CachedPackageState, PackageCache};
use crate::chunker::ChunkerConfig;
use crate::listener::DownloadSummary;
use crate::verifier::{ArtifactVerifier, VerifyExpectation};
use crate::{Error, Result};

mod coverage;
mod diff;
mod full;
#[cfg(test)]
mod tests;

pub(crate) use diff::DiffSessionOpenParams;
pub use diff::{DiffPlanInfo, DiffSession};
pub use full::FullSession;

/// Verified package stored on disk along with transfer metrics.
#[derive(Debug, Clone)]
pub struct DownloadedPackage {
    /// Path to the verified package committed inside the updater cache.
    pub path: PathBuf,
    /// Transfer statistics and mode used for this download.
    pub summary: DownloadSummary,
}

pub(crate) fn verify_reconstructed_file(
    path: &Path,
    blockmap: &BlockMap,
    expectation: &VerifyExpectation,
) -> Result<()> {
    ArtifactVerifier::verify_file(path, expectation)?;
    if expectation.sha512.is_none() {
        let mut file = File::open(path)?;
        let mut scratch = Vec::new();
        for entry in &blockmap.files {
            file.seek(SeekFrom::Start(entry.offset))?;
            for (checksum, &size) in entry.checksums.iter().zip(&entry.sizes) {
                scratch.resize(size as usize, 0);
                file.read_exact(&mut scratch)?;
                if !BlockMap::verify_chunk(&scratch, checksum) {
                    return Err(Error::DifferentialTransfer(
                        "reconstructed chunk failed blockmap SHA-256 verification".into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn commit_verified_file(
    cache: &PackageCache,
    version: &str,
    verification: &VerifyExpectation,
    verified_file: &Path,
    known_blockmap: Option<&BlockMap>,
) -> Result<PathBuf> {
    let actual_size = std::fs::metadata(verified_file)?.len();
    let (blockmap, sha512) = match known_blockmap {
        Some(map) if map.total_size() == actual_size => (map.clone(), verification.sha512.clone()),
        _ => {
            let artifact = BlockMap::generate_from_path(verified_file, ChunkerConfig::default())?;
            (artifact.blockmap, Some(artifact.sha512))
        }
    };
    let state = CachedPackageState {
        version: version.to_string(),
        size: actual_size,
        sha512,
    };
    cache.commit(verified_file, &blockmap, &state)
}
