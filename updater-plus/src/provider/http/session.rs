use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures_util::StreamExt;
use http::StatusCode;
use http::header::{CONTENT_TYPE, RANGE};
use tokio::sync::{Mutex, OnceCell, RwLock};
use url::Url;

use super::downloader::{HttpDownloader, OriginSendParams};
use super::middleware::{HttpRequest, RequestStage};
use super::wire::{
    explicitly_rejects_ranges, format_range_header, read_multipart_partial, read_single_partial,
    redact_url, response_to_byte_stream, split_by_header_budget,
};
use crate::multipart::ByteRangeParser;
use crate::planner::ByteRange;
use crate::provider::{
    BoxFuture, ByteStream, DownloadSession, RangeCapability, RangeFetchResult,
    TransferCapabilities, TransferOverrides, TransferStats,
};
use crate::{Error, Result};

#[derive(Clone)]
struct EffectiveUrlState {
    url: Url,
    generation: u64,
}

#[derive(Clone)]
struct SessionHandle {
    downloader: HttpDownloader,
    original_url: Url,
    overrides: TransferOverrides,
    client: Arc<OnceCell<reqwest::Client>>,
    effective: Arc<RwLock<EffectiveUrlState>>,
    refresh_lock: Arc<Mutex<()>>,
    degraded_to_single: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
}

/// Active per-download HTTP session caching resolved 302 CDN URLs with anti-thundering-herd refresh.
pub struct HttpDownloadSession {
    handle: SessionHandle,
}

impl HttpDownloadSession {
    pub(crate) fn new(
        downloader: HttpDownloader,
        original_url: Url,
        overrides: TransferOverrides,
    ) -> Self {
        let effective = EffectiveUrlState {
            url: original_url.clone(),
            generation: 0,
        };
        Self {
            handle: SessionHandle {
                downloader,
                original_url,
                overrides,
                client: Arc::new(OnceCell::new()),
                effective: Arc::new(RwLock::new(effective)),
                refresh_lock: Arc::new(Mutex::new(())),
                degraded_to_single: Arc::new(AtomicBool::new(false)),
                requests: Arc::new(AtomicUsize::new(0)),
            },
        }
    }

    fn should_start_single_range(&self) -> bool {
        if self.handle.degraded_to_single.load(Ordering::SeqCst) {
            return true;
        }
        match self
            .handle
            .downloader
            .transfer_options()
            .use_multiple_range_request
        {
            Some(use_multi) => !use_multi,
            None => HttpDownloader::is_known_single_range_host(&self.handle.original_url),
        }
    }
}

impl DownloadSession for HttpDownloadSession {
    fn capabilities(&self) -> TransferCapabilities {
        let opts = self.handle.downloader.transfer_options();
        let range = if self.should_start_single_range() {
            RangeCapability::SingleRange {
                max_concurrency: opts.max_concurrent_ranges,
            }
        } else {
            RangeCapability::MultiRange {
                max_ranges_per_call: opts.max_ranges_per_request,
                max_concurrency: opts.max_concurrent_ranges,
            }
        };
        TransferCapabilities {
            range,
            suggested_max_range_gap: 0,
        }
    }

    fn fetch_full(&self) -> BoxFuture<'static, Result<ByteStream>> {
        let handle = self.handle.clone();
        Box::pin(async move {
            handle.requests.fetch_add(1, Ordering::SeqCst);
            let client = handle.resolved_client().await?;
            let resp = handle
                .downloader
                .send_with_client(
                    &client,
                    OriginSendParams {
                        req: HttpRequest::get(handle.original_url.clone()),
                        original_url: &handle.original_url,
                        stage: RequestStage::Artifact,
                        overrides: &handle.overrides,
                    },
                )
                .await?;
            if !resp.status().is_success() {
                return Err(Error::HttpStatus {
                    url: redact_url(&handle.original_url),
                    status: resp.status().as_u16(),
                });
            }
            Ok(response_to_byte_stream(resp.response))
        })
    }

    fn fetch_ranges(&self, ranges: Vec<ByteRange>) -> BoxFuture<'static, Result<RangeFetchResult>> {
        let handle = self.handle.clone();
        Box::pin(async move {
            if ranges.is_empty() {
                return Ok(RangeFetchResult::Parts(Vec::new()));
            }
            let sub_batches = split_by_header_budget(&ranges);
            if sub_batches.len() == 1 {
                let result = handle.fetch_one_batch(&sub_batches[0]).await?;
                if matches!(result, RangeFetchResult::DegradeToSingle) {
                    handle.degraded_to_single.store(true, Ordering::SeqCst);
                }
                return Ok(result);
            }

            let concurrency = handle
                .downloader
                .transfer_options()
                .max_concurrent_ranges
                .clamp(1, 16);
            let mut all_parts = Vec::with_capacity(ranges.len());
            for (idx, sub_batch) in sub_batches.iter().enumerate() {
                match handle.fetch_one_batch(sub_batch).await? {
                    RangeFetchResult::Parts(parts) => all_parts.extend(parts),
                    RangeFetchResult::DegradeToSingle if idx == 0 => {
                        handle.degraded_to_single.store(true, Ordering::SeqCst);
                        return Ok(RangeFetchResult::DegradeToSingle);
                    }
                    RangeFetchResult::DegradeToSingle => {
                        handle.degraded_to_single.store(true, Ordering::SeqCst);
                        let remaining: Vec<ByteRange> =
                            sub_batches[idx..].iter().flatten().copied().collect();
                        let mut stream = futures_util::stream::iter(remaining)
                            .map(|range| {
                                let h = handle.clone();
                                async move { h.fetch_one_batch(&[range]).await }
                            })
                            .buffer_unordered(concurrency);
                        while let Some(res) = stream.next().await {
                            match res? {
                                RangeFetchResult::Parts(parts) => all_parts.extend(parts),
                                other => return Ok(other),
                            }
                        }
                        break;
                    }
                    other => return Ok(other),
                }
            }
            Ok(RangeFetchResult::Parts(all_parts))
        })
    }

    fn stats(&self) -> TransferStats {
        TransferStats {
            requests: self.handle.requests.load(Ordering::SeqCst),
        }
    }
}

impl SessionHandle {
    async fn resolved_client(&self) -> Result<reqwest::Client> {
        let client = self
            .client
            .get_or_try_init(|| async { self.downloader.resolve_client(&self.overrides) })
            .await?;
        Ok(client.clone())
    }

    async fn fetch_one_batch(&self, ranges: &[ByteRange]) -> Result<RangeFetchResult> {
        let snapshot = { self.effective.read().await.clone() };
        let first = self.send_range_attempt(&snapshot.url, ranges).await;

        if is_expired_redirect_error(&first, &snapshot.url, &self.original_url) {
            return self.refresh_and_retry(snapshot.generation, ranges).await;
        }

        let (outcome, final_url) = first?;
        if let Some(next_url) = final_url {
            let mut guard = self.effective.write().await;
            if guard.generation == snapshot.generation && guard.url != next_url {
                guard.url = next_url;
                guard.generation = guard.generation.saturating_add(1);
            }
        }
        Ok(outcome)
    }

    async fn refresh_and_retry(
        &self,
        stale_generation: u64,
        ranges: &[ByteRange],
    ) -> Result<RangeFetchResult> {
        let refreshed = {
            let _refresh_guard = self.refresh_lock.lock().await;
            let current = { self.effective.read().await.clone() };
            if current.generation != stale_generation {
                current
            } else {
                let (outcome, final_url) =
                    self.send_range_attempt(&self.original_url, ranges).await?;
                {
                    let mut guard = self.effective.write().await;
                    guard.url = final_url.unwrap_or_else(|| self.original_url.clone());
                    guard.generation = guard.generation.saturating_add(1);
                }
                return Ok(outcome);
            }
        };

        let (outcome, final_url) = self.send_range_attempt(&refreshed.url, ranges).await?;
        if let Some(next_url) = final_url {
            let mut guard = self.effective.write().await;
            if guard.generation == refreshed.generation && guard.url != next_url {
                guard.url = next_url;
                guard.generation = guard.generation.saturating_add(1);
            }
        }
        Ok(outcome)
    }

    async fn send_range_attempt(
        &self,
        request_url: &Url,
        ranges: &[ByteRange],
    ) -> Result<(RangeFetchResult, Option<Url>)> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let client = self.resolved_client().await?;
        let mut req = HttpRequest::get(request_url.clone());
        req.headers.insert(RANGE, format_range_header(ranges)?);

        let resp = self
            .downloader
            .send_with_client(
                &client,
                OriginSendParams {
                    req,
                    original_url: &self.original_url,
                    stage: RequestStage::Artifact,
                    overrides: &self.overrides,
                },
            )
            .await?;
        let final_url = resp.final_url.clone();
        let status = resp.status();

        if status == StatusCode::OK {
            if ranges.len() > 1 && !explicitly_rejects_ranges(resp.headers()) {
                return Ok((RangeFetchResult::DegradeToSingle, None));
            }
            return Ok((
                RangeFetchResult::FallbackFull(response_to_byte_stream(resp.response)),
                None,
            ));
        }

        if status != StatusCode::PARTIAL_CONTENT {
            if ranges.len() > 1
                && matches!(
                    status,
                    StatusCode::BAD_REQUEST
                        | StatusCode::RANGE_NOT_SATISFIABLE
                        | StatusCode::NOT_IMPLEMENTED
                )
            {
                return Ok((RangeFetchResult::DegradeToSingle, None));
            }
            return Err(Error::HttpStatus {
                url: redact_url(request_url),
                status: status.as_u16(),
            });
        }

        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if let Some(boundary) = ByteRangeParser::extract_boundary(&content_type) {
            let result = read_multipart_partial(resp.response, &boundary, ranges).await?;
            let next_url = matches!(result, RangeFetchResult::Parts(_)).then_some(final_url);
            return Ok((result, next_url));
        }

        if ranges.len() == 1 {
            let part = read_single_partial(resp.response, ranges[0]).await?;
            return Ok((RangeFetchResult::Parts(vec![part]), Some(final_url)));
        }

        Ok((RangeFetchResult::DegradeToSingle, None))
    }
}

fn is_expired_redirect_error<T>(result: &Result<T>, current_url: &Url, original_url: &Url) -> bool {
    current_url != original_url
        && matches!(
            result,
            Err(Error::HttpStatus {
                status: 401 | 403 | 404 | 410,
                ..
            })
        )
}
