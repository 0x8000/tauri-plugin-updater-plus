use std::sync::Arc;

use futures_util::StreamExt;
#[cfg(feature = "http")]
use http::HeaderMap;
#[cfg(feature = "http")]
use url::Url;

use crate::blockmap::BlockMap;
use crate::cache::PackageCache;
use crate::listener::{DownloadEvent, DownloadListener};
#[cfg(feature = "http")]
use crate::provider::{ArtifactId, TransferOverrides};
use crate::provider::{ByteStream, DownloadSession, UpdateProvider};
use crate::session::{DownloadedPackage, FullSession};
use crate::{Error, Result};

mod plan;
mod range_loop;
mod request;
#[cfg(all(test, feature = "http"))]
mod tests;
mod transfer;

use plan::{DifferentialContext, PlanResolution, prepare_differential};
use range_loop::{FallbackDetail, RangeLoopOutcome, fetch_ranges_into_session};
#[cfg(feature = "http")]
pub use request::DownloadRequest;
pub use request::ProviderDownloadRequest;
pub use transfer::TransferOptions;

/// Transport-agnostic blockmap differential downloader with transparent full-download fallback.
pub struct DifferentialDownloader {
    provider: Arc<dyn UpdateProvider>,
    cache: PackageCache,
}

impl DifferentialDownloader {
    /// Constructs a downloader backed by any [`UpdateProvider`] (HTTP, gRPC, MQTT, custom) and `cache`.
    pub fn with_provider(provider: Arc<dyn UpdateProvider>, cache: PackageCache) -> Self {
        Self { provider, cache }
    }

    /// Constructs an HTTP downloader backed by `headers`, `cache`, and `options`.
    #[cfg(feature = "http")]
    pub fn new(
        _client: reqwest::Client,
        headers: HeaderMap,
        cache: PackageCache,
        options: TransferOptions,
    ) -> Self {
        let downloader = crate::provider::http::HttpDownloader::builder()
            .default_headers(headers)
            .transfer_options(options)
            .build()
            .expect("valid default HttpDownloader");
        let placeholder = Url::parse("http://localhost/").expect("valid placeholder url");
        let provider =
            crate::provider::http::StaticHttpProvider::new(downloader, vec![placeholder])
                .expect("valid StaticHttpProvider");
        Self::with_provider(Arc::new(provider), cache)
    }

    /// Downloads, reconstructs, verifies, and caches a package using the configured [`UpdateProvider`].
    pub async fn download_with_provider<L: DownloadListener>(
        &self,
        request: &ProviderDownloadRequest,
        listener: &L,
    ) -> Result<DownloadedPackage> {
        std::fs::create_dir_all(self.cache.directory())?;
        let session = self
            .provider
            .open_session(&request.artifact, &request.overrides)
            .await?;

        let (fallback_reason, remote_blockmap) = if request.differential {
            match prepare_differential(
                self.provider.as_ref(),
                &self.cache,
                request,
                session.as_ref(),
            )
            .await?
            {
                PlanResolution::Ready(ctx) => {
                    let remote_blockmap = Some(ctx.blockmap.clone());
                    match self
                        .execute_differential(session.as_ref(), *ctx, listener)
                        .await?
                    {
                        DifferentialOutcome::Completed(package) => return Ok(package),
                        DifferentialOutcome::Fallback(fallback) => {
                            tracing::info!(
                                reason = %fallback.reason,
                                "falling back to full package download"
                            );
                            let reason = Some(fallback.reason);
                            if let Some(byte_stream) = fallback.full_stream {
                                return self
                                    .finish_full_stream(
                                        request,
                                        FullStreamContext {
                                            byte_stream,
                                            remote_blockmap,
                                            request_count: session.stats().requests.max(1),
                                            fallback_reason: reason,
                                        },
                                        listener,
                                    )
                                    .await;
                            }
                            (reason, remote_blockmap)
                        }
                    }
                }
                PlanResolution::Fallback { reason, blockmap } => {
                    tracing::info!(reason = %reason, "skipping differential download");
                    (Some(reason), blockmap)
                }
            }
        } else {
            (Some("differential updates disabled".into()), None)
        };

        let byte_stream = session.fetch_full().await?;
        self.finish_full_stream(
            request,
            FullStreamContext {
                byte_stream,
                remote_blockmap,
                request_count: session.stats().requests.max(1),
                fallback_reason,
            },
            listener,
        )
        .await
    }

    /// Convenience method accepting an HTTP [`DownloadRequest`].
    #[cfg(feature = "http")]
    pub async fn download<L: DownloadListener>(
        &self,
        request: &DownloadRequest,
        listener: &L,
    ) -> Result<DownloadedPackage> {
        let provider_req = ProviderDownloadRequest {
            version: request.version.clone(),
            artifact: ArtifactId::new(request.artifact_url.as_str()),
            blockmap: Some(ArtifactId::new(request.blockmap_url.as_str())),
            expected_size: request.expected_size,
            verification: request.verification.clone(),
            differential: request.differential,
            full_download_threshold: request.full_download_threshold,
            max_range_gap: None,
            overrides: TransferOverrides::default(),
        };
        self.download_with_provider(&provider_req, listener).await
    }

    async fn execute_differential<L: DownloadListener>(
        &self,
        session: &dyn DownloadSession,
        mut ctx: DifferentialContext,
        listener: &L,
    ) -> Result<DifferentialOutcome> {
        listener.on_event(DownloadEvent::Started {
            content_length: Some(ctx.expected_download),
            total_size: Some(ctx.total_size),
            differential: true,
            reused_bytes: ctx.reused_bytes,
        });

        let fetch_outcome = fetch_ranges_into_session(
            session,
            &ctx.missing_ranges,
            &mut ctx.diff_session,
            listener,
        )
        .await?;

        if let RangeLoopOutcome::Fallback(detail) = fetch_outcome {
            return Ok(DifferentialOutcome::Fallback(detail));
        }

        let requests = session.stats().requests;
        let diff_session = ctx.diff_session;
        let finished =
            tokio::task::spawn_blocking(move || diff_session.finish_with_stats(requests)).await;

        match finished {
            Ok(Ok(package)) => {
                listener.on_event(DownloadEvent::Finished {
                    summary: package.summary.clone(),
                });
                Ok(DifferentialOutcome::Completed(package))
            }
            Ok(Err(error)) => Ok(DifferentialOutcome::Fallback(FallbackDetail::reason(
                format!("reconstructed package verification failed: {error}"),
            ))),
            Err(error) => Ok(DifferentialOutcome::Fallback(FallbackDetail::reason(
                format!("differential verify task failed: {error}"),
            ))),
        }
    }

    async fn finish_full_stream<L: DownloadListener>(
        &self,
        request: &ProviderDownloadRequest,
        mut ctx: FullStreamContext,
        listener: &L,
    ) -> Result<DownloadedPackage> {
        let content_length = ctx.byte_stream.content_length.or(request.expected_size);
        listener.on_event(DownloadEvent::Started {
            content_length,
            total_size: content_length,
            differential: false,
            reused_bytes: 0,
        });

        let mut full_session = FullSession::open_with_expectation(
            self.cache.clone(),
            request.version.clone(),
            request.verification.clone(),
        )?;

        while let Some(chunk) = ctx.byte_stream.stream.next().await {
            let chunk = chunk?;
            full_session.write_chunk(&chunk)?;
            listener.on_event(DownloadEvent::Progress {
                chunk_length: chunk.len(),
            });
        }

        let package = tokio::task::spawn_blocking(move || {
            full_session.finish_with_stats(
                ctx.remote_blockmap.as_ref(),
                ctx.request_count,
                ctx.fallback_reason,
            )
        })
        .await
        .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))??;

        listener.on_event(DownloadEvent::Finished {
            summary: package.summary.clone(),
        });
        Ok(package)
    }
}

struct FullStreamContext {
    byte_stream: ByteStream,
    remote_blockmap: Option<BlockMap>,
    request_count: usize,
    fallback_reason: Option<String>,
}

enum DifferentialOutcome {
    Completed(DownloadedPackage),
    Fallback(FallbackDetail),
}
