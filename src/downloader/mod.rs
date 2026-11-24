use std::io::Write;
use std::path::PathBuf;

use reqwest::header::HeaderMap;
use url::Url;

use crate::blockmap::BlockMap;
use crate::cache::{LocalBasePackage, PackageCache};
use crate::listener::{DownloadEvent, DownloadListener, DownloadMode, DownloadSummary};
use crate::planner::{DownloadPlan, PlanOptions};
use crate::verifier::{ArtifactVerifier, VerifyExpectation};
use crate::{Error, Result};

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
        let cache = self.cache.clone();
        let loaded_base = match tokio::task::spawn_blocking(move || cache.load_base()).await {
            Ok(base) => base,
            Err(error) => {
                return PlanResolution::Fallback {
                    reason: format!("load cached base package panicked: {error}"),
                    blockmap: None,
                };
            }
        };
        let Some(base) = loaded_base else {
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

        let base_path = ctx.base.package_path.clone();
        let map_clone = ctx.blockmap.clone();
        let plan_clone = ctx.plan.clone();
        let (mut target, copy) = match tokio::task::spawn_blocking(move || {
            let copy = PackageReconstructor::apply_local_copies(
                &base_path,
                &mut target,
                &map_clone,
                &plan_clone,
            )?;
            Ok::<_, Error>((target, copy))
        })
        .await
        {
            Ok(Ok(pair)) => pair,
            Ok(Err(error)) => {
                return DifferentialOutcome::fallback(format!("copy local blocks failed: {error}"));
            }
            Err(error) => {
                return DifferentialOutcome::fallback(format!(
                    "copy local blocks task panicked: {error}"
                ));
            }
        };

        let expected_download: u64 = copy.missing_ranges.iter().map(|r| r.len()).sum();
        listener.on_event(DownloadEvent::Started {
            content_length: Some(expected_download),
            total_size: Some(ctx.plan.total_size),
            differential: true,
            reused_bytes: copy.reused_bytes,
        });

        let session = RangeSession::new(&self.transfer, &request.artifact_url, ctx.plan.total_size);
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

        let verify_path = temp_file.path().to_path_buf();
        let verify_map = ctx.blockmap.clone();
        let verify_exp = request.verification.clone();
        let commit_cache = self.cache.clone();
        let commit_ver = request.version.clone();
        let verified_result = tokio::task::spawn_blocking(move || {
            PackageReconstructor::verify_reconstructed(&verify_path, &verify_map, &verify_exp)
                .map_err(|err| format!("reconstructed package verification failed: {err}"))?;
            PackageReconstructor::commit_verified_file(
                &commit_cache,
                &commit_ver,
                &verify_exp,
                &verify_path,
                Some(&verify_map),
            )
            .map_err(|err| format!("cache commit failed: {err}"))
        })
        .await;

        let summary = DownloadSummary::from_counts(
            DownloadMode::Differential,
            ctx.plan.total_size,
            fetch.downloaded_bytes,
            copy.reused_bytes,
        )
        .with_details(fetch.http_requests, None);

        match verified_result {
            Ok(Ok(path)) => {
                listener.on_event(DownloadEvent::Finished {
                    summary: summary.clone(),
                });
                DifferentialOutcome::Completed(DownloadedPackage { path, summary })
            }
            Ok(Err(reason)) => DifferentialOutcome::fallback(reason),
            Err(error) => {
                DifferentialOutcome::fallback(format!("differential verify task failed: {error}"))
            }
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

        let verify_path = temp_file.path().to_path_buf();
        let verify_exp = request.verification.clone();
        let known_map = ctx.remote_blockmap;
        let commit_cache = self.cache.clone();
        let commit_ver = request.version.clone();
        let path = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
            ArtifactVerifier::verify_file(&verify_path, &verify_exp)?;
            PackageReconstructor::commit_verified_file(
                &commit_cache,
                &commit_ver,
                &verify_exp,
                &verify_path,
                known_map.as_ref(),
            )
        })
        .await
        .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))??;

        let summary = DownloadSummary::from_counts(DownloadMode::Full, downloaded, downloaded, 0)
            .with_details(1, ctx.fallback_reason);
        listener.on_event(DownloadEvent::Finished {
            summary: summary.clone(),
        });
        Ok(DownloadedPackage { path, summary })
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
