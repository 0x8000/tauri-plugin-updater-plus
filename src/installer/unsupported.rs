use std::path::Path;

use super::InstallContext;
use crate::{Error, Result};

pub(crate) struct UnsupportedInstaller;

impl UnsupportedInstaller {
    pub(crate) fn install(_package_path: &Path, _context: &InstallContext) -> Result<()> {
        Err(Error::UnsupportedPlatform)
    }
}
