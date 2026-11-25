use std::collections::BTreeMap;
#[cfg(target_os = "windows")]
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use lazy_regex::regex_is_match;

use crate::{Error, Result};

mod dn;

use dn::DistinguishedName;

const AUTHENTICODE_FILE_ENV: &str = "UPDATER_PLUS_AUTHENTICODE_FILE";
const CATALOG_SIGNATURE_TYPE: i64 = 2;

const POWERSHELL_VERIFY_SCRIPT: &str = concat!(
    "$ProgressPreference = 'SilentlyContinue'; ",
    "Import-Module \"$PSHOME\\Modules\\Microsoft.PowerShell.Security\"; ",
    "Import-Module \"$PSHOME\\Modules\\Microsoft.PowerShell.Utility\"; ",
    "$env:PSModulePath = \"\"; ",
    "function ConvertTo-AsciiEscaped([string]$text) { ",
    "if ($text -cmatch '[^\\x00-\\x7F]') { ",
    "-join $(foreach ($c in $text.ToCharArray()) { ",
    "if ([int]$c -gt 127) { '\\u{0:x4}' -f [int]$c } else { $c } ",
    "}) } else { $text } }; ",
    "try { $json = Get-AuthenticodeSignature -LiteralPath $env:UPDATER_PLUS_AUTHENTICODE_FILE ",
    "-ErrorAction Stop | ConvertTo-Json -Compress } ",
    "catch { Write-Error -Message (ConvertTo-AsciiEscaped ($_ | Out-String)); exit 1 }; ",
    "ConvertTo-AsciiEscaped $json"
);

/// Policy controlling Windows Authenticode verification, matching `electron-updater` defaults.
#[derive(Debug, Clone, Copy)]
pub struct AuthenticodePolicy<'a> {
    /// Allowed certificate subject Distinguished Names (e.g. `"CN=Acme, O=Acme, C=US"`).
    pub publisher_names: &'a [String],
    /// Allow signatures reported from a Windows security catalog instead of embedded in the file.
    pub allow_catalog_signature: bool,
}

/// Verifies Windows Authenticode signatures on installers using `electron-updater`'s fail-closed rules.
pub struct WindowsAuthenticodeVerifier;

impl WindowsAuthenticodeVerifier {
    /// Resolves an absolute executable path inside `%SystemRoot%\System32`.
    pub fn windows_system32_path(system_root: Option<&str>, sub_path: &str) -> Result<String> {
        let root = system_root.ok_or_else(|| {
            Error::Signature(
                "cannot locate Windows System32 tool: SystemRoot environment variable is not set"
                    .into(),
            )
        })?;
        // Matches absolute Windows SystemRoot paths:
        // - `C:\Windows`
        // - `D:/Windows`
        if !regex_is_match!(r"^[a-zA-Z]:[\\/]", root) {
            return Err(Error::Signature(format!(
                "cannot locate Windows System32 tool: SystemRoot is not an absolute path ({root})"
            )));
        }
        let trimmed = root.replace('/', "\\");
        let trimmed = trimmed.trim_end_matches('\\');
        let clean_sub = sub_path.trim_start_matches('\\');
        Ok(format!("{trimmed}\\System32\\{clean_sub}"))
    }

    /// Resolves the absolute path to in-box Windows PowerShell 5.1 from `%SystemRoot%`.
    pub fn windows_powershell_path(system_root: Option<&str>) -> Result<String> {
        Self::windows_system32_path(system_root, "WindowsPowerShell\\v1.0\\powershell.exe").map_err(
            |error| match error {
                Error::Signature(msg) => {
                    Error::Signature(msg.replace("Windows System32 tool", "Windows PowerShell"))
                }
                other => other,
            },
        )
    }

    /// Builds PowerShell CLI arguments running `Get-AuthenticodeSignature` via UTF-16LE `-EncodedCommand`.
    pub fn build_powershell_args() -> Vec<String> {
        let utf16le: Vec<u8> = POWERSHELL_VERIFY_SCRIPT
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-InputFormat".into(),
            "None".into(),
            "-EncodedCommand".into(),
            STANDARD.encode(utf16le),
        ]
    }

    /// Environment variable carrying the target file path into PowerShell without script interpolation.
    pub fn file_env_var() -> &'static str {
        AUTHENTICODE_FILE_ENV
    }

    /// Parses an RFC 2253 Distinguished Name string into key-value attribute pairs.
    pub fn parse_dn(subject: &str) -> BTreeMap<String, String> {
        DistinguishedName::parse(subject).into_map()
    }

    /// Returns true if `name` is a non-empty DN whose attributes all have non-empty keys and values.
    pub fn is_valid_publisher_name_dn(name: &str) -> bool {
        DistinguishedName::parse(name).is_valid()
    }

    /// Returns true if any configured DN in `publisher_names` matches `subject`.
    pub fn match_publisher_name(subject: &str, publisher_names: &[String]) -> bool {
        let subject_dn = DistinguishedName::parse(subject);
        publisher_names
            .iter()
            .any(|candidate| subject_dn.matches_candidate(&DistinguishedName::parse(candidate)))
    }

    /// Normalizes Windows paths for `LiteralPath` equality checks (uppercase drive letter, `\` separators).
    pub fn normalize_windows_path(path: &str) -> String {
        let replaced = path.replace('/', "\\");
        let (prefix, rest) = if let Some(unc) = replaced.strip_prefix("\\\\") {
            ("\\\\".to_string(), unc)
        } else if let Some(after_drive) = replaced.get(2..)
            && replaced.as_bytes().get(1) == Some(&b':')
            && replaced.as_bytes()[0].is_ascii_alphabetic()
        {
            let drive = replaced.as_bytes()[0].to_ascii_uppercase() as char;
            if let Some(stripped) = after_drive.strip_prefix('\\') {
                (format!("{drive}:\\"), stripped)
            } else {
                (format!("{drive}:"), after_drive)
            }
        } else if let Some(rooted) = replaced.strip_prefix('\\') {
            ("\\".to_string(), rooted)
        } else {
            (String::new(), replaced.as_str())
        };

        let mut segments: Vec<&str> = Vec::new();
        for part in rest.split('\\') {
            match part {
                "" | "." => {}
                ".." => {
                    segments.pop();
                }
                other => segments.push(other),
            }
        }
        format!("{prefix}{}", segments.join("\\"))
    }

    /// Evaluates JSON printed by `Get-AuthenticodeSignature` against `expected_file` and `policy`.
    pub fn evaluate_signature_json(
        stdout: &str,
        expected_file: &str,
        policy: &AuthenticodePolicy<'_>,
    ) -> Result<()> {
        if policy.publisher_names.is_empty() {
            return Err(Error::Signature(
                "Windows Authenticode verification requires at least one publisherName DN".into(),
            ));
        }

        let data: serde_json::Value = serde_json::from_str(stdout).map_err(|error| {
            Error::Signature(format!(
                "cannot parse Get-AuthenticodeSignature JSON output: {error}"
            ))
        })?;
        let Some(object) = data.as_object() else {
            return Err(Error::Signature(
                "Get-AuthenticodeSignature output is not a JSON object".into(),
            ));
        };

        let status = object.get("Status").and_then(serde_json::Value::as_i64);
        if status != Some(0) {
            let status_text = status
                .map(|code| code.to_string())
                .unwrap_or_else(|| "missing".into());
            return Err(Error::Signature(format!(
                "Windows Authenticode signature is not valid (Status={status_text})"
            )));
        }

        let reported_path = object
            .get("Path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                Error::Signature(
                    "Get-AuthenticodeSignature did not report the verified file Path".into(),
                )
            })?;
        let norm_reported = Self::normalize_windows_path(reported_path);
        let norm_expected = Self::normalize_windows_path(expected_file);
        if norm_reported != norm_expected {
            return Err(Error::Signature(format!(
                "Get-AuthenticodeSignature LiteralPath mismatch: reported '{norm_reported}', expected '{norm_expected}'"
            )));
        }

        let subject = object
            .get("SignerCertificate")
            .and_then(|cert| cert.get("Subject"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                Error::Signature(
                    "Get-AuthenticodeSignature reported a valid signature without SignerCertificate.Subject"
                        .into(),
                )
            })?;

        let is_catalog = object
            .get("SignatureType")
            .and_then(serde_json::Value::as_i64)
            == Some(CATALOG_SIGNATURE_TYPE);
        if is_catalog && !policy.allow_catalog_signature {
            return Err(Error::Signature(
                "Windows Authenticode rejected catalog signature; embedded signature required"
                    .into(),
            ));
        }

        if !Self::match_publisher_name(subject, policy.publisher_names) {
            return Err(Error::Signature(format!(
                "Windows Authenticode signer '{subject}' does not match configured publisherName DN(s)"
            )));
        }

        if is_catalog {
            tracing::warn!(
                signer = subject,
                file = expected_file,
                "accepted Windows security catalog Authenticode signature via allowCatalogSignature"
            );
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn verify_file(path: &Path, policy: &AuthenticodePolicy<'_>) -> Result<()> {
        use std::io::Read;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const VERIFY_TIMEOUT: Duration = Duration::from_secs(60);
        const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

        fn spawn_bounded_reader<R: Read + Send + 'static>(
            mut reader: R,
        ) -> std::thread::JoinHandle<Result<String>> {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                reader
                    .by_ref()
                    .take((MAX_OUTPUT_BYTES + 1) as u64)
                    .read_to_end(&mut buf)?;
                if buf.len() > MAX_OUTPUT_BYTES {
                    return Err(Error::Signature(
                        "Get-AuthenticodeSignature printed more output than allowed".into(),
                    ));
                }
                Ok(String::from_utf8_lossy(&buf).into_owned())
            })
        }

        if policy.publisher_names.is_empty() {
            return Err(Error::Signature(
                "Windows Authenticode verification requires at least one publisherName DN".into(),
            ));
        }

        let system_root = std::env::var("SystemRoot")
            .or_else(|_| std::env::var("windir"))
            .ok();
        let powershell = Self::windows_powershell_path(system_root.as_deref())?;

        let mut command = Command::new(&powershell);
        command
            .args(Self::build_powershell_args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW);

        for (key, _) in std::env::vars_os() {
            let lower = key.to_string_lossy().to_ascii_lowercase();
            if lower == "psmodulepath" || lower == AUTHENTICODE_FILE_ENV.to_ascii_lowercase() {
                command.env_remove(&key);
            }
        }
        command.env(AUTHENTICODE_FILE_ENV, path);

        let mut child = command.spawn().map_err(|error| {
            Error::Signature(format!(
                "cannot run Windows PowerShell for Authenticode verification: {error}"
            ))
        })?;
        let stdout_handle = child.stdout.take().map(spawn_bounded_reader);
        let stderr_handle = child.stderr.take().map(spawn_bounded_reader);

        let deadline = Instant::now() + VERIFY_TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Signature(format!(
                    "Get-AuthenticodeSignature timed out after {}s",
                    VERIFY_TIMEOUT.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(20));
        };

        let stdout = stdout_handle
            .and_then(|handle| handle.join().ok())
            .transpose()?
            .unwrap_or_default();
        let stderr = stderr_handle
            .and_then(|handle| handle.join().ok())
            .transpose()?
            .unwrap_or_default();

        if !status.success() {
            return Err(Error::Signature(format!(
                "Get-AuthenticodeSignature exited with {status}: {}",
                stderr.trim()
            )));
        }
        if !stderr.trim().is_empty() {
            return Err(Error::Signature(format!(
                "Get-AuthenticodeSignature wrote to stderr: {}",
                stderr.trim()
            )));
        }
        Self::evaluate_signature_json(&stdout, &path.to_string_lossy(), policy)
    }
}
