use std::path::PathBuf;
#[cfg(feature = "http")]
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "http")]
use http::{HeaderMap, HeaderName, HeaderValue};
use semver::Version;
use url::Url;

use crate::cache::PackageCache;
use crate::config::Config;
#[cfg(feature = "http")]
use crate::downloader::TransferOptions;
use crate::installer::{UpdateInstaller, default_installer};
use crate::provider::{ResolveContext, ResolvedRelease, TransferOverrides, UpdateProvider};
use crate::{Error, Result};

mod target;
#[cfg(all(test, feature = "http"))]
mod tests;
mod update;

pub use target::*;
pub use update::*;

/// Builder for configuring an [`Updater`] instance backed by a default HTTP or custom [`UpdateProvider`].
#[derive(Debug, Clone)]
pub struct UpdaterBuilder {
    app_name: String,
    current_version: Version,
    extract_path: PathBuf,
    cache: PackageCache,
    config: Config,
    provider: Option<Arc<dyn UpdateProvider>>,
    installer: Option<Arc<dyn UpdateInstaller>>,
    target: Option<String>,
    bundle_type: Option<String>,
    overrides: TransferOverrides,
}

impl UpdaterBuilder {
    /// Creates an updater builder from core application coordinates and cache state.
    pub fn new(
        app_name: String,
        current_version: Version,
        extract_path: PathBuf,
        cache: PackageCache,
    ) -> Self {
        Self {
            app_name,
            current_version,
            extract_path,
            cache,
            config: Config::default(),
            provider: None,
            installer: None,
            target: None,
            bundle_type: None,
            overrides: TransferOverrides::default(),
        }
    }

    /// Injects a custom [`UpdateInstaller`] (e.g. [`crate::PlatformInstaller`],
    /// [`crate::DesktopInstaller`], [`crate::AndroidInstaller`], or a hot-update bundle applier).
    pub fn installer(mut self, installer: Arc<dyn UpdateInstaller>) -> Self {
        self.installer = Some(installer);
        self
    }

    /// Replaces the updater configuration.
    pub fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Injects a custom [`UpdateProvider`] (e.g. `GitHubProvider`, `S3Provider`, `FailoverProvider`,
    /// gRPC, or MQTT pull provider) instead of the default [`crate::StaticHttpProvider`].
    pub fn provider(mut self, provider: Arc<dyn UpdateProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Overrides the endpoint list from configuration (used by the default HTTP provider).
    pub fn endpoints(mut self, endpoints: Vec<Url>) -> Self {
        self.config.endpoints = endpoints;
        self
    }

    /// Overrides the target triple used for manifest platform selection.
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Sets the active bundle type hint (e.g. `"appimage"`, `"deb"`, `"rpm"`, `"app"`, `"msi"`, `"nsis"`).
    pub fn bundle_type(mut self, bundle_type: impl Into<String>) -> Self {
        self.bundle_type = Some(bundle_type.into());
        self
    }

    /// Appends a protocol-agnostic transport metadata entry (HTTP header or gRPC metadata).
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.overrides.push_metadata(key, value);
        self
    }

    /// Replaces all HTTP request headers sent to manifest and artifact endpoints.
    #[cfg(feature = "http")]
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.overrides.metadata = headers
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
        self
    }

    /// Appends an HTTP request header sent to manifest and artifact endpoints.
    #[cfg(feature = "http")]
    pub fn header(mut self, key: &str, value: &str) -> Result<Self> {
        let name = HeaderName::from_str(key).map_err(|error| {
            Error::InvalidHeader(format!("invalid header name '{key}': {error}"))
        })?;
        let val = HeaderValue::from_str(value).map_err(|error| {
            Error::InvalidHeader(format!("invalid header value for '{key}': {error}"))
        })?;
        let val_str = val.to_str().map_err(|error| {
            Error::InvalidHeader(format!("header value for '{key}' is not UTF-8: {error}"))
        })?;
        self.overrides.push_metadata(name.as_str(), val_str);
        Ok(self)
    }

    /// Sets the per-request transport timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.overrides.timeout = Some(timeout);
        self
    }

    /// Routes HTTP requests through an explicit proxy URL.
    pub fn proxy(mut self, proxy: Url) -> Self {
        self.overrides.proxy = Some(proxy);
        self
    }

    /// Builds a validated [`Updater`].
    pub fn build(self) -> Result<Updater> {
        if !(0.0..=1.0).contains(&self.config.full_download_threshold) {
            return Err(Error::InvalidConfig(
                "fullDownloadThreshold must be within 0.0..=1.0".into(),
            ));
        }
        if let Some(windows) = &self.config.windows {
            for dn in &windows.publisher_name {
                if !crate::installer::WindowsAuthenticodeVerifier::is_valid_publisher_name_dn(dn) {
                    return Err(Error::InvalidConfig(format!(
                        "invalid Windows publisherName Distinguished Name: '{dn}'"
                    )));
                }
            }
        }

        let provider: Arc<dyn UpdateProvider> = match self.provider {
            Some(custom) => custom,
            None => {
                #[cfg(feature = "http")]
                {
                    if self.config.endpoints.is_empty() {
                        return Err(Error::InvalidConfig(
                            "at least one updater endpoint must be configured".into(),
                        ));
                    }
                    let mut dl_builder = crate::provider::http::HttpDownloader::builder()
                        .transfer_options(TransferOptions {
                            max_ranges_per_request: self.config.max_ranges_per_request,
                            use_multiple_range_request: self.config.use_multiple_range_request,
                            max_concurrent_ranges: self.config.max_concurrent_ranges,
                        });
                    if let Some(timeout) = self.overrides.timeout {
                        dl_builder = dl_builder.timeout(timeout);
                    }
                    if let Some(proxy) = &self.overrides.proxy {
                        dl_builder = dl_builder.proxy(proxy.clone());
                    }
                    let downloader = dl_builder.build()?;
                    let static_provider = crate::provider::http::StaticHttpProvider::new(
                        downloader,
                        self.config.endpoints.clone(),
                    )?;
                    Arc::new(static_provider)
                }
                #[cfg(not(feature = "http"))]
                {
                    return Err(Error::InvalidConfig(
                        "custom UpdateProvider is required when the `http` feature is disabled"
                            .into(),
                    ));
                }
            }
        };

        let installer = self.installer.unwrap_or_else(default_installer);

        Ok(Updater {
            app_name: self.app_name,
            current_version: self.current_version,
            extract_path: self.extract_path,
            cache: self.cache,
            config: self.config,
            provider,
            installer,
            target: self.target,
            bundle_type: self.bundle_type,
            overrides: self.overrides,
        })
    }
}

/// Checks remote providers for newer releases and constructs downloadable [`Update`] handles.
#[derive(Debug, Clone)]
pub struct Updater {
    app_name: String,
    current_version: Version,
    extract_path: PathBuf,
    cache: PackageCache,
    config: Config,
    provider: Arc<dyn UpdateProvider>,
    installer: Arc<dyn UpdateInstaller>,
    target: Option<String>,
    bundle_type: Option<String>,
    overrides: TransferOverrides,
}

impl Updater {
    /// Checks the active [`UpdateProvider`] and returns a downloadable [`Update`] when newer.
    pub async fn check(&self) -> Result<Option<Update>> {
        let candidates = TargetResolver::target_candidates_with_bundle(
            self.target.as_deref(),
            self.bundle_type.as_deref(),
        );
        let ctx = ResolveContext {
            current_version: self.current_version.clone(),
            target_candidates: candidates,
            arch: TargetResolver::current_arch()
                .unwrap_or("unknown")
                .to_string(),
            bundle_type: self.bundle_type.clone(),
            require_checksum: self.config.pubkey.is_none(),
            overrides: self.overrides.clone(),
        };

        let Some(release) = self.provider.resolve_release(&ctx).await? else {
            return Ok(None);
        };
        self.prepare_release(release)
    }

    /// Prepares a downloadable [`Update`] directly from a push-delivered [`ResolvedRelease`]
    /// (for hybrid control planes such as MQTT/DDS/WebSocket notifications).
    pub fn prepare_release(&self, release: ResolvedRelease) -> Result<Option<Update>> {
        if !self.should_install(&release.version) {
            return Ok(None);
        }
        if self.config.pubkey.is_none()
            && release
                .sha512
                .as_deref()
                .is_none_or(|digest| digest.trim().is_empty())
        {
            return Err(Error::InvalidManifest(
                "manifest platform must provide non-empty `sha512` when `pubkey` is not configured"
                    .into(),
            ));
        }

        let date = release.formatted_pub_date();
        Ok(Some(Update {
            current_version: self.current_version.to_string(),
            version: release.version.to_string(),
            date,
            body: release.notes,
            raw_json: release.raw_json,
            artifact: release.artifact,
            blockmap: release.blockmap,
            sha512: release.sha512,
            signature: release.signature,
            size: release.size,
            provider: Arc::clone(&self.provider),
            installer: Arc::clone(&self.installer),
            app_name: self.app_name.clone(),
            extract_path: self.extract_path.clone(),
            bundle_type: self.bundle_type.clone(),
            cache: self.cache.clone(),
            overrides: self.overrides.clone(),
            config: self.config.clone(),
            restart_after_install: true,
        }))
    }

    fn should_install(&self, remote: &Version) -> bool {
        if self.config.allow_downgrades {
            remote != &self.current_version
        } else {
            remote > &self.current_version
        }
    }
}
