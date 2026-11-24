use std::str::FromStr;

use lazy_regex::regex_captures;
use semver::Version;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;

use super::{ReleaseManifest, ReleasePlatform, ReleasePlatforms};
use crate::{Error, Result};

#[derive(Default)]
struct YamlFileEntry {
    url: Option<String>,
    sha512: Option<String>,
    size: Option<u64>,
    blockmap_size: Option<u64>,
    blockmap_url: Option<String>,
}

pub(crate) struct ElectronYamlParser;

impl ElectronYamlParser {
    pub(crate) fn parse(body: &str, source_url: &Url) -> Result<ReleaseManifest> {
        let mut version: Option<String> = None;
        let mut top_path: Option<String> = None;
        let mut top_sha512: Option<String> = None;
        let mut release_date: Option<String> = None;
        let mut release_notes: Option<String> = None;
        let mut files: Vec<YamlFileEntry> = Vec::new();
        let mut in_files = false;

        for line in body.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((key, raw_value)) = Self::parse_top_level_kv(line) {
                in_files = key == "files";
                let value = Self::unquote_yaml(raw_value);
                match key {
                    "version" => version = Some(value),
                    "path" => top_path = Some(value),
                    "sha512" => top_sha512 = Some(value),
                    "releaseDate" => release_date = Some(value),
                    "releaseNotes" => release_notes = Some(value),
                    _ => {}
                }
                continue;
            }
            if in_files {
                Self::parse_file_item_line(line, &mut files);
            }
        }

        let version_str = version.ok_or_else(|| {
            Error::InvalidManifest("electron YAML manifest missing `version`".into())
        })?;
        let parsed_version = Version::from_str(version_str.trim_start_matches('v'))?;
        let pub_date = release_date
            .as_deref()
            .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok());

        let selected = files.into_iter().next().unwrap_or_default();
        let artifact_ref = selected.url.or(top_path).ok_or_else(|| {
            Error::InvalidManifest(
                "electron YAML manifest missing `files[0].url` and `path`".into(),
            )
        })?;
        let resolved_url = Self::resolve_manifest_url(source_url, &artifact_ref)?;
        let resolved_blockmap = selected
            .blockmap_url
            .as_deref()
            .map(|raw| Self::resolve_manifest_url(source_url, raw))
            .transpose()?;

        let platform = ReleasePlatform {
            url: resolved_url,
            signature: None,
            sha512: selected.sha512.or(top_sha512),
            size: selected.size,
            blockmap_url: resolved_blockmap,
            blockmap_size: selected.blockmap_size,
        };
        let raw_json = serde_json::json!({
            "version": parsed_version.to_string(),
            "notes": release_notes,
            "pub_date": release_date,
            "url": platform.url,
            "sha512": platform.sha512,
            "size": platform.size,
            "blockmap_url": platform.blockmap_url,
            "blockmap_size": platform.blockmap_size,
        });

        Ok(ReleaseManifest {
            version: parsed_version,
            notes: release_notes,
            pub_date,
            platforms: ReleasePlatforms::Single(Box::new(platform)),
            raw_json,
        })
    }

    fn parse_top_level_kv(line: &str) -> Option<(&str, &str)> {
        // Matches top-level unindented YAML keys in Electron latest*.yml manifests:
        // - `version: 1.2.3`
        // - `files:`
        // - `releaseDate: '2026-11-24T00:00:00.000Z'`
        regex_captures!(r"^(\w+):\s*(.*?)\s*$", line).map(|(_, key, value)| (key, value))
    }

    fn parse_file_item_line(line: &str, files: &mut Vec<YamlFileEntry>) {
        // Matches an indented YAML sequence item start under `files:`:
        // - `  - url: MyApp-0.2.0.AppImage`
        // - `  - sha512: abcdef==`
        if let Some((_, key, value)) = regex_captures!(r"^\s+-\s+(\w+):\s*(.*?)\s*$", line) {
            let mut entry = YamlFileEntry::default();
            Self::assign_file_field(&mut entry, key, &Self::unquote_yaml(value));
            files.push(entry);
            return;
        }

        // Matches an indented property line belonging to the current `files:` sequence item:
        // - `    sha512: abcdef==`
        // - `    size: 134217728`
        // - `    blockMapSize: 65432`
        if let Some((_, key, value)) = regex_captures!(r"^\s+(\w+):\s*(.*?)\s*$", line)
            && let Some(entry) = files.last_mut()
        {
            Self::assign_file_field(entry, key, &Self::unquote_yaml(value));
        }
    }

    fn assign_file_field(entry: &mut YamlFileEntry, key: &str, value: &str) {
        match key {
            "url" => entry.url = Some(value.to_string()),
            "sha512" => entry.sha512 = Some(value.to_string()),
            "size" => entry.size = value.parse().ok(),
            "blockMapSize" => entry.blockmap_size = value.parse().ok(),
            "blockMapUrl" => entry.blockmap_url = Some(value.to_string()),
            _ => {}
        }
    }

    fn unquote_yaml(raw: &str) -> String {
        // Matches single-quoted or double-quoted YAML scalar values:
        // - `'2026-11-24T00:00:00.000Z'`
        // - `"MyApp 0.2.0"`
        if let Some((_, inner)) = regex_captures!(r"^'(.*)'$", raw) {
            return inner.replace("''", "'");
        }
        if let Some((_, inner)) = regex_captures!(r#"^"(.*)"$"#, raw) {
            return inner.replace("\\\"", "\"");
        }
        raw.to_string()
    }

    fn resolve_manifest_url(source_url: &Url, reference: &str) -> Result<Url> {
        if let Ok(absolute) = Url::parse(reference) {
            return Ok(absolute);
        }
        Ok(source_url.join(reference)?)
    }
}
