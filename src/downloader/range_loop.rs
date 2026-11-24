use futures_util::StreamExt;

use crate::Result;
use crate::listener::{DownloadEvent, DownloadListener};
use crate::planner::ByteRange;
use crate::provider::{ByteStream, DownloadSession, RangeCapability, RangeFetchResult, RangePart};
use crate::session::DiffSession;

pub(crate) struct FallbackDetail {
    pub(crate) reason: String,
    pub(crate) full_stream: Option<ByteStream>,
}

impl FallbackDetail {
    pub(crate) fn reason(reason: String) -> Self {
        Self {
            reason,
            full_stream: None,
        }
    }
}

pub(crate) enum RangeLoopOutcome {
    Completed,
    Fallback(FallbackDetail),
}

pub(crate) async fn fetch_ranges_into_session<L: DownloadListener>(
    session: &dyn DownloadSession,
    ranges: &[ByteRange],
    diff_session: &mut DiffSession,
    listener: &L,
) -> Result<RangeLoopOutcome> {
    if ranges.is_empty() {
        return Ok(RangeLoopOutcome::Completed);
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
                if batch.len() == 1 {
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
                        for part in parts {
                            if let Err(error) = write_part_to_diff(diff_session, &part, listener) {
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
            Ok(RangeLoopOutcome::Completed)
        }
    }
}

async fn fetch_single_ranges<L: DownloadListener>(
    session: &dyn DownloadSession,
    ranges: &[ByteRange],
    max_concurrency: usize,
    diff_session: &mut DiffSession,
    listener: &L,
) -> Result<RangeLoopOutcome> {
    let Some((&first, rest)) = ranges.split_first() else {
        return Ok(RangeLoopOutcome::Completed);
    };

    if let Some(fallback) = apply_single_outcome(
        session.fetch_ranges(vec![first]).await,
        diff_session,
        listener,
    )? {
        return Ok(RangeLoopOutcome::Fallback(fallback));
    }

    if rest.is_empty() {
        return Ok(RangeLoopOutcome::Completed);
    }

    let mut stream = futures_util::stream::iter(rest.iter().copied())
        .map(|range| session.fetch_ranges(vec![range]))
        .buffer_unordered(max_concurrency.clamp(1, 16));

    while let Some(outcome) = stream.next().await {
        if let Some(fallback) = apply_single_outcome(outcome, diff_session, listener)? {
            return Ok(RangeLoopOutcome::Fallback(fallback));
        }
    }

    Ok(RangeLoopOutcome::Completed)
}

fn apply_single_outcome<L: DownloadListener>(
    outcome: Result<RangeFetchResult>,
    diff_session: &mut DiffSession,
    listener: &L,
) -> Result<Option<FallbackDetail>> {
    match outcome {
        Ok(RangeFetchResult::Parts(parts)) => {
            for part in parts {
                if let Err(error) = write_part_to_diff(diff_session, &part, listener) {
                    return Ok(Some(FallbackDetail::reason(format!(
                        "write range failed: {error}"
                    ))));
                }
            }
            Ok(None)
        }
        Ok(RangeFetchResult::FallbackFull(stream)) => Ok(Some(FallbackDetail {
            reason: "server does not support HTTP Range requests".into(),
            full_stream: Some(stream),
        })),
        Ok(RangeFetchResult::DegradeToSingle | RangeFetchResult::Unsupported) => Ok(Some(
            FallbackDetail::reason("server does not support HTTP Range requests".into()),
        )),
        Err(error) if error.is_auth_error() => Err(error),
        Err(error) => Ok(Some(FallbackDetail::reason(format!(
            "range download failed: {error}"
        )))),
    }
}

fn write_part_to_diff<L: DownloadListener>(
    diff_session: &mut DiffSession,
    part: &RangePart,
    listener: &L,
) -> Result<()> {
    let newly_covered = diff_session.write_range(part.start, &part.data)?;
    if newly_covered > 0 {
        listener.on_event(DownloadEvent::Progress {
            chunk_length: newly_covered,
        });
    }
    Ok(())
}
