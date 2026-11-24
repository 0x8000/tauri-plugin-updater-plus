use std::time::Duration;

use http::header::{AUTHORIZATION, COOKIE, HeaderName, LOCATION, PROXY_AUTHORIZATION};
use http::{HeaderMap, StatusCode};
use url::Url;

use super::downloader::HttpDownloaderInner;
use super::middleware::{HttpRequest, HttpRequestContext, HttpResponse, RequestStage};
use crate::{Error, Result};

const MAX_REDIRECT_HOPS: usize = 10;
const DEFAULT_SENSITIVE_HEADERS: [&str; 6] = [
    "authorization",
    "cookie",
    "proxy-authorization",
    "x-amz-security-token",
    "x-amz-date",
    "x-amz-content-sha256",
];

pub(crate) struct RedirectRequest {
    pub(crate) initial_req: HttpRequest,
    pub(crate) original_url: Url,
    pub(crate) stage: RequestStage,
    pub(crate) timeout: Option<Duration>,
}

pub(crate) async fn execute_with_manual_redirects(
    client: &reqwest::Client,
    inner: &HttpDownloaderInner,
    params: RedirectRequest,
) -> Result<HttpResponse> {
    let base_method = params.initial_req.method;
    let base_headers = params.initial_req.headers;
    let mut current_url = params.initial_req.url;
    let mut hops = 0usize;

    loop {
        let is_cross_origin = params.original_url.origin() != current_url.origin();
        let mut hop_headers = base_headers.clone();
        if is_cross_origin {
            strip_sensitive_headers(&mut hop_headers, &inner.sensitive_headers);
        }

        let mut hop_req = HttpRequest {
            method: base_method.clone(),
            url: current_url.clone(),
            headers: hop_headers,
        };
        let ctx = HttpRequestContext {
            stage: params.stage,
            original_url: params.original_url.clone(),
            current_url: current_url.clone(),
            is_cross_origin_redirect: is_cross_origin,
            hop: hops,
        };
        for authorizer in &inner.authorizers {
            authorizer.authorize(&ctx, &mut hop_req).await?;
        }

        let mut req_builder = client
            .request(hop_req.method, hop_req.url.clone())
            .headers(hop_req.headers);
        if let Some(timeout) = params.timeout {
            req_builder = req_builder.timeout(timeout);
        }
        let response = req_builder.send().await?;
        let status = response.status();

        if is_redirect_status(status)
            && let Some(location) = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
        {
            if hops >= MAX_REDIRECT_HOPS {
                return Err(Error::DifferentialTransfer(format!(
                    "exceeded maximum HTTP redirect hops ({MAX_REDIRECT_HOPS}) for {}",
                    params.original_url
                )));
            }
            current_url = current_url.join(location)?;
            hops += 1;
            continue;
        }

        return Ok(HttpResponse {
            final_url: current_url,
            redirect_hops: hops,
            response,
        });
    }
}

fn is_redirect_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

fn strip_sensitive_headers(headers: &mut HeaderMap, extra_sensitive: &[HeaderName]) {
    headers.remove(AUTHORIZATION);
    headers.remove(COOKIE);
    headers.remove(PROXY_AUTHORIZATION);
    for name in DEFAULT_SENSITIVE_HEADERS {
        headers.remove(name);
    }
    for name in extra_sensitive {
        headers.remove(name);
    }
}
