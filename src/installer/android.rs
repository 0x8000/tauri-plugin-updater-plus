use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use crate::{Error, Result};

type ApkLaunchFn = dyn Fn(&Path) -> Result<()> + Send + Sync;

/// Platform callback bridging a verified APK file path to the Android installer activity.
#[derive(Clone)]
pub(crate) struct AndroidApkLauncher(Arc<ApkLaunchFn>);

impl AndroidApkLauncher {
    #[cfg(any(test, target_os = "android"))]
    pub(crate) fn new<F>(launcher: F) -> Self
    where
        F: Fn(&Path) -> Result<()> + Send + Sync + 'static,
    {
        Self(Arc::new(launcher))
    }

    pub(crate) fn launch(&self, package_path: &Path) -> Result<()> {
        (self.0)(package_path)
    }
}

impl fmt::Debug for AndroidApkLauncher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AndroidApkLauncher(..)")
    }
}

pub(crate) struct AndroidInstaller;

impl AndroidInstaller {
    pub(crate) fn validate_apk_file(package_path: &Path) -> Result<()> {
        let mut header = [0u8; 512];
        let len = File::open(package_path)?.read(&mut header)?;
        if !infer::archive::is_zip(&header[..len]) {
            return Err(Error::InvalidPackageFormat(
                "Android update package must be an .apk archive".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn install_with_launcher(
        package_path: &Path,
        launcher: &AndroidApkLauncher,
    ) -> Result<()> {
        Self::validate_apk_file(package_path)?;
        launcher.launch(package_path)
    }
}
