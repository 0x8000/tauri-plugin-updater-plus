use lazy_regex::regex_captures;

use crate::{Error, Result};

#[cfg(test)]
mod tests;

/// Parsed slice extracted from a single-range or `multipart/byteranges` HTTP 206 response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteRangePart {
    /// Inclusive starting byte offset.
    pub start: u64,
    /// Inclusive ending byte offset.
    pub end: u64,
    /// Exact payload bytes for `[start..=end]`.
    pub data: Vec<u8>,
}

/// Parses RFC 9110 `Content-Range` headers and `multipart/byteranges` HTTP 206 payloads.
pub struct ByteRangeParser;

impl ByteRangeParser {
    /// Extracts the boundary token from a `multipart/byteranges; boundary=...` header.
    pub fn extract_boundary(content_type: &str) -> Option<String> {
        // Matches multipart/byteranges headers with quoted or unquoted boundary parameters:
        // - `multipart/byteranges; boundary=3d6b6a416f9b5`
        // - `multipart/byteranges; boundary="abc-123_XYZ"`
        // - `MULTIPART/BYTERANGES; charset=utf-8; boundary=boundary_token`
        regex_captures!(
            r#"(?i)multipart/byteranges\b.*?\bboundary=(?:"([^"]+)"|([^\s;]+))"#,
            content_type
        )
        .map(|(_, quoted, bare)| {
            if quoted.is_empty() {
                bare.to_string()
            } else {
                quoted.to_string()
            }
        })
    }

    /// Parses a `Content-Range` header into `(start, end)` inclusive byte offsets.
    pub fn parse_content_range(header: &str) -> Result<(u64, u64)> {
        // Matches RFC 9110 byte Content-Range headers:
        // - `bytes 0-1023/4096`
        // - `bytes 100-200/*`
        let Some((_, start_str, end_str)) =
            regex_captures!(r"(?i)^\s*bytes\s+(\d+)-(\d+)/(?:\d+|\*)\s*$", header)
        else {
            return Err(Error::DifferentialTransfer(format!(
                "invalid Content-Range header '{header}'"
            )));
        };
        let start: u64 = start_str
            .parse()
            .map_err(|_| Error::DifferentialTransfer("invalid Content-Range start".into()))?;
        let end: u64 = end_str
            .parse()
            .map_err(|_| Error::DifferentialTransfer("invalid Content-Range end".into()))?;
        if end < start {
            return Err(Error::DifferentialTransfer(
                "Content-Range end precedes start".into(),
            ));
        }
        Ok((start, end))
    }

    /// Splits a `multipart/byteranges` body into validated [`ByteRangePart`] entries.
    pub fn parse_multipart(body: &[u8], boundary: &str) -> Result<Vec<ByteRangePart>> {
        let delimiter = format!("--{boundary}");
        let delimiter_bytes = delimiter.as_bytes();
        let mut parts = Vec::new();
        let mut cursor = Self::find_subslice(body, delimiter_bytes, 0).ok_or_else(|| {
            Error::DifferentialTransfer("multipart/byteranges missing opening boundary".into())
        })?;

        loop {
            cursor += delimiter_bytes.len();
            if body[cursor..].starts_with(b"--") {
                break;
            }
            if body[cursor..].starts_with(b"\r\n") {
                cursor += 2;
            } else if body[cursor..].starts_with(b"\n") {
                cursor += 1;
            } else {
                return Err(Error::DifferentialTransfer(
                    "malformed newline after multipart boundary".into(),
                ));
            }

            let (header_end, separator_len) =
                Self::find_header_end(body, cursor).ok_or_else(|| {
                    Error::DifferentialTransfer("multipart part missing header terminator".into())
                })?;
            let header_text = std::str::from_utf8(&body[cursor..header_end]).map_err(|_| {
                Error::DifferentialTransfer("multipart part headers are not UTF-8".into())
            })?;
            let (start, end) = Self::extract_part_content_range(header_text)?;
            let expected_len_u64 = (end - start).checked_add(1).ok_or_else(|| {
                Error::DifferentialTransfer("multipart part length overflows u64".into())
            })?;
            let expected_len = usize::try_from(expected_len_u64).map_err(|_| {
                Error::DifferentialTransfer("multipart part length overflows usize".into())
            })?;

            let data_start = header_end + separator_len;
            let data_end = data_start.checked_add(expected_len).ok_or_else(|| {
                Error::DifferentialTransfer("multipart part offset overflow".into())
            })?;
            if data_end > body.len() {
                return Err(Error::DifferentialTransfer(
                    "multipart part truncated before expected Content-Range length".into(),
                ));
            }

            parts.push(ByteRangePart {
                start,
                end,
                data: body[data_start..data_end].to_vec(),
            });

            cursor = Self::find_subslice(body, delimiter_bytes, data_end).ok_or_else(|| {
                Error::DifferentialTransfer("multipart/byteranges missing closing boundary".into())
            })?;
        }

        if parts.is_empty() {
            return Err(Error::DifferentialTransfer(
                "multipart/byteranges contained no parts".into(),
            ));
        }
        Ok(parts)
    }

    fn extract_part_content_range(headers: &str) -> Result<(u64, u64)> {
        // Matches a Content-Range header line inside a multipart/byteranges part:
        // - `Content-Range: bytes 0-15/1024`
        // - `content-range: bytes 100-199/*`
        let Some((_, value)) = regex_captures!(r"(?im)^content-range:\s*([^\r\n]+)$", headers)
        else {
            return Err(Error::DifferentialTransfer(
                "multipart part missing Content-Range header".into(),
            ));
        };
        Self::parse_content_range(value)
    }

    fn find_header_end(haystack: &[u8], start: usize) -> Option<(usize, usize)> {
        let crlf = Self::find_subslice(haystack, b"\r\n\r\n", start).map(|pos| (pos, 4));
        let lf = Self::find_subslice(haystack, b"\n\n", start).map(|pos| (pos, 2));
        match (crlf, lf) {
            (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    fn find_subslice(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
        if needle.is_empty() || start + needle.len() > haystack.len() {
            return None;
        }
        haystack[start..]
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|offset| start + offset)
    }
}
