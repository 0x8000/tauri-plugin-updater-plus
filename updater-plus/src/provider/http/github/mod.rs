use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use http::HeaderValue;
use http::header::ACCEPT;
use url::Url;

use super::downloader::HttpDownloader;
use super::middleware::RequestStage;
use crate::downloader::TransferOptions;
use crate::manifest::ReleaseManifest;
use crate::provider::{
    ArtifactId, BoxFuture, DownloadSession, ResolveContext, ResolvedRelease, TransferOverrides,
    UpdateProvider,
};
use crate::{Error, Result};

mod auth;

use auth::GitHubAuthorizer;

const MAX_GITHUB_JSON_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_API_BASE: &str = "https://api.github.com/";
const DEFAULT_WEB_BASE: &str = "https://github.com/";

/// Builder for [`GitHubProvider`].
#[derive(Debug, Clone)]
pub struct GitHubProviderBuilder {
    owner: String,
    repo: String,
    manifest_name: String,
    token: Option<String>,
    include_prerelease: bool,
    api_base_url: Url,
    web_base_url: Url,
    downloader: Option<HttpDownloader>,
}

impl GitHubProviderBuilder {
    /// Creates a GitHub release provider builder for `{owner}/{repo}`.
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            manifest_name: "latest.json".into(),
            token: None,
            include_prerelease: false,
            api_base_url: Url::parse(DEFAULT_API_BASE).expect("valid default GitHub API URL"),
            web_base_url: Url::parse(DEFAULT_WEB_BASE).expect("valid default GitHub web URL"),
            downloader: None,
        }
    }

    /// Sets the release manifest asset name (e.g. `"latest.json"` or `"latest-linux.yml"`).
    pub fn manifest_name(mut self, name: impl Into<String>) -> Self {
        self.manifest_name = name.into();
        self
    }

    /// Configures a GitHub bearer token (enables private repository API asset downloads).
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Allows selecting prerelease entries when querying the GitHub Releases API.
    pub fn include_prerelease(mut self, include: bool) -> Self {
        self.include_prerelease = include;
        self
    }

    /// Overrides the GitHub API base URL (for GitHub Enterprise or local/BDD testing).
    pub fn api_base_url(mut self, url: Url) -> Self {
        self.api_base_url = ensure_trailing_slash(url);
        self
    }

    /// Overrides the GitHub web base URL (used for unauthenticated public direct downloads).
    pub fn web_base_url(mut self, url: Url) -> Self {
        self.web_base_url = ensure_trailing_slash(url);
        self
    }

    /// Reuses a pre-configured [`HttpDownloader`] (preserving custom middleware, timeouts, or TLS).
    pub fn downloader(mut self, downloader: HttpDownloader) -> Self {
        self.downloader = Some(downloader);
        self
    }

    /// Builds a [`GitHubProvider`].
    pub fn build(self) -> Result<GitHubProvider> {
        if self.owner.trim().is_empty() || self.repo.trim().is_empty() {
            return Err(Error::InvalidConfig(
                "GitHubProvider requires non-empty owner and repo".into(),
            ));
        }
        let base_downloader = match self.downloader {
            Some(dl) => dl,
            None => HttpDownloader::builder().build()?,
        };
        let single_opts = TransferOptions {
            use_multiple_range_request: Some(false),
            ..base_downloader.transfer_options()
        };
        let use_api = self.token.is_some()
            || self.include_prerelease
            || self.api_base_url.as_str() != DEFAULT_API_BASE;
        let downloader = if use_api {
            base_downloader
                .with_transfer_options(single_opts)
                .with_added_authorizer(Arc::new(GitHubAuthorizer {
                    token: self.token.clone(),
                }))
        } else {
            base_downloader.with_transfer_options(single_opts)
        };

        Ok(GitHubProvider {
            downloader,
            owner: self.owner,
            repo: self.repo,
            manifest_name: self.manifest_name,
            include_prerelease: self.include_prerelease,
            use_api,
            api_base_url: self.api_base_url,
            web_base_url: self.web_base_url,
        })
    }
}

/// GitHub Releases provider composed on top of [`HttpDownloader`].
///
/// - Public repositories without prereleases download directly from
///   `{web_base}/{owner}/{repo}/releases/latest/download/{manifest}` to avoid GitHub's 60 req/hr
///   anonymous API rate limit.
/// - Private repositories (with a token) or prerelease channels query the Releases API, fetch the
///   manifest asset with `Accept: application/octet-stream`, remap artifact/blockmap filenames to
///   their private API asset URLs, and strip GitHub credentials on cross-origin 302 CDN redirects.
#[derive(Debug, Clone)]
pub struct GitHubProvider {
    downloader: HttpDownloader,
    owner: String,
    repo: String,
    manifest_name: String,
    include_prerelease: bool,
    use_api: bool,
    api_base_url: Url,
    web_base_url: Url,
}

impl GitHubProvider {
    /// Returns a [`GitHubProviderBuilder`] for `{owner}/{repo}`.
    pub fn builder(owner: impl Into<String>, repo: impl Into<String>) -> GitHubProviderBuilder {
        GitHubProviderBuilder::new(owner, repo)
    }

    async fn resolve_public_direct(&self, ctx: &ResolveContext) -> Result<Option<ResolvedRelease>> {
        let url = self.web_base_url.join(&format!(
            "{}/{}/releases/latest/download/{}",
            self.owner, self.repo, self.manifest_name
        ))?;
        let Some(body) = self
            .downloader
            .fetch_text_bounded(
                &url,
                RequestStage::Manifest,
                MAX_GITHUB_JSON_BYTES,
                None,
                &ctx.overrides,
            )
            .await?
        else {
            return Ok(None);
        };
        parse_release_from_manifest(&body, &url, None, ctx)
    }

    async fn resolve_via_api(&self, ctx: &ResolveContext) -> Result<Option<ResolvedRelease>> {
        let api_path = if self.include_prerelease {
            format!("repos/{}/{}/releases", self.owner, self.repo)
        } else {
            format!("repos/{}/{}/releases/latest", self.owner, self.repo)
        };
        let release_url = self.api_base_url.join(&api_path)?;
        let Some(release_body) = self
            .downloader
            .fetch_text_bounded(
                &release_url,
                RequestStage::Manifest,
                MAX_GITHUB_JSON_BYTES,
                None,
                &ctx.overrides,
            )
            .await?
        else {
            return Ok(None);
        };

        let release_json: serde_json::Value = serde_json::from_str(&release_body)?;
        let selected_release = if self.include_prerelease {
            let Some(list) = release_json.as_array() else {
                return Err(Error::InvalidManifest(
                    "GitHub releases API response was not a JSON array".into(),
                ));
            };
            let Some(found) = list.iter().find(|entry| {
                !entry
                    .get("draft")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            }) else {
                return Ok(None);
            };
            found
        } else {
            &release_json
        };

        let assets = selected_release
            .get("assets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                Error::InvalidManifest("GitHub release JSON missing `assets` array".into())
            })?;

        let mut asset_urls: HashMap<String, Url> = HashMap::new();
        for asset in assets {
            if let (Some(name), Some(raw_url)) = (
                asset.get("name").and_then(|v| v.as_str()),
                asset.get("url").and_then(|v| v.as_str()),
            ) && let Ok(parsed) = Url::parse(raw_url)
            {
                asset_urls.insert(name.to_string(), parsed);
            }
        }

        let manifest_asset_url = asset_urls.get(&self.manifest_name).ok_or_else(|| {
            Error::InvalidManifest(format!(
                "GitHub release missing manifest asset '{}'",
                self.manifest_name
            ))
        })?;

        let mut octet_headers = http::HeaderMap::new();
        octet_headers.insert(ACCEPT, HeaderValue::from_static("application/octet-stream"));
        let Some(manifest_body) = self
            .downloader
            .fetch_text_bounded(
                manifest_asset_url,
                RequestStage::Manifest,
                MAX_GITHUB_JSON_BYTES,
                Some(&octet_headers),
                &ctx.overrides,
            )
            .await?
        else {
            return Ok(None);
        };

        parse_release_from_manifest(&manifest_body, manifest_asset_url, Some(&asset_urls), ctx)
    }
}

impl UpdateProvider for GitHubProvider {
    fn resolve_release<'a>(
        &'a self,
        ctx: &'a ResolveContext,
    ) -> BoxFuture<'a, Result<Option<ResolvedRelease>>> {
        Box::pin(async move {
            if self.use_api {
                self.resolve_via_api(ctx).await
            } else {
                self.resolve_public_direct(ctx).await
            }
        })
    }

    fn fetch_blockmap<'a>(
        &'a self,
        id: &'a ArtifactId,
        max_bytes: usize,
        overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Bytes>> {
        Box::pin(async move {
            let url = Url::parse(id.as_str())?;
            self.downloader
                .fetch_bytes_bounded(
                    &url,
                    RequestStage::Blockmap,
                    max_bytes,
                    "blockmap",
                    overrides,
                )
                .await
        })
    }

    fn open_session<'a>(
        &'a self,
        id: &'a ArtifactId,
        overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Box<dyn DownloadSession>>> {
        Box::pin(async move {
            let url = Url::parse(id.as_str())?;
            Ok(self.downloader.open_download_session(url, overrides))
        })
    }
}

fn parse_release_from_manifest(
    body: &str,
    source_url: &Url,
    asset_map: Option<&HashMap<String, Url>>,
    ctx: &ResolveContext,
) -> Result<Option<ResolvedRelease>> {
    let manifest = ReleaseManifest::parse(body, source_url)?;
    let platform = manifest.resolve_platform(&ctx.target_candidates)?.clone();
    if ctx.require_checksum
        && platform
            .sha512
            .as_deref()
            .is_none_or(|digest| digest.trim().is_empty())
    {
        return Err(Error::InvalidManifest(
            "manifest platform must provide non-empty `sha512` when `pubkey` is not configured"
                .into(),
        ));
    }

    let artifact_url = remap_asset_url(&platform.url, asset_map);
    let blockmap_url = remap_asset_url(&platform.resolved_blockmap_url()?, asset_map);
    Ok(Some(ResolvedRelease {
        version: manifest.version,
        notes: manifest.notes,
        pub_date: manifest.pub_date,
        artifact: ArtifactId::new(artifact_url.as_str()),
        blockmap: Some(ArtifactId::new(blockmap_url.as_str())),
        sha512: platform.sha512,
        signature: platform.signature,
        size: platform.size,
        raw_json: manifest.raw_json,
    }))
}

fn remap_asset_url(url: &Url, asset_map: Option<&HashMap<String, Url>>) -> Url {
    let Some(map) = asset_map else {
        return url.clone();
    };
    let Some(filename) = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())
    else {
        return url.clone();
    };
    map.get(filename).cloned().unwrap_or_else(|| url.clone())
}

fn ensure_trailing_slash(mut url: Url) -> Url {
    if !url.path().ends_with('/') {
        let next = format!("{}/", url.path());
        url.set_path(&next);
    }
    url
}
