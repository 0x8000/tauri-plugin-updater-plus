use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use flate2::read::MultiGzDecoder;
use tauri::utils::config::BundleType;
use tauri::utils::platform::bundle_type;

use super::InstallContext;
use crate::{Error, Result};

pub(crate) struct LinuxInstaller;

impl LinuxInstaller {
    pub(crate) fn install(package_path: &Path, context: &InstallContext) -> Result<()> {
        let header = Self::read_header(package_path)?;
        if infer::archive::is_deb(&header) {
            return Self::install_system_package(package_path, "dpkg", "-i");
        }
        if infer::archive::is_rpm(&header) {
            return Self::install_system_package(package_path, "rpm", "-U");
        }
        match bundle_type() {
            Some(BundleType::Deb) => Err(Error::InvalidPackageFormat(
                "expected a .deb package for deb installation".into(),
            )),
            Some(BundleType::Rpm) => Err(Error::InvalidPackageFormat(
                "expected a .rpm package for rpm installation".into(),
            )),
            _ => Self::install_appimage(package_path, &header, &context.extract_path),
        }
    }

    pub(crate) fn install_appimage(
        package_path: &Path,
        header: &[u8],
        target_path: &Path,
    ) -> Result<()> {
        let target_perms = target_path.metadata()?.permissions();
        let target_dir = target_path.parent().ok_or_else(|| {
            Error::Install("cannot determine parent directory of target AppImage".into())
        })?;

        // Stage inside the destination directory (same filesystem) so `rename` atomically replaces
        // the target AppImage without ever leaving it missing, matching electron-updater's AppImageUpdater.
        let stage_dir = tempfile::Builder::new()
            .prefix(".updater-plus-appimage-stage-")
            .tempdir_in(target_dir)?;
        std::fs::set_permissions(stage_dir.path(), std::fs::Permissions::from_mode(0o700))?;
        let staged_path = stage_dir.path().join("update.AppImage");

        if infer::archive::is_gz(header) {
            Self::extract_appimage_from_targz(package_path, &staged_path)?;
        } else if infer::archive::is_zip(header) {
            Self::extract_appimage_from_zip(package_path, &staged_path)?;
        } else {
            std::fs::copy(package_path, &staged_path)?;
        }

        let final_perms = if target_perms.mode() & 0o100 != 0 {
            target_perms
        } else {
            std::fs::Permissions::from_mode(0o755)
        };
        std::fs::set_permissions(&staged_path, final_perms)?;
        std::fs::rename(&staged_path, target_path)?;
        Ok(())
    }

    fn extract_appimage_from_targz(archive_path: &Path, target_path: &Path) -> Result<()> {
        let file = File::open(archive_path)?;
        let decoder = MultiGzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let is_appimage_file = entry.header().entry_type().is_file()
                && entry
                    .path()
                    .ok()
                    .and_then(|path| path.extension().map(|ext| ext == OsStr::new("AppImage")))
                    .unwrap_or(false);
            if is_appimage_file {
                entry.unpack(target_path)?;
                return Ok(());
            }
        }
        Err(Error::InvalidPackageFormat(
            "AppImage binary not found in tar.gz archive".into(),
        ))
    }

    fn extract_appimage_from_zip(archive_path: &Path, target_path: &Path) -> Result<()> {
        let file = File::open(archive_path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let is_appimage_file = entry.is_file()
                && Path::new(entry.name())
                    .extension()
                    .is_some_and(|ext| ext == OsStr::new("AppImage"));
            if is_appimage_file {
                let mut out = File::create(target_path)?;
                std::io::copy(&mut entry, &mut out)?;
                return Ok(());
            }
        }
        Err(Error::InvalidPackageFormat(
            "AppImage binary not found in zip archive".into(),
        ))
    }

    fn install_system_package(package_path: &Path, tool: &str, flag: &str) -> Result<()> {
        if let Ok(status) = Command::new("pkexec")
            .arg(tool)
            .arg(flag)
            .arg(package_path)
            .status()
            && status.success()
        {
            return Ok(());
        }

        if let Some(password) = Self::prompt_graphical_password() {
            let mut child = Command::new("sudo")
                .arg("-S")
                .arg(tool)
                .arg(flag)
                .arg(package_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                writeln!(stdin, "{password}")?;
            }
            if child.wait()?.success() {
                return Ok(());
            }
        }

        let status = Command::new("sudo")
            .arg(tool)
            .arg(flag)
            .arg(package_path)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::Install(format!(
                "package installation via `{tool} {flag}` exited with status {status}"
            )))
        }
    }

    fn prompt_graphical_password() -> Option<String> {
        let zenity = Command::new("zenity")
            .args([
                "--password",
                "--title=Authentication Required",
                "--text=Enter your password to install the update:",
            ])
            .output();
        if let Ok(output) = zenity
            && output.status.success()
        {
            return Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
        let kdialog = Command::new("kdialog")
            .args(["--password", "Enter your password to install the update:"])
            .output();
        if let Ok(output) = kdialog
            && output.status.success()
        {
            return Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
        None
    }

    fn read_header(path: &Path) -> Result<Vec<u8>> {
        let mut file = File::open(path)?;
        let mut header = vec![0u8; 512];
        let len = file.read(&mut header)?;
        header.truncate(len);
        Ok(header)
    }
}
