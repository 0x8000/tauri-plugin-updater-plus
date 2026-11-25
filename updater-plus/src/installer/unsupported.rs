use std::path::Path;

use super::{InstallContext, UpdateInstaller};
use crate::{Error, Result};

/// Fallback [`UpdateInstaller`] returning [`Error::UnsupportedPlatform`] on unsupported targets.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedInstaller;

impl UpdateInstaller for UnsupportedInstaller {
    fn install(&self, _package_path: &Path, _context: &InstallContext) -> Result<()> {
        Err(Error::UnsupportedPlatform)
    }
}
