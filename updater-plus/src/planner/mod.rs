use std::collections::HashMap;

use crate::blockmap::BlockMap;
use crate::{Error, Result};

#[cfg(test)]
mod tests;

/// Inclusive byte range `[start..=end]` requested during a differential update transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteRange {
    /// Inclusive starting byte offset.
    pub start: u64,
    /// Inclusive ending byte offset.
    pub end: u64,
}

impl ByteRange {
    /// Constructs a validated inclusive byte range `[start..=end]`.
    pub fn new(start: u64, end: u64) -> Result<Self> {
        if end < start {
            return Err(Error::DifferentialTransfer(format!(
                "byte range end {end} precedes start {start}"
            )));
        }
        Ok(Self { start, end })
    }

    /// Constructs an inclusive byte range from an `offset` and positive byte `length`.
    pub fn from_offset_len(offset: u64, length: u64) -> Result<Self> {
        if length == 0 {
            return Err(Error::DifferentialTransfer(
                "byte range length must be positive".into(),
            ));
        }
        let end = offset
            .checked_add(length - 1)
            .ok_or_else(|| Error::DifferentialTransfer("byte range end overflow".into()))?;
        Ok(Self { start: offset, end })
    }

    /// Returns the number of bytes covered by this inclusive range.
    pub fn len(self) -> u64 {
        self.end - self.start + 1
    }

    /// Always returns `false` because [`ByteRange`] invariants enforce non-empty ranges.
    pub fn is_empty(self) -> bool {
        false
    }

    /// Returns the exclusive end offset `end + 1`, or `None` on `u64` overflow.
    pub fn end_exclusive(self) -> Option<u64> {
        self.end.checked_add(1)
    }
}

/// Single contiguous operation required to reconstruct a target artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOperation {
    /// Copy an unchanged byte span from the local old artifact.
    Copy {
        /// Byte offset inside the local old artifact.
        old_offset: u64,
        /// Byte offset inside the reconstructed new artifact.
        new_offset: u64,
        /// Number of bytes to copy.
        length: u64,
    },
    /// Download a changed or newly inserted byte span from the remote artifact.
    Download {
        /// Byte offset inside the remote and reconstructed new artifact.
        offset: u64,
        /// Number of bytes to download.
        length: u64,
    },
}

/// Tuning knobs for differential plan generation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanOptions {
    /// Coalesce adjacent Download ranges separated by a gap smaller than or equal to this size.
    pub max_range_gap: u64,
}

/// Coalesced copy/download execution plan derived from two [`BlockMap`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadPlan {
    /// Ordered copy and download operations covering the target artifact.
    pub operations: Vec<PlanOperation>,
    /// Total target artifact size in bytes.
    pub total_size: u64,
    /// Bytes scheduled to be reused from the local old artifact.
    pub reused_bytes: u64,
    /// Bytes scheduled to be downloaded over HTTP Range requests.
    pub download_bytes: u64,
}

impl DownloadPlan {
    /// Builds a differential plan from `old_map` to `new_map`.
    pub fn build(old_map: &BlockMap, new_map: &BlockMap, options: PlanOptions) -> Result<Self> {
        old_map.validate()?;
        new_map.validate()?;

        let old_index = Self::index_old_blocks(old_map);
        let mut operations: Vec<PlanOperation> = Vec::new();
        let mut reused_bytes = 0u64;
        let mut download_bytes = 0u64;
        let mut last_old_end = 0u64;
        let mut expected_new_offset = 0u64;

        for file in &new_map.files {
            if file.offset > expected_new_offset {
                let gap = file.offset - expected_new_offset;
                Self::push_download(&mut operations, expected_new_offset, gap);
                download_bytes = download_bytes.saturating_add(gap);
            }
            let mut new_cursor = file.offset;
            for (checksum, &size) in file.checksums.iter().zip(&file.sizes) {
                let len = u64::from(size);
                if let Some(offsets) = old_index.get(&(checksum.as_str(), size)) {
                    let old_offset = Self::pick_nearest_offset(offsets, last_old_end);
                    Self::push_copy(&mut operations, old_offset, new_cursor, len);
                    last_old_end = old_offset.saturating_add(len);
                    reused_bytes = reused_bytes.saturating_add(len);
                } else {
                    Self::push_download(&mut operations, new_cursor, len);
                    download_bytes = download_bytes.saturating_add(len);
                }
                new_cursor = new_cursor.saturating_add(len);
            }
            expected_new_offset = new_cursor;
        }

        if options.max_range_gap > 0 {
            Self::coalesce_small_gaps(
                &mut operations,
                options.max_range_gap,
                &mut reused_bytes,
                &mut download_bytes,
            );
        }

        Ok(Self {
            operations,
            total_size: new_map.total_size(),
            reused_bytes,
            download_bytes,
        })
    }

    /// Returns the number of discrete HTTP byte ranges needed by this plan.
    pub fn range_count(&self) -> usize {
        self.operations
            .iter()
            .filter(|op| matches!(op, PlanOperation::Download { .. }))
            .count()
    }

    /// Returns true when the delta is too large to justify differential patching.
    pub fn should_fallback_to_full(&self, threshold: f64) -> bool {
        if self.total_size == 0 || self.reused_bytes == 0 {
            return true;
        }
        let ratio = self.download_bytes as f64 / self.total_size as f64;
        ratio >= threshold
    }

    /// Returns the percentage of artifact bytes reused locally.
    pub fn saved_percent(&self) -> f64 {
        if self.total_size == 0 {
            return 0.0;
        }
        (self.reused_bytes as f64 / self.total_size as f64) * 100.0
    }

    fn index_old_blocks(old_map: &BlockMap) -> HashMap<(&str, u32), Vec<u64>> {
        let mut index: HashMap<(&str, u32), Vec<u64>> = HashMap::new();
        for file in &old_map.files {
            let mut cursor = file.offset;
            for (checksum, &size) in file.checksums.iter().zip(&file.sizes) {
                index
                    .entry((checksum.as_str(), size))
                    .or_default()
                    .push(cursor);
                cursor = cursor.saturating_add(u64::from(size));
            }
        }
        index
    }

    fn pick_nearest_offset(offsets: &[u64], cursor: u64) -> u64 {
        let pos = offsets.partition_point(|&candidate| candidate < cursor);
        match (pos.checked_sub(1).map(|i| offsets[i]), offsets.get(pos)) {
            (Some(prev), Some(&next)) => {
                if next.abs_diff(cursor) < prev.abs_diff(cursor) {
                    next
                } else {
                    prev
                }
            }
            (Some(prev), None) => prev,
            (None, Some(&next)) => next,
            (None, None) => 0,
        }
    }

    fn push_copy(
        operations: &mut Vec<PlanOperation>,
        old_offset: u64,
        new_offset: u64,
        length: u64,
    ) {
        if let Some(PlanOperation::Copy {
            old_offset: prev_old,
            new_offset: prev_new,
            length: prev_len,
        }) = operations.last_mut()
            && prev_old.saturating_add(*prev_len) == old_offset
            && prev_new.saturating_add(*prev_len) == new_offset
        {
            *prev_len = prev_len.saturating_add(length);
            return;
        }
        operations.push(PlanOperation::Copy {
            old_offset,
            new_offset,
            length,
        });
    }

    fn push_download(operations: &mut Vec<PlanOperation>, offset: u64, length: u64) {
        if let Some(PlanOperation::Download {
            offset: prev_offset,
            length: prev_len,
        }) = operations.last_mut()
            && prev_offset.saturating_add(*prev_len) == offset
        {
            *prev_len = prev_len.saturating_add(length);
            return;
        }
        operations.push(PlanOperation::Download { offset, length });
    }

    fn coalesce_small_gaps(
        operations: &mut Vec<PlanOperation>,
        max_gap: u64,
        reused_bytes: &mut u64,
        download_bytes: &mut u64,
    ) {
        let mut merged: Vec<PlanOperation> = Vec::with_capacity(operations.len());
        let mut index = 0;
        while index < operations.len() {
            if index + 1 < operations.len()
                && let Some(PlanOperation::Download {
                    offset: left_off,
                    length: left_len,
                }) = merged.last()
                && let (
                    PlanOperation::Copy {
                        new_offset: gap_off,
                        length: gap_len,
                        ..
                    },
                    PlanOperation::Download {
                        offset: right_off,
                        length: right_len,
                    },
                ) = (&operations[index], &operations[index + 1])
                && *gap_len <= max_gap
                && left_off.saturating_add(*left_len) == *gap_off
                && gap_off.saturating_add(*gap_len) == *right_off
            {
                let gap = *gap_len;
                let extra = gap.saturating_add(*right_len);
                if let Some(PlanOperation::Download { length, .. }) = merged.last_mut() {
                    *length = length.saturating_add(extra);
                }
                *reused_bytes = reused_bytes.saturating_sub(gap);
                *download_bytes = download_bytes.saturating_add(gap);
                index += 2;
                continue;
            }
            match operations[index].clone() {
                PlanOperation::Copy {
                    old_offset,
                    new_offset,
                    length,
                } => Self::push_copy(&mut merged, old_offset, new_offset, length),
                PlanOperation::Download { offset, length } => {
                    Self::push_download(&mut merged, offset, length);
                }
            }
            index += 1;
        }
        *operations = merged;
    }
}
