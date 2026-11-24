#[cfg(feature = "plugin")]
use std::sync::Arc;

#[cfg(all(feature = "plugin", feature = "http"))]
use http::HeaderMap;
#[cfg(feature = "plugin")]
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
#[cfg(feature = "plugin")]
use tauri::{Manager, Runtime};

mod blockmap;
#[cfg(feature = "engine")]
mod cache;
mod chunker;
#[cfg(feature = "plugin")]
mod commands;
#[cfg(feature = "engine")]
mod config;
#[cfg(feature = "engine")]
mod downloader;
mod error;
#[cfg(feature = "engine")]
mod installer;
#[cfg(feature = "engine")]
mod listener;
mod manifest;
#[cfg(feature = "engine")]
mod multipart;
mod planner;
#[cfg(feature = "engine")]
mod provider;
#[cfg(feature = "engine")]
mod session;
#[cfg(feature = "engine")]
mod updater;
#[cfg(feature = "engine")]
mod verifier;

pub use blockmap::{BlockMap, BlockMapArtifact, BlockMapFile};
#[cfg(feature = "engine")]
pub use cache::{CachedPackageState, LocalBasePackage, PackageCache};
pub use chunker::{ChunkerConfig, FastCdc};
#[cfg(feature = "plugin")]
pub use commands::{CheckOptions, DownloadOptions, InstallOptions};
#[cfg(feature = "engine")]
pub use config::{Config, WindowsConfig, WindowsInstallMode};
#[cfg(feature = "http")]
pub use downloader::DownloadRequest;
#[cfg(feature = "engine")]
pub use downloader::{DifferentialDownloader, ProviderDownloadRequest, TransferOptions};
pub use error::{Error, Result, TransportError, TransportErrorKind};
#[cfg(feature = "engine")]
pub use installer::{
    AuthenticodePolicy, InstallContext, MacosCodeSignVerifier, MacosSigningRequirement,
    PlatformInstaller, WindowsAuthenticodeVerifier,
};
#[cfg(feature = "engine")]
pub use listener::{DownloadEvent, DownloadListener, DownloadMode, DownloadSummary, DummyListener};
pub use manifest::{ReleaseManifest, ReleasePlatform, ReleasePlatforms};
#[cfg(feature = "engine")]
pub use multipart::{ByteRangeParser, ByteRangePart};
pub use planner::{ByteRange, DownloadPlan, PlanOperation, PlanOptions};
#[cfg(feature = "http")]
pub use provider::http::{
    AwsCredentials, AwsCredentialsProvider, FnAuthorizer, FnCredentialsProvider, GitHubProvider,
    GitHubProviderBuilder, HttpAuthorizer, HttpDownloadSession, HttpDownloader,
    HttpDownloaderBuilder, HttpMiddleware, HttpNext, HttpRequest, HttpRequestContext, HttpResponse,
    RequestStage, S3Provider, SigV4Authorizer, StaticAwsCredentials, StaticHttpProvider,
};
#[cfg(feature = "engine")]
pub use provider::{
    ArtifactId, BoxByteStream, BoxFuture, ByteStream, DownloadSession, FailoverProvider,
    RangeCapability, RangeFetchResult, RangePart, ResolveContext, ResolvedRelease,
    TransferCapabilities, TransferOverrides, TransferStats, UpdateProvider,
};
#[cfg(feature = "engine")]
pub use session::{DiffPlanInfo, DiffSession, DownloadedPackage, FullSession};
#[cfg(feature = "engine")]
pub use updater::{TargetResolver, Update, Updater, UpdaterBuilder};
#[cfg(feature = "engine")]
pub use verifier::{ArtifactVerifier, VerifyExpectation, VerifyPolicy};

#[cfg(feature = "plugin")]
struct PluginState {
    config: Config,
    provider: Option<Arc<dyn UpdateProvider>>,
    target: Option<String>,
    metadata: Vec<(String, String)>,
    cache: PackageCache,
    android_installer: Option<installer::AndroidApkLauncher>,
}

/// Extension methods on Tauri managers to build and invoke the differential updater.
#[cfg(feature = "plugin")]
pub trait UpdaterPlusExt<R: Runtime> {
    /// Returns an [`UpdaterBuilder`] pre-populated with plugin config and cache state.
    fn updater_plus_builder(&self) -> Result<UpdaterBuilder>;

    /// Builds an [`Updater`] ready to check configured endpoints or custom provider.
    fn updater_plus(&self) -> Result<Updater>;

    /// Returns the plugin's [`PackageCache`] for seeding or inspecting cached base packages.
    fn updater_plus_cache(&self) -> PackageCache;
}

#[cfg(feature = "plugin")]
impl<R: Runtime, T: Manager<R>> UpdaterPlusExt<R> for T {
    fn updater_plus_builder(&self) -> Result<UpdaterBuilder> {
        let app = self.app_handle();
        let state = self.state::<PluginState>();
        #[cfg(target_os = "android")]
        let extract_path = state.cache.directory().to_path_buf();
        #[cfg(not(target_os = "android"))]
        let extract_path = {
            let current_exe = tauri::utils::platform::current_exe()?;
            TargetResolver::extract_path_from_executable(&current_exe)?
        };

        let mut builder = UpdaterBuilder::new(
            app.package_info().name.clone(),
            app.package_info().version.clone(),
            extract_path,
            state.cache.clone(),
        )
        .config(state.config.clone());

        if let Some(provider) = &state.provider {
            builder = builder.provider(Arc::clone(provider));
        }
        if let Some(target) = &state.target {
            builder = builder.target(target.clone());
        }
        for (key, value) in &state.metadata {
            builder = builder.metadata(key.clone(), value.clone());
        }
        if let Some(launcher) = &state.android_installer {
            builder = builder.android_installer(launcher.clone());
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
    provider: Option<Arc<dyn UpdateProvider>>,
    target: Option<String>,
    pubkey: Option<String>,
    metadata: Vec<(String, String)>,
}

#[cfg(feature = "plugin")]
impl Builder {
    /// Creates a plugin builder using defaults from `tauri.conf.json`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures a custom [`UpdateProvider`] (e.g. `GitHubProvider`, `S3Provider`, `FailoverProvider`,
    /// gRPC, or MQTT) to replace the default static HTTP provider.
    pub fn provider(mut self, provider: Arc<dyn UpdateProvider>) -> Self {
        self.provider = Some(provider);
        self
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

    /// Appends default transport metadata (HTTP header or gRPC metadata).
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.push((key.into(), value.into()));
        self
    }

    /// Replaces the default HTTP headers sent with all updater requests.
    #[cfg(feature = "http")]
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.metadata = headers
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
        self
    }

    /// Builds the Tauri plugin with IPC commands for check, download, and install.
    pub fn build<R: Runtime>(self) -> TauriPlugin<R, Option<Config>> {
        let provider = self.provider;
        let target = self.target;
        let pubkey = self.pubkey;
        let metadata = self.metadata;

        PluginBuilder::<R, Option<Config>>::new("updater-plus")
            .setup(move |app, api| {
                let mut config = Config::from_tauri_plugins(&app.config().plugins.0)?;
                if let Some(key) = pubkey {
                    config.pubkey = Some(key);
                }
                let cache_dir = app
                    .path()
                    .app_cache_dir()
                    .unwrap_or_else(|_| std::env::temp_dir().join(&app.package_info().name))
                    .join("updater-plus");
                #[cfg(target_os = "android")]
                let (cache, android_installer) = {
                    #[derive(serde::Deserialize)]
                    struct BaseApkResponse {
                        path: Option<String>,
                    }
                    #[derive(serde::Serialize)]
                    struct InstallApkPayload {
                        path: String,
                    }
                    let mobile =
                        api.register_android_plugin("com.plugin.updaterplus", "UpdaterPlusPlugin")?;
                    let base_apk = mobile
                        .run_mobile_plugin::<BaseApkResponse>("getBaseApkPath", ())
                        .ok()
                        .and_then(|resp| resp.path)
                        .filter(|path| !path.is_empty())
                        .map(std::path::PathBuf::from);
                    let cache = PackageCache::with_appimage_fallback(cache_dir, base_apk);
                    let launcher = installer::AndroidApkLauncher::new(move |apk_path| {
                        mobile
                            .run_mobile_plugin::<serde_json::Value>(
                                "installApk",
                                InstallApkPayload {
                                    path: apk_path.to_string_lossy().into_owned(),
                                },
                            )
                            .map(|_| ())
                            .map_err(|err| {
                                Error::Install(format!("Android APK install failed: {err}"))
                            })
                    });
                    (cache, Some(launcher))
                };
                #[cfg(not(target_os = "android"))]
                let (cache, android_installer) = {
                    let _ = api;
                    (PackageCache::new(cache_dir), None)
                };
                app.manage(PluginState {
                    config,
                    provider,
                    target,
                    metadata,
                    cache,
                    android_installer,
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
