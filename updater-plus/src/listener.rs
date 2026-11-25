use serde::{Deserialize, Serialize};

/// Transfer mode chosen for an update download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadMode {
    /// Reconstructed the target package from local blocks plus HTTP Range deltas.
    Differential,
    /// Downloaded the full package directly.
    Full,
}

/// Transfer metrics recorded after an update download completes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSummary {
    /// Whether differential patching or a full download was used.
    pub mode: DownloadMode,
    /// Total reconstructed package size in bytes.
    pub total_bytes: u64,
    /// Bytes fetched over HTTP (excluding manifest and blockmap metadata).
    pub downloaded_bytes: u64,
    /// Bytes reused locally from the cached base package or running AppImage.
    pub reused_bytes: u64,
    /// Number of HTTP requests issued for package data.
    pub http_requests: usize,
    /// Percentage of package bytes saved by local block reuse (0.0..=100.0).
    pub saved_percent: f64,
    /// Diagnostic reason when differential mode fell back to a full download.
    pub fallback_reason: Option<String>,
}

impl DownloadSummary {
    /// Builds a summary from transfer counters.
    pub fn from_counts(
        mode: DownloadMode,
        total_bytes: u64,
        downloaded_bytes: u64,
        reused_bytes: u64,
    ) -> Self {
        let saved_percent = if total_bytes == 0 {
            0.0
        } else {
            (reused_bytes as f64 / total_bytes as f64) * 100.0
        };
        Self {
            mode,
            total_bytes,
            downloaded_bytes,
            reused_bytes,
            http_requests: 0,
            saved_percent,
            fallback_reason: None,
        }
    }

    /// Attaches the HTTP request count and optional fallback reason.
    pub fn with_details(mut self, http_requests: usize, fallback_reason: Option<String>) -> Self {
        self.http_requests = http_requests;
        self.fallback_reason = fallback_reason;
        self
    }
}

/// Progress events emitted while downloading and reconstructing an update package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum DownloadEvent {
    /// Emitted once the transfer plan is finalized and downloading begins.
    #[serde(rename_all = "camelCase")]
    Started {
        /// Expected number of bytes to download over HTTP.
        content_length: Option<u64>,
        /// Total target artifact size in bytes.
        total_size: Option<u64>,
        /// True when differential blockmap patching is active.
        differential: bool,
        /// Bytes scheduled to be reused from the local base package.
        reused_bytes: u64,
    },
    /// Emitted as downloaded chunks arrive over the network.
    #[serde(rename_all = "camelCase")]
    Progress {
        /// Newly downloaded byte count for this progress step.
        chunk_length: usize,
    },
    /// Emitted once download, reconstruction, and verification succeed.
    #[serde(rename_all = "camelCase")]
    Finished {
        /// Final transfer metrics.
        summary: DownloadSummary,
    },
}

/// Observer interface for receiving download progress events.
pub trait DownloadListener: Send + Sync {
    /// Invoked for each transfer lifecycle event.
    fn on_event(&self, event: DownloadEvent);
}

/// No-op [`DownloadListener`] used when callers do not observe progress.
#[derive(Debug, Default, Clone, Copy)]
pub struct DummyListener;

impl DownloadListener for DummyListener {
    fn on_event(&self, _event: DownloadEvent) {}
}
