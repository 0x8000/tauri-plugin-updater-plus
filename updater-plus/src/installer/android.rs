use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use super::{InstallContext, UpdateInstaller};
use crate::{Error, Result};

type ApkLaunchFn = dyn Fn(&Path) -> Result<()> + Send + Sync;

/// Platform callback bridging a verified APK file path to the Android installer activity.
#[derive(Clone)]
pub struct AndroidApkLauncher(Arc<ApkLaunchFn>);

impl AndroidApkLauncher {
    /// Wraps a thread-safe callback that launches Android's package installer for an APK file path.
    pub fn new<F>(launcher: F) -> Self
    where
        F: Fn(&Path) -> Result<()> + Send + Sync + 'static,
    {
        Self(Arc::new(launcher))
    }

    /// Invokes the underlying Android APK launcher callback.
    pub fn launch(&self, package_path: &Path) -> Result<()> {
        (self.0)(package_path)
    }
}

impl fmt::Debug for AndroidApkLauncher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AndroidApkLauncher(..)")
    }
}

/// Android APK [`UpdateInstaller`] validating `.apk` archive magic before handing off to [`AndroidApkLauncher`].
#[derive(Debug, Clone)]
pub struct AndroidInstaller {
    launcher: AndroidApkLauncher,
}

impl AndroidInstaller {
    /// Creates an [`AndroidInstaller`] backed by `launcher`.
    pub fn new(launcher: AndroidApkLauncher) -> Self {
        Self { launcher }
    }

    /// Validates that `package_path` begins with ZIP magic bytes expected of an `.apk` package.
    pub fn validate_apk_file(package_path: &Path) -> Result<()> {
        let mut header = [0u8; 512];
        let len = File::open(package_path)?.read(&mut header)?;
        if !infer::archive::is_zip(&header[..len]) {
            return Err(Error::InvalidPackageFormat(
                "Android update package must be an .apk archive".into(),
            ));
        }
        Ok(())
    }
}

impl UpdateInstaller for AndroidInstaller {
    fn install(&self, package_path: &Path, _context: &InstallContext) -> Result<()> {
        Self::validate_apk_file(package_path)?;
        self.launcher.launch(package_path)
    }
}
