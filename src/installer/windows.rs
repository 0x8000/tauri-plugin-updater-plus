use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOW;
use windows_sys::w;

use super::InstallContext;
use super::authenticode::{AuthenticodePolicy, WindowsAuthenticodeVerifier};
use crate::config::WindowsInstallMode;
use crate::{Error, Result};

pub(crate) struct WindowsInstaller;

impl WindowsInstaller {
    pub(crate) fn install(package_path: &Path, context: &InstallContext) -> Result<()> {
        let (temp_dir, installer_path, is_msi) = Self::prepare_installer(package_path, context)?;

        if let Some(windows) = context.windows.as_ref()
            && !windows.publisher_name.is_empty()
        {
            WindowsAuthenticodeVerifier::verify_file(
                &installer_path,
                &AuthenticodePolicy {
                    publisher_names: &windows.publisher_name,
                    allow_catalog_signature: windows.allow_catalog_signature,
                },
            )?;
        }

        let (executable, parameters) = Self::build_launch_args(&installer_path, is_msi, context)?;

        let file_wide = Self::encode_wide(&executable);
        let params_wide = Self::encode_wide(&parameters);
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                w!("open"),
                file_wide.as_ptr(),
                params_wide.as_ptr(),
                std::ptr::null(),
                SW_SHOW,
            )
        };
        if (result as isize) <= 32 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        // Keep the staging directory only after the OS installer process has launched.
        let _ = temp_dir.keep();
        std::process::exit(0);
    }

    fn prepare_installer(
        package_path: &Path,
        context: &InstallContext,
    ) -> Result<(tempfile::TempDir, PathBuf, bool)> {
        let mut header = [0u8; 512];
        let len = File::open(package_path)?.read(&mut header)?;
        let header = &header[..len];
        let safe_name = Self::sanitize_stem(&context.app_name);

        let temp_dir = tempfile::Builder::new()
            .prefix(&format!("{safe_name}-{}-updater-", context.version))
            .tempdir()?;

        if infer::archive::is_zip(header) {
            let mut archive = zip::ZipArchive::new(File::open(package_path)?)?;
            archive.extract(temp_dir.path())?;
            for entry in std::fs::read_dir(temp_dir.path())? {
                let path = entry?.path();
                if !path.is_file() {
                    continue;
                }
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                {
                    return Ok((temp_dir, path, false));
                }
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("msi"))
                {
                    return Ok((temp_dir, path, true));
                }
            }
            return Err(Error::InvalidPackageFormat(
                "Windows installer (.exe or .msi) not found inside zip archive".into(),
            ));
        }

        if infer::app::is_exe(header) {
            let dest = temp_dir
                .path()
                .join(format!("{safe_name}-{}-setup.exe", context.version));
            std::fs::copy(package_path, &dest)?;
            return Ok((temp_dir, dest, false));
        }
        if infer::archive::is_msi(header) {
            let dest = temp_dir
                .path()
                .join(format!("{safe_name}-{}.msi", context.version));
            std::fs::copy(package_path, &dest)?;
            return Ok((temp_dir, dest, true));
        }
        Err(Error::InvalidPackageFormat(
            "Windows update package must be .exe, .msi, or .zip".into(),
        ))
    }

    fn build_launch_args(
        installer_path: &Path,
        is_msi: bool,
        context: &InstallContext,
    ) -> Result<(OsString, OsString)> {
        let mode = context
            .windows
            .as_ref()
            .map(|w| w.install_mode.clone())
            .unwrap_or_default();
        let extra = context
            .windows
            .as_ref()
            .map(|w| w.installer_args.as_slice())
            .unwrap_or(&[]);

        if is_msi {
            let system_root = std::env::var("SystemRoot")
                .or_else(|_| std::env::var("windir"))
                .ok();
            let msiexec = WindowsAuthenticodeVerifier::windows_system32_path(
                system_root.as_deref(),
                "msiexec.exe",
            )
            .map_err(|err| Error::Install(err.to_string()))?;
            let mut args = vec![format!("/i \"{}\"", installer_path.display())];
            match mode {
                WindowsInstallMode::Passive => args.push("/passive".into()),
                WindowsInstallMode::Quiet => args.push("/quiet".into()),
                WindowsInstallMode::BasicUi => args.push("/qb".into()),
            }
            if context.restart_after_install {
                args.push("LAUNCHAPPONEXIT=1".into());
            }
            args.extend(extra.iter().cloned());
            return Ok((OsString::from(msiexec), OsString::from(args.join(" "))));
        }

        let mut args = Vec::new();
        match mode {
            WindowsInstallMode::Passive => args.push("/P".into()),
            WindowsInstallMode::Quiet => args.push("/S".into()),
            WindowsInstallMode::BasicUi => {}
        }
        args.push("/UPDATE".into());
        if context.restart_after_install {
            args.push("/R".into());
        }
        args.extend(extra.iter().cloned());
        Ok((
            installer_path.as_os_str().to_os_string(),
            OsString::from(args.join(" ")),
        ))
    }

    fn sanitize_stem(raw: &str) -> String {
        let cleaned: String = raw
            .chars()
            .map(|ch| {
                if matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                    || ch.is_control()
                {
                    '_'
                } else {
                    ch
                }
            })
            .collect();
        if cleaned.is_empty() {
            "app".into()
        } else {
            cleaned
        }
    }

    fn encode_wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }
}
