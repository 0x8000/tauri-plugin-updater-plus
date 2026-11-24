use serde::{Deserialize, Serialize};
use url::Url;

/// Windows installer interaction mode matching Tauri's NSIS/MSI conventions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowsInstallMode {
    /// Quiet installation with a minimal progress bar.
    #[default]
    Passive,
    /// Unattended installation without UI.
    Quiet,
    /// Full interactive installer wizard.
    BasicUi,
}

/// Windows-specific installer configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsConfig {
    /// Installer UI mode.
    #[serde(default)]
    pub install_mode: WindowsInstallMode,
    /// Additional arguments forwarded to the installer.
    #[serde(default)]
    pub installer_args: Vec<String>,
    /// Expected Windows Authenticode certificate subject DN(s) (e.g. `"CN=Acme, O=Acme, C=US"`).
    /// When non-empty, installers are verified via `Get-AuthenticodeSignature` before launch.
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub publisher_name: Vec<String>,
    /// Accept Windows security catalog signatures in addition to embedded Authenticode signatures.
    #[serde(default)]
    pub allow_catalog_signature: bool,
}

/// Plugin configuration read from `tauri.conf.json` under `plugins.updater-plus`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Update manifest endpoints (JSON or Electron YAML), tried in order.
    #[serde(default)]
    pub endpoints: Vec<Url>,
    /// Optional base64-encoded minisign public key for signature verification.
    #[serde(default)]
    pub pubkey: Option<String>,
    /// Reject signed artifacts that do not embed a matching version comment.
    #[serde(default)]
    pub require_signed_version: bool,
    /// Enable blockmap differential downloading when a local base package exists.
    #[serde(default = "default_true")]
    pub differential: bool,
    /// Allow installing versions lower than the running version.
    #[serde(default)]
    pub allow_downgrades: bool,
    /// Fallback to a full download when delta ratio exceeds this fraction (0.0..=1.0).
    #[serde(default = "default_fallback_ratio")]
    pub full_download_threshold: f64,
    /// Maximum number of byte ranges packed into a single multipart Range request.
    #[serde(default = "default_max_ranges_per_request")]
    pub max_ranges_per_request: usize,
    /// Control `multipart/byteranges` usage (`None` auto-detects GitHub/S3/R2/OSS hosts).
    #[serde(default)]
    pub use_multiple_range_request: Option<bool>,
    /// Maximum concurrent single-range HTTP requests when multi-range is unavailable or disabled.
    #[serde(default = "default_max_concurrent_ranges")]
    pub max_concurrent_ranges: usize,
    /// Optional Windows installer configuration.
    #[serde(default)]
    pub windows: Option<WindowsConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoints: Vec::new(),
            pubkey: None,
            require_signed_version: false,
            differential: default_true(),
            allow_downgrades: false,
            full_download_threshold: default_fallback_ratio(),
            max_ranges_per_request: default_max_ranges_per_request(),
            use_multiple_range_request: None,
            max_concurrent_ranges: default_max_concurrent_ranges(),
            windows: None,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_fallback_ratio() -> f64 {
    0.85
}

fn default_max_ranges_per_request() -> usize {
    128
}

fn default_max_concurrent_ranges() -> usize {
    4
}

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    match Option::<OneOrMany>::deserialize(deserializer)? {
        Some(OneOrMany::One(single)) => Ok(vec![single]),
        Some(OneOrMany::Many(many)) => Ok(many),
        None => Ok(Vec::new()),
    }
}
