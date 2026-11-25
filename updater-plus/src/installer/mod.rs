use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(feature = "native-installer")]
use std::sync::Arc;

use crate::Result;
use crate::config::WindowsConfig;

#[cfg(feature = "native-installer")]
mod android;
mod authenticode;
mod codesign;
#[cfg(feature = "native-installer")]
mod desktop;
#[cfg(all(feature = "native-installer", target_os = "linux"))]
mod linux;
#[cfg(all(feature = "native-installer", target_os = "macos"))]
mod macos;
#[cfg(test)]
mod tests;
mod unsupported;
#[cfg(all(feature = "native-installer", target_os = "windows"))]
mod windows;

#[cfg(feature = "native-installer")]
pub use android::{AndroidApkLauncher, AndroidInstaller};
pub use authenticode::{AuthenticodePolicy, WindowsAuthenticodeVerifier};
pub use codesign::{MacosCodeSignVerifier, MacosSigningRequirement};
#[cfg(feature = "native-installer")]
pub use desktop::DesktopInstaller;
pub use unsupported::UnsupportedInstaller;

/// Context passed to an [`UpdateInstaller`] when applying a verified package.
#[derive(Debug, Clone)]
pub struct InstallContext {
    /// Application or bundle name used for staging directories.
    pub app_name: String,
    /// Target version being installed.
    pub version: String,
    /// Destination path (AppImage file, macOS `.app` bundle, executable directory, or custom root).
    pub extract_path: PathBuf,
    /// Optional bundle type hint (e.g. `"appimage"`, `"deb"`, `"rpm"`, `"app"`, `"msi"`, `"nsis"`).
    pub bundle_type: Option<String>,
    /// Whether the installer should relaunch the application after completing (Windows).
    pub restart_after_install: bool,
    /// Optional Windows installer configuration.
    pub windows: Option<WindowsConfig>,
}

/// Pluggable installation strategy that applies a downloaded and verified update package.
///
/// Built-in implementations include [`PlatformInstaller`] (OS-native desktop/Android facade under
/// the `native-installer` feature) and [`UnsupportedInstaller`]. Custom hot-update flows (such as
/// versioned web bundles or Lua scripts) can implement this trait and pass it to
/// [`crate::UpdaterBuilder::installer`].
pub trait UpdateInstaller: Send + Sync + fmt::Debug {
    /// Applies the verified package at `package_path` using `context`.
    fn install(&self, package_path: &Path, context: &InstallContext) -> Result<()>;
}

/// Facade [`UpdateInstaller`] dispatching to the active host platform's native installer.
///
/// - On Linux, macOS, and Windows, delegates to [`DesktopInstaller`].
/// - On Android, delegates to [`AndroidInstaller`] when configured with an [`AndroidApkLauncher`],
///   or falls back to [`UnsupportedInstaller`] otherwise.
/// - On unsupported platforms (such as iOS), delegates to [`UnsupportedInstaller`].
#[cfg(feature = "native-installer")]
#[derive(Debug, Clone)]
pub struct PlatformInstaller {
    inner: Arc<dyn UpdateInstaller>,
}

#[cfg(feature = "native-installer")]
impl PlatformInstaller {
    /// Creates the default OS facade for the compiled host platform.
    pub fn current() -> Self {
        Self::with_android_launcher(None)
    }

    /// Creates the OS facade with an optional [`AndroidApkLauncher`] used on Android hosts.
    pub fn with_android_launcher(launcher: Option<AndroidApkLauncher>) -> Self {
        Self {
            inner: Self::default_backend(launcher),
        }
    }

    /// Wraps [`DesktopInstaller`] behind the facade.
    pub fn desktop() -> Self {
        Self {
            inner: Arc::new(DesktopInstaller::new()),
        }
    }

    /// Wraps [`AndroidInstaller`] backed by `launcher` behind the facade.
    pub fn android(launcher: AndroidApkLauncher) -> Self {
        Self {
            inner: Arc::new(AndroidInstaller::new(launcher)),
        }
    }

    /// Wraps any custom [`UpdateInstaller`] behind the facade.
    pub fn custom(installer: Arc<dyn UpdateInstaller>) -> Self {
        Self { inner: installer }
    }

    /// Installs `package_path` using the default host backend without constructing a handle.
    pub fn install_default(package_path: &Path, context: &InstallContext) -> Result<()> {
        Self::current().install(package_path, context)
    }

    fn default_backend(android_launcher: Option<AndroidApkLauncher>) -> Arc<dyn UpdateInstaller> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            let _ = android_launcher;
            Arc::new(DesktopInstaller::new())
        }
        #[cfg(target_os = "android")]
        {
            match android_launcher {
                Some(launcher) => Arc::new(AndroidInstaller::new(launcher)),
                None => Arc::new(UnsupportedInstaller),
            }
        }
        #[cfg(not(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows",
            target_os = "android"
        )))]
        {
            let _ = android_launcher;
            Arc::new(UnsupportedInstaller)
        }
    }
}

#[cfg(feature = "native-installer")]
impl Default for PlatformInstaller {
    fn default() -> Self {
        Self::current()
    }
}

#[cfg(feature = "native-installer")]
impl UpdateInstaller for PlatformInstaller {
    fn install(&self, package_path: &Path, context: &InstallContext) -> Result<()> {
        self.inner.install(package_path, context)
    }
}

pub(crate) fn default_installer() -> std::sync::Arc<dyn UpdateInstaller> {
    #[cfg(feature = "native-installer")]
    {
        std::sync::Arc::new(PlatformInstaller::current())
    }
    #[cfg(not(feature = "native-installer"))]
    {
        std::sync::Arc::new(UnsupportedInstaller)
    }
}
