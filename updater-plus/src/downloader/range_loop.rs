use futures_util::StreamExt;

use crate::listener::{DownloadEvent, DownloadListener};
use crate::planner::ByteRange;
use crate::provider::{ByteStream, DownloadSession, RangeCapability, RangeFetchResult, RangePart};
use crate::session::DiffSession;
use crate::{Error, Result};

pub(crate) struct FallbackDetail {
    pub(crate) reason: String,
    pub(crate) full_stream: Option<ByteStream>,
    pub(crate) invalidate_blockmap: bool,
}

impl FallbackDetail {
    pub(crate) fn reason(reason: String) -> Self {
        Self {
            reason,
            full_stream: None,
            invalidate_blockmap: false,
        }
    }

    pub(crate) fn invalidating_blockmap(reason: String) -> Self {
        Self {
            reason,
            full_stream: None,
            invalidate_blockmap: true,
        }
    }
}

pub(crate) enum RangeLoopOutcome {
    Completed(Box<DiffSession>),
    Fallback(FallbackDetail),
}

pub(crate) async fn fetch_ranges_into_session<L: DownloadListener>(
    session: &dyn DownloadSession,
    ranges: &[ByteRange],
    mut diff_session: DiffSession,
    listener: &L,
) -> Result<RangeLoopOutcome> {
    if ranges.is_empty() {
        return Ok(RangeLoopOutcome::Completed(Box::new(diff_session)));
    }

    match session.capabilities().range {
        RangeCapability::FullOnly => Ok(RangeLoopOutcome::Fallback(FallbackDetail::reason(
            "provider only supports full downloads".into(),
        ))),
        RangeCapability::SingleRange { max_concurrency } => {
            fetch_single_ranges(
                session,
                ranges,
                max_concurrency.max(1),
                diff_session,
                listener,
            )
            .await
        }
        RangeCapability::MultiRange {
            max_ranges_per_call,
            max_concurrency,
        } => {
            if ranges.len() == 1 {
                return fetch_single_ranges(
                    session,
                    ranges,
                    max_concurrency.max(1),
                    diff_session,
                    listener,
                )
                .await;
            }
            let batch_size = max_ranges_per_call.max(1);
            let chunks: Vec<&[ByteRange]> = ranges.chunks(batch_size).collect();
            for (batch_idx, batch) in chunks.iter().enumerate() {
                let degraded = matches!(
                    session.capabilities().range,
                    RangeCapability::SingleRange { .. }
                );
                if batch.len() == 1 || degraded {
                    let remaining: Vec<ByteRange> = chunks[batch_idx..]
                        .iter()
                        .copied()
                        .flatten()
                        .copied()
                        .collect();
                    return fetch_single_ranges(
                        session,
                        &remaining,
                        max_concurrency.max(1),
                        diff_session,
                        listener,
                    )
                    .await;
                }
                match session.fetch_ranges(batch.to_vec()).await {
                    Ok(RangeFetchResult::Parts(parts)) => {
                        match write_parts_to_diff(diff_session, parts, listener).await {
                            Ok(updated) => diff_session = updated,
                            Err(error) => {
                                return Ok(RangeLoopOutcome::Fallback(FallbackDetail::reason(
                                    format!("write multipart range failed: {error}"),
                                )));
                            }
                        }
                    }
                    Ok(RangeFetchResult::DegradeToSingle) => {
                        let remaining: Vec<ByteRange> = chunks[batch_idx..]
                            .iter()
                            .copied()
                            .flatten()
                            .copied()
                            .collect();
                        return fetch_single_ranges(
                            session,
                            &remaining,
                            max_concurrency.max(1),
                            diff_session,
                            listener,
                        )
                        .await;
                    }
                    Ok(RangeFetchResult::FallbackFull(stream)) => {
                        return Ok(RangeLoopOutcome::Fallback(FallbackDetail {
                            reason: "server returned 200 OK instead of 206 Partial Content".into(),
                            full_stream: Some(stream),
                            invalidate_blockmap: false,
                        }));
                    }
                    Ok(RangeFetchResult::Unsupported) => {
                        return Ok(RangeLoopOutcome::Fallback(FallbackDetail::reason(
                            "provider does not support range requests".into(),
                        )));
                    }
                    Err(error) if error.is_auth_error() => return Err(error),
                    Err(error) => {
                        return Ok(RangeLoopOutcome::Fallback(FallbackDetail::reason(format!(
                            "multipart range download failed: {error}"
                        ))));
                    }
                }
            }
            Ok(RangeLoopOutcome::Completed(Box::new(diff_session)))
        }
    }
}

async fn fetch_single_ranges<L: DownloadListener>(
    session: &dyn DownloadSession,
    ranges: &[ByteRange],
    max_concurrency: usize,
    mut diff_session: DiffSession,
    listener: &L,
) -> Result<RangeLoopOutcome> {
    let Some((&first, rest)) = ranges.split_first() else {
        return Ok(RangeLoopOutcome::Completed(Box::new(diff_session)));
    };

    match apply_single_outcome(
        session.fetch_ranges(vec![first]).await,
        diff_session,
        listener,
    )
    .await?
    {
        SingleStep::Continue(updated) => diff_session = *updated,
        SingleStep::Fallback(fallback) => return Ok(RangeLoopOutcome::Fallback(fallback)),
    }

    if rest.is_empty() {
        return Ok(RangeLoopOutcome::Completed(Box::new(diff_session)));
    }

    let mut stream = futures_util::stream::iter(rest.iter().copied())
        .map(|range| session.fetch_ranges(vec![range]))
        .buffer_unordered(max_concurrency.clamp(1, 16));

    while let Some(outcome) = stream.next().await {
        match apply_single_outcome(outcome, diff_session, listener).await? {
            SingleStep::Continue(updated) => diff_session = *updated,
            SingleStep::Fallback(fallback) => return Ok(RangeLoopOutcome::Fallback(fallback)),
        }
    }

    Ok(RangeLoopOutcome::Completed(Box::new(diff_session)))
}

enum SingleStep {
    Continue(Box<DiffSession>),
    Fallback(FallbackDetail),
}

async fn apply_single_outcome<L: DownloadListener>(
    outcome: Result<RangeFetchResult>,
    diff_session: DiffSession,
    listener: &L,
) -> Result<SingleStep> {
    match outcome {
        Ok(RangeFetchResult::Parts(parts)) => {
            match write_parts_to_diff(diff_session, parts, listener).await {
                Ok(updated) => Ok(SingleStep::Continue(Box::new(updated))),
                Err(error) => Ok(SingleStep::Fallback(FallbackDetail::reason(format!(
                    "write range failed: {error}"
                )))),
            }
        }
        Ok(RangeFetchResult::FallbackFull(stream)) => Ok(SingleStep::Fallback(FallbackDetail {
            reason: "server does not support HTTP Range requests".into(),
            full_stream: Some(stream),
            invalidate_blockmap: false,
        })),
        Ok(RangeFetchResult::DegradeToSingle | RangeFetchResult::Unsupported) => {
            Ok(SingleStep::Fallback(FallbackDetail::reason(
                "server does not support HTTP Range requests".into(),
            )))
        }
        Err(error) if error.is_auth_error() => Err(error),
        Err(error) => Ok(SingleStep::Fallback(FallbackDetail::reason(format!(
            "range download failed: {error}"
        )))),
    }
}

async fn write_parts_to_diff<L: DownloadListener>(
    mut diff_session: DiffSession,
    parts: Vec<RangePart>,
    listener: &L,
) -> Result<DiffSession> {
    let (updated, progress_counts) = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut counts = Vec::with_capacity(parts.len());
        for part in parts {
            let newly_covered = diff_session.write_range(part.start, &part.data)?;
            if newly_covered > 0 {
                counts.push(newly_covered);
            }
        }
        Ok((diff_session, counts))
    })
    .await
    .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))??;

    for chunk_length in progress_counts {
        listener.on_event(DownloadEvent::Progress { chunk_length });
    }
    Ok(updated)
}
