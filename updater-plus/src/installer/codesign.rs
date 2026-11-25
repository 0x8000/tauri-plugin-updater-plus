#[cfg(target_os = "macos")]
use std::path::Path;

use lazy_regex::{regex_captures, regex_is_match};

use crate::{Error, Result};

/// Signing requirement extracted from the currently installed macOS `.app` bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacosSigningRequirement {
    /// Current bundle is not code-signed (e.g. local unsigned development build).
    Unsigned,
    /// Current bundle uses an ad-hoc signature (`cdhash` only); verify deep integrity without cross-version `cdhash` match.
    AdHoc,
    /// Current bundle carries a certificate-backed Designated Requirement (Squirrel.Mac parity).
    Designated(String),
}

/// Verifies macOS `.app` bundle signatures before replacement, matching Squirrel.Mac's security checks.
pub struct MacosCodeSignVerifier;

impl MacosCodeSignVerifier {
    /// Parses `codesign -d -r -` output into a [`MacosSigningRequirement`].
    pub fn parse_designated_requirement(
        stdout: &str,
        stderr: &str,
    ) -> Result<MacosSigningRequirement> {
        if stderr.contains("code object is not signed at all") {
            return Ok(MacosSigningRequirement::Unsigned);
        }

        let combined = if stdout.is_empty() {
            stderr.to_string()
        } else {
            format!("{stdout}\n{stderr}")
        };

        // Matches `codesign -d -r -` designated requirement output lines:
        // - `designated => identifier "com.example.app" and anchor apple generic and certificate leaf[subject.OU] = TEAM123`
        // - `designated => cdhash H"0123456789abcdef0123456789abcdef01234567"`
        let Some((_, requirement)) = regex_captures!(
            r"(?m)^designated[ \t]*=>[ \t]*(\S[^\r\n]*?)[ \t]*\r?$",
            &combined
        ) else {
            return Err(Error::Signature(format!(
                "failed to extract macOS codesign designated requirement: {}",
                stderr.trim()
            )));
        };

        // Matches bare ad-hoc macOS codesign requirement bound only to the old binary's cdhash:
        // - `cdhash H"0123456789abcdef0123456789abcdef01234567"`
        if regex_is_match!(r#"^cdhash[ \t]+H"[0-9a-fA-F]+"$"#, requirement) {
            return Ok(MacosSigningRequirement::AdHoc);
        }

        Ok(MacosSigningRequirement::Designated(requirement.to_string()))
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn verify_bundle_update(current_app: &Path, new_app: &Path) -> Result<()> {
        use std::process::Command;

        const CODESIGN_BIN: &str = "/usr/bin/codesign";

        let inspect = Command::new(CODESIGN_BIN)
            .args(["-d", "-r", "-"])
            .arg(current_app)
            .output()
            .map_err(|error| {
                Error::Signature(format!("failed to invoke macOS codesign: {error}"))
            })?;
        let inspect_stdout = String::from_utf8_lossy(&inspect.stdout);
        let inspect_stderr = String::from_utf8_lossy(&inspect.stderr);
        let requirement = Self::parse_designated_requirement(&inspect_stdout, &inspect_stderr)?;

        if matches!(requirement, MacosSigningRequirement::Unsigned) {
            tracing::warn!(
                app = %current_app.display(),
                "current macOS app bundle is unsigned; skipping codesign verification"
            );
            return Ok(());
        }

        // Pass 1: recursively verify all nested frameworks, helpers, and CodeResources seals.
        let deep = Command::new(CODESIGN_BIN)
            .args(["--verify", "--deep", "--strict"])
            .arg(new_app)
            .output()
            .map_err(|error| {
                Error::Signature(format!("failed to verify macOS bundle signature: {error}"))
            })?;
        if !deep.status.success() {
            let stderr = String::from_utf8_lossy(&deep.stderr);
            return Err(Error::Signature(format!(
                "macOS codesign deep verification failed: {}",
                stderr.trim()
            )));
        }

        match requirement {
            MacosSigningRequirement::Unsigned => Ok(()),
            MacosSigningRequirement::AdHoc => {
                tracing::warn!(
                    app = %current_app.display(),
                    "current macOS app bundle is ad-hoc signed; skipping designated requirement match"
                );
                Ok(())
            }
            MacosSigningRequirement::Designated(designated) => {
                // Pass 2 (without --deep): verify top-level bundle satisfies current app's Designated Requirement.
                let req_check = Command::new(CODESIGN_BIN)
                    .args(["--verify", "--strict", "-R", &designated])
                    .arg(new_app)
                    .output()
                    .map_err(|error| {
                        Error::Signature(format!(
                            "failed to verify macOS designated requirement: {error}"
                        ))
                    })?;
                if !req_check.status.success() {
                    let stderr = String::from_utf8_lossy(&req_check.stderr);
                    return Err(Error::Signature(format!(
                        "macOS codesign designated requirement mismatch: {}",
                        stderr.trim()
                    )));
                }
                Ok(())
            }
        }
    }
}
