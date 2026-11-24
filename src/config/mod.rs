use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::{Error, Result};

#[cfg(test)]
mod tests;

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

/// Plugin configuration read from `tauri.conf.json` under `plugins.updater` and/or `plugins.updater-plus`.
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
    /// Optional gap coalescing limit in bytes for merging nearby differential ranges (`None` uses provider hint).
    #[serde(default)]
    pub max_range_gap: Option<u64>,
    /// Optional Windows installer configuration.
    #[serde(default)]
    pub windows: Option<WindowsConfig>,
}

impl Config {
    /// Resolves updater configuration from Tauri's `plugins` config map.
    ///
    /// Reads `plugins.updater` as the base configuration so apps with
    /// `bundle.createUpdaterArtifacts: true` satisfy Tauri CLI without duplicating keys,
    /// and overlays any fields from `plugins.updater-plus` on top.
    pub fn from_tauri_plugins(plugins: &HashMap<String, serde_json::Value>) -> Result<Self> {
        fn non_null_object<'a>(
            plugins: &'a HashMap<String, serde_json::Value>,
            key: &str,
        ) -> Result<Option<&'a serde_json::Value>> {
            match plugins.get(key) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(val) if val.is_object() => Ok(Some(val)),
                Some(_) => Err(Error::InvalidConfig(format!(
                    "plugins.{key} must be a JSON object"
                ))),
            }
        }

        fn merge_json_objects(base: &mut serde_json::Value, overlay: serde_json::Value) {
            if let (serde_json::Value::Object(base_map), serde_json::Value::Object(overlay_map)) =
                (base, overlay)
            {
                for (key, value) in overlay_map {
                    match base_map.get_mut(&key) {
                        Some(existing) if existing.is_object() && value.is_object() => {
                            merge_json_objects(existing, value);
                        }
                        _ => {
                            base_map.insert(key, value);
                        }
                    }
                }
            }
        }

        let base = non_null_object(plugins, "updater")?;
        let overlay = non_null_object(plugins, "updater-plus")?;
        let merged = match (base, overlay) {
            (None, None) => return Ok(Self::default()),
            (Some(value), None) | (None, Some(value)) => value.clone(),
            (Some(base_val), Some(overlay_val)) => {
                let mut combined = base_val.clone();
                merge_json_objects(&mut combined, overlay_val.clone());
                combined
            }
        };
        serde_json::from_value(merged).map_err(|error| {
            Error::InvalidConfig(format!("failed to parse updater config: {error}"))
        })
    }
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
            max_range_gap: None,
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
