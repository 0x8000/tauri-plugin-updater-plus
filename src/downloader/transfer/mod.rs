/// Tuning options controlling HTTP range batching and concurrency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferOptions {
    /// Maximum number of byte ranges packed into one `multipart/byteranges` request.
    pub max_ranges_per_request: usize,
    /// Override multi-range usage (`None` auto-detects single-range-only hosts like GitHub/S3).
    pub use_multiple_range_request: Option<bool>,
    /// Maximum concurrent single-range requests when running in single-range mode.
    pub max_concurrent_ranges: usize,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            max_ranges_per_request: 128,
            use_multiple_range_request: None,
            max_concurrent_ranges: 4,
        }
    }
}
