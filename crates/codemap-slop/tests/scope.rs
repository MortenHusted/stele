//! A report held to a scope: run-once code left out of the figures the
//! change is held to, never out of the page.

mod common;

use codemap_slop::diff::change_report;
use codemap_slop::metrics::{per_file, summary, summary_all_files};
use codemap_slop::policy::{ALL_FILES, Scope};
use codemap_slop::render::{change_text, snapshot_report, snapshot_text};
use codemap_slop::{measured, measured_in, measured_pair_in};

use common::{BASE_COMMIT, HEAD_COMMIT, fn_id, snapshot_with_run_once_code};

#[test]
fn a_scope_leaves_run_once_code_out_of_the_figures_and_prints_both() {
    let snapshot = snapshot_with_run_once_code(BASE_COMMIT);
    let scope = Scope::excluding(["db/migrate/", "script/"]).expect("valid scope");
    let m = measured_in(&snapshot, &scope).expect("admitted");

    let in_scope = summary(&m);
    assert_eq!(in_scope.counts.callables, 3, "big, grows, small");
    assert_eq!(in_scope.counts.high_cc, 1, "big");
    assert_eq!(in_scope.counts.max_cc, 15);
    assert_eq!(in_scope.files, 1);
    assert_eq!(in_scope.excluded.callables, 2, "up and fix");
    assert_eq!(in_scope.excluded.files, 2);

    let all = summary_all_files(&m);
    assert_eq!(all.counts.callables, 5);
    assert_eq!(all.counts.high_cc, 3, "big, up, fix");
    assert_eq!(
        all.counts.max_cc, 30,
        "the migration is still the max over every file"
    );
    assert_eq!(all.files, 3);
    assert_eq!(all.excluded.callables, 0);
    assert!(
        in_scope.erosion < all.erosion,
        "leaving the migration out lowers the ratio, and the line above says by how much"
    );

    let report = snapshot_report(&m, 10);
    let names: Vec<&str> = report.hotspots.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["big", "grows", "small"], "hotspots are in scope");
    let files: Vec<String> = per_file(&m).into_iter().map(|g| g.key).collect();
    assert_eq!(files, ["app/core.rs"]);
    assert_eq!(report.scope, scope);
    assert!(report.all_files.is_some());

    let text = snapshot_text(&report);
    let all_line = text
        .lines()
        .find(|l| l.starts_with("erosion, all files:"))
        .expect("all-files line");
    assert!(
        all_line.contains("over cc 10: 3") && all_line.contains("max cc 30"),
        "{all_line}"
    );
    let scope_line = text
        .lines()
        .find(|l| l.starts_with("erosion, in scope:"))
        .expect("in-scope line");
    assert!(
        scope_line.contains("over cc 10: 1") && scope_line.contains("max cc 15"),
        "{scope_line}"
    );
    assert!(
        scope_line.contains("excluded 2 callables in 2 files under db/migrate/, script/"),
        "{scope_line}"
    );
    assert!(
        !text.contains("001_drop"),
        "an excluded hotspot is not listed: {text}"
    );
}

#[test]
fn without_a_scope_the_report_is_what_it_was() {
    let snapshot = snapshot_with_run_once_code(BASE_COMMIT);
    let m = measured(&snapshot).expect("admitted");
    assert_eq!(*m.scope, ALL_FILES);
    let report = snapshot_report(&m, 10);
    assert!(report.all_files.is_none(), "one figure, printed once");
    assert_eq!(report.summary.excluded.callables, 0);
    let text = snapshot_text(&report);
    assert!(text.contains("\nerosion: erosion "), "{text}");
    assert!(!text.contains("in scope"), "{text}");
    assert!(
        text.contains("001_drop"),
        "the migration is a hotspot over every file"
    );
}

#[test]
fn a_prefix_that_excludes_nothing_is_refused_by_name() {
    let snapshot = snapshot_with_run_once_code(BASE_COMMIT);
    let scope = Scope::excluding(["lib/"]).expect("well formed");
    let error = measured_in(&snapshot, &scope).expect_err("refused");
    assert!(
        error
            .to_string()
            .starts_with("scope prefix `lib/` matches no file"),
        "{error}"
    );
}

#[test]
fn a_scope_that_excludes_every_file_is_refused() {
    let snapshot = snapshot_with_run_once_code(BASE_COMMIT);
    let scope = Scope::excluding(["app/", "db/", "script/"]).expect("well formed");
    let error = measured_in(&snapshot, &scope).expect_err("refused");
    assert!(
        error.to_string().starts_with("scope excludes every file"),
        "{error}"
    );
}

#[test]
fn the_change_report_holds_both_sides_to_the_scope() {
    let base = snapshot_with_run_once_code(BASE_COMMIT);
    let mut head = snapshot_with_run_once_code(HEAD_COMMIT);
    // The migration gets worse and `grows` crosses the cutoff.
    for measure in &mut head.measures {
        if measure.node_id == fn_id("db/migrate/001_drop.rs", "up") {
            measure.cc = 45;
        }
        if measure.node_id == fn_id("app/core.rs", "grows") {
            measure.cc = 12;
        }
    }
    let scope = Scope::excluding(["db/migrate/"]).expect("valid scope");
    let (b, h) = measured_pair_in(&base, &head, &scope).expect("pair");
    let report = change_report(&b, &h, 10);

    let names: Vec<&str> = report.findings.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        ["grows"],
        "the migration's move is outside the scope, so it is no finding"
    );
    assert_eq!(report.new_high_cc_callables(), 1);
    assert_eq!(report.head.counts.max_cc, 15);
    let all = report.all_files.as_ref().expect("all-files summaries");
    assert_eq!(all.base.counts.max_cc, 30);
    assert_eq!(
        all.head.counts.max_cc, 45,
        "and the page still shows it moved"
    );
    assert!(
        report
            .hotspots
            .iter()
            .all(|h| h.hotspot.rel_path != "db/migrate/001_drop.rs"),
        "hotspots are in scope"
    );

    let text = change_text(&report);
    assert!(text.contains("head, all files: erosion"), "{text}");
    assert!(text.contains("max cc 45"), "{text}");
    assert!(
        text.contains("head, in scope: erosion") && text.contains("max cc 15"),
        "{text}"
    );
    assert!(
        text.contains("excluded 1 callables in 1 files under db/migrate/"),
        "{text}"
    );
}
