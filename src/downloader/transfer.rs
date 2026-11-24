use std::fs::File;
use std::io::Write;
use std::path::Path;

use futures_util::StreamExt;
use http::StatusCode;
use lazy_regex::regex_is_match;
use reqwest::header::{
    ACCEPT_RANGES, AUTHORIZATION, CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue, RANGE,
};
use url::Url;

use crate::blockmap::BlockMap;
use crate::listener::{DownloadEvent, DownloadListener};
use crate::multipart::{ByteRangeParser, ByteRangePart};
use crate::{Error, Result};

const MAX_RANGE_HEADER_BYTES: usize = 6 * 1024;

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

pub(crate) struct TransferExecutor {
    client: reqwest::Client,
    headers: HeaderMap,
    options: TransferOptions,
}

impl TransferExecutor {
    pub(crate) fn new(
        client: reqwest::Client,
        headers: HeaderMap,
        options: TransferOptions,
    ) -> Self {
        Self {
            client,
            headers,
            options: TransferOptions {
                max_ranges_per_request: options.max_ranges_per_request.max(1),
                use_multiple_range_request: options.use_multiple_range_request,
                max_concurrent_ranges: options.max_concurrent_ranges.clamp(1, 16),
            },
        }
    }

    pub(crate) fn max_concurrent_ranges(&self) -> usize {
        self.options.max_concurrent_ranges
    }

    pub(crate) fn should_start_single_range(&self, url: &Url) -> bool {
        match self.options.use_multiple_range_request {
            Some(use_multi) => !use_multi,
            None => Self::is_known_single_range_host(url),
        }
    }

    pub(crate) fn is_known_single_range_host(url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        // Matches hosts that support single HTTP Range requests (206) but ignore or reject
        // multi-range requests (GitHub Releases, AWS S3, Cloudflare R2, Aliyun OSS, Azure Blob, GCS):
        // - `github.com`, `api.github.com`
        // - `objects.githubusercontent.com`, `release-assets.githubusercontent.com`
        // - `bucket.s3.us-east-1.amazonaws.com`
        // - `account.r2.cloudflarestorage.com`
        // - `bucket.oss-cn-hangzhou.aliyuncs.com`
        // - `account.blob.core.windows.net`
        // - `storage.googleapis.com`
        regex_is_match!(
            r"(?i)^(?:(?:api\.)?github\.com|(?:objects|release-assets)\.githubusercontent\.com|(?:[\w-]+\.)+amazonaws\.com|(?:[\w-]+\.)+r2\.cloudflarestorage\.com|(?:[\w-]+\.)+aliyuncs\.com|(?:[\w-]+\.)+blob\.core\.windows\.net|storage\.googleapis\.com)$",
            host
        )
    }

    pub(crate) fn split_batches<'a>(&self, ranges: &'a [ByteRange]) -> Vec<&'a [ByteRange]> {
        let mut batches = Vec::new();
        let mut start_idx = 0;
        while start_idx < ranges.len() {
            let mut end_idx = start_idx;
            let mut header_bytes = "bytes=".len();
            while end_idx < ranges.len()
                && (end_idx - start_idx) < self.options.max_ranges_per_request
            {
                let piece_len = format!("{}-{}", ranges[end_idx].start, ranges[end_idx].end).len()
                    + usize::from(end_idx > start_idx) * 2;
                if end_idx > start_idx && header_bytes + piece_len > MAX_RANGE_HEADER_BYTES {
                    break;
                }
                header_bytes += piece_len;
                end_idx += 1;
            }
            batches.push(&ranges[start_idx..end_idx]);
            start_idx = end_idx;
        }
        batches
    }

    pub(crate) async fn fetch_blockmap(&self, url: &Url) -> Result<BlockMap> {
        let response = self
            .client
            .get(url.clone())
            .headers(self.headers.clone())
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::HttpStatus {
                url: url.to_string(),
                status: response.status().as_u16(),
            });
        }
        let bytes = response.bytes().await?;
        BlockMap::from_bytes(&bytes)
    }

    pub(crate) async fn fetch_range_batch(
        &self,
        target: RangeRequestTarget<'_>,
        ranges: &[ByteRange],
    ) -> Result<BatchOutcome> {
        let header_value = Self::format_range_header(ranges)?;
        let mut headers = self.headers_for_target(&target);
        headers.insert(RANGE, header_value);

        let response = self
            .client
            .get(target.request_url.clone())
            .headers(headers)
            .send()
            .await?;
        let final_url = response.url().clone();
        let status = response.status();
        if status == StatusCode::OK {
            if ranges.len() > 1 && !Self::explicitly_rejects_ranges(response.headers()) {
                return Ok(BatchOutcome::NeedSingleRange);
            }
            return Ok(BatchOutcome::FullResponse(response));
        }
        if status != StatusCode::PARTIAL_CONTENT {
            if ranges.len() > 1
                && matches!(
                    status,
                    StatusCode::BAD_REQUEST
                        | StatusCode::RANGE_NOT_SATISFIABLE
                        | StatusCode::NOT_IMPLEMENTED
                )
            {
                return Ok(BatchOutcome::NeedSingleRange);
            }
            return Err(Error::HttpStatus {
                url: target.request_url.to_string(),
                status: status.as_u16(),
            });
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if let Some(boundary) = ByteRangeParser::extract_boundary(&content_type) {
            let body = response.bytes().await?;
            let parts = ByteRangeParser::parse_multipart(&body, &boundary)?;
            if parts.len() != ranges.len() {
                return Ok(BatchOutcome::NeedSingleRange);
            }
            return Ok(BatchOutcome::Parts { parts, final_url });
        }

        if ranges.len() == 1 {
            let part = Self::read_single_partial(response, ranges[0]).await?;
            return Ok(BatchOutcome::Parts {
                parts: vec![part],
                final_url,
            });
        }

        Ok(BatchOutcome::NeedSingleRange)
    }

    pub(crate) async fn fetch_single_range(
        &self,
        target: RangeRequestTarget<'_>,
        range: ByteRange,
    ) -> Result<SingleOutcome> {
        let header_value = Self::format_range_header(&[range])?;
        let mut headers = self.headers_for_target(&target);
        headers.insert(RANGE, header_value);

        let response = self
            .client
            .get(target.request_url.clone())
            .headers(headers)
            .send()
            .await?;
        let final_url = response.url().clone();
        let status = response.status();
        if status == StatusCode::OK {
            return Ok(SingleOutcome::FullResponse(response));
        }
        if status != StatusCode::PARTIAL_CONTENT {
            return Err(Error::HttpStatus {
                url: target.request_url.to_string(),
                status: status.as_u16(),
            });
        }
        let part = Self::read_single_partial(response, range).await?;
        Ok(SingleOutcome::Part { part, final_url })
    }

    pub(crate) async fn open_full_response(&self, url: &Url) -> Result<reqwest::Response> {
        let response = self
            .client
            .get(url.clone())
            .headers(self.headers.clone())
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::HttpStatus {
                url: url.to_string(),
                status: response.status().as_u16(),
            });
        }
        Ok(response)
    }

    pub(crate) async fn write_full_response<L: DownloadListener>(
        response: reqwest::Response,
        target: &Path,
        listener: &L,
    ) -> Result<u64> {
        let mut file = File::create(target)?;
        let mut downloaded = 0u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk)?;
            downloaded = downloaded.saturating_add(chunk.len() as u64);
            listener.on_event(DownloadEvent::Progress {
                chunk_length: chunk.len(),
            });
        }
        file.flush()?;
        Ok(downloaded)
    }

    fn headers_for_target(&self, target: &RangeRequestTarget<'_>) -> HeaderMap {
        let mut headers = self.headers.clone();
        if target.original_url.origin() != target.request_url.origin() {
            headers.remove(AUTHORIZATION);
            headers.remove(COOKIE);
        }
        headers
    }

    fn explicitly_rejects_ranges(headers: &HeaderMap) -> bool {
        headers
            .get(ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("none"))
    }

    fn format_range_header(ranges: &[ByteRange]) -> Result<HeaderValue> {
        let list = ranges
            .iter()
            .map(|range| format!("{}-{}", range.start, range.end))
            .collect::<Vec<_>>()
            .join(", ");
        HeaderValue::from_str(&format!("bytes={list}"))
            .map_err(|error| Error::InvalidHeader(format!("invalid Range header: {error}")))
    }

    async fn read_single_partial(
        response: reqwest::Response,
        expected: ByteRange,
    ) -> Result<ByteRangePart> {
        if let Some(header) = response
            .headers()
            .get(http::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
        {
            let (start, end) = ByteRangeParser::parse_content_range(header)?;
            if start != expected.start || end != expected.end {
                return Err(Error::DifferentialTransfer(format!(
                    "Content-Range {start}-{end} disagreed with requested {}-{}",
                    expected.start, expected.end
                )));
            }
        }
        let bytes = response.bytes().await?;
        if bytes.len() as u64 != expected.len() {
            return Err(Error::DifferentialTransfer(format!(
                "partial response size {} disagreed with expected {}",
                bytes.len(),
                expected.len()
            )));
        }
        Ok(ByteRangePart {
            start: expected.start,
            end: expected.end,
            data: bytes.to_vec(),
        })
    }
}
