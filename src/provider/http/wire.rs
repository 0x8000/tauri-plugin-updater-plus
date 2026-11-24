use bytes::Bytes;
use futures_util::StreamExt;
use http::HeaderMap;
use http::header::{ACCEPT_RANGES, CONTENT_RANGE, HeaderValue};

use crate::multipart::ByteRangeParser;
use crate::planner::ByteRange;
use crate::provider::{ByteStream, RangeFetchResult, RangePart};
use crate::{Error, Result};

const MAX_RANGE_HEADER_BYTES: usize = 6 * 1024;
const MULTIPART_FRAMING_SLACK_PER_PART: u64 = 1024;
const MULTIPART_FRAMING_BASE_SLACK: u64 = 64 * 1024;

pub(crate) async fn read_bounded_body(
    response: reqwest::Response,
    max_bytes: usize,
    label: &str,
) -> Result<Vec<u8>> {
    if let Some(len) = response.content_length()
        && len > max_bytes as u64
    {
        return Err(Error::DifferentialTransfer(format!(
            "{label} Content-Length {len} exceeds maximum allowed {max_bytes} bytes"
        )));
    }
    let mut buffer = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if buffer.len().saturating_add(chunk.len()) > max_bytes {
            return Err(Error::DifferentialTransfer(format!(
                "{label} exceeded maximum allowed {max_bytes} bytes"
            )));
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(buffer)
}

pub(super) fn response_to_byte_stream(response: reqwest::Response) -> ByteStream {
    let content_length = response.content_length();
    let stream = response
        .bytes_stream()
        .map(|item| item.map_err(Error::from));
    ByteStream {
        content_length,
        stream: Box::pin(stream),
    }
}

pub(super) fn explicitly_rejects_ranges(headers: &HeaderMap) -> bool {
    headers
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("none"))
}

pub(super) fn format_range_header(ranges: &[ByteRange]) -> Result<HeaderValue> {
    let list = ranges
        .iter()
        .map(|range| format!("{}-{}", range.start, range.end))
        .collect::<Vec<_>>()
        .join(", ");
    HeaderValue::from_str(&format!("bytes={list}"))
        .map_err(|error| Error::InvalidHeader(format!("invalid Range header: {error}")))
}

pub(super) fn split_by_header_budget(ranges: &[ByteRange]) -> Vec<Vec<ByteRange>> {
    let mut batches = Vec::new();
    let mut start_idx = 0;
    while start_idx < ranges.len() {
        let mut end_idx = start_idx;
        let mut header_bytes = "bytes=".len();
        while end_idx < ranges.len() {
            let piece_len = format!("{}-{}", ranges[end_idx].start, ranges[end_idx].end).len()
                + usize::from(end_idx > start_idx) * 2;
            if end_idx > start_idx && header_bytes + piece_len > MAX_RANGE_HEADER_BYTES {
                break;
            }
            header_bytes += piece_len;
            end_idx += 1;
        }
        batches.push(ranges[start_idx..end_idx].to_vec());
        start_idx = end_idx;
    }
    batches
}

pub(super) async fn read_multipart_partial(
    response: reqwest::Response,
    boundary: &str,
    ranges: &[ByteRange],
) -> Result<RangeFetchResult> {
    let payload_sum: u64 = ranges.iter().map(|r| r.len()).sum();
    let max_wire = payload_sum
        .saturating_add(MULTIPART_FRAMING_BASE_SLACK)
        .saturating_add((ranges.len() as u64) * MULTIPART_FRAMING_SLACK_PER_PART);
    let cap = usize::try_from(max_wire).unwrap_or(usize::MAX);
    let body = read_bounded_body(response, cap, "multipart range response").await?;
    let parsed = ByteRangeParser::parse_multipart(&body, boundary)?;
    if parsed.len() != ranges.len() {
        return Ok(RangeFetchResult::DegradeToSingle);
    }
    let mut expected: Vec<(u64, u64)> = ranges.iter().map(|r| (r.start, r.end)).collect();
    let mut actual: Vec<(u64, u64)> = parsed.iter().map(|p| (p.start, p.end)).collect();
    expected.sort_unstable();
    actual.sort_unstable();
    if actual != expected {
        return Err(Error::DifferentialTransfer(
            "multipart Content-Range offsets disagreed with requested ranges".into(),
        ));
    }
    let parts = parsed
        .into_iter()
        .map(|p| RangePart {
            start: p.start,
            end: p.end,
            data: Bytes::from(p.data),
        })
        .collect();
    Ok(RangeFetchResult::Parts(parts))
}

pub(super) async fn read_single_partial(
    response: reqwest::Response,
    expected: ByteRange,
) -> Result<RangePart> {
    let raw_header = response.headers().get(CONTENT_RANGE).ok_or_else(|| {
        Error::DifferentialTransfer(
            "HTTP 206 Partial Content response missing Content-Range header".into(),
        )
    })?;
    let header = raw_header.to_str().map_err(|_| {
        Error::DifferentialTransfer("Content-Range header is not valid ASCII".into())
    })?;
    let (start, end) = ByteRangeParser::parse_content_range(header)?;
    if start != expected.start || end != expected.end {
        return Err(Error::DifferentialTransfer(format!(
            "Content-Range {start}-{end} disagreed with requested {}-{}",
            expected.start, expected.end
        )));
    }
    let max_len = usize::try_from(expected.len()).map_err(|_| {
        Error::DifferentialTransfer("expected partial range overflows usize".into())
    })?;
    let bytes = read_bounded_body(response, max_len, "partial range response").await?;
    if bytes.len() as u64 != expected.len() {
        return Err(Error::DifferentialTransfer(format!(
            "partial response size {} disagreed with expected {}",
            bytes.len(),
            expected.len()
        )));
    }
    Ok(RangePart {
        start: expected.start,
        end: expected.end,
        data: Bytes::from(bytes),
    })
}
