use std::path::{Path, PathBuf};

use crate::Result;
use crate::config::WindowsConfig;

mod authenticode;
mod codesign;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(test)]
mod tests;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

pub use authenticode::*;
pub use codesign::*;

/// Context required to install a verified package onto the current platform.
#[derive(Debug, Clone)]
pub struct InstallContext {
    /// Application name used for staging directories.
    pub app_name: String,
    /// Target version being installed.
    pub version: String,
    /// Destination path (AppImage file, macOS `.app` bundle, or executable directory).
    pub extract_path: PathBuf,
    /// Whether the installer should relaunch the application after completing (Windows).
    pub restart_after_install: bool,
    /// Optional Windows installer configuration.
    pub windows: Option<WindowsConfig>,
}

/// Platform-specific installer that applies a verified update package in place.
pub struct PlatformInstaller;

impl PlatformInstaller {
    /// Installs the verified package at `package_path` using the active OS backend.
    pub fn install(package_path: &Path, context: &InstallContext) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            linux::LinuxInstaller::install(package_path, context)
        }
        #[cfg(target_os = "macos")]
        {
            macos::MacosInstaller::install(package_path, context)
        }
        #[cfg(target_os = "windows")]
        {
            windows::WindowsInstaller::install(package_path, context)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            unsupported::UnsupportedInstaller::install(package_path, context)
        }
    }
}
