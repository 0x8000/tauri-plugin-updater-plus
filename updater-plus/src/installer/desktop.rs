use std::path::Path;

use super::{InstallContext, UpdateInstaller};
use crate::Result;

/// OS-native desktop installer dispatching to Linux (AppImage/deb/rpm), macOS (`.app`), or Windows (NSIS/MSI).
#[derive(Debug, Default, Clone, Copy)]
pub struct DesktopInstaller;

impl DesktopInstaller {
    /// Creates a desktop OS installer handle.
    pub fn new() -> Self {
        Self
    }
}

impl UpdateInstaller for DesktopInstaller {
    fn install(&self, package_path: &Path, context: &InstallContext) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            super::linux::LinuxInstaller::install(package_path, context)
        }
        #[cfg(target_os = "macos")]
        {
            super::macos::MacosInstaller::install(package_path, context)
        }
        #[cfg(target_os = "windows")]
        {
            super::windows::WindowsInstaller::install(package_path, context)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            super::UnsupportedInstaller.install(package_path, context)
        }
    }
}
