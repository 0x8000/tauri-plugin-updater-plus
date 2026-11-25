use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use http::StatusCode;
use http::header::{CONTENT_TYPE, RANGE};
use tokio::sync::RwLock;
use url::Url;

use super::downloader::HttpDownloader;
use super::middleware::{HttpRequest, RequestStage};
use super::wire::{
    explicitly_rejects_ranges, format_range_header, read_multipart_partial, read_single_partial,
    response_to_byte_stream, split_by_header_budget,
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
    effective: Arc<RwLock<EffectiveUrlState>>,
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
                effective: Arc::new(RwLock::new(effective)),
                requests: Arc::new(AtomicUsize::new(0)),
            },
        }
    }

    fn should_start_single_range(&self) -> bool {
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
            let resp = handle
                .downloader
                .send(
                    HttpRequest::get(handle.original_url.clone()),
                    RequestStage::Artifact,
                    &handle.overrides,
                )
                .await?;
            if !resp.status().is_success() {
                return Err(Error::HttpStatus {
                    url: handle.original_url.to_string(),
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
                return handle.fetch_one_batch(&sub_batches[0]).await;
            }

            let mut all_parts = Vec::with_capacity(ranges.len());
            for (idx, sub_batch) in sub_batches.iter().enumerate() {
                match handle.fetch_one_batch(sub_batch).await? {
                    RangeFetchResult::Parts(parts) => all_parts.extend(parts),
                    RangeFetchResult::DegradeToSingle if idx == 0 => {
                        return Ok(RangeFetchResult::DegradeToSingle);
                    }
                    RangeFetchResult::DegradeToSingle => {
                        for range in sub_batches[idx..].iter().flatten().copied() {
                            match handle.fetch_one_batch(&[range]).await? {
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
    async fn fetch_one_batch(&self, ranges: &[ByteRange]) -> Result<RangeFetchResult> {
        let snapshot = self.effective.read().await.clone();
        let first = self.send_range_attempt(&snapshot.url, ranges).await;

        let (outcome, final_url) =
            if is_expired_redirect_error(&first, &snapshot.url, &self.original_url) {
                let retry_url = {
                    let mut guard = self.effective.write().await;
                    if guard.generation == snapshot.generation {
                        guard.url = self.original_url.clone();
                        guard.generation = guard.generation.saturating_add(1);
                        self.original_url.clone()
                    } else {
                        guard.url.clone()
                    }
                };
                self.send_range_attempt(&retry_url, ranges).await?
            } else {
                first?
            };

        if let Some(next_url) = final_url {
            let mut guard = self.effective.write().await;
            if guard.url != next_url {
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
        let mut req = HttpRequest::get(request_url.clone());
        req.headers.insert(RANGE, format_range_header(ranges)?);

        let resp = self
            .downloader
            .send_with_origin(
                req,
                &self.original_url,
                RequestStage::Artifact,
                &self.overrides,
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
                url: request_url.to_string(),
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
