use super::{DownloadPlan, PlanOperation, PlanOptions};
use crate::Error;
use crate::blockmap::{BlockMap, BlockMapFile};

fn make_map(blocks: &[(&str, u32)]) -> BlockMap {
    BlockMap {
        version: "2".into(),
        files: vec![BlockMapFile {
            name: "file".into(),
            offset: 0,
            checksums: blocks.iter().map(|(h, _)| (*h).to_string()).collect(),
            sizes: blocks.iter().map(|(_, s)| *s).collect(),
        }],
    }
}

#[test]
fn coalesces_contiguous_copy_and_download_segments() {
    let old_map = make_map(&[("a", 100), ("b", 200), ("c", 300), ("d", 400)]);
    let new_map = make_map(&[("a", 100), ("b", 200), ("x", 150), ("y", 150), ("d", 400)]);

    let plan = DownloadPlan::build(&old_map, &new_map, PlanOptions::default()).expect("plan");
    assert_eq!(plan.total_size, 1000);
    assert_eq!(plan.reused_bytes, 700);
    assert_eq!(plan.download_bytes, 300);
    assert_eq!(plan.range_count(), 1);
    assert_eq!(
        plan.operations,
        vec![
            PlanOperation::Copy {
                old_offset: 0,
                new_offset: 0,
                length: 300,
            },
            PlanOperation::Download {
                offset: 300,
                length: 300,
            },
            PlanOperation::Copy {
                old_offset: 600,
                new_offset: 600,
                length: 400,
            },
        ]
    );
}

#[test]
fn handles_inserted_prefix_and_shifted_reused_blocks() {
    let old_map = make_map(&[("a", 100), ("b", 200), ("c", 300)]);
    let new_map = make_map(&[("hdr", 50), ("a", 100), ("b", 200), ("c", 300)]);

    let plan = DownloadPlan::build(&old_map, &new_map, PlanOptions::default()).expect("plan");
    assert_eq!(plan.reused_bytes, 600);
    assert_eq!(plan.download_bytes, 50);
    assert_eq!(
        plan.operations,
        vec![
            PlanOperation::Download {
                offset: 0,
                length: 50,
            },
            PlanOperation::Copy {
                old_offset: 0,
                new_offset: 50,
                length: 600,
            },
        ]
    );
}

#[test]
fn coalesces_chained_small_copy_gaps_and_preserves_larger_gaps() {
    let old_map = make_map(&[
        ("a", 500),
        ("g1", 32),
        ("b", 500),
        ("g2", 64),
        ("c", 500),
        ("big", 65),
        ("d", 500),
    ]);
    let new_map = make_map(&[
        ("x", 500),
        ("g1", 32),
        ("y", 500),
        ("g2", 64),
        ("z", 500),
        ("big", 65),
        ("w", 500),
    ]);

    let plan =
        DownloadPlan::build(&old_map, &new_map, PlanOptions { max_range_gap: 64 }).expect("plan");
    assert_eq!(plan.range_count(), 2);
    assert_eq!(plan.reused_bytes, 65);
    assert_eq!(plan.download_bytes, 2096);
    assert_eq!(
        plan.operations,
        vec![
            PlanOperation::Download {
                offset: 0,
                length: 1596,
            },
            PlanOperation::Copy {
                old_offset: 1596,
                new_offset: 1596,
                length: 65,
            },
            PlanOperation::Download {
                offset: 1661,
                length: 500,
            },
        ]
    );
}

#[test]
fn picks_nearest_duplicate_block_for_sequential_copy_coalescing() {
    let old_map = make_map(&[("dup", 100), ("mid", 200), ("dup", 100), ("tail", 300)]);
    let new_map = make_map(&[("mid", 200), ("dup", 100), ("tail", 300)]);

    let plan = DownloadPlan::build(&old_map, &new_map, PlanOptions::default()).expect("plan");
    assert_eq!(plan.reused_bytes, 600);
    assert_eq!(plan.download_bytes, 0);
    assert_eq!(
        plan.operations,
        vec![PlanOperation::Copy {
            old_offset: 100,
            new_offset: 0,
            length: 600,
        }]
    );
}

#[test]
fn evaluates_full_fallback_threshold_and_boundary_cases() {
    let old_map = make_map(&[("a", 100), ("b", 900)]);
    let new_map = make_map(&[("a", 100), ("x", 900)]);
    let plan = DownloadPlan::build(&old_map, &new_map, PlanOptions::default()).expect("plan");
    assert!(plan.should_fallback_to_full(0.85));
    assert!(plan.should_fallback_to_full(0.90));
    assert!(!plan.should_fallback_to_full(0.95));

    let identical = DownloadPlan::build(&old_map, &old_map, PlanOptions::default()).expect("same");
    assert_eq!(identical.download_bytes, 0);
    assert_eq!(identical.saved_percent(), 100.0);
    assert!(!identical.should_fallback_to_full(0.85));

    let disjoint = DownloadPlan::build(&old_map, &make_map(&[("z", 1000)]), PlanOptions::default())
        .expect("disjoint");
    assert_eq!(disjoint.reused_bytes, 0);
    assert!(disjoint.should_fallback_to_full(1.0));

    let invalid = BlockMap {
        version: "9".into(),
        files: vec![],
    };
    assert!(matches!(
        DownloadPlan::build(&invalid, &old_map, PlanOptions::default()),
        Err(Error::InvalidBlockMap(_))
    ));
}
