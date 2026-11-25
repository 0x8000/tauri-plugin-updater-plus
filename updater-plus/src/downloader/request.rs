#[cfg(feature = "http")]
use url::Url;

use crate::provider::{ArtifactId, TransferOverrides};
use crate::verifier::VerifyExpectation;

/// Protocol-agnostic package download and verification contract driven by an [`crate::UpdateProvider`].
#[derive(Debug, Clone)]
pub struct ProviderDownloadRequest {
    /// Target version being downloaded.
    pub version: String,
    /// Opaque identifier of the remote artifact.
    pub artifact: ArtifactId,
    /// Optional opaque identifier of the remote `.blockmap`.
    pub blockmap: Option<ArtifactId>,
    /// Optional expected artifact byte length from release metadata.
    pub expected_size: Option<u64>,
    /// Integrity and signature verification requirements.
    pub verification: VerifyExpectation,
    /// Whether differential downloading should be attempted before falling back to full.
    pub differential: bool,
    /// Maximum changed ratio (`0.0..=1.0`) allowed before switching to a full download.
    pub full_download_threshold: f64,
    /// Optional override for coalescing nearby missing ranges (`None` uses session capability hint).
    pub max_range_gap: Option<u64>,
    /// Per-call transport overrides (timeout, metadata, proxy).
    pub overrides: TransferOverrides,
}

/// HTTP-oriented package download contract (convenience wrapper over [`ProviderDownloadRequest`]).
#[cfg(feature = "http")]
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    /// Target version being downloaded.
    pub version: String,
    /// Full artifact download URL.
    pub artifact_url: Url,
    /// Remote `.blockmap` URL used when differential mode is active.
    pub blockmap_url: Url,
    /// Optional expected artifact byte length from the manifest.
    pub expected_size: Option<u64>,
    /// Integrity and signature verification requirements.
    pub verification: VerifyExpectation,
    /// Whether differential downloading should be attempted before falling back to full.
    pub differential: bool,
    /// Maximum changed ratio (0.0..=1.0) allowed before switching to a full download.
    pub full_download_threshold: f64,
}
