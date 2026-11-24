use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::blockmap::BlockMap;
use crate::chunker::ChunkerConfig;

#[cfg(test)]
mod tests;

const PACKAGE_FILENAME: &str = "current.pkg";
const BLOCKMAP_FILENAME: &str = "current.blockmap";
const STATE_FILENAME: &str = "state.json";

/// Metadata describing the locally cached base package used for differential patching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedPackageState {
    /// Semantic version of the cached package.
    pub version: String,
    /// Package size in bytes.
    pub size: u64,
    /// Optional base64-encoded SHA-512 checksum.
    pub sha512: Option<String>,
}

/// Resolved local base artifact and its corresponding blockmap.
#[derive(Debug, Clone)]
pub struct LocalBasePackage {
    /// Path to the existing artifact file on disk.
    pub package_path: PathBuf,
    /// Parsed blockmap describing `package_path`.
    pub blockmap: BlockMap,
}

/// Manages the single cached base package and `.blockmap` used across sequential updates.
#[derive(Debug, Clone)]
pub struct PackageCache {
    directory: PathBuf,
    appimage_fallback: Option<PathBuf>,
}

impl PackageCache {
    /// Creates a cache rooted at `directory`, falling back to `$APPIMAGE` when empty.
    pub fn new(directory: PathBuf) -> Self {
        Self::with_appimage_fallback(directory, std::env::var_os("APPIMAGE").map(PathBuf::from))
    }

    /// Creates a cache rooted at `directory` with an explicit optional AppImage fallback path.
    pub fn with_appimage_fallback(directory: PathBuf, appimage_fallback: Option<PathBuf>) -> Self {
        Self {
            directory,
            appimage_fallback,
        }
    }

    /// Returns the cache directory path.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Resolves the best available local base package, falling back to `$APPIMAGE` when running as an AppImage.
    pub fn load_base(&self) -> Option<LocalBasePackage> {
        if let Some(cached) = self.load_cached() {
            return Some(cached);
        }
        self.load_running_appimage()
    }

    /// Commits a verified downloaded package and its blockmap into the cache atomically.
    pub fn commit(
        &self,
        package_file: &Path,
        blockmap: &BlockMap,
        state: &CachedPackageState,
    ) -> Result<PathBuf> {
        let blockmap_bytes = blockmap.to_gzip_bytes()?;
        let state_bytes = serde_json::to_vec_pretty(state)?;
        std::fs::create_dir_all(&self.directory)?;

        let staging_pkg = self.directory.join("staging.pkg");
        scopeguard::defer! {
            let _ = std::fs::remove_file(&staging_pkg);
        }
        if std::fs::rename(package_file, &staging_pkg).is_err() {
            std::fs::copy(package_file, &staging_pkg)?;
        }

        let staging_map = self.directory.join("staging.blockmap");
        scopeguard::defer! {
            let _ = std::fs::remove_file(&staging_map);
        }
        std::fs::write(&staging_map, blockmap_bytes)?;

        let staging_state = self.directory.join("staging.state.json");
        scopeguard::defer! {
            let _ = std::fs::remove_file(&staging_state);
        }
        std::fs::write(&staging_state, state_bytes)?;

        let target_pkg = self.directory.join(PACKAGE_FILENAME);
        std::fs::rename(&staging_pkg, &target_pkg)?;
        std::fs::rename(&staging_map, self.directory.join(BLOCKMAP_FILENAME))?;
        std::fs::rename(&staging_state, self.directory.join(STATE_FILENAME))?;

        Ok(target_pkg)
    }

    /// Seeds the cache from an existing package file on disk without removing the source file.
    pub fn seed_from_file(&self, package_path: &Path, version: &str) -> Result<LocalBasePackage> {
        std::fs::create_dir_all(&self.directory)?;
        let artifact = BlockMap::generate_from_path(package_path, ChunkerConfig::default())?;
        let state = CachedPackageState {
            version: version.to_string(),
            size: artifact.size,
            sha512: Some(artifact.sha512),
        };
        let copy_staging = self.directory.join("seed.pkg");
        scopeguard::defer! {
            let _ = std::fs::remove_file(&copy_staging);
        }
        std::fs::copy(package_path, &copy_staging)?;
        let stored_path = self.commit(&copy_staging, &artifact.blockmap, &state)?;
        Ok(LocalBasePackage {
            package_path: stored_path,
            blockmap: artifact.blockmap,
        })
    }

    /// Reads the stored state metadata if present and valid.
    pub fn state(&self) -> Option<CachedPackageState> {
        let raw = std::fs::read(self.directory.join(STATE_FILENAME)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    fn load_cached(&self) -> Option<LocalBasePackage> {
        let package_path = self.directory.join(PACKAGE_FILENAME);
        let blockmap_path = self.directory.join(BLOCKMAP_FILENAME);
        if !package_path.is_file() || !blockmap_path.is_file() {
            return None;
        }
        let raw_map = std::fs::read(&blockmap_path).ok()?;
        let blockmap = BlockMap::from_bytes(&raw_map).ok()?;
        let actual_size = std::fs::metadata(&package_path).ok()?.len();
        if actual_size != blockmap.total_size() {
            tracing::warn!(
                expected = blockmap.total_size(),
                actual = actual_size,
                "cached update package size disagrees with cached blockmap; ignoring cache"
            );
            return None;
        }
        Some(LocalBasePackage {
            package_path,
            blockmap,
        })
    }

    fn load_running_appimage(&self) -> Option<LocalBasePackage> {
        let appimage_path = self.appimage_fallback.clone()?;
        if !appimage_path.is_file() {
            return None;
        }
        match BlockMap::generate_from_path(&appimage_path, ChunkerConfig::default()) {
            Ok(artifact) => Some(LocalBasePackage {
                package_path: appimage_path,
                blockmap: artifact.blockmap,
            }),
            Err(error) => {
                tracing::warn!(error = %error, "generate blockmap from running AppImage failed");
                None
            }
        }
    }
}
