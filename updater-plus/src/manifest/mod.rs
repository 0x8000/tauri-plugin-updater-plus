use std::collections::HashMap;
use std::str::FromStr;

use semver::Version;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;

use crate::{Error, Result};

mod electron_yaml;
#[cfg(test)]
mod tests;

/// Artifact metadata for one target platform inside a release manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasePlatform {
    /// Download URL for the target package or installer.
    pub url: Url,
    /// Optional base64-encoded minisign signature (Tauri convention).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Optional base64-encoded SHA-512 checksum (Electron convention).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha512: Option<String>,
    /// Optional total artifact byte size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Optional explicit `.blockmap` URL; defaults to `{url}.blockmap` when omitted.
    #[serde(
        default,
        alias = "blockMapUrl",
        skip_serializing_if = "Option::is_none"
    )]
    pub blockmap_url: Option<Url>,
    /// Optional compressed `.blockmap` size in bytes.
    #[serde(
        default,
        alias = "blockMapSize",
        skip_serializing_if = "Option::is_none"
    )]
    pub blockmap_size: Option<u64>,
}

impl ReleasePlatform {
    /// Resolves the `.blockmap` URL, falling back to Electron's `{url}.blockmap` suffix convention.
    pub fn resolved_blockmap_url(&self) -> Result<Url> {
        if let Some(url) = &self.blockmap_url {
            return Ok(url.clone());
        }
        let mut derived = self.url.clone();
        let path = format!("{}.blockmap", derived.path());
        derived.set_path(&path);
        Ok(derived)
    }
}

/// Platform table shape supported by a release manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleasePlatforms {
    /// Single platform resolved directly by the endpoint (dynamic JSON or Electron YAML).
    Single(Box<ReleasePlatform>),
    /// Multi-platform map keyed by target triple (e.g. `linux-x86_64-appimage`, `linux-x86_64`).
    Map(HashMap<String, ReleasePlatform>),
}

/// Normalized release metadata parsed from Tauri `latest.json` or Electron `latest*.yml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseManifest {
    /// Semantic version announced by the release.
    pub version: Version,
    /// Optional release notes body.
    pub notes: Option<String>,
    /// Optional RFC 3339 publication timestamp.
    pub pub_date: Option<OffsetDateTime>,
    /// Platform artifact entries.
    pub platforms: ReleasePlatforms,
    /// Raw JSON representation preserved for diagnostic inspection.
    pub raw_json: serde_json::Value,
}

impl ReleaseManifest {
    /// Parses either Tauri `latest.json` or Electron `latest*.yml` resolved against `source_url`.
    pub fn parse(body: &str, source_url: &Url) -> Result<Self> {
        let trimmed = body.trim_start();
        if trimmed.starts_with('{') {
            Self::parse_tauri_json(trimmed)
        } else {
            electron_yaml::ElectronYamlParser::parse(body, source_url)
        }
    }

    /// Resolves the best matching platform entry from an ordered list of candidate target keys.
    pub fn resolve_platform(&self, target_candidates: &[String]) -> Result<&ReleasePlatform> {
        match &self.platforms {
            ReleasePlatforms::Single(platform) => Ok(platform),
            ReleasePlatforms::Map(map) => {
                for candidate in target_candidates {
                    if let Some(platform) = map.get(candidate) {
                        return Ok(platform);
                    }
                }
                Err(Error::TargetNotFound(target_candidates.join(", ")))
            }
        }
    }

    /// Formats the publication date as an RFC 3339 string when present.
    pub fn formatted_pub_date(&self) -> Option<String> {
        self.pub_date.and_then(|date| date.format(&Rfc3339).ok())
    }

    /// Serializes a Tauri-compatible JSON manifest extended with blockmap fields.
    pub fn to_tauri_json(&self) -> Result<String> {
        #[derive(Serialize)]
        struct TauriStaticManifest<'a> {
            version: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            notes: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub_date: Option<String>,
            platforms: &'a HashMap<String, ReleasePlatform>,
        }

        let pub_date = self.formatted_pub_date();
        match &self.platforms {
            ReleasePlatforms::Map(platforms) => {
                Ok(serde_json::to_string_pretty(&TauriStaticManifest {
                    version: self.version.to_string(),
                    notes: &self.notes,
                    pub_date,
                    platforms,
                })?)
            }
            ReleasePlatforms::Single(platform) => {
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "version": self.version.to_string(),
                    "notes": self.notes,
                    "pub_date": pub_date,
                    "url": platform.url,
                    "signature": platform.signature,
                    "sha512": platform.sha512,
                    "size": platform.size,
                    "blockmap_url": platform.blockmap_url,
                    "blockmap_size": platform.blockmap_size,
                }))?)
            }
        }
    }

    /// Serializes an Electron-compatible `latest*.yml` manifest for a single platform artifact.
    pub fn to_electron_yaml(&self, target_candidates: &[String]) -> Result<String> {
        let platform = self.resolve_platform(target_candidates)?;
        let file_ref = platform
            .url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|segment| !segment.is_empty())
            .unwrap_or(platform.url.as_str());
        let sha512 = platform.sha512.as_deref().unwrap_or("");
        let release_date = self
            .formatted_pub_date()
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

        let mut lines = vec![
            format!("version: {}", self.version),
            "files:".to_string(),
            format!("  - url: {file_ref}"),
            format!("    sha512: {sha512}"),
        ];
        if let Some(size) = platform.size {
            lines.push(format!("    size: {size}"));
        }
        if let Some(blockmap_size) = platform.blockmap_size {
            lines.push(format!("    blockMapSize: {blockmap_size}"));
        }
        if let Some(blockmap_url) = &platform.blockmap_url {
            let escaped_map = blockmap_url.as_str().replace('\'', "''");
            lines.push(format!("    blockMapUrl: '{escaped_map}'"));
        }
        lines.push(format!("path: {file_ref}"));
        lines.push(format!("sha512: {sha512}"));
        lines.push(format!("releaseDate: '{release_date}'"));
        if let Some(notes) = &self.notes {
            if notes.contains(['\n', '\r']) {
                let escaped = notes
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r");
                lines.push(format!("releaseNotes: \"{escaped}\""));
            } else {
                let escaped = notes.replace('\'', "''");
                lines.push(format!("releaseNotes: '{escaped}'"));
            }
        }
        lines.push(String::new());
        Ok(lines.join("\n"))
    }

    fn parse_tauri_json(text: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct RawTauriManifest {
            #[serde(alias = "name")]
            version: String,
            notes: Option<String>,
            pub_date: Option<String>,
            platforms: Option<HashMap<String, ReleasePlatform>>,
            url: Option<Url>,
            signature: Option<String>,
            sha512: Option<String>,
            size: Option<u64>,
            #[serde(alias = "blockMapUrl")]
            blockmap_url: Option<Url>,
            #[serde(alias = "blockMapSize")]
            blockmap_size: Option<u64>,
        }

        let raw_json: serde_json::Value = serde_json::from_str(text)?;
        let parsed: RawTauriManifest = serde_json::from_value(raw_json.clone())?;
        let version = Version::from_str(parsed.version.trim_start_matches('v'))?;
        let pub_date = parsed
            .pub_date
            .as_deref()
            .map(|value| {
                OffsetDateTime::parse(value, &Rfc3339)
                    .map_err(|error| Error::InvalidManifest(format!("invalid pub_date: {error}")))
            })
            .transpose()?;

        let platforms = if let Some(map) = parsed.platforms {
            ReleasePlatforms::Map(map)
        } else {
            let url = parsed.url.ok_or_else(|| {
                Error::InvalidManifest("dynamic manifest missing `url` field".into())
            })?;
            ReleasePlatforms::Single(Box::new(ReleasePlatform {
                url,
                signature: parsed.signature,
                sha512: parsed.sha512,
                size: parsed.size,
                blockmap_url: parsed.blockmap_url,
                blockmap_size: parsed.blockmap_size,
            }))
        };

        Ok(Self {
            version,
            notes: parsed.notes,
            pub_date,
            platforms,
            raw_json,
        })
    }
}
