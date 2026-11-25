use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures_util::Stream;
use semver::Version;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;

use crate::Result;
use crate::planner::ByteRange;

mod failover;
#[cfg(feature = "http")]
pub mod http;
#[cfg(test)]
mod tests;

pub use failover::FailoverProvider;

/// Boxed `Send` future alias used by object-safe provider traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Boxed `Send` byte chunk stream returned by transport download sessions.
pub type BoxByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send + 'static>>;

/// Opaque protocol-agnostic identifier pointing to a remote artifact or blockmap.
///
/// [`Display`] redacts URL query strings so presigned S3/CDN tokens are never leaked into logs.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ArtifactId {
    raw: String,
    route: Vec<usize>,
}

impl ArtifactId {
    /// Creates a new opaque artifact identifier.
    pub fn new(raw: impl Into<String>) -> Self {
        Self {
            raw: raw.into(),
            route: Vec::new(),
        }
    }

    /// Returns the raw underlying identifier string for the owning provider.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub(crate) fn with_route(mut self, provider_index: usize) -> Self {
        self.route.push(provider_index);
        self
    }

    pub(crate) fn pop_route(&self) -> Option<(usize, Self)> {
        let mut route = self.route.clone();
        let index = route.pop()?;
        Some((
            index,
            Self {
                raw: self.raw.clone(),
                route,
            },
        ))
    }

    fn redacted_str(&self) -> &str {
        self.raw.split('?').next().unwrap_or(&self.raw)
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.raw.contains('?') {
            write!(f, "{}?<redacted>", self.redacted_str())
        } else {
            f.write_str(&self.raw)
        }
    }
}

impl fmt::Debug for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ArtifactId(\"{self}\")")
    }
}

impl From<String> for ArtifactId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for ArtifactId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// Per-request transport overrides passed from `check` / `download` calls down to providers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferOverrides {
    /// Optional per-call timeout override.
    pub timeout: Option<Duration>,
    /// Protocol-agnostic key-value metadata (mapped to HTTP headers or gRPC metadata).
    pub metadata: Vec<(String, String)>,
    /// Optional HTTP proxy URL (non-HTTP providers should reject this with `InvalidConfig`).
    pub proxy: Option<Url>,
}

impl TransferOverrides {
    /// Appends a metadata key-value pair.
    pub fn push_metadata(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.metadata.push((key.into(), value.into()));
    }
}

/// Context passed to [`UpdateProvider::resolve_release`] when checking for newer releases.
#[derive(Debug, Clone)]
pub struct ResolveContext {
    /// Currently installed application semantic version.
    pub current_version: Version,
    /// Ordered platform lookup keys (e.g. `["linux-x86_64-appimage", "linux-x86_64"]`).
    pub target_candidates: Vec<String>,
    /// Host architecture identifier (e.g. `"x86_64"`, `"aarch64"`).
    pub arch: String,
    /// Optional active bundle type name (e.g. `"appimage"`, `"deb"`, `"rpm"`).
    pub bundle_type: Option<String>,
    /// When true, manifests omitting `sha512` are invalid because no minisign pubkey is configured.
    pub require_checksum: bool,
    /// Per-call transport overrides.
    pub overrides: TransferOverrides,
}

/// Normalized release discovery result returned by an [`UpdateProvider`].
#[derive(Debug, Clone)]
pub struct ResolvedRelease {
    /// Semantic version offered by the remote release.
    pub version: Version,
    /// Optional release notes text.
    pub notes: Option<String>,
    /// Optional publication timestamp.
    pub pub_date: Option<OffsetDateTime>,
    /// Opaque identifier of the target artifact.
    pub artifact: ArtifactId,
    /// Optional opaque identifier of the artifact's Electron v2 `.blockmap`.
    pub blockmap: Option<ArtifactId>,
    /// Optional base64-encoded SHA-512 checksum.
    pub sha512: Option<String>,
    /// Optional base64-encoded minisign signature.
    pub signature: Option<String>,
    /// Optional expected artifact byte length.
    pub size: Option<u64>,
    /// Raw manifest/release metadata preserved for diagnostics.
    pub raw_json: serde_json::Value,
}

impl ResolvedRelease {
    /// Formats `pub_date` as an RFC 3339 string when present.
    pub fn formatted_pub_date(&self) -> Option<String> {
        self.pub_date.and_then(|date| date.format(&Rfc3339).ok())
    }
}

/// Range-fetch capability advertised by an active [`DownloadSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeCapability {
    /// Session can fetch multiple disjoint ranges in a single request (e.g. Nginx multipart, batch gRPC).
    MultiRange {
        /// Maximum number of ranges to batch in one call.
        max_ranges_per_call: usize,
        /// Maximum concurrent single-range requests if the session degrades to single-range mode.
        max_concurrency: usize,
    },
    /// Session fetches one range per request and supports concurrent single-range calls.
    SingleRange {
        /// Maximum concurrent range requests.
        max_concurrency: usize,
    },
    /// Session only supports full-artifact streaming downloads.
    FullOnly,
}

/// Dynamic transfer capabilities advertised by an opened [`DownloadSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferCapabilities {
    /// Range transfer mode supported by this target.
    pub range: RangeCapability,
    /// Suggested gap threshold (bytes) for coalescing nearby ranges in [`crate::DownloadPlan`].
    pub suggested_max_range_gap: u64,
}

impl Default for TransferCapabilities {
    fn default() -> Self {
        Self {
            range: RangeCapability::SingleRange { max_concurrency: 4 },
            suggested_max_range_gap: 0,
        }
    }
}

/// Streaming byte response paired with an optional known content length.
pub struct ByteStream {
    /// Expected byte length reported by the transport, when known.
    pub content_length: Option<u64>,
    /// Asynchronous byte chunk stream.
    pub stream: BoxByteStream,
}

/// Verified byte payload corresponding to one inclusive `[start..=end]` byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangePart {
    /// Inclusive starting byte offset.
    pub start: u64,
    /// Inclusive ending byte offset.
    pub end: u64,
    /// Exact payload bytes for `[start..=end]`.
    pub data: Bytes,
}

/// Outcome returned by [`DownloadSession::fetch_ranges`].
pub enum RangeFetchResult {
    /// One or more requested byte ranges were returned.
    Parts(Vec<RangePart>),
    /// Target rejected or ignored multi-range; caller should retry using single-range calls.
    DegradeToSingle,
    /// Target ignored range headers and already returned the full artifact stream.
    FallbackFull(ByteStream),
    /// Target does not support range requests at all; caller should call [`DownloadSession::fetch_full`].
    Unsupported,
}

/// Request telemetry reported by a [`DownloadSession`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferStats {
    /// Number of underlying transport requests issued for artifact bytes.
    pub requests: usize,
}

/// Pull-model release discovery and artifact transport provider.
///
/// Implemented by built-in HTTP providers (`StaticHttpProvider`, `GitHubProvider`, `S3Provider`)
/// as well as arbitrary custom transports (gRPC, MQTT request/response, VCS, LAN sync, etc.).
///
/// Transport-level retries, backoff, circuit breakers, and connection pooling belong inside the
/// provider or its middleware rather than in the update engine.
pub trait UpdateProvider: Send + Sync + fmt::Debug {
    /// Discovers the latest release for the host described in `ctx`.
    fn resolve_release<'a>(
        &'a self,
        ctx: &'a ResolveContext,
    ) -> BoxFuture<'a, Result<Option<ResolvedRelease>>>;

    /// Fetches the raw `.blockmap` bytes for `id`, rejecting payloads exceeding `max_bytes`.
    fn fetch_blockmap<'a>(
        &'a self,
        id: &'a ArtifactId,
        max_bytes: usize,
        overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Bytes>>;

    /// Opens a RAII per-download session for artifact `id`.
    fn open_session<'a>(
        &'a self,
        id: &'a ArtifactId,
        overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Box<dyn DownloadSession>>>;
}

/// Active per-download session owning transient transport state (e.g. redirect URLs or streams).
pub trait DownloadSession: Send + Sync {
    /// Returns the transfer capabilities for this specific opened artifact.
    fn capabilities(&self) -> TransferCapabilities;

    /// Opens a full-artifact byte stream.
    fn fetch_full(&self) -> BoxFuture<'static, Result<ByteStream>>;

    /// Fetches the requested inclusive byte ranges.
    ///
    /// Providers that only support full transfers may inherit this default implementation.
    fn fetch_ranges(
        &self,
        _ranges: Vec<ByteRange>,
    ) -> BoxFuture<'static, Result<RangeFetchResult>> {
        Box::pin(async { Ok(RangeFetchResult::Unsupported) })
    }

    /// Returns cumulative artifact request counts recorded by this session.
    fn stats(&self) -> TransferStats;
}
