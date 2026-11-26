//! Differential in-app updater plugin for Tauri v3 applications, powered by [`updater_plus`].
//!
//! # Feature flags
//!
#![doc = document_features::document_features!()]

use std::sync::Arc;

#[cfg(feature = "http")]
use http::HeaderMap;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::utils::config::BundleType;
use tauri::utils::platform::bundle_type;
use tauri::{Manager, Runtime};

mod commands;

pub use commands::{CheckOptions, DownloadOptions, InstallOptions};
pub use updater_plus::*;

struct PluginState {
    config: Config,
    provider: Option<Arc<dyn UpdateProvider>>,
    installer: Arc<dyn UpdateInstaller>,
    target: Option<String>,
    metadata: Vec<(String, String)>,
    cache: PackageCache,
}

fn current_tauri_bundle_name() -> Option<&'static str> {
    match bundle_type()? {
        BundleType::AppImage => Some("appimage"),
        BundleType::Deb => Some("deb"),
        BundleType::Rpm => Some("rpm"),
        BundleType::App => Some("app"),
        BundleType::Msi => Some("msi"),
        BundleType::Nsis => Some("nsis"),
        _ => None,
    }
}

/// Extension methods on Tauri managers to build and invoke the differential updater.
pub trait UpdaterPlusExt<R: Runtime> {
    /// Returns an [`UpdaterBuilder`] pre-populated with plugin config, installer, and cache state.
    fn updater_plus_builder(&self) -> Result<UpdaterBuilder>;

    /// Builds an [`Updater`] ready to check configured endpoints or custom provider.
    fn updater_plus(&self) -> Result<Updater>;

    /// Returns the plugin's [`PackageCache`] for seeding or inspecting cached base packages.
    fn updater_plus_cache(&self) -> PackageCache;
}

impl<R: Runtime, T: Manager<R>> UpdaterPlusExt<R> for T {
    fn updater_plus_builder(&self) -> Result<UpdaterBuilder> {
        let app = self.app_handle();
        let state = self.state::<PluginState>();
        let bundle_name = current_tauri_bundle_name();
        #[cfg(target_os = "android")]
        let extract_path = state.cache.directory().to_path_buf();
        #[cfg(not(target_os = "android"))]
        let extract_path = {
            let current_exe = tauri::utils::platform::current_exe()?;
            TargetResolver::extract_path_from_executable_with_bundle(&current_exe, bundle_name)?
        };

        let mut builder = UpdaterBuilder::new(
            app.package_info().name.clone(),
            app.package_info().version.clone(),
            extract_path,
            state.cache.clone(),
        )
        .config(state.config.clone())
        .installer(Arc::clone(&state.installer));

        if let Some(bundle) = bundle_name {
            builder = builder.bundle_type(bundle);
        }
        if let Some(provider) = &state.provider {
            builder = builder.provider(Arc::clone(provider));
        }
        if let Some(target) = &state.target {
            builder = builder.target(target.clone());
        }
        for (key, value) in &state.metadata {
            builder = builder.metadata(key.clone(), value.clone());
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
#[derive(Default)]
pub struct Builder {
    provider: Option<Arc<dyn UpdateProvider>>,
    installer: Option<Arc<dyn UpdateInstaller>>,
    target: Option<String>,
    pubkey: Option<String>,
    metadata: Vec<(String, String)>,
}

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

    /// Configures a custom [`UpdateInstaller`] to replace the default OS [`PlatformInstaller`] facade.
    pub fn installer(mut self, installer: Arc<dyn UpdateInstaller>) -> Self {
        self.installer = Some(installer);
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
        let custom_installer = self.installer;
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
                    .or_else(|_| app.path().app_local_data_dir())?
                    .join("updater-plus");
                #[cfg(target_os = "android")]
                let (cache, default_installer) = {
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
                    #[cfg(feature = "native-installer")]
                    let installer: Arc<dyn UpdateInstaller> = {
                        let launcher = AndroidApkLauncher::new(move |apk_path| {
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
                        Arc::new(PlatformInstaller::with_android_launcher(Some(launcher)))
                    };
                    #[cfg(not(feature = "native-installer"))]
                    let installer: Arc<dyn UpdateInstaller> = Arc::new(UnsupportedInstaller);
                    (cache, installer)
                };
                #[cfg(not(target_os = "android"))]
                let (cache, default_installer) = {
                    let _ = api;
                    let cache = if matches!(current_tauri_bundle_name(), Some("deb" | "rpm")) {
                        PackageCache::with_appimage_fallback(cache_dir, None)
                    } else {
                        PackageCache::new(cache_dir)
                    };
                    #[cfg(feature = "native-installer")]
                    let installer: Arc<dyn UpdateInstaller> =
                        Arc::new(PlatformInstaller::current());
                    #[cfg(not(feature = "native-installer"))]
                    let installer: Arc<dyn UpdateInstaller> = Arc::new(UnsupportedInstaller);
                    (cache, installer)
                };
                let installer = custom_installer.unwrap_or(default_installer);
                app.manage(PluginState {
                    config,
                    provider,
                    installer,
                    target,
                    metadata,
                    cache,
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
pub fn init<R: Runtime>() -> TauriPlugin<R, Option<Config>> {
    Builder::new().build()
}
