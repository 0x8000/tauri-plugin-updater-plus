#[cfg(feature = "plugin")]
use http::HeaderMap;
#[cfg(feature = "plugin")]
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
#[cfg(feature = "plugin")]
use tauri::{Manager, Runtime};

mod blockmap;
#[cfg(feature = "plugin")]
mod cache;
mod chunker;
#[cfg(feature = "plugin")]
mod commands;
#[cfg(feature = "plugin")]
mod config;
#[cfg(feature = "plugin")]
mod downloader;
mod error;
#[cfg(feature = "plugin")]
mod installer;
#[cfg(feature = "plugin")]
mod listener;
mod manifest;
#[cfg(feature = "plugin")]
mod multipart;
mod planner;
#[cfg(feature = "plugin")]
mod updater;
#[cfg(feature = "plugin")]
mod verifier;

pub use blockmap::{BlockMap, BlockMapArtifact, BlockMapFile};
#[cfg(feature = "plugin")]
pub use cache::{CachedPackageState, LocalBasePackage, PackageCache};
pub use chunker::{ChunkerConfig, FastCdc};
#[cfg(feature = "plugin")]
pub use commands::{CheckOptions, DownloadOptions, InstallOptions};
#[cfg(feature = "plugin")]
pub use config::{Config, WindowsConfig, WindowsInstallMode};
#[cfg(feature = "plugin")]
pub use downloader::{DifferentialDownloader, DownloadRequest, DownloadedPackage, TransferOptions};
pub use error::{Error, Result};
#[cfg(feature = "plugin")]
pub use installer::{
    AuthenticodePolicy, InstallContext, MacosCodeSignVerifier, MacosSigningRequirement,
    PlatformInstaller, WindowsAuthenticodeVerifier,
};
#[cfg(feature = "plugin")]
pub use listener::{DownloadEvent, DownloadListener, DownloadMode, DownloadSummary, DummyListener};
pub use manifest::{ReleaseManifest, ReleasePlatform, ReleasePlatforms};
#[cfg(feature = "plugin")]
pub use multipart::{ByteRangeParser, ByteRangePart};
pub use planner::{DownloadPlan, PlanOperation, PlanOptions};
#[cfg(feature = "plugin")]
pub use updater::{TargetResolver, Update, Updater, UpdaterBuilder};
#[cfg(feature = "plugin")]
pub use verifier::{ArtifactVerifier, VerifyExpectation};

#[cfg(feature = "plugin")]
struct PluginState {
    config: Config,
    target: Option<String>,
    headers: HeaderMap,
    cache: PackageCache,
}

/// Extension methods on Tauri managers to build and invoke the differential updater.
#[cfg(feature = "plugin")]
pub trait UpdaterPlusExt<R: Runtime> {
    /// Returns an [`UpdaterBuilder`] pre-populated with plugin config and cache state.
    fn updater_plus_builder(&self) -> Result<UpdaterBuilder>;

    /// Builds an [`Updater`] ready to check configured endpoints.
    fn updater_plus(&self) -> Result<Updater>;

    /// Returns the plugin's [`PackageCache`] for seeding or inspecting cached base packages.
    fn updater_plus_cache(&self) -> PackageCache;
}

#[cfg(feature = "plugin")]
impl<R: Runtime, T: Manager<R>> UpdaterPlusExt<R> for T {
    fn updater_plus_builder(&self) -> Result<UpdaterBuilder> {
        let app = self.app_handle();
        let state = self.state::<PluginState>();
        let current_exe = tauri::utils::platform::current_exe()?;
        let extract_path = TargetResolver::extract_path_from_executable(&current_exe)?;

        let mut builder = UpdaterBuilder::new(
            app.package_info().name.clone(),
            app.package_info().version.clone(),
            extract_path,
            state.cache.clone(),
        )
        .config(state.config.clone());

        if let Some(target) = &state.target {
            builder = builder.target(target.clone());
        }
        for (key, value) in &state.headers {
            if let Ok(val) = value.to_str() {
                builder = builder.header(key.as_str(), val)?;
            }
        }
        Ok(builder)
    }

    fn updater_plus(&self) -> Result<Updater> {
        self.updater_plus_builder()?.build()
    }

    fn updater_plus_cache(&self) -> PackageCache {
        self.state::<PluginState>().cache.clone()
    }
}

/// Builder for registering the `updater-plus` Tauri plugin.
#[cfg(feature = "plugin")]
#[derive(Default)]
pub struct Builder {
    target: Option<String>,
    pubkey: Option<String>,
    headers: HeaderMap,
}

#[cfg(feature = "plugin")]
impl Builder {
    /// Creates a plugin builder using defaults from `tauri.conf.json`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the default target triple used for manifest lookup.
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Overrides the minisign public key configured in `tauri.conf.json`.
    pub fn pubkey(mut self, pubkey: impl Into<String>) -> Self {
        self.pubkey = Some(pubkey.into());
        self
    }

    /// Replaces the default HTTP headers sent with all updater requests.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    /// Builds the Tauri plugin with IPC commands for check, download, and install.
    pub fn build<R: Runtime>(self) -> TauriPlugin<R, Option<Config>> {
        let target = self.target;
        let pubkey = self.pubkey;
        let headers = self.headers;

        PluginBuilder::<R, Option<Config>>::new("updater-plus")
            .setup(move |app, api| {
                let mut config = api.config().clone().unwrap_or_default();
                if let Some(key) = pubkey {
                    config.pubkey = Some(key);
                }
                let cache_dir = app
                    .path()
                    .app_cache_dir()
                    .unwrap_or_else(|_| std::env::temp_dir().join(&app.package_info().name))
                    .join("updater-plus");
                app.manage(PluginState {
                    config,
                    target,
                    headers,
                    cache: PackageCache::new(cache_dir),
                });
                Ok(())
            })
            .invoke_handler(tauri::generate_handler![
                commands::check,
                commands::download,
                commands::install,
                commands::download_and_install,
            ])
            .build()
    }
}

/// Initializes the `updater-plus` plugin with default `tauri.conf.json` settings.
#[cfg(feature = "plugin")]
pub fn init<R: Runtime>() -> TauriPlugin<R, Option<Config>> {
    Builder::new().build()
}
