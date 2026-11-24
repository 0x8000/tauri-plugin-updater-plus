use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::HeaderMap;
use tauri::Resource;
use url::Url;

use crate::Result;
use crate::cache::PackageCache;
use crate::config::Config;
use crate::downloader::{
    DifferentialDownloader, DownloadRequest, DownloadedPackage, TransferOptions,
};
use crate::installer::{InstallContext, PlatformInstaller};
use crate::listener::{DownloadListener, DownloadSummary};
use crate::verifier::VerifyExpectation;

/// Discovered update ready to be downloaded (differentially via blockmap) and installed.
#[derive(Debug, Clone)]
pub struct Update {
    /// Currently running application version.
    pub current_version: String,
    /// Target version offered by the remote release.
    pub version: String,
    /// Optional RFC 3339 publication timestamp.
    pub date: Option<String>,
    /// Optional release notes text.
    pub body: Option<String>,
    /// Raw manifest JSON payload.
    pub raw_json: serde_json::Value,
    /// Target artifact download URL.
    pub download_url: Url,
    /// Resolved `.blockmap` URL.
    pub blockmap_url: Url,
    /// Optional base64 SHA-512 checksum.
    pub sha512: Option<String>,
    /// Optional base64 minisign signature.
    pub signature: Option<String>,
    /// Optional manifest-reported artifact size.
    pub size: Option<u64>,
    pub(crate) app_name: String,
    pub(crate) extract_path: PathBuf,
    pub(crate) cache: PackageCache,
    pub(crate) headers: HeaderMap,
    pub(crate) timeout: Option<Duration>,
    pub(crate) proxy: Option<Url>,
    pub(crate) config: Config,
    pub(crate) restart_after_install: bool,
}

impl Resource for Update {}

impl Update {
    /// Overrides request headers used during blockmap and range downloads.
    pub fn with_headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    /// Overrides request timeout for download transfers.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Controls whether the Windows installer automatically restarts the app.
    pub fn restart_after_install(mut self, restart: bool) -> Self {
        self.restart_after_install = restart;
        self
    }

    /// Downloads (using blockmap delta when possible), verifies, and caches the package on disk.
    pub async fn download<L: DownloadListener>(&self, listener: &L) -> Result<DownloadedPackage> {
        let client = self.build_client()?;
        let downloader = DifferentialDownloader::new(
            client,
            self.headers.clone(),
            self.cache.clone(),
            TransferOptions {
                max_ranges_per_request: self.config.max_ranges_per_request,
                use_multiple_range_request: self.config.use_multiple_range_request,
                max_concurrent_ranges: self.config.max_concurrent_ranges,
            },
        );
        let request = DownloadRequest {
            version: self.version.clone(),
            artifact_url: self.download_url.clone(),
            blockmap_url: self.blockmap_url.clone(),
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
        };
        downloader.download(&request, listener).await
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
        self.install_file(&downloaded.path)?;
        Ok(downloaded.summary)
    }

    fn build_client(&self) -> Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder().user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ));
        if let Some(timeout) = self.timeout {
            builder = builder.timeout(timeout);
        }
        if let Some(proxy_url) = &self.proxy {
            builder = builder.proxy(reqwest::Proxy::all(proxy_url.as_str())?);
        }
        Ok(builder.build()?)
    }
}
