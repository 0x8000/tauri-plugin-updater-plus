use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "http")]
use http::HeaderMap;
#[cfg(feature = "plugin")]
use tauri::Resource;
use url::Url;

use crate::cache::PackageCache;
use crate::config::Config;
use crate::downloader::{DifferentialDownloader, ProviderDownloadRequest};
use crate::installer::{InstallContext, PlatformInstaller};
use crate::listener::{DownloadListener, DownloadSummary};
use crate::provider::{ArtifactId, TransferOverrides, UpdateProvider};
use crate::session::DownloadedPackage;
use crate::verifier::VerifyExpectation;
use crate::{Error, Result};

/// Discovered update ready to be downloaded (differentially via blockmap) and installed.
#[derive(Clone)]
pub struct Update {
    /// Currently running application version.
    pub current_version: String,
    /// Target version offered by the remote release.
    pub version: String,
    /// Optional RFC 3339 publication timestamp.
    pub date: Option<String>,
    /// Optional release notes text.
    pub body: Option<String>,
    /// Raw manifest/release metadata JSON payload.
    pub raw_json: serde_json::Value,
    /// Opaque target artifact identifier resolved by the active [`UpdateProvider`].
    pub artifact: ArtifactId,
    /// Optional opaque `.blockmap` identifier resolved by the active [`UpdateProvider`].
    pub blockmap: Option<ArtifactId>,
    /// Optional base64 SHA-512 checksum.
    pub sha512: Option<String>,
    /// Optional base64 minisign signature.
    pub signature: Option<String>,
    /// Optional manifest-reported artifact size.
    pub size: Option<u64>,
    pub(crate) provider: Arc<dyn UpdateProvider>,
    pub(crate) app_name: String,
    pub(crate) extract_path: PathBuf,
    pub(crate) cache: PackageCache,
    pub(crate) overrides: TransferOverrides,
    pub(crate) config: Config,
    pub(crate) restart_after_install: bool,
}

impl fmt::Debug for Update {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Update")
            .field("current_version", &self.current_version)
            .field("version", &self.version)
            .field("date", &self.date)
            .field("artifact", &self.artifact)
            .field("blockmap", &self.blockmap)
            .field("size", &self.size)
            .field("provider", &self.provider)
            .finish()
    }
}

#[cfg(feature = "plugin")]
impl Resource for Update {}

impl Update {
    /// Returns the opaque artifact identifier for this release.
    pub fn artifact_id(&self) -> &ArtifactId {
        &self.artifact
    }

    /// Returns the opaque `.blockmap` identifier, if announced by the provider.
    pub fn blockmap_id(&self) -> Option<&ArtifactId> {
        self.blockmap.as_ref()
    }

    /// Parses `artifact` as a [`Url`] when backed by a URL-addressed provider (e.g. HTTP).
    pub fn download_url(&self) -> Option<Url> {
        Url::parse(self.artifact.as_str()).ok()
    }

    /// Replaces transport metadata (mapped to HTTP headers or gRPC metadata).
    pub fn with_metadata(mut self, metadata: Vec<(String, String)>) -> Self {
        self.overrides.metadata = metadata;
        self
    }

    /// Appends a single transport metadata key-value entry.
    pub fn with_metadata_entry(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.overrides.push_metadata(key, value);
        self
    }

    /// Overrides HTTP request headers used during blockmap and range downloads.
    #[cfg(feature = "http")]
    pub fn with_headers(mut self, headers: HeaderMap) -> Self {
        self.overrides.metadata = headers
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
        self
    }

    /// Overrides request timeout for download transfers.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.overrides.timeout = Some(timeout);
        self
    }

    /// Controls whether the Windows installer automatically restarts the app.
    pub fn restart_after_install(mut self, restart: bool) -> Self {
        self.restart_after_install = restart;
        self
    }

    /// Downloads (using blockmap delta when possible), verifies, and caches the package on disk.
    pub async fn download<L: DownloadListener>(&self, listener: &L) -> Result<DownloadedPackage> {
        let downloader =
            DifferentialDownloader::with_provider(Arc::clone(&self.provider), self.cache.clone());
        let request = ProviderDownloadRequest {
            version: self.version.clone(),
            artifact: self.artifact.clone(),
            blockmap: self.blockmap.clone(),
            expected_size: self.size,
            verification: VerifyExpectation {
                sha512: self.sha512.clone(),
                signature: self.signature.clone(),
                pubkey: self.config.pubkey.clone(),
                announced_version: self.version.clone(),
                require_signed_version: self.config.require_signed_version,
            },
            differential: self.config.differential,
            full_download_threshold: self.config.full_download_threshold,
            max_range_gap: self.config.max_range_gap,
            overrides: self.overrides.clone(),
        };
        downloader.download_with_provider(&request, listener).await
    }

    /// Installs a previously downloaded and verified package file in place.
    pub fn install_file(&self, package_path: &Path) -> Result<()> {
        let context = InstallContext {
            app_name: self.app_name.clone(),
            version: self.version.clone(),
            extract_path: self.extract_path.clone(),
            restart_after_install: self.restart_after_install,
            windows: self.config.windows.clone(),
        };
        PlatformInstaller::install(package_path, &context)
    }

    /// Downloads, verifies, caches, and installs this update in one operation.
    pub async fn download_and_install<L: DownloadListener>(
        &self,
        listener: &L,
    ) -> Result<DownloadSummary> {
        let downloaded = self.download(listener).await?;
        let update = self.clone();
        let pkg_path = downloaded.path;
        tokio::task::spawn_blocking(move || update.install_file(&pkg_path))
            .await
            .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))??;
        Ok(downloaded.summary)
    }
}
