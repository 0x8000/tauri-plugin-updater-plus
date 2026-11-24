use http::HeaderValue;
use http::header::{ACCEPT, AUTHORIZATION, HeaderName};

use super::super::middleware::{HttpAuthorizer, HttpRequest, HttpRequestContext, RequestStage};
use crate::provider::BoxFuture;
use crate::{Error, Result};

#[derive(Debug)]
pub(super) struct GitHubAuthorizer {
    pub(super) token: Option<String>,
}

impl HttpAuthorizer for GitHubAuthorizer {
    fn authorize<'a>(
        &'a self,
        ctx: &'a HttpRequestContext,
        req: &'a mut HttpRequest,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Never leak GitHub API tokens or API-only Accept headers onto cross-origin CDN hops
            // (such as `objects.githubusercontent.com`).
            if ctx.is_cross_origin_redirect {
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
