use crate::planner::ByteRange;

/// Tracks uncovered inclusive byte ranges so partial, overlapping, or duplicate writes
/// (such as MQTT QoS 1 redeliveries) are handled idempotently and verified for 100% coverage.
#[derive(Debug, Clone, Default)]
pub(crate) struct RangeCoverage {
    missing: Vec<ByteRange>,
}

impl RangeCoverage {
    pub(crate) fn new(ranges: Vec<ByteRange>) -> Self {
        Self {
            missing: Self::coalesce(ranges),
        }
    }

    pub(crate) fn coalesce(mut ranges: Vec<ByteRange>) -> Vec<ByteRange> {
        if ranges.is_empty() {
            return ranges;
        }
        ranges.sort_by_key(|range| range.start);
        let mut merged: Vec<ByteRange> = Vec::with_capacity(ranges.len());
        for range in ranges {
            if let Some(last) = merged.last_mut()
                && last.end.saturating_add(1) >= range.start
            {
                last.end = last.end.max(range.end);
                continue;
            }
            merged.push(range);
        }
        merged
    }

    /// Marks `[offset, offset + len - 1]` as written and returns the number of newly covered bytes.
    pub(crate) fn record_write(&mut self, offset: u64, len: u64) -> u64 {
        if len == 0 || self.missing.is_empty() {
            return 0;
        }
        let Some(write_end) = offset.checked_add(len - 1) else {
            return 0;
        };

        let mut newly_covered = 0u64;
        let mut next = Vec::with_capacity(self.missing.len());

        for range in self.missing.drain(..) {
            if write_end < range.start || offset > range.end {
                next.push(range);
                continue;
            }

            let overlap_start = offset.max(range.start);
            let overlap_end = write_end.min(range.end);
            newly_covered = newly_covered.saturating_add(overlap_end - overlap_start + 1);

            if range.start < overlap_start {
                next.push(ByteRange {
                    start: range.start,
                    end: overlap_start - 1,
                });
            }
            if overlap_end < range.end {
                next.push(ByteRange {
                    start: overlap_end + 1,
                    end: range.end,
                });
            }
        }

        self.missing = next;
        newly_covered
    }

    pub(crate) fn missing_ranges(&self) -> &[ByteRange] {
        &self.missing
    }

    pub(crate) fn remaining_bytes(&self) -> u64 {
        self.missing.iter().map(|range| range.len()).sum()
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}
