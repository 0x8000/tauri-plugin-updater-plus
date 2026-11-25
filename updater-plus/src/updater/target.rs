use std::path::{Path, PathBuf};

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use url::Url;

use crate::{Error, Result};

const QUERY_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?');

/// Resolves updater target strings, endpoint templates, and installation paths.
pub struct TargetResolver;

impl TargetResolver {
    /// Returns the base `{os}-{arch}` target string for the compiled host platform.
    pub fn current_target() -> Option<String> {
        let os = Self::current_os()?;
        let arch = Self::current_arch()?;
        Some(format!("{os}-{arch}"))
    }

    /// Builds ordered manifest lookup candidates for the compiled host platform.
    pub fn target_candidates(override_target: Option<&str>) -> Vec<String> {
        Self::target_candidates_with_bundle(override_target, None)
    }

    /// Builds ordered manifest lookup candidates, preferring `{target}-{bundle_type}` over `{target}`.
    pub fn target_candidates_with_bundle(
        override_target: Option<&str>,
        bundle_type: Option<&str>,
    ) -> Vec<String> {
        if let Some(target) = override_target {
            return vec![target.to_string()];
        }
        let (Some(os), Some(arch)) = (Self::current_os(), Self::current_arch()) else {
            return vec!["unknown".to_string()];
        };
        Self::target_candidates_for(os, arch, bundle_type)
    }

    /// Builds ordered manifest lookup candidates for an explicit `(os, arch, bundle)` tuple.
    pub fn target_candidates_for(os: &str, arch: &str, bundle: Option<&str>) -> Vec<String> {
        let base = format!("{os}-{arch}");
        if os == "android" {
            let mut candidates = vec![format!("{base}-apk"), base];
            candidates.push("android-universal".to_string());
            candidates.push("android".to_string());
            return candidates;
        }
        let mut candidates = Vec::new();
        if let Some(bundle) = bundle {
            candidates.push(format!("{base}-{bundle}"));
        }
        candidates.push(base);
        candidates
    }

    /// Expands `{{target}}`, `{{arch}}`, `{{current_version}}`, and `{{bundle_type}}` template variables.
    pub fn expand_endpoint(endpoint: &Url, target: &str, current_version: &str) -> Result<Url> {
        let arch = Self::current_arch().unwrap_or("unknown");
        Self::expand_endpoint_with_parts(endpoint, target, arch, "", current_version)
    }

    /// Expands endpoint template placeholders with explicit `arch` and `bundle` strings.
    pub fn expand_endpoint_with_parts(
        endpoint: &Url,
        target: &str,
        arch: &str,
        bundle: &str,
        current_version: &str,
    ) -> Result<Url> {
        let encoded_version = utf8_percent_encode(current_version, QUERY_ENCODE_SET).to_string();
        let expanded = endpoint
            .as_str()
            .replace("{{target}}", target)
            .replace("%7B%7Btarget%7D%7D", target)
            .replace("{{arch}}", arch)
            .replace("%7B%7Barch%7D%7D", arch)
            .replace("{{current_version}}", &encoded_version)
            .replace("%7B%7Bcurrent_version%7D%7D", &encoded_version)
            .replace("{{bundle_type}}", bundle)
            .replace("%7B%7Bbundle_type%7D%7D", bundle);
        Ok(Url::parse(&expanded)?)
    }

    /// Resolves the installation target path from the current executable path.
    pub fn extract_path_from_executable(executable_path: &Path) -> Result<PathBuf> {
        Self::extract_path_from_executable_with_bundle(executable_path, None)
    }

    /// Resolves the installation target path from the current executable path and optional bundle hint.
    pub fn extract_path_from_executable_with_bundle(
        executable_path: &Path,
        bundle_type: Option<&str>,
    ) -> Result<PathBuf> {
        #[cfg(target_os = "linux")]
        if !matches!(bundle_type, Some("deb" | "rpm"))
            && let Some(appimage) = std::env::var_os("APPIMAGE")
        {
            return Ok(PathBuf::from(appimage));
        }
        #[cfg(not(target_os = "linux"))]
        let _ = bundle_type;

        let parent = executable_path.parent().map(PathBuf::from).ok_or_else(|| {
            Error::Install("failed to resolve executable parent directory".into())
        })?;

        #[cfg(target_os = "macos")]
        if parent.to_string_lossy().contains("Contents/MacOS") {
            return parent
                .parent()
                .and_then(Path::parent)
                .map(PathBuf::from)
                .ok_or_else(|| Error::Install("failed to resolve macOS .app bundle path".into()));
        }

        Ok(parent)
    }

    /// Returns the compiled host architecture name (e.g. `"x86_64"`, `"aarch64"`).
    pub fn current_arch() -> Option<&'static str> {
        if cfg!(target_arch = "x86") {
            Some("i686")
        } else if cfg!(target_arch = "x86_64") {
            Some("x86_64")
        } else if cfg!(target_arch = "arm") {
            Some("armv7")
        } else if cfg!(target_arch = "aarch64") {
            Some("aarch64")
        } else if cfg!(target_arch = "riscv64") {
            Some("riscv64")
        } else {
            None
        }
    }

    fn current_os() -> Option<&'static str> {
        if cfg!(target_os = "linux") {
            Some("linux")
        } else if cfg!(target_os = "macos") {
            Some("darwin")
        } else if cfg!(target_os = "windows") {
            Some("windows")
        } else if cfg!(target_os = "android") {
            Some("android")
        } else {
            None
        }
    }
}
