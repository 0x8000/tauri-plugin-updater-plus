use bytes::Bytes;
use url::Url;

use super::downloader::HttpDownloader;
use super::middleware::RequestStage;
use crate::manifest::ReleaseManifest;
use crate::provider::{
    ArtifactId, BoxFuture, DownloadSession, ResolveContext, ResolvedRelease, TransferOverrides,
    UpdateProvider,
};
use crate::updater::TargetResolver;
use crate::{Error, Result};

const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

/// Static HTTP release provider composed on top of [`HttpDownloader`].
///
/// Serves static hosts (Nginx / Caddy / Apache) as well as public or signed object stores
/// (S3 / R2 / OSS / GCS) publishing Tauri `latest.json` or Electron `latest*.yml` manifests.
#[derive(Debug, Clone)]
pub struct StaticHttpProvider {
    downloader: HttpDownloader,
    endpoints: Vec<Url>,
}

impl StaticHttpProvider {
    /// Creates a static HTTP provider backed by `downloader` and ordered manifest `endpoints`.
    pub fn new(downloader: HttpDownloader, endpoints: Vec<Url>) -> Result<Self> {
        if endpoints.is_empty() {
            return Err(Error::InvalidConfig(
                "at least one updater endpoint must be configured".into(),
            ));
        }
        Ok(Self {
            downloader,
            endpoints,
        })
    }

    /// Returns a reference to the underlying [`HttpDownloader`].
    pub fn downloader(&self) -> &HttpDownloader {
        &self.downloader
    }

    /// Returns the configured manifest endpoints.
    pub fn endpoints(&self) -> &[Url] {
        &self.endpoints
    }
}

impl UpdateProvider for StaticHttpProvider {
    fn resolve_release<'a>(
        &'a self,
        ctx: &'a ResolveContext,
    ) -> BoxFuture<'a, Result<Option<ResolvedRelease>>> {
        Box::pin(async move {
            let primary_target = ctx
                .target_candidates
                .first()
                .map(String::as_str)
                .unwrap_or("unknown");
            let current_ver_str = ctx.current_version.to_string();

            let mut last_error: Option<Error> = None;
            for endpoint in &self.endpoints {
                let attempt = async {
                    let expanded = TargetResolver::expand_endpoint_with_parts(
                        endpoint,
                        primary_target,
                        &ctx.arch,
                        ctx.bundle_type.as_deref().unwrap_or(""),
                        &current_ver_str,
                    )?;
                    let Some(body) = self
                        .downloader
                        .fetch_text_bounded(
                            &expanded,
                            RequestStage::Manifest,
                            MAX_MANIFEST_BYTES,
                            None,
                            &ctx.overrides,
                        )
                        .await?
                    else {
                        return Ok(None);
                    };

                    let manifest = ReleaseManifest::parse(&body, &expanded)?;
                    let platform = manifest.resolve_platform(&ctx.target_candidates)?.clone();
                    if ctx.require_checksum
                        && platform
                            .sha512
                            .as_deref()
                            .is_none_or(|digest| digest.trim().is_empty())
                    {
                        return Err(Error::InvalidManifest(
                            "manifest platform must provide non-empty `sha512` when `pubkey` is not configured".into(),
                        ));
                    }
                    let blockmap_url = platform.resolved_blockmap_url()?;
                    Ok::<_, Error>(Some(ResolvedRelease {
                        version: manifest.version,
                        notes: manifest.notes,
                        pub_date: manifest.pub_date,
                        artifact: ArtifactId::new(platform.url.as_str()),
                        blockmap: Some(ArtifactId::new(blockmap_url.as_str())),
                        sha512: platform.sha512,
                        signature: platform.signature,
                        size: platform.size,
                        raw_json: manifest.raw_json,
                    }))
                }
                .await;

                match attempt {
                    Ok(None) => return Ok(None),
                    Ok(Some(release)) => return Ok(Some(release)),
                    Err(error) => {
                        tracing::warn!(
                            endpoint = %endpoint,
                            error = %error,
                            "updater endpoint check failed"
                        );
                        last_error = Some(error);
                    }
                }
            }

            match last_error {
                Some(error) => Err(error),
                None => Ok(None),
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
