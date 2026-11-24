use std::fs::File;

use futures_util::StreamExt;
use tokio::sync::RwLock;
use url::Url;

use crate::Error;
use crate::listener::DownloadListener;

use super::reconstruct::PackageReconstructor;
use super::transfer::{
    BatchOutcome, ByteRange, RangeRequestTarget, SingleOutcome, TransferExecutor,
};

pub(crate) struct FallbackDetail {
    pub(crate) reason: String,
    pub(crate) full_response: Option<reqwest::Response>,
}

impl FallbackDetail {
    pub(crate) fn reason(reason: String) -> Self {
        Self {
            reason,
            full_response: None,
        }
    }
}

pub(crate) struct RangeFetchStats {
    pub(crate) downloaded_bytes: u64,
    pub(crate) http_requests: usize,
}

pub(crate) enum RangeFetchOutcome {
    Completed(RangeFetchStats),
    Fallback(FallbackDetail),
}

impl RangeFetchOutcome {
    fn with_prior(self, prior: RangeFetchStats) -> Self {
        match self {
            Self::Completed(stats) => Self::Completed(RangeFetchStats {
                downloaded_bytes: prior
                    .downloaded_bytes
                    .saturating_add(stats.downloaded_bytes),
                http_requests: prior.http_requests + stats.http_requests,
            }),
            fallback => fallback,
        }
    }
}

pub(crate) struct RangeSession<'a> {
    transfer: &'a TransferExecutor,
    artifact_url: &'a Url,
    effective_url: RwLock<Url>,
}

impl<'a> RangeSession<'a> {
    pub(crate) fn new(transfer: &'a TransferExecutor, artifact_url: &'a Url) -> Self {
        Self {
            transfer,
            artifact_url,
            effective_url: RwLock::new(artifact_url.clone()),
        }
    }

    pub(crate) async fn fetch_all<L: DownloadListener>(
        &self,
        ranges: &[ByteRange],
        target: &mut File,
        listener: &L,
    ) -> RangeFetchOutcome {
        if ranges.is_empty() {
            return RangeFetchOutcome::Completed(RangeFetchStats {
                downloaded_bytes: 0,
                http_requests: 0,
            });
        }

        if self.transfer.should_start_single_range(self.artifact_url) || ranges.len() == 1 {
            return self.fetch_single_ranges(ranges, target, listener).await;
        }

        let mut downloaded_bytes = 0u64;
        let mut http_requests = 0usize;
        let batches = self.transfer.split_batches(ranges);

        for (batch_idx, batch) in batches.iter().enumerate() {
            if batch.len() == 1 {
                let remaining: Vec<ByteRange> = batches[batch_idx..]
                    .iter()
                    .copied()
                    .flatten()
                    .copied()
                    .collect();
                let prior = RangeFetchStats {
                    downloaded_bytes,
                    http_requests,
                };
                return self
                    .fetch_single_ranges(&remaining, target, listener)
                    .await
                    .with_prior(prior);
            }

            let (outcome, attempts) = self.fetch_batch_with_retry(batch).await;
            http_requests += attempts;
            match outcome {
                Ok(BatchOutcome::Parts { parts, final_url }) => {
                    self.store_effective_url(final_url).await;
                    for part in parts {
                        if let Err(error) =
                            PackageReconstructor::write_part(target, &part, listener)
                        {
                            return RangeFetchOutcome::Fallback(FallbackDetail::reason(format!(
                                "write multipart range failed: {error}"
                            )));
                        }
                        downloaded_bytes = downloaded_bytes.saturating_add(part.data.len() as u64);
                    }
                }
                Ok(BatchOutcome::NeedSingleRange) => {
                    let remaining: Vec<ByteRange> = batches[batch_idx..]
                        .iter()
                        .copied()
                        .flatten()
                        .copied()
                        .collect();
                    let prior = RangeFetchStats {
                        downloaded_bytes,
                        http_requests,
                    };
                    return self
                        .fetch_single_ranges(&remaining, target, listener)
                        .await
                        .with_prior(prior);
                }
                Ok(BatchOutcome::FullResponse(response)) => {
                    return RangeFetchOutcome::Fallback(FallbackDetail {
                        reason: "server returned 200 OK instead of 206 Partial Content".into(),
                        full_response: Some(response),
                    });
                }
                Err(error) => {
                    return RangeFetchOutcome::Fallback(FallbackDetail::reason(format!(
                        "multipart range download failed: {error}"
                    )));
                }
            }
        }

        RangeFetchOutcome::Completed(RangeFetchStats {
            downloaded_bytes,
            http_requests,
        })
    }

    async fn fetch_single_ranges<L: DownloadListener>(
        &self,
        ranges: &[ByteRange],
        target: &mut File,
        listener: &L,
    ) -> RangeFetchOutcome {
        let Some((&first, rest)) = ranges.split_first() else {
            return RangeFetchOutcome::Completed(RangeFetchStats {
                downloaded_bytes: 0,
                http_requests: 0,
            });
        };

        let mut http_requests = 0usize;
        let mut downloaded_bytes = 0u64;

        let (first_outcome, first_attempts) = self.fetch_single_with_retry(first).await;
        http_requests += first_attempts;
        match first_outcome {
            Ok(SingleOutcome::Part { part, final_url }) => {
                self.store_effective_url(final_url).await;
                if let Err(error) = PackageReconstructor::write_part(target, &part, listener) {
                    return RangeFetchOutcome::Fallback(FallbackDetail::reason(format!(
                        "write range failed: {error}"
                    )));
                }
                downloaded_bytes = downloaded_bytes.saturating_add(part.data.len() as u64);
            }
            Ok(SingleOutcome::FullResponse(response)) => {
                return RangeFetchOutcome::Fallback(FallbackDetail {
                    reason: "server does not support HTTP Range requests".into(),
                    full_response: Some(response),
                });
            }
            Err(error) => {
                return RangeFetchOutcome::Fallback(FallbackDetail::reason(format!(
                    "range download failed: {error}"
                )));
            }
        }

        if rest.is_empty() {
            return RangeFetchOutcome::Completed(RangeFetchStats {
                downloaded_bytes,
                http_requests,
            });
        }

        let concurrency = self.transfer.max_concurrent_ranges();
        let mut stream = futures_util::stream::iter(rest.iter().copied())
            .map(|range| async move { self.fetch_single_with_retry(range).await })
            .buffer_unordered(concurrency);

        while let Some((outcome, attempts)) = stream.next().await {
            http_requests += attempts;
            match outcome {
                Ok(SingleOutcome::Part { part, final_url }) => {
                    self.store_effective_url(final_url).await;
                    if let Err(error) = PackageReconstructor::write_part(target, &part, listener) {
                        return RangeFetchOutcome::Fallback(FallbackDetail::reason(format!(
                            "write range failed: {error}"
                        )));
                    }
                    downloaded_bytes = downloaded_bytes.saturating_add(part.data.len() as u64);
                }
                Ok(SingleOutcome::FullResponse(response)) => {
                    return RangeFetchOutcome::Fallback(FallbackDetail {
                        reason: "server does not support HTTP Range requests".into(),
                        full_response: Some(response),
                    });
                }
                Err(error) => {
                    return RangeFetchOutcome::Fallback(FallbackDetail::reason(format!(
                        "range download failed: {error}"
                    )));
                }
            }
        }

        RangeFetchOutcome::Completed(RangeFetchStats {
            downloaded_bytes,
            http_requests,
        })
    }

    async fn fetch_batch_with_retry(
        &self,
        batch: &[ByteRange],
    ) -> (crate::Result<BatchOutcome>, usize) {
        let current = self.current_effective_url().await;
        let first = self
            .transfer
            .fetch_range_batch(
                RangeRequestTarget {
                    original_url: self.artifact_url,
                    request_url: &current,
                },
                batch,
            )
            .await;
        if Self::is_expired_redirect_error(&first, &current, self.artifact_url) {
            self.store_effective_url(self.artifact_url.clone()).await;
            let second = self
                .transfer
                .fetch_range_batch(
                    RangeRequestTarget {
                        original_url: self.artifact_url,
                        request_url: self.artifact_url,
                    },
                    batch,
                )
                .await;
            return (second, 2);
        }
        (first, 1)
    }

    async fn fetch_single_with_retry(
        &self,
        range: ByteRange,
    ) -> (crate::Result<SingleOutcome>, usize) {
        let current = self.current_effective_url().await;
        let first = self
            .transfer
            .fetch_single_range(
                RangeRequestTarget {
                    original_url: self.artifact_url,
                    request_url: &current,
                },
                range,
            )
            .await;
        if Self::is_expired_redirect_error(&first, &current, self.artifact_url) {
            self.store_effective_url(self.artifact_url.clone()).await;
            let second = self
                .transfer
                .fetch_single_range(
                    RangeRequestTarget {
                        original_url: self.artifact_url,
                        request_url: self.artifact_url,
                    },
                    range,
                )
                .await;
            return (second, 2);
        }
        (first, 1)
    }

    async fn current_effective_url(&self) -> Url {
        self.effective_url.read().await.clone()
    }

    async fn store_effective_url(&self, next: Url) {
        let mut guard = self.effective_url.write().await;
        *guard = next;
    }

    fn is_expired_redirect_error<T>(
        result: &crate::Result<T>,
        current_url: &Url,
        original_url: &Url,
    ) -> bool {
        current_url != original_url
            && matches!(
                result,
                Err(Error::HttpStatus {
                    status: 401 | 403 | 404 | 410,
                    ..
                })
            )
    }
}
