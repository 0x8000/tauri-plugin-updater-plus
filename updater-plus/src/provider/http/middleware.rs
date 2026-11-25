use std::fmt;
use std::sync::Arc;

use http::{HeaderMap, Method, StatusCode};
use url::Url;

use crate::Result;
use crate::provider::BoxFuture;

/// Logical update stage associated with an outbound HTTP request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStage {
    /// Fetching a release manifest (`latest.json`, `latest*.yml`, or GitHub Releases API JSON).
    Manifest,
    /// Fetching a `.blockmap` metadata file.
    Blockmap,
    /// Fetching full or partial bytes of the update artifact itself.
    Artifact,
}

/// Per-hop request metadata supplied to [`HttpAuthorizer::authorize`].
#[derive(Debug, Clone)]
pub struct HttpRequestContext {
    /// Logical update stage for this request.
    pub stage: RequestStage,
    /// Initial URL before any HTTP 3xx redirects.
    pub original_url: Url,
    /// Target URL for this specific HTTP hop.
    pub current_url: Url,
    /// True when `current_url` has a different scheme/host/port origin than `original_url`.
    pub is_cross_origin_redirect: bool,
    /// Zero-based redirect hop index (`0` for the initial request).
    pub hop: usize,
}

/// Mutable HTTP request passed through [`HttpMiddleware`] and [`HttpAuthorizer`].
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// HTTP method (typically `GET`).
    pub method: Method,
    /// Target URL for this request.
    pub url: Url,
    /// Request headers attached to this request.
    pub headers: HeaderMap,
}

impl HttpRequest {
    /// Constructs a `GET` request for `url` with empty headers.
    pub fn get(url: Url) -> Self {
        Self {
            method: Method::GET,
            url,
            headers: HeaderMap::new(),
        }
    }
}

/// Logical HTTP response returned after manual redirect resolution.
#[derive(Debug)]
pub struct HttpResponse {
    /// Final resolved URL after following any HTTP 3xx redirects.
    pub final_url: Url,
    /// Number of redirect hops followed (`0` if no redirect occurred).
    pub redirect_hops: usize,
    /// Underlying `reqwest::Response` positioned at the final response body.
    pub response: reqwest::Response,
}

impl HttpResponse {
    /// Returns the HTTP status code of the final response.
    pub fn status(&self) -> StatusCode {
        self.response.status()
    }

    /// Returns the response headers of the final response.
    pub fn headers(&self) -> &HeaderMap {
        self.response.headers()
    }
}

/// Per-hop asynchronous request authorizer hook for dynamic headers, OAuth2, GitHub, or SigV4.
///
/// Invoked on every hop inside `HttpDownloader`'s manual redirect loop *after* cross-origin
/// sensitive headers have been stripped, so implementations can inspect
/// [`HttpRequestContext::is_cross_origin_redirect`] and [`HttpRequestContext::stage`] to decide
/// whether to attach credentials or re-sign for the target hop.
pub trait HttpAuthorizer: Send + Sync + fmt::Debug {
    /// Mutates `req` (typically adding authentication or signature headers) for the current hop.
    fn authorize<'a>(
        &'a self,
        ctx: &'a HttpRequestContext,
        req: &'a mut HttpRequest,
    ) -> BoxFuture<'a, Result<()>>;
}

/// Closure adapter turning a synchronous signing/header function into an [`HttpAuthorizer`].
pub struct FnAuthorizer<F> {
    func: F,
}

impl<F> FnAuthorizer<F>
where
    F: Fn(&HttpRequestContext, &mut HttpRequest) -> Result<()> + Send + Sync + 'static,
{
    /// Wraps `func` as an [`HttpAuthorizer`].
    pub fn new(func: F) -> Self {
        Self { func }
    }
}

impl<F> fmt::Debug for FnAuthorizer<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FnAuthorizer(..)")
    }
}

impl<F> HttpAuthorizer for FnAuthorizer<F>
where
    F: Fn(&HttpRequestContext, &mut HttpRequest) -> Result<()> + Send + Sync + 'static,
{
    fn authorize<'a>(
        &'a self,
        ctx: &'a HttpRequestContext,
        req: &'a mut HttpRequest,
    ) -> BoxFuture<'a, Result<()>> {
        let result = (self.func)(ctx, req);
        Box::pin(async move { result })
    }
}

/// Continuation passed to [`HttpMiddleware::handle`] to invoke the next middleware or HTTP core.
///
/// Implements [`Clone`] and [`Copy`] so retry/failover middlewares can invoke downstream hops
/// multiple times with a cloned [`HttpRequest`].
#[derive(Clone, Copy)]
pub struct HttpNext<'a> {
    pub(crate) middlewares: &'a [Arc<dyn HttpMiddleware>],
    pub(crate) terminal: &'a (
            dyn Fn(HttpRequest, RequestStage) -> BoxFuture<'a, Result<HttpResponse>> + Send + Sync
        ),
}

impl<'a> HttpNext<'a> {
    /// Executes the next layer in the HTTP middleware pipeline.
    pub fn run(self, req: HttpRequest, stage: RequestStage) -> BoxFuture<'a, Result<HttpResponse>> {
        if let Some((first, rest)) = self.middlewares.split_first() {
            let next = HttpNext {
                middlewares: rest,
                terminal: self.terminal,
            };
            first.handle(req, stage, next)
        } else {
            (self.terminal)(req, stage)
        }
    }
}

/// Outer HTTP middleware wrapping logical request execution (outside redirect + signing loops).
///
/// Use this to add transport-level retries, exponential backoff, circuit breakers, rate limits,
/// or observability without rewriting `HttpDownloader`'s range or redirect machinery.
pub trait HttpMiddleware: Send + Sync + fmt::Debug {
    /// Intercepts an outbound logical HTTP request and delegates to `next`.
    fn handle<'a>(
        &'a self,
        req: HttpRequest,
        stage: RequestStage,
        next: HttpNext<'a>,
    ) -> BoxFuture<'a, Result<HttpResponse>>;
}
