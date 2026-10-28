//! The change report on the fixture pair: one function crosses the
//! cutoff, one hotspot is split into three simple functions, one moves
//! between files.

mod common;

use codemap_slop::diff::{FindingKind, change_report};
use codemap_slop::metrics::{callables, counts, erosion};
use codemap_slop::render::change_text;
use codemap_slop::{measured, measured_pair, policy};

use common::{base_snapshot, fn_id, head_snapshot, head_with_clone};

#[test]
fn the_fixtures_are_admitted() {
    let base = base_snapshot();
    let head = head_snapshot(30);
    measured(&base).expect("base admitted");
    measured(&head).expect("head admitted");
    measured_pair(&base, &head).expect("pair admitted");
}

#[test]
fn a_crossing_a_split_and_a_move_are_reported_as_such() {
    let base = base_snapshot();
    let head = head_snapshot(30);
    let (b, h) = measured_pair(&base, &head).expect("pair");
    let report = change_report(&b, &h, 10);

    let kinds: Vec<(FindingKind, &str)> = report
        .findings
        .iter()
        .map(|f| (f.kind, f.name.as_str()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (FindingKind::CrossedAboveCutoff, "grows"),
            (FindingKind::RemovedAboveCutoff, "big"),
        ],
        "grows crossed up, big is gone; the three small halves and the move are not findings"
    );

    let grows = &report.findings[0];
    assert_eq!(grows.cc_delta, 4);
    assert_eq!(grows.sloc_delta, 4);
    assert_eq!(grows.moved_from, None);
    assert_eq!(
        (grows.fan_out.base, grows.fan_out.head),
        (Some(0), Some(1)),
        "grows now calls small; both snapshots are semantic, so zero is a real zero"
    );
    assert_eq!(
        (grows.fan_in.base, grows.fan_in.head),
        (Some(0), Some(0)),
        "the contains edge from the file is structure, not a caller"
    );
    assert_eq!(report.verified_calls.base, Some(0));
    assert_eq!(report.verified_calls.head, Some(1));

    assert_eq!(report.added_callables, 3, "big_a, big_b, big_c");
    assert_eq!(report.removed_callables, 1, "big");
    assert_eq!(report.moved_callables, 1, "moved, paired across files");
    assert_eq!(report.new_high_cc_callables(), 1);
}

#[test]
fn a_moved_callable_that_got_harder_is_paired_not_reborn() {
    let base = base_snapshot();
    let mut head = head_snapshot(30);
    let moved = fn_id("src/b.rs", "moved");
    head.measures
        .iter_mut()
        .find(|m| m.node_id == moved)
        .expect("moved measure")
        .cc = 4;
    let (b, h) = measured_pair(&base, &head).expect("pair");
    let report = change_report(&b, &h, 10);
    let finding = report
        .findings
        .iter()
        .find(|f| f.name == "moved")
        .expect("moved is a finding once it changed");
    assert_eq!(finding.kind, FindingKind::Harder);
    assert_eq!(
        finding.moved_from.as_deref(),
        Some(fn_id("src/a.rs", "moved").as_str())
    );
    assert_eq!(finding.cc_delta, 1);
    assert_eq!(report.moved_callables, 1);
}

#[test]
fn the_split_lowers_erosion_while_the_hotspot_count_stands() {
    let base = base_snapshot();
    let head = head_snapshot(30);
    let (b, h) = measured_pair(&base, &head).expect("pair");
    let report = change_report(&b, &h, 10);

    assert!(
        report.head.erosion < report.base.erosion,
        "the split lowered the ratio"
    );
    assert_eq!(report.base.counts.high_cc, 2, "monster and big");
    assert_eq!(report.head.counts.high_cc, 2, "monster and grows");
    assert_eq!(report.head.counts.max_cc, 30, "monster untouched");
    assert_eq!(
        report.ratio_flag, None,
        "nothing worsened, so the ratio stands"
    );

    let text = change_text(&report);
    let head_line = text
        .lines()
        .find(|line| line.starts_with("head: erosion"))
        .expect("head ratio line");
    assert!(
        head_line.contains("over cc 10: 2") && head_line.contains("max cc 30"),
        "the ratio prints next to its counts on one line: {head_line}"
    );
    assert!(
        text.contains("monster"),
        "the untouched hotspot is still listed"
    );
}

#[test]
fn an_improved_ratio_over_a_worse_hotspot_is_flagged() {
    let base = base_snapshot();
    let head = head_snapshot(31);
    let (b, h) = measured_pair(&base, &head).expect("pair");
    let report = change_report(&b, &h, 10);
    assert!(report.head.erosion < report.base.erosion);
    let flag = report.ratio_flag.as_deref().expect("flagged");
    assert!(flag.contains("max cc rose 30 -> 31"), "{flag}");
    assert!(change_text(&report).contains("flag: ratio-never-alone"));
}

#[test]
fn erosion_is_the_share_of_mass_above_the_cutoff() {
    let base = base_snapshot();
    let all = callables(&base);
    let c = counts(&all);
    let expected_total: f64 = [(30, 100), (15, 50), (8, 20), (3, 10), (1, 5)]
        .iter()
        .map(|&(cc, sloc)| policy::mass(cc, sloc))
        .sum();
    let expected_above = policy::mass(30, 100) + policy::mass(15, 50);
    assert!((c.mass_total - expected_total).abs() < 1e-9);
    assert!((c.mass_above_cutoff - expected_above).abs() < 1e-9);
    assert!((erosion(&c) - expected_above / expected_total).abs() < 1e-12);
}

#[test]
fn the_report_is_byte_deterministic() {
    let base = base_snapshot();
    let head = head_snapshot(31);
    let (b, h) = measured_pair(&base, &head).expect("pair");
    let first = serde_json::to_string(&change_report(&b, &h, 10)).expect("json");
    let second = serde_json::to_string(&change_report(&b, &h, 10)).expect("json");
    assert_eq!(first, second);
    assert_eq!(
        change_text(&change_report(&b, &h, 10)),
        change_text(&change_report(&b, &h, 10))
    );
}

#[test]
fn an_introduced_clone_group_is_reported_with_verbosity_and_its_counts() {
    let base = base_snapshot();
    let head = head_with_clone();
    measured(&head).expect("a head with a clone group is admitted");
    let (b, h) = measured_pair(&base, &head).expect("pair");
    let report = change_report(&b, &h, 10);

    assert_eq!(report.base.clones.unwrap().groups, 0);
    assert_eq!(report.head.clones.unwrap().groups, 1);
    assert_eq!(report.head.clones.unwrap().clone_sloc, 32);
    assert_eq!(report.head.clones.unwrap().largest_group, 2);
    let expected = 32.0 / f64::from(u32::try_from(report.head.loc).expect("fixture loc fits u32"));
    assert!((report.head.verbosity_clone.unwrap() - expected).abs() < 1e-12);
    assert_eq!(report.base.verbosity_clone, Some(0.0));

    assert_eq!(report.clone_groups_introduced.as_ref().unwrap().len(), 1);
    assert_eq!(report.clone_groups_removed, Some(0));
    let introduced = &report.clone_groups_introduced.as_ref().unwrap()[0];
    assert_eq!(introduced.occurrences, 2);
    assert_eq!(introduced.tokens, 60);
    assert_eq!(introduced.first, "src/a.rs:120-137");
    assert_eq!(
        report.moved_callables, 1,
        "the move is listed beside the clones, not among them"
    );

    let text = change_text(&report);
    let line = text
        .lines()
        .find(|l| l.starts_with("clones:"))
        .expect("clones line");
    assert!(line.contains("introduced 1"), "{line}");
    assert!(line.contains("clone lines 0 -> 32"), "{line}");
    assert!(line.contains("verbosity_clone 0.000 ->"), "{line}");
    assert!(line.contains("1 moved callable"), "{line}");
    assert!(
        text.contains(
            "clone:0123456789abcdef01234567  2 occurrences  60 tokens  18 lines  src/a.rs:120-137"
        ),
        "{text}"
    );

    // The same head against itself introduces nothing.
    let (h1, h2) = measured_pair(&head, &head).expect("pair");
    let same = change_report(&h1, &h2, 10);
    assert!(same.clone_groups_introduced.as_ref().unwrap().is_empty());
    assert_eq!(same.clone_groups_removed, Some(0));
}
