use std::fmt;

use http::HeaderValue;
use http::header::{ACCEPT, AUTHORIZATION, HeaderName};
use url::Origin;

use super::super::middleware::{HttpAuthorizer, HttpRequest, HttpRequestContext, RequestStage};
use crate::provider::BoxFuture;
use crate::{Error, Result};

pub(super) struct GitHubAuthorizer {
    pub(super) token: Option<String>,
    pub(super) api_origin: Origin,
}

impl fmt::Debug for GitHubAuthorizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubAuthorizer")
            .field("has_token", &self.token.is_some())
            .field("api_origin", &self.api_origin)
            .finish()
    }
}

impl HttpAuthorizer for GitHubAuthorizer {
    fn authorize<'a>(
        &'a self,
        ctx: &'a HttpRequestContext,
        req: &'a mut HttpRequest,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Never leak GitHub API tokens or API-only Accept headers onto cross-origin CDN hops
            // or third-party hosts that do not match the configured GitHub API origin.
            if ctx.is_cross_origin_redirect || ctx.current_url.origin() != self.api_origin {
                return Ok(());
            }
            if let Some(token) = &self.token {
                let val = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|err| {
                    Error::InvalidHeader(format!("invalid GitHub token header: {err}"))
                })?;
                req.headers.insert(AUTHORIZATION, val);
            }
            req.headers.insert(
                HeaderName::from_static("x-github-api-version"),
                HeaderValue::from_static("2022-11-28"),
            );
            match ctx.stage {
                RequestStage::Manifest if !req.headers.contains_key(ACCEPT) => {
                    req.headers.insert(
                        ACCEPT,
                        HeaderValue::from_static("application/vnd.github+json"),
                    );
                }
                RequestStage::Blockmap | RequestStage::Artifact => {
                    req.headers
                        .insert(ACCEPT, HeaderValue::from_static("application/octet-stream"));
                }
                _ => {}
            }
            Ok(())
        })
    }
}
