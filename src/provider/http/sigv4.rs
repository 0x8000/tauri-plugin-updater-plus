use std::fmt;
use std::sync::Arc;

use aws_lc_rs::{digest, hmac};
use http::HeaderValue;
use http::header::{AUTHORIZATION, HOST, HeaderName};
use time::OffsetDateTime;
use url::Url;

use super::downloader::HttpDownloader;
use super::middleware::{HttpAuthorizer, HttpRequest, HttpRequestContext};
use super::static_http::StaticHttpProvider;
use crate::downloader::TransferOptions;
use crate::provider::BoxFuture;
use crate::{Error, Result};

const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// Temporary or static AWS/S3-compatible credentials used by [`SigV4Authorizer`].
///
/// For desktop applications, prefer issuing short-lived STS credentials (with `session_token`)
/// via a dynamic [`AwsCredentialsProvider`] instead of embedding static IAM keys in binaries.
#[derive(Clone)]
pub struct AwsCredentials {
    /// AWS Access Key ID.
    pub access_key_id: String,
    /// AWS Secret Access Key.
    pub secret_access_key: String,
    /// Optional STS temporary session token (`x-amz-security-token`).
    pub session_token: Option<String>,
}

impl fmt::Debug for AwsCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AwsCredentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("has_session_token", &self.session_token.is_some())
            .finish()
    }
}

/// Asynchronous provider supplying fresh AWS/S3 credentials (e.g. STS token refresh).
pub trait AwsCredentialsProvider: Send + Sync + fmt::Debug {
    /// Returns credentials for signing the current request hop.
    fn credentials(&self) -> BoxFuture<'_, Result<AwsCredentials>>;
}

/// Static credentials holder (convenient for tests, CLI, or fixed-credential environments).
#[derive(Debug, Clone)]
pub struct StaticAwsCredentials(pub AwsCredentials);

impl AwsCredentialsProvider for StaticAwsCredentials {
    fn credentials(&self) -> BoxFuture<'_, Result<AwsCredentials>> {
        let creds = self.0.clone();
        Box::pin(async move { Ok(creds) })
    }
}

/// Closure adapter for dynamic AWS credential retrieval (e.g. refreshing STS tokens).
pub struct FnCredentialsProvider<F> {
    func: F,
}

impl<F> FnCredentialsProvider<F>
where
    F: Fn() -> Result<AwsCredentials> + Send + Sync + 'static,
{
    /// Wraps `func` as an [`AwsCredentialsProvider`].
    pub fn new(func: F) -> Self {
        Self { func }
    }
}

impl<F> fmt::Debug for FnCredentialsProvider<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FnCredentialsProvider(..)")
    }
}

impl<F> AwsCredentialsProvider for FnCredentialsProvider<F>
where
    F: Fn() -> Result<AwsCredentials> + Send + Sync + 'static,
{
    fn credentials(&self) -> BoxFuture<'_, Result<AwsCredentials>> {
        let result = (self.func)();
        Box::pin(async move { result })
    }
}

/// Lightweight AWS Signature Version 4 (`AWS4-HMAC-SHA256`) per-hop [`HttpAuthorizer`]
/// implemented directly over `aws-lc-rs` with zero `aws-sdk-s3` dependency overhead.
///
/// Works with AWS S3, Cloudflare R2, MinIO, Aliyun OSS (S3-compatible endpoint), and GCS HMAC.
#[derive(Debug, Clone)]
pub struct SigV4Authorizer {
    region: String,
    service: String,
    credentials: Arc<dyn AwsCredentialsProvider>,
}

impl SigV4Authorizer {
    /// Creates a SigV4 authorizer targeting `region` and `"s3"` service.
    pub fn s3(region: impl Into<String>, credentials: Arc<dyn AwsCredentialsProvider>) -> Self {
        Self {
            region: region.into(),
            service: "s3".into(),
            credentials,
        }
    }

    /// Creates a SigV4 authorizer for a custom AWS service name.
    pub fn new(
        region: impl Into<String>,
        service: impl Into<String>,
        credentials: Arc<dyn AwsCredentialsProvider>,
    ) -> Self {
        Self {
            region: region.into(),
            service: service.into(),
            credentials,
        }
    }

    /// Signs `req` in place using `creds` and timestamp `now` (exposed for deterministic verification).
    pub fn sign_request_at(
        &self,
        req: &mut HttpRequest,
        creds: &AwsCredentials,
        now: OffsetDateTime,
    ) -> Result<()> {
        let host = host_header_value(&req.url)?;
        let amz_date = format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
            now.year(),
            now.month() as u8,
            now.day(),
            now.hour(),
            now.minute(),
            now.second()
        );
        let date_stamp = &amz_date[..8];

        req.headers.insert(
            HOST,
            HeaderValue::from_str(&host)
                .map_err(|err| Error::InvalidHeader(format!("invalid Host header: {err}")))?,
        );
        req.headers.insert(
            HeaderName::from_static("x-amz-content-sha256"),
            HeaderValue::from_static(UNSIGNED_PAYLOAD),
        );
        req.headers.insert(
            HeaderName::from_static("x-amz-date"),
            HeaderValue::from_str(&amz_date)
                .map_err(|err| Error::InvalidHeader(format!("invalid x-amz-date: {err}")))?,
        );
        if let Some(token) = &creds.session_token {
            req.headers.insert(
                HeaderName::from_static("x-amz-security-token"),
                HeaderValue::from_str(token).map_err(|err| {
                    Error::InvalidHeader(format!("invalid x-amz-security-token: {err}"))
                })?,
            );
        } else {
            req.headers.remove("x-amz-security-token");
        }

        let mut signed_pairs: Vec<(String, String)> = vec![
            ("host".into(), host),
            ("x-amz-content-sha256".into(), UNSIGNED_PAYLOAD.into()),
            ("x-amz-date".into(), amz_date.clone()),
        ];
        if let Some(token) = &creds.session_token {
            signed_pairs.push(("x-amz-security-token".into(), token.trim().to_string()));
        }
        signed_pairs.sort_by(|a, b| a.0.cmp(&b.0));

        let canonical_headers: String = signed_pairs
            .iter()
            .map(|(k, v)| format!("{k}:{v}\n"))
            .collect();
        let signed_headers: String = signed_pairs
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";");

        let canonical_uri = canonical_s3_uri(req.url.path());
        let canonical_query = canonical_query_string(&req.url);
        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            req.method.as_str(),
            canonical_uri,
            canonical_query,
            canonical_headers,
            signed_headers,
            UNSIGNED_PAYLOAD,
        );

        let canonical_hash =
            hex_encode(digest::digest(&digest::SHA256, canonical_request.as_bytes()).as_ref());
        let credential_scope =
            format!("{date_stamp}/{}/{}/aws4_request", self.region, self.service);
        let string_to_sign =
            format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_hash}");

        let k_date = hmac_sha256(
            format!("AWS4{}", creds.secret_access_key).as_bytes(),
            date_stamp.as_bytes(),
        );
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, self.service.as_bytes());
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        let signature = hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        let auth_value = format!(
            "AWS4-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
            creds.access_key_id
        );
        req.headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth_value).map_err(|err| {
                Error::InvalidHeader(format!("invalid SigV4 Authorization header: {err}"))
            })?,
        );
        Ok(())
    }
}

impl HttpAuthorizer for SigV4Authorizer {
    fn authorize<'a>(
        &'a self,
        ctx: &'a HttpRequestContext,
        req: &'a mut HttpRequest,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if ctx.is_cross_origin_redirect {
                return Ok(());
            }
            let creds = self.credentials.credentials().await?;
            self.sign_request_at(req, &creds, OffsetDateTime::now_utc())
        })
    }
}

/// Convenience factory composing a [`StaticHttpProvider`] with [`SigV4Authorizer`] and
/// single-range mode for private S3 / R2 / MinIO / OSS buckets.
pub struct S3Provider;

impl S3Provider {
    /// Builds a SigV4-authenticated [`StaticHttpProvider`] backed by `downloader`.
    pub fn build(
        manifest_endpoints: Vec<Url>,
        region: impl Into<String>,
        credentials: Arc<dyn AwsCredentialsProvider>,
        downloader: Option<HttpDownloader>,
    ) -> Result<StaticHttpProvider> {
        let base = match downloader {
            Some(dl) => dl,
            None => HttpDownloader::builder().build()?,
        };
        let signed_downloader = base
            .with_transfer_options(TransferOptions {
                use_multiple_range_request: Some(false),
                ..base.transfer_options()
            })
            .with_added_authorizer(Arc::new(SigV4Authorizer::s3(region, credentials)));
        StaticHttpProvider::new(signed_downloader, manifest_endpoints)
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let signing_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let tag = hmac::sign(&signing_key, data);
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn host_header_value(url: &Url) -> Result<String> {
    let host = url
        .host_str()
        .ok_or_else(|| Error::InvalidConfig(format!("SigV4 URL missing host: {url}")))?;
    match url.port() {
        Some(port) => Ok(format!("{host}:{port}")),
        None => Ok(host.to_string()),
    }
}

fn aws_uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let unreserved = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (!encode_slash && *byte == b'/');
        if unreserved {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

fn canonical_s3_uri(path: &str) -> String {
    if path.is_empty() {
        return "/".into();
    }
    // `url::Url::path()` is already percent-encoded; decode-then-encode avoids double-encoding.
    let decoded = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    aws_uri_encode(&decoded, false)
}

fn canonical_query_string(url: &Url) -> String {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (aws_uri_encode(&k, true), aws_uri_encode(&v, true)))
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}
