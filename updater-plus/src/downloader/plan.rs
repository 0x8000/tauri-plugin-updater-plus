use crate::blockmap::BlockMap;
use crate::cache::PackageCache;
use crate::planner::{ByteRange, DownloadPlan, PlanOptions};
use crate::provider::{DownloadSession, RangeCapability, UpdateProvider};
use crate::session::{DiffSession, DiffSessionOpenParams};
use crate::{Result, TransportErrorKind};

use super::request::ProviderDownloadRequest;

const MAX_BLOCKMAP_WIRE_BYTES: usize = 32 * 1024 * 1024;

pub(super) struct DifferentialContext {
    pub(super) blockmap: BlockMap,
    pub(super) diff_session: DiffSession,
    pub(super) missing_ranges: Vec<ByteRange>,
    pub(super) expected_download: u64,
    pub(super) reused_bytes: u64,
    pub(super) total_size: u64,
}

pub(super) enum PlanResolution {
    Ready(Box<DifferentialContext>),
    Fallback {
        reason: String,
        blockmap: Option<BlockMap>,
    },
}

pub(super) async fn prepare_differential(
    provider: &dyn UpdateProvider,
    cache: &PackageCache,
    request: &ProviderDownloadRequest,
    session: &dyn DownloadSession,
) -> Result<PlanResolution> {
    let capabilities = session.capabilities();
    if matches!(capabilities.range, RangeCapability::FullOnly) {
        return Ok(PlanResolution::Fallback {
            reason: "provider does not support range requests".into(),
            blockmap: None,
        });
    }

    let Some(blockmap_id) = request.blockmap.as_ref() else {
        return Ok(PlanResolution::Fallback {
            reason: "release metadata omitted blockmap identifier".into(),
            blockmap: None,
        });
    };

    let cache_clone = cache.clone();
    let loaded_base = match tokio::task::spawn_blocking(move || cache_clone.load_base()).await {
        Ok(base) => base,
        Err(error) => {
            return Ok(PlanResolution::Fallback {
                reason: format!("load cached base package panicked: {error}"),
                blockmap: None,
            });
        }
    };
    let Some(base) = loaded_base else {
        return Ok(PlanResolution::Fallback {
            reason: "no cached base package available".into(),
            blockmap: None,
        });
    };

    let raw_blockmap = match provider
        .fetch_blockmap(blockmap_id, MAX_BLOCKMAP_WIRE_BYTES, &request.overrides)
        .await
    {
        Ok(bytes) => bytes,
        // Fail fast on 401 Unauthorized, but allow 403 Forbidden on `.blockmap` to fall back
        // to `fetch_full()` (private S3 buckets without `s3:ListBucket` and presigned URLs
        // without an explicit blockmap return 403 for missing `.blockmap` objects).
        Err(error)
            if matches!(
                error.transport_kind(),
                Some(TransportErrorKind::Unauthorized)
            ) =>
        {
            return Err(error);
        }
        Err(error) => {
            return Ok(PlanResolution::Fallback {
                reason: format!("remote blockmap unavailable: {error}"),
                blockmap: None,
            });
        }
    };
    let remote_blockmap = match BlockMap::from_bytes(&raw_blockmap) {
        Ok(map) => map,
        Err(error) => {
            return Ok(PlanResolution::Fallback {
                reason: format!("remote blockmap unavailable: {error}"),
                blockmap: None,
            });
        }
    };

    if let Some(expected_size) = request.expected_size
        && expected_size != remote_blockmap.total_size()
    {
        return Ok(PlanResolution::Fallback {
            reason: "remote blockmap size disagrees with manifest size".into(),
            blockmap: None,
        });
    }

    let max_range_gap = request
        .max_range_gap
        .unwrap_or(capabilities.suggested_max_range_gap);
    let plan = match DownloadPlan::build(
        &base.blockmap,
        &remote_blockmap,
        PlanOptions { max_range_gap },
    ) {
        Ok(plan) => plan,
        Err(error) => {
            return Ok(PlanResolution::Fallback {
                reason: format!("differential plan failed: {error}"),
                blockmap: Some(remote_blockmap),
            });
        }
    };

    if plan.should_fallback_to_full(request.full_download_threshold) {
        return Ok(PlanResolution::Fallback {
            reason: format!(
                "delta requires {:.1}% of full package",
                (plan.download_bytes as f64 / plan.total_size.max(1) as f64) * 100.0
            ),
            blockmap: Some(remote_blockmap),
        });
    }

    let open_params = DiffSessionOpenParams {
        cache: cache.clone(),
        version: request.version.clone(),
        remote_blockmap: remote_blockmap.clone(),
        expectation: request.verification.clone(),
    };
    let base_path = base.package_path.clone();
    let plan_clone = plan.clone();

    let opened = tokio::task::spawn_blocking(move || {
        DiffSession::open_with_plan(open_params, &base_path, &plan_clone)
    })
    .await;

    let (diff_session, plan_info) = match opened {
        Ok(Ok(pair)) => pair,
        Ok(Err(error)) => {
            return Ok(PlanResolution::Fallback {
                reason: format!("copy local blocks failed: {error}"),
                blockmap: Some(remote_blockmap),
            });
        }
        Err(error) => {
            return Ok(PlanResolution::Fallback {
                reason: format!("copy local blocks task panicked: {error}"),
                blockmap: Some(remote_blockmap),
            });
        }
    };

    Ok(PlanResolution::Ready(Box::new(DifferentialContext {
        blockmap: remote_blockmap,
        diff_session,
        missing_ranges: plan_info.missing_ranges,
        expected_download: plan_info.expected_download_bytes,
        reused_bytes: plan_info.reused_bytes,
        total_size: plan_info.total_size,
    })))
}
