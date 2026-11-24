use thiserror::Error;

/// Errors returned by blockmap generation, differential download, verification, or installation.
#[derive(Debug, Error)]
pub enum Error {
    #[error("updater configuration invalid: {0}")]
    InvalidConfig(String),
    #[error("updater endpoint URL invalid: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("updater HTTP header invalid: {0}")]
    InvalidHeader(String),
    #[cfg(feature = "plugin")]
    #[error("updater HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("updater HTTP status failed for {url}: {status}")]
    HttpStatus { url: String, status: u16 },
    #[error("updater manifest invalid: {0}")]
    InvalidManifest(String),
    #[error("updater target not found in manifest: {0}")]
    TargetNotFound(String),
    #[error("updater blockmap invalid: {0}")]
    InvalidBlockMap(String),
    #[error("updater differential transfer failed: {0}")]
    DifferentialTransfer(String),
    #[error("updater checksum mismatch: expected {expected}, actual {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("updater signature invalid: {0}")]
    Signature(String),
    #[error("updater signed version mismatch: signed {signed}, announced {announced}")]
    SignedVersionMismatch { signed: String, announced: String },
    #[error("updater missing signed version in signature trusted comment")]
    MissingSignedVersion,
    #[error("updater package format invalid: {0}")]
    InvalidPackageFormat(String),
    #[error("updater installation failed: {0}")]
    Install(String),
    #[error("updater platform unsupported for in-place installation")]
    UnsupportedPlatform,
    #[error("updater I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("updater JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("updater archive extraction failed: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("updater semver parse failed: {0}")]
    Semver(#[from] semver::Error),
    #[cfg(feature = "plugin")]
    #[error("updater Tauri runtime error: {0}")]
    Tauri(#[from] tauri::Error),
}

impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Result alias for `tauri-plugin-updater-plus`.
pub type Result<T> = std::result::Result<T, Error>;
