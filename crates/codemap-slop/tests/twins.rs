//! Twin pairs in the report and the change report: counted beside the
//! clone figure, never inside it; near pairs first; held to the scope;
//! `n/a`, never zero, without `--twins`.

mod common;

use codemap_slop::diff::change_report;
use codemap_slop::metrics::twins;
use codemap_slop::policy::{Scope, TWINS_NEVER_FOLDED};
use codemap_slop::render::{change_text, snapshot_report, snapshot_text};
use codemap_slop::{measured, measured_in, measured_pair, measured_pair_in};

use common::{
    BASE_COMMIT, HEAD_COMMIT, head_snapshot, head_with_clone_and_twins,
    snapshot_with_run_once_code, snapshot_with_twins,
};

#[test]
fn twins_print_beside_the_clone_figure_near_pairs_first() {
    let snapshot = snapshot_with_twins(BASE_COMMIT);
    let m = measured(&snapshot).expect("admitted");
    let report = snapshot_report(&m, 10);
    let counts = report.summary.twins.expect("twins were computed");
    assert_eq!((counts.pairs, counts.near, counts.exact), (2, 1, 1));
    assert_eq!(report.twins.len(), 2);
    assert!(!report.twins[0].exact, "the edited copy comes first");
    assert_eq!(report.twins[0].b.name, "grows");
    assert!((report.twins[0].containment - 0.9).abs() < 1e-12);
    assert!(report.twins[1].exact);
    assert!(
        !report.twins[1].clone_shared,
        "the fixture has no clone group behind the exact pair"
    );

    let text = snapshot_text(&report);
    let line = text
        .lines()
        .find(|l| l.starts_with("twins:"))
        .expect("twins line");
    assert!(
        line.contains("twin pairs 2  (1 near, 1 exact; 5-token shingles, containment >= 70%, both bodies >= 80 tokens)"),
        "{line}"
    );
    let clones = text
        .lines()
        .find(|l| l.starts_with("clones:"))
        .expect("clones line");
    assert!(
        clones.contains("a clone is a run of 50 or more equal normalised tokens over 5 or more lines, exact or renamed"),
        "the clone line says what a clone is: {clones}"
    );
    assert!(
        !clones.contains("twin"),
        "{TWINS_NEVER_FOLDED}: the twin count is not inside the clone line"
    );
    let rows: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("twin pairs by containment"))
        .skip(1)
        .take(2)
        .collect();
    assert!(
        rows[0].starts_with("  0.90  shared   90 of  100  big  app/core.rs"),
        "{}",
        rows[0]
    );
    assert!(rows[0].contains("<->  grows  app/core.rs"), "{}", rows[0]);
    assert!(
        rows[1].starts_with("  1.00  shared  100 of  100  big"),
        "{}",
        rows[1]
    );
    assert!(
        rows[1].ends_with("exact, also a clone group") || rows[1].ends_with("001_drop.rs:1-42"),
        "{}",
        rows[1]
    );
}

#[test]
fn a_scope_drops_pairs_with_a_side_outside_it_and_prints_both_counts() {
    let snapshot = snapshot_with_twins(BASE_COMMIT);
    let scope = Scope::excluding(["db/migrate/"]).expect("valid scope");
    let m = measured_in(&snapshot, &scope).expect("admitted");
    let report = snapshot_report(&m, 10);
    assert_eq!(report.summary.twins.expect("in scope").pairs, 1);
    assert_eq!(
        report
            .all_files
            .as_ref()
            .and_then(|s| s.twins)
            .expect("all files")
            .pairs,
        2
    );
    assert_eq!(report.twins.len(), 1);
    assert_eq!(report.twins[0].b.name, "grows");
    let text = snapshot_text(&report);
    assert!(text.contains("twins, all files: twin pairs 2"), "{text}");
    assert!(text.contains("twins, in scope: twin pairs 1"), "{text}");
}

#[test]
fn without_twins_the_figure_is_absent_never_zero() {
    let snapshot = snapshot_with_run_once_code(BASE_COMMIT);
    let m = measured(&snapshot).expect("admitted");
    let report = snapshot_report(&m, 10);
    assert!(report.summary.twins.is_none());
    assert!(report.twins.is_empty());
    assert!(twins(&m, 10).is_empty());
    let text = snapshot_text(&report);
    assert!(
        text.contains("twins: twin pairs n/a (not computed; extract with --measures --twins)"),
        "{text}"
    );
    assert!(!text.contains("twin pairs 0"), "{text}");
    let json = serde_json::to_value(&report).expect("json");
    assert!(json["summary"].get("twins").is_none(), "absent, not null");
}

#[test]
fn an_exact_pair_behind_a_clone_group_says_so() {
    let head = head_with_clone_and_twins();
    let m = measured(&head).expect("admitted");
    let report = snapshot_report(&m, 10);
    assert_eq!(report.twins.len(), 1);
    assert!(report.twins[0].exact);
    assert!(report.twins[0].clone_shared);
    assert!(
        snapshot_text(&report).contains("big_b  src/a.rs:140-157  exact, also a clone group"),
        "{}",
        snapshot_text(&report)
    );
}

#[test]
fn the_change_report_lists_introduced_pairs_and_needs_twins_on_both_sides() {
    let base = snapshot_with_run_once_code(BASE_COMMIT);
    let head = snapshot_with_twins(HEAD_COMMIT);
    let (b, h) = measured_pair(&base, &head).expect("pair");
    let report = change_report(&b, &h, 10);
    assert!(
        report.twin_pairs_introduced.is_none(),
        "the base has no twins"
    );
    assert!(report.twin_pairs_removed.is_none());
    assert!(
        change_text(&report).contains("twins: twin pairs n/a (not computed on both sides"),
        "{}",
        change_text(&report)
    );

    let mut base = snapshot_with_twins(BASE_COMMIT);
    base.twin_pairs.truncate(1);
    base.identity.snapshot_id = base.identity.compute_snapshot_id();
    let (b, h) = measured_pair(&base, &head).expect("pair");
    let report = change_report(&b, &h, 10);
    let introduced = report
        .twin_pairs_introduced
        .as_ref()
        .expect("both sides carry twins");
    assert_eq!(introduced.len(), 1);
    assert_eq!(introduced[0].b.name, "grows");
    assert_eq!(report.twin_pairs_removed, Some(0));
    let text = change_text(&report);
    assert!(
        text.contains("twins: pairs 1 -> 2  introduced 1  removed 0  (near 0 -> 1, exact 1 -> 1)"),
        "{text}"
    );
    assert!(text.contains("  0.90  shared   90 of  100  big"), "{text}");

    // Held to a scope, a pair with a side outside it is not introduced.
    let scope = Scope::excluding(["db/migrate/"]).expect("valid scope");
    let mut head_only_migration = snapshot_with_twins(HEAD_COMMIT);
    head_only_migration.twin_pairs.truncate(1);
    head_only_migration.identity.snapshot_id = head_only_migration.identity.compute_snapshot_id();
    let mut empty_base = snapshot_with_twins(BASE_COMMIT);
    empty_base.twin_pairs.clear();
    empty_base.identity.snapshot_id = empty_base.identity.compute_snapshot_id();
    let (b, h) = measured_pair_in(&empty_base, &head_only_migration, &scope).expect("pair");
    let report = change_report(&b, &h, 10);
    assert_eq!(report.twin_pairs_introduced.as_ref().map(Vec::len), Some(0));

    // The unrelated Rust fixture keeps its bytes shape: no twins, n/a.
    let plain = head_snapshot(30);
    let m = measured(&plain).expect("admitted");
    assert!(snapshot_report(&m, 3).summary.twins.is_none());
}
