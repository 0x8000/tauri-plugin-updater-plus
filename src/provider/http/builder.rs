use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use http::HeaderMap;
use http::header::{HeaderName, HeaderValue};
use url::Url;

use super::downloader::{ClientConfigurator, HttpDownloader, HttpDownloaderInner};
use super::middleware::{
    FnAuthorizer, HttpAuthorizer, HttpMiddleware, HttpRequest, HttpRequestContext,
};
use crate::downloader::TransferOptions;
use crate::{Error, Result};

/// Builder for configuring a reusable [`HttpDownloader`].
#[derive(Default)]
pub struct HttpDownloaderBuilder {
    default_headers: HeaderMap,
    sensitive_headers: Vec<HeaderName>,
    timeout: Option<Duration>,
    proxy: Option<Url>,
    transfer_options: TransferOptions,
    client_configurator: Option<ClientConfigurator>,
    authorizers: Vec<Arc<dyn HttpAuthorizer>>,
    middlewares: Vec<Arc<dyn HttpMiddleware>>,
}

impl fmt::Debug for HttpDownloaderBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpDownloaderBuilder")
            .field("timeout", &self.timeout)
            .field("proxy", &self.proxy)
            .field("transfer_options", &self.transfer_options)
            .field("authorizers_count", &self.authorizers.len())
            .field("middlewares_count", &self.middlewares.len())
            .finish()
    }
}

impl HttpDownloaderBuilder {
    /// Creates a new builder with default HTTP settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the default headers attached to outbound HTTP requests.
    pub fn default_headers(mut self, headers: HeaderMap) -> Self {
        self.default_headers = headers;
        self
    }

    /// Appends a default header attached to outbound HTTP requests.
    pub fn header(mut self, key: &str, value: &str) -> Result<Self> {
        let name = HeaderName::from_str(key).map_err(|error| {
            Error::InvalidHeader(format!("invalid header name '{key}': {error}"))
        })?;
        let val = HeaderValue::from_str(value).map_err(|error| {
            Error::InvalidHeader(format!("invalid header value for '{key}': {error}"))
        })?;
        self.default_headers.append(name, val);
        Ok(self)
    }

    /// Marks an additional header name as sensitive so it is always stripped on cross-origin redirects.
    pub fn sensitive_header(mut self, name: HeaderName) -> Self {
        self.sensitive_headers.push(name);
        self
    }

    /// Sets the default HTTP request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets an explicit HTTP proxy URL.
    pub fn proxy(mut self, proxy: Url) -> Self {
        self.proxy = Some(proxy);
        self
    }

    /// Configures HTTP range batching and concurrency defaults.
    pub fn transfer_options(mut self, options: TransferOptions) -> Self {
        self.transfer_options = options;
        self
    }

    /// Customizes the underlying `reqwest::ClientBuilder` (e.g. mTLS certificates, root CAs, DNS).
    ///
    /// Note: `HttpDownloader` always enforces `redirect::Policy::none()` after this hook runs so
    /// manual cross-origin credential stripping and per-hop authorization can never be bypassed.
    pub fn client_configurator<F>(mut self, func: F) -> Self
    where
        F: Fn(reqwest::ClientBuilder) -> reqwest::ClientBuilder + Send + Sync + 'static,
    {
        self.client_configurator = Some(Arc::new(func));
        self
    }

    /// Appends a per-hop [`HttpAuthorizer`] hook (runs on every redirect hop).
    pub fn authorizer(mut self, authorizer: Arc<dyn HttpAuthorizer>) -> Self {
        self.authorizers.push(authorizer);
        self
    }

    /// Appends a synchronous closure as a per-hop [`HttpAuthorizer`].
    pub fn with_authorizer<F>(self, func: F) -> Self
    where
        F: Fn(&HttpRequestContext, &mut HttpRequest) -> Result<()> + Send + Sync + 'static,
    {
        self.authorizer(Arc::new(FnAuthorizer::new(func)))
    }

    /// Appends an outer [`HttpMiddleware`] (e.g. retries, circuit breaker, telemetry).
    pub fn middleware(mut self, middleware: Arc<dyn HttpMiddleware>) -> Self {
        self.middlewares.push(middleware);
        self
    }

    /// Builds a validated [`HttpDownloader`].
    pub fn build(self) -> Result<HttpDownloader> {
        let client = build_reqwest_client(
            self.timeout,
            self.proxy.as_ref(),
            self.client_configurator.as_ref(),
        )?;
        let options = TransferOptions {
            max_ranges_per_request: self.transfer_options.max_ranges_per_request.max(1),
            use_multiple_range_request: self.transfer_options.use_multiple_range_request,
            max_concurrent_ranges: self.transfer_options.max_concurrent_ranges.clamp(1, 16),
        };
        Ok(HttpDownloader::from_inner(HttpDownloaderInner {
            client,
            default_headers: self.default_headers,
            sensitive_headers: self.sensitive_headers,
            timeout: self.timeout,
            proxy: self.proxy,
            transfer_options: options,
            client_configurator: self.client_configurator,
            authorizers: self.authorizers,
            middlewares: self.middlewares,
        }))
    }
}

pub(crate) fn build_reqwest_client(
    timeout: Option<Duration>,
    proxy: Option<&Url>,
    configurator: Option<&ClientConfigurator>,
) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().user_agent(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ));
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    if let Some(proxy_url) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy_url.as_str())?);
    }
    if let Some(func) = configurator {
        builder = func(builder);
    }
    builder = builder.redirect(reqwest::redirect::Policy::none());
    Ok(builder.build()?)
}
