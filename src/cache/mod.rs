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
    /// Creates a cache rooted at `directory`, falling back to `$APPIMAGE` when empty on Linux AppImage builds.
    pub fn new(directory: PathBuf) -> Self {
        #[cfg(target_os = "linux")]
        let fallback = if matches!(
            tauri::utils::platform::bundle_type(),
            Some(tauri::utils::config::BundleType::Deb | tauri::utils::config::BundleType::Rpm)
        ) {
            None
        } else {
            std::env::var_os("APPIMAGE").map(PathBuf::from)
        };
        #[cfg(not(target_os = "linux"))]
        let fallback = None;
        Self::with_appimage_fallback(directory, fallback)
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
        use std::io::Write;

        let blockmap_bytes = blockmap.to_gzip_bytes()?;
        let state_bytes = serde_json::to_vec_pretty(state)?;
        std::fs::create_dir_all(&self.directory)?;

        let staging_pkg = tempfile::Builder::new()
            .prefix(".staging-pkg-")
            .tempfile_in(&self.directory)?
            .into_temp_path();
        if std::fs::rename(package_file, &staging_pkg).is_err() {
            std::fs::copy(package_file, &staging_pkg)?;
        }

        let mut staging_map = tempfile::Builder::new()
            .prefix(".staging-map-")
            .tempfile_in(&self.directory)?;
        staging_map.write_all(&blockmap_bytes)?;
        staging_map.as_file().sync_all()?;

        let mut staging_state = tempfile::Builder::new()
            .prefix(".staging-state-")
            .tempfile_in(&self.directory)?;
        staging_state.write_all(&state_bytes)?;
        staging_state.as_file().sync_all()?;

        let target_pkg = self.directory.join(PACKAGE_FILENAME);
        staging_pkg.persist(&target_pkg).map_err(|e| e.error)?;
        staging_map
            .persist(self.directory.join(BLOCKMAP_FILENAME))
            .map_err(|e| e.error)?;
        staging_state
            .persist(self.directory.join(STATE_FILENAME))
            .map_err(|e| e.error)?;

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
        let copy_staging = tempfile::Builder::new()
            .prefix(".seed-pkg-")
            .tempfile_in(&self.directory)?
            .into_temp_path();
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
