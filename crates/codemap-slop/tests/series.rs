//! `series` over a measured history walk: one line per commit, the slope
//! per author label, and the fail-closed cases.

mod common;

use codemap_graph::schema::Snapshot;
use codemap_slop::policy::{ALL_FILES, Scope};
use codemap_slop::series::{MEASURED_LISTING, series, series_text};

use common::{
    base_snapshot, foreign_policy_snapshot, head_snapshot, snapshot_with_run_once_code,
    unmeasured_snapshot,
};

/// Write a history directory the way `stele code history --measures` does:
/// `measures/<seq>-<sha>.json` per commit and one listing line each.
fn history_dir(entries: &[(&str, &str, &Snapshot)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("measures")).expect("measures dir");
    let mut listing = String::new();
    for (seq, (commit, author, snapshot)) in entries.iter().enumerate() {
        let rel = format!("measures/{seq:05}-{}.json", &commit[..12]);
        std::fs::write(
            dir.path().join(&rel),
            serde_json::to_string(snapshot).expect("serialize"),
        )
        .expect("write snapshot");
        listing.push_str(
            &serde_json::json!({
                "seq": seq,
                "commit": commit,
                "ts": 946_684_800 + i64::try_from(seq).expect("small"),
                "author": author,
                "author_email": format!("{}@example.invalid", author.to_lowercase()),
                "measures_policy": snapshot.identity.measures_policy,
                "snapshot_id": snapshot.identity.snapshot_id,
                "snapshot": rel,
            })
            .to_string(),
        );
        listing.push('\n');
    }
    std::fs::write(dir.path().join(MEASURED_LISTING), listing).expect("write listing");
    dir
}

const C0: &str = "b0000000b0000000b0000000b0000000b0000000";
const C1: &str = "a1111111a1111111a1111111a1111111a1111111";
const C2: &str = "c2222222c2222222c2222222c2222222c2222222";

#[test]
fn one_line_per_commit_with_counts_and_the_slope_per_author_label() {
    let base = base_snapshot();
    let head = head_snapshot(30);
    let worse = head_snapshot(31);
    let dir = history_dir(&[
        (C0, "Alice", &base),
        (C1, "Alice", &head),
        (C2, "Bob", &worse),
    ]);

    let doc = series(dir.path(), None, &ALL_FILES).expect("series");
    assert_eq!(doc.commits, 3);
    assert_eq!(doc.policy, "cc-sloc-clones-form/rust/1");
    assert_eq!(doc.language, "rust");
    assert_eq!(doc.window, 3);
    let seqs: Vec<usize> = doc.points.iter().map(|p| p.seq).collect();
    assert_eq!(seqs, [0, 1, 2]);
    assert!(
        doc.points.iter().all(|p| p.verbosity.is_none()),
        "verbosity proper is absent until P3, never zero"
    );
    // The split-into-three head: erosion falls while callables over the
    // cutoff stay at two; the counts travel with every point.
    assert!(doc.points[1].erosion < doc.points[0].erosion);
    assert_eq!(doc.points[0].counts.high_cc, 2);
    assert_eq!(doc.points[1].counts.high_cc, 2);
    assert_eq!(doc.points[2].counts.max_cc, 31);

    let alice = doc
        .slopes
        .iter()
        .find(|s| s.author == "Alice")
        .expect("alice slope");
    assert_eq!((alice.commits, alice.first_seq, alice.last_seq), (2, 0, 1));
    let expected = doc.points[1].erosion - doc.points[0].erosion;
    assert!(
        (alice.erosion.expect("two commits fit a slope") - expected).abs() < 1e-12,
        "two points: the slope is the difference per commit"
    );
    let bob = doc
        .slopes
        .iter()
        .find(|s| s.author == "Bob")
        .expect("bob slope");
    assert_eq!(bob.commits, 1);
    assert_eq!(bob.erosion, None, "one commit fits nothing");

    let text = series_text(&doc);
    assert!(text.contains("verbosity n/a"), "{text}");
    assert!(text.contains("over cc 10: 2"), "{text}");
    assert!(
        text.contains("the operator decides what it means"),
        "{text}"
    );
    let json = serde_json::to_value(&doc).expect("json");
    assert!(json["points"][0]["verbosity"].is_null());
}

#[test]
fn the_window_limits_the_fit_and_never_the_lines() {
    let base = base_snapshot();
    let head = head_snapshot(30);
    let worse = head_snapshot(31);
    let dir = history_dir(&[
        (C0, "Alice", &base),
        (C1, "Alice", &head),
        (C2, "Alice", &worse),
    ]);
    let doc = series(dir.path(), Some(2), &ALL_FILES).expect("series");
    assert_eq!(doc.points.len(), 3, "every commit is printed");
    assert_eq!(doc.window, 2);
    let alice = &doc.slopes[0];
    assert_eq!((alice.commits, alice.first_seq, alice.last_seq), (2, 1, 2));
    let wide = series(dir.path(), Some(50), &ALL_FILES).expect("series");
    assert_eq!(wide.window, 3, "a window past the walk is the walk");
}

#[test]
fn a_walk_without_measures_is_refused_with_the_commit_named() {
    let base = base_snapshot();
    let plain = unmeasured_snapshot();
    let dir = history_dir(&[(C0, "Alice", &base), (C1, "Alice", &plain)]);
    let error = format!(
        "{:#}",
        series(dir.path(), None, &ALL_FILES).expect_err("refused")
    );
    assert!(error.contains("unmeasured snapshot"), "{error}");
    assert!(error.contains(C1), "{error}");
}

#[test]
fn mixed_policies_are_refused() {
    let base = base_snapshot();
    let foreign = foreign_policy_snapshot();
    let dir = history_dir(&[(C0, "Alice", &base), (C1, "Alice", &foreign)]);
    let error = format!(
        "{:#}",
        series(dir.path(), None, &ALL_FILES).expect_err("refused")
    );
    // The foreign policy is refused at admission, before any mismatch check
    // could run: no snapshot this crate cannot read joins a series.
    assert!(error.contains("unsupported policy"), "{error}");
}

#[test]
fn an_empty_or_missing_walk_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let error = format!(
        "{:#}",
        series(dir.path(), None, &ALL_FILES).expect_err("refused")
    );
    assert!(error.contains("no measured walk"), "{error}");
    assert!(error.contains("stele code history --measures"), "{error}");

    std::fs::write(dir.path().join(MEASURED_LISTING), "\n").expect("empty listing");
    let error = format!(
        "{:#}",
        series(dir.path(), None, &ALL_FILES).expect_err("refused")
    );
    assert!(error.contains("empty walk"), "{error}");
}

#[test]
fn a_listed_snapshot_that_is_missing_is_refused() {
    let base = base_snapshot();
    let dir = history_dir(&[(C0, "Alice", &base)]);
    std::fs::remove_file(
        dir.path()
            .join(format!("measures/00000-{}.json", &C0[..12])),
    )
    .expect("remove");
    let error = format!(
        "{:#}",
        series(dir.path(), None, &ALL_FILES).expect_err("refused")
    );
    assert!(error.contains("missing snapshot"), "{error}");
    assert!(error.contains(C0), "{error}");
}

#[test]
fn ruby_history_has_erosion_and_clone_points_under_its_own_policy() {
    let base = common::ruby_snapshot(C0, 3);
    let head = common::ruby_snapshot(C1, 12);
    let dir = history_dir(&[(C0, "Alice", &base), (C1, "Alice", &head)]);
    let doc = series(dir.path(), None, &ALL_FILES).expect("Ruby series");
    assert_eq!(doc.policy, "cc-sloc-clones-form/ruby/1");
    assert!(
        doc.points
            .iter()
            .all(|p| p.verbosity_clone == Some(0.0) && p.clones.is_some()),
        "a Ruby snapshot with no clone group reads zero, not absent"
    );
    assert_eq!(doc.slopes[0].erosion, Some(1.0));
    assert_eq!(doc.slopes[0].verbosity_clone, Some(0.0));
    assert!(series_text(&doc).contains("verbosity_clone 0.000 (groups 0, clone lines 0 of"));
}

#[test]
fn a_scoped_series_prints_the_all_files_erosion_beside_each_point() {
    let base = snapshot_with_run_once_code("c000000000000000000000000000000000000000");
    let mut head = snapshot_with_run_once_code("c111111111111111111111111111111111111111");
    for measure in &mut head.measures {
        if measure.node_id == common::fn_id("db/migrate/001_drop.rs", "up") {
            measure.cc = 45;
        }
    }
    let dir = history_dir(&[
        ("c000000000000000000000000000000000000000", "Ada", &base),
        ("c111111111111111111111111111111111111111", "Ada", &head),
    ]);
    let scope = Scope::excluding(["db/migrate/"]).expect("valid scope");
    let doc = series(dir.path(), None, &scope).expect("series");
    assert_eq!(doc.scope, scope);
    assert_eq!(doc.points.len(), 2);
    assert_eq!(
        doc.points[0].erosion.to_bits(),
        doc.points[1].erosion.to_bits(),
        "the migration moved, the scope did not"
    );
    let all: Vec<f64> = doc
        .points
        .iter()
        .map(|p| {
            p.erosion_all_files
                .expect("all-files erosion under a scope")
        })
        .collect();
    assert!(all[1] > all[0], "and the all-files figure says so");
    assert_eq!(doc.points[0].excluded.callables, 1);
    let ada = doc.slopes.iter().find(|s| s.author == "Ada").expect("Ada");
    assert_eq!(ada.erosion, Some(0.0), "the in-scope slope is flat");

    let text = series_text(&doc);
    assert!(text.contains("scope: excluding db/migrate/"), "{text}");
    assert!(text.contains("(all files 0."), "{text}");
    assert!(text.contains("excluded 1 callables in 1 files"), "{text}");

    let plain = series(dir.path(), None, &ALL_FILES).expect("series");
    assert!(plain.points[0].erosion_all_files.is_none());
    assert!(!series_text(&plain).contains("all files"));
}
