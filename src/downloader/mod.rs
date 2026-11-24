use std::io::Write;
use std::path::{Path, PathBuf};

use reqwest::header::HeaderMap;
use url::Url;

use crate::Result;
use crate::blockmap::BlockMap;
use crate::cache::{CachedPackageState, LocalBasePackage, PackageCache};
use crate::chunker::ChunkerConfig;
use crate::listener::{DownloadEvent, DownloadListener, DownloadMode, DownloadSummary};
use crate::planner::{DownloadPlan, PlanOptions};
use crate::verifier::{ArtifactVerifier, VerifyExpectation};

mod range_session;
mod reconstruct;
#[cfg(test)]
mod tests;
mod transfer;

use range_session::{FallbackDetail, RangeFetchOutcome, RangeSession};
use reconstruct::PackageReconstructor;
pub use transfer::*;

/// Inputs describing a package download and verification contract.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    /// Target version being downloaded.
    pub version: String,
    /// Full artifact download URL.
    pub artifact_url: Url,
    /// Remote `.blockmap` URL used when differential mode is active.
    pub blockmap_url: Url,
    /// Optional expected artifact byte length from the manifest.
    pub expected_size: Option<u64>,
    /// Integrity and signature verification requirements.
    pub verification: VerifyExpectation,
    /// Whether differential downloading should be attempted before falling back to full.
    pub differential: bool,
    /// Maximum changed ratio (0.0..=1.0) allowed before switching to a full download.
    pub full_download_threshold: f64,
}

/// Verified package stored on disk along with transfer metrics.
#[derive(Debug, Clone)]
pub struct DownloadedPackage {
    /// Path to the verified package committed inside the updater cache.
    pub path: PathBuf,
    /// Transfer statistics and mode used for this download.
    pub summary: DownloadSummary,
}

/// Executes Electron-style blockmap differential downloads with transparent full-download fallback.
pub struct DifferentialDownloader {
    transfer: TransferExecutor,
    cache: PackageCache,
}

impl DifferentialDownloader {
    /// Constructs a downloader backed by `client`, default request `headers`, `cache`, and `options`.
    pub fn new(
        client: reqwest::Client,
        headers: HeaderMap,
        cache: PackageCache,
        options: TransferOptions,
    ) -> Self {
        Self {
            transfer: TransferExecutor::new(client, headers, options),
            cache,
        }
    }

    /// Downloads, reconstructs, verifies, and caches the requested update package.
    pub async fn download<L: DownloadListener>(
        &self,
        request: &DownloadRequest,
        listener: &L,
    ) -> Result<DownloadedPackage> {
        std::fs::create_dir_all(self.cache.directory())?;
        let (fallback_reason, remote_blockmap) = if request.differential {
            match self.prepare_plan(request).await {
                PlanResolution::Ready(ctx) => {
                    let remote_blockmap = Some(ctx.blockmap.clone());
                    match self.execute_differential(request, &ctx, listener).await {
                        DifferentialOutcome::Completed(package) => return Ok(package),
                        DifferentialOutcome::Fallback(fallback) => {
                            tracing::info!(
                                reason = %fallback.reason,
                                "falling back to full package download"
                            );
                            let reason = Some(fallback.reason);
                            if let Some(response) = fallback.full_response {
                                let full_ctx = FullDownloadContext {
                                    response,
                                    remote_blockmap,
                                    fallback_reason: reason,
                                };
                                return self
                                    .finish_full_response(request, full_ctx, listener)
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

        let response = self
            .transfer
            .open_full_response(&request.artifact_url)
            .await?;
        let ctx = FullDownloadContext {
            response,
            remote_blockmap,
            fallback_reason,
        };
        self.finish_full_response(request, ctx, listener).await
    }

    async fn prepare_plan(&self, request: &DownloadRequest) -> PlanResolution {
        let Some(base) = self.cache.load_base() else {
            return PlanResolution::Fallback {
                reason: "no cached base package available".into(),
                blockmap: None,
            };
        };

        let remote_blockmap = match self.transfer.fetch_blockmap(&request.blockmap_url).await {
            Ok(map) => map,
            Err(error) => {
                return PlanResolution::Fallback {
                    reason: format!("remote blockmap unavailable: {error}"),
                    blockmap: None,
                };
            }
        };

        if let Some(expected_size) = request.expected_size
            && expected_size != remote_blockmap.total_size()
        {
            return PlanResolution::Fallback {
                reason: "remote blockmap size disagrees with manifest size".into(),
                blockmap: None,
            };
        }

        let plan =
            match DownloadPlan::build(&base.blockmap, &remote_blockmap, PlanOptions::default()) {
                Ok(plan) => plan,
                Err(error) => {
                    return PlanResolution::Fallback {
                        reason: format!("differential plan failed: {error}"),
                        blockmap: Some(remote_blockmap),
                    };
                }
            };

        if plan.should_fallback_to_full(request.full_download_threshold) {
            return PlanResolution::Fallback {
                reason: format!(
                    "delta requires {:.1}% of full package",
                    (plan.download_bytes as f64 / plan.total_size.max(1) as f64) * 100.0
                ),
                blockmap: Some(remote_blockmap),
            };
        }

        PlanResolution::Ready(DifferentialContext {
            base,
            blockmap: remote_blockmap,
            plan,
        })
    }

    async fn execute_differential<L: DownloadListener>(
        &self,
        request: &DownloadRequest,
        ctx: &DifferentialContext,
        listener: &L,
    ) -> DifferentialOutcome {
        let temp_file = match tempfile::NamedTempFile::new_in(self.cache.directory()) {
            Ok(file) => file,
            Err(error) => {
                return DifferentialOutcome::fallback(format!(
                    "create staging file failed: {error}"
                ));
            }
        };
        let mut target = match temp_file.reopen() {
            Ok(file) => file,
            Err(error) => {
                return DifferentialOutcome::fallback(format!("open staging file failed: {error}"));
            }
        };
        if let Err(error) = target.set_len(ctx.plan.total_size) {
            return DifferentialOutcome::fallback(format!(
                "preallocate staging file failed: {error}"
            ));
        }

        let copy = match PackageReconstructor::apply_local_copies(
            &ctx.base.package_path,
            &mut target,
            &ctx.blockmap,
            &ctx.plan,
        ) {
            Ok(copy) => copy,
            Err(error) => {
                return DifferentialOutcome::fallback(format!("copy local blocks failed: {error}"));
            }
        };

        let expected_download: u64 = copy.missing_ranges.iter().map(|r| r.len()).sum();
        listener.on_event(DownloadEvent::Started {
            content_length: Some(expected_download),
            total_size: Some(ctx.plan.total_size),
            differential: true,
            reused_bytes: copy.reused_bytes,
        });

        let session = RangeSession::new(&self.transfer, &request.artifact_url);
        let fetch = match session
            .fetch_all(&copy.missing_ranges, &mut target, listener)
            .await
        {
            RangeFetchOutcome::Completed(stats) => stats,
            RangeFetchOutcome::Fallback(detail) => return DifferentialOutcome::Fallback(detail),
        };

        if let Err(error) = target.flush() {
            return DifferentialOutcome::fallback(format!(
                "flush reconstructed file failed: {error}"
            ));
        }
        drop(target);

        if let Err(error) = PackageReconstructor::verify_reconstructed(
            temp_file.path(),
            &ctx.blockmap,
            &request.verification,
        ) {
            return DifferentialOutcome::fallback(format!(
                "reconstructed package verification failed: {error}"
            ));
        }

        let summary = DownloadSummary::from_counts(
            DownloadMode::Differential,
            ctx.plan.total_size,
            fetch.downloaded_bytes,
            copy.reused_bytes,
        )
        .with_details(fetch.http_requests, None);

        match self.commit_verified(request, temp_file.path(), Some(&ctx.blockmap)) {
            Ok(path) => {
                listener.on_event(DownloadEvent::Finished {
                    summary: summary.clone(),
                });
                DifferentialOutcome::Completed(DownloadedPackage { path, summary })
            }
            Err(error) => DifferentialOutcome::fallback(format!("cache commit failed: {error}")),
        }
    }

    async fn finish_full_response<L: DownloadListener>(
        &self,
        request: &DownloadRequest,
        ctx: FullDownloadContext,
        listener: &L,
    ) -> Result<DownloadedPackage> {
        let content_length = ctx.response.content_length().or(request.expected_size);
        listener.on_event(DownloadEvent::Started {
            content_length,
            total_size: content_length,
            differential: false,
            reused_bytes: 0,
        });

        let temp_file = tempfile::NamedTempFile::new_in(self.cache.directory())?;
        let downloaded =
            TransferExecutor::write_full_response(ctx.response, temp_file.path(), listener).await?;

        ArtifactVerifier::verify_file(temp_file.path(), &request.verification)?;
        let path = self.commit_verified(request, temp_file.path(), ctx.remote_blockmap.as_ref())?;

        let summary = DownloadSummary::from_counts(DownloadMode::Full, downloaded, downloaded, 0)
            .with_details(1, ctx.fallback_reason);
        listener.on_event(DownloadEvent::Finished {
            summary: summary.clone(),
        });
        Ok(DownloadedPackage { path, summary })
    }

    fn commit_verified(
        &self,
        request: &DownloadRequest,
        verified_file: &Path,
        known_blockmap: Option<&BlockMap>,
    ) -> Result<PathBuf> {
        let actual_size = std::fs::metadata(verified_file)?.len();
        let (blockmap, sha512) = match known_blockmap {
            Some(map) if map.total_size() == actual_size => {
                (map.clone(), request.verification.sha512.clone())
            }
            _ => {
                let artifact =
                    BlockMap::generate_from_path(verified_file, ChunkerConfig::default())?;
                (artifact.blockmap, Some(artifact.sha512))
            }
        };
        let state = CachedPackageState {
            version: request.version.clone(),
            size: actual_size,
            sha512,
        };
        self.cache.commit(verified_file, &blockmap, &state)
    }
}

struct DifferentialContext {
    base: LocalBasePackage,
    blockmap: BlockMap,
    plan: DownloadPlan,
}

struct FullDownloadContext {
    response: reqwest::Response,
    remote_blockmap: Option<BlockMap>,
    fallback_reason: Option<String>,
}

enum PlanResolution {
    Ready(DifferentialContext),
    Fallback {
        reason: String,
        blockmap: Option<BlockMap>,
    },
}

enum DifferentialOutcome {
    Completed(DownloadedPackage),
    Fallback(FallbackDetail),
}

impl DifferentialOutcome {
    fn fallback(reason: String) -> Self {
        Self::Fallback(FallbackDetail::reason(reason))
    }
}
