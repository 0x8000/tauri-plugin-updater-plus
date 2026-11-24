use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::read::MultiGzDecoder;

use super::InstallContext;
use super::codesign::MacosCodeSignVerifier;
use crate::{Error, Result};

pub(crate) struct MacosInstaller;

impl MacosInstaller {
    pub(crate) fn install(package_path: &Path, context: &InstallContext) -> Result<()> {
        let mut header = [0u8; 512];
        let len = File::open(package_path)?.read(&mut header)?;
        let header = &header[..len];

        let tmp_extract = tempfile::Builder::new()
            .prefix("updater_plus_updated_app")
            .tempdir()?;
        let extracted_app = if infer::archive::is_zip(header) {
            Self::extract_app_from_zip(package_path, tmp_extract.path())?
        } else if infer::archive::is_gz(header) {
            Self::extract_app_from_targz(package_path, tmp_extract.path())?
        } else {
            return Err(Error::InvalidPackageFormat(
                "macOS update package must be a .zip or .app.tar.gz archive".into(),
            ));
        };

        MacosCodeSignVerifier::verify_bundle_update(&context.extract_path, &extracted_app)?;

        let tmp_backup = tempfile::Builder::new()
            .prefix("updater_plus_current_app")
            .tempdir()?;
        let backup_dest = tmp_backup.path().join("current.app");

        let move_result = std::fs::rename(&context.extract_path, &backup_dest);
        match move_result {
            Ok(()) => {
                if let Err(error) = std::fs::rename(&extracted_app, &context.extract_path) {
                    let _ = std::fs::rename(&backup_dest, &context.extract_path);
                    return Err(error.into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                Self::elevate_replace(&extracted_app, &context.extract_path)?;
            }
            Err(error) => return Err(error.into()),
        }

        let _ = Command::new("/usr/bin/touch")
            .arg(&context.extract_path)
            .status();
        Ok(())
    }

    fn extract_app_from_targz(archive_path: &Path, dest: &Path) -> Result<PathBuf> {
        let file = File::open(archive_path)?;
        let decoder = MultiGzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(dest)?;
        Self::locate_app_bundle(dest)
    }

    fn extract_app_from_zip(archive_path: &Path, dest: &Path) -> Result<PathBuf> {
        // Use macOS native `ditto` (matching Squirrel.Mac) so framework symlinks, executable bits,
        // and extended attributes required by codesign/Gatekeeper remain intact.
        let output = Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(archive_path)
            .arg(dest)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Install(format!(
                "ditto zip extraction failed: {}",
                stderr.trim()
            )));
        }
        Self::locate_app_bundle(dest)
    }

    fn locate_app_bundle(dir: &Path) -> Result<PathBuf> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() && path.extension().and_then(|s| s.to_str()) == Some("app") {
                return Ok(path);
            }
        }
        Err(Error::InvalidPackageFormat(
            "macOS .app bundle not found inside update archive".into(),
        ))
    }

    fn elevate_replace(new_app: &Path, target_app: &Path) -> Result<()> {
        fn shell_quote(path: &Path) -> String {
            let escaped = path.to_string_lossy().replace('\'', "'\\''");
            format!("'{escaped}'")
        }

        fn escape_applescript_string(shell_cmd: &str) -> String {
            shell_cmd.replace('\\', "\\\\").replace('"', "\\\"")
        }

        let shell_cmd = format!(
            "rm -rf {} && mv -f {} {}",
            shell_quote(target_app),
            shell_quote(new_app),
            shell_quote(target_app)
        );
        let script_src = format!(
            "do shell script \"{}\" with administrator privileges",
            escape_applescript_string(&shell_cmd)
        );
        let mut script =
            osakit::Script::new_from_source(osakit::Language::AppleScript, &script_src);
        script
            .compile()
            .map_err(|error| Error::Install(format!("compile AppleScript failed: {error:?}")))?;
        script.execute().map_err(|error| {
            Error::Install(format!("elevated macOS app swap failed: {error:?}"))
        })?;
        Ok(())
    }
}
