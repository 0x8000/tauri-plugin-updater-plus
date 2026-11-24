use url::Url;

use crate::multipart::ByteRangePart;
use crate::{Error, Result};

mod executor;

pub(crate) use executor::TransferExecutor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ByteRange {
    pub(crate) start: u64,
    pub(crate) end: u64,
}

impl ByteRange {
    pub(crate) fn from_offset_len(offset: u64, length: u64) -> Result<Self> {
        if length == 0 {
            return Err(Error::DifferentialTransfer(
                "byte range length must be positive".into(),
            ));
        }
        let end = offset
            .checked_add(length - 1)
            .ok_or_else(|| Error::DifferentialTransfer("byte range end overflow".into()))?;
        Ok(Self { start: offset, end })
    }

    pub(crate) fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

pub(crate) struct RangeRequestTarget<'a> {
    pub(crate) original_url: &'a Url,
    pub(crate) request_url: &'a Url,
}

pub(crate) enum BatchOutcome {
    Parts {
        parts: Vec<ByteRangePart>,
        final_url: Url,
    },
    NeedSingleRange,
    FullResponse(reqwest::Response),
}

pub(crate) enum SingleOutcome {
    Part { part: ByteRangePart, final_url: Url },
    FullResponse(reqwest::Response),
}

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
