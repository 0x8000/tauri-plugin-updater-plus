use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{HeaderName, HeaderValue};
use http::{HeaderMap, StatusCode};
use lazy_regex::regex_is_match;
use url::Url;

use super::builder::{HttpDownloaderBuilder, build_reqwest_client};
use super::middleware::{
    HttpAuthorizer, HttpMiddleware, HttpNext, HttpRequest, HttpResponse, RequestStage,
};
use super::redirect::{RedirectRequest, execute_with_manual_redirects};
use super::session::HttpDownloadSession;
use super::wire::read_bounded_body;
use crate::downloader::TransferOptions;
use crate::provider::{BoxFuture, DownloadSession, TransferOverrides};
use crate::{Error, Result};

pub(crate) type ClientConfigurator =
    Arc<dyn Fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder + Send + Sync>;

pub(crate) struct HttpDownloaderInner {
    pub(crate) client: reqwest::Client,
    pub(crate) default_headers: HeaderMap,
    pub(crate) sensitive_headers: Vec<HeaderName>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) proxy: Option<Url>,
    pub(crate) transfer_options: TransferOptions,
    pub(crate) client_configurator: Option<ClientConfigurator>,
    pub(crate) authorizers: Vec<Arc<dyn HttpAuthorizer>>,
    pub(crate) middlewares: Vec<Arc<dyn HttpMiddleware>>,
}

/// Reusable HTTP transport engine shared by `StaticHttpProvider`, `GitHubProvider`, `S3Provider`,
/// and custom HTTP-based providers.
#[derive(Clone)]
pub struct HttpDownloader {
    inner: Arc<HttpDownloaderInner>,
}

impl fmt::Debug for HttpDownloader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpDownloader")
            .field("timeout", &self.inner.timeout)
            .field("transfer_options", &self.inner.transfer_options)
            .field("authorizers", &self.inner.authorizers)
            .field("middlewares", &self.inner.middlewares)
            .finish()
    }
}

impl HttpDownloader {
    pub(crate) fn from_inner(inner: HttpDownloaderInner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Returns a new [`HttpDownloaderBuilder`].
    pub fn builder() -> HttpDownloaderBuilder {
        HttpDownloaderBuilder::new()
    }

    /// Returns the configured [`TransferOptions`].
    pub fn transfer_options(&self) -> TransferOptions {
        self.inner.transfer_options
    }

    /// Returns a cloned downloader augmented with an additional [`HttpAuthorizer`].
    pub fn with_added_authorizer(&self, authorizer: Arc<dyn HttpAuthorizer>) -> Self {
        let mut authorizers = self.inner.authorizers.clone();
        authorizers.push(authorizer);
        Self::from_inner(HttpDownloaderInner {
            client: self.inner.client.clone(),
            default_headers: self.inner.default_headers.clone(),
            sensitive_headers: self.inner.sensitive_headers.clone(),
            timeout: self.inner.timeout,
            proxy: self.inner.proxy.clone(),
            transfer_options: self.inner.transfer_options,
            client_configurator: self.inner.client_configurator.clone(),
            authorizers,
            middlewares: self.inner.middlewares.clone(),
        })
    }

    /// Returns a cloned downloader with updated [`TransferOptions`].
    pub fn with_transfer_options(&self, options: TransferOptions) -> Self {
        let options = TransferOptions {
            max_ranges_per_request: options.max_ranges_per_request.max(1),
            use_multiple_range_request: options.use_multiple_range_request,
            max_concurrent_ranges: options.max_concurrent_ranges.clamp(1, 16),
        };
        Self::from_inner(HttpDownloaderInner {
            client: self.inner.client.clone(),
            default_headers: self.inner.default_headers.clone(),
            sensitive_headers: self.inner.sensitive_headers.clone(),
            timeout: self.inner.timeout,
            proxy: self.inner.proxy.clone(),
            transfer_options: options,
            client_configurator: self.inner.client_configurator.clone(),
            authorizers: self.inner.authorizers.clone(),
            middlewares: self.inner.middlewares.clone(),
        })
    }

    /// Detects hosts known to support single HTTP Range requests while ignoring/rejecting multi-range.
    pub fn is_known_single_range_host(url: &Url) -> bool {
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

    /// Executes an HTTP request through the middleware -> manual redirect -> per-hop authorizer pipeline.
    pub async fn send(
        &self,
        req: HttpRequest,
        stage: RequestStage,
        overrides: &TransferOverrides,
    ) -> Result<HttpResponse> {
        let original_url = req.url.clone();
        self.send_with_origin(req, &original_url, stage, overrides)
            .await
    }

    pub(crate) async fn send_with_origin(
        &self,
        mut req: HttpRequest,
        original_url: &Url,
        stage: RequestStage,
        overrides: &TransferOverrides,
    ) -> Result<HttpResponse> {
        let mut base_headers = self.inner.default_headers.clone();
        for (key, value) in &overrides.metadata {
            let name = HeaderName::from_str(key).map_err(|error| {
                Error::InvalidHeader(format!("invalid header name '{key}': {error}"))
            })?;
            let val = HeaderValue::from_str(value).map_err(|error| {
                Error::InvalidHeader(format!("invalid header value for '{key}': {error}"))
            })?;
            base_headers.insert(name, val);
        }
        for (name, value) in req.headers.drain() {
            if let Some(name) = name {
                base_headers.insert(name, value);
            }
        }
        req.headers = base_headers;

        let client = self.resolve_client(overrides)?;
        let original_url_owned = original_url.clone();
        let timeout = overrides.timeout;
        let inner = Arc::clone(&self.inner);

        let terminal = move |logical_req: HttpRequest,
                             logical_stage: RequestStage|
              -> BoxFuture<'_, Result<HttpResponse>> {
            let client = client.clone();
            let original_url = original_url_owned.clone();
            let inner = Arc::clone(&inner);
            Box::pin(async move {
                execute_with_manual_redirects(
                    &client,
                    &inner,
                    RedirectRequest {
                        initial_req: logical_req,
                        original_url,
                        stage: logical_stage,
                        timeout,
                    },
                )
                .await
            })
        };

        let next = HttpNext {
            middlewares: &self.inner.middlewares,
            terminal: &terminal,
        };
        next.run(req, stage).await
    }

    /// Fetches a bounded UTF-8 text payload (e.g. release manifest), returning `Ok(None)` on `204`.
    pub async fn fetch_text_bounded(
        &self,
        url: &Url,
        stage: RequestStage,
        max_bytes: usize,
        extra_headers: Option<&HeaderMap>,
        overrides: &TransferOverrides,
    ) -> Result<Option<String>> {
        let mut req = HttpRequest::get(url.clone());
        if let Some(headers) = extra_headers {
            for (k, v) in headers {
                req.headers.insert(k, v.clone());
            }
        }
        let resp = self.send(req, stage, overrides).await?;
        let status = resp.status();
        if status == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(Error::HttpStatus {
                url: url.to_string(),
                status: status.as_u16(),
            });
        }
        let bytes = read_bounded_body(resp.response, max_bytes, "manifest").await?;
        let text = String::from_utf8(bytes)
            .map_err(|err| Error::InvalidManifest(format!("manifest is not valid UTF-8: {err}")))?;
        Ok(Some(text))
    }

    /// Fetches a bounded binary payload (e.g. `.blockmap`).
    pub async fn fetch_bytes_bounded(
        &self,
        url: &Url,
        stage: RequestStage,
        max_bytes: usize,
        label: &str,
        overrides: &TransferOverrides,
    ) -> Result<Bytes> {
        let resp = self
            .send(HttpRequest::get(url.clone()), stage, overrides)
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::HttpStatus {
                url: url.to_string(),
                status: status.as_u16(),
            });
        }
        let bytes = read_bounded_body(resp.response, max_bytes, label).await?;
        Ok(Bytes::from(bytes))
    }

    /// Opens a RAII [`HttpDownloadSession`] bound to `url`.
    pub fn open_download_session(
        &self,
        url: Url,
        overrides: &TransferOverrides,
    ) -> Box<dyn DownloadSession> {
        Box::new(HttpDownloadSession::new(
            self.clone(),
            url,
            overrides.clone(),
        ))
    }

    fn resolve_client(&self, overrides: &TransferOverrides) -> Result<reqwest::Client> {
        if overrides.proxy.is_none() {
            return Ok(self.inner.client.clone());
        }
        build_reqwest_client(
            overrides.timeout.or(self.inner.timeout),
            overrides.proxy.as_ref().or(self.inner.proxy.as_ref()),
            self.inner.client_configurator.as_ref(),
        )
    }
}
