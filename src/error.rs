use std::fmt;

use thiserror::Error;

/// Semantic transport error classification used by the update engine for protocol-level decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportErrorKind {
    /// Requested resource (e.g. manifest or `.blockmap`) does not exist.
    NotFound,
    /// Caller authentication failed or credentials expired.
    Unauthorized,
    /// Access to the resource was denied (e.g. expired signed token or bad signature).
    Forbidden,
    /// Remote peer is throttling requests.
    RateLimited,
    /// Transfer timed out.
    Timeout,
    /// Remote peer or channel is temporarily unavailable.
    Unavailable,
    /// Protocol-specific status code (such as an HTTP status code).
    Status(u16),
    /// Unclassified transport failure.
    Other,
}

/// Protocol-agnostic transport error returned by custom [`crate::UpdateProvider`] implementations.
#[derive(Debug)]
pub struct TransportError {
    /// High-level semantic classification of the failure.
    pub kind: TransportErrorKind,
    /// Human-readable error context.
    pub message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl TransportError {
    /// Constructs a transport error with semantic `kind` and human-readable `message`.
    pub fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// Attaches an underlying transport error source (such as a gRPC or MQTT client error).
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Maps an HTTP status code into a structured [`TransportErrorKind`].
    pub fn kind_from_http_status(status: u16) -> TransportErrorKind {
        match status {
            401 => TransportErrorKind::Unauthorized,
            403 => TransportErrorKind::Forbidden,
            404 => TransportErrorKind::NotFound,
            408 => TransportErrorKind::Timeout,
            429 => TransportErrorKind::RateLimited,
            502..=504 => TransportErrorKind::Unavailable,
            code => TransportErrorKind::Status(code),
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|err| err as &(dyn std::error::Error + 'static))
    }
}

/// Errors returned by blockmap generation, differential download, verification, or installation.
#[derive(Debug, Error)]
pub enum Error {
    #[error("updater configuration invalid: {0}")]
    InvalidConfig(String),
    #[error("updater endpoint URL invalid: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("updater HTTP header invalid: {0}")]
    InvalidHeader(String),
    #[cfg(feature = "http")]
    #[error("updater HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("updater HTTP status failed for {url}: {status}")]
    HttpStatus { url: String, status: u16 },
    #[error("updater transport failed: {0}")]
    Transport(#[from] TransportError),
    #[error("updater manifest invalid: {0}")]
    InvalidManifest(String),
    #[error("updater target not found in manifest: {0}")]
    TargetNotFound(String),
    #[error("updater blockmap invalid: {0}")]
    InvalidBlockMap(String),
    #[error("updater differential transfer failed: {0}")]
    DifferentialTransfer(String),
    #[error("updater transfer incomplete: {missing_bytes} byte(s) missing")]
    IncompleteTransfer { missing_bytes: u64 },
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

impl Error {
    /// Returns the structured [`TransportErrorKind`] when this error originated from a transport layer.
    pub fn transport_kind(&self) -> Option<TransportErrorKind> {
        match self {
            Self::Transport(err) => Some(err.kind),
            Self::HttpStatus { status, .. } => Some(TransportError::kind_from_http_status(*status)),
            #[cfg(feature = "http")]
            Self::Http(err) => {
                if err.is_timeout() {
                    Some(TransportErrorKind::Timeout)
                } else if let Some(status) = err.status() {
                    Some(TransportError::kind_from_http_status(status.as_u16()))
                } else {
                    Some(TransportErrorKind::Unavailable)
                }
            }
            _ => None,
        }
    }

    /// Returns true when the error represents an authentication or authorization failure.
    pub fn is_auth_error(&self) -> bool {
        matches!(
            self.transport_kind(),
            Some(TransportErrorKind::Unauthorized | TransportErrorKind::Forbidden)
        )
    }
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
