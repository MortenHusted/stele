//! `report` on one snapshot and `gate` over the fixture pair.

mod common;

use codemap_graph::schema::{CallSite, CallSiteCategory, Span};
use codemap_slop::diff::change_report;
use codemap_slop::gate::{Budget, evaluate, parse_budget};
use codemap_slop::measured_pair;
use codemap_slop::metrics::{SiteCounts, call_sites, per_cluster, per_file};
use codemap_slop::render::{snapshot_report, snapshot_text};
use codemap_slop::{measured, policy};

use common::{base_snapshot, head_snapshot};

#[test]
fn the_report_prints_the_ratio_with_its_counts_and_the_hotspots() {
    let base = base_snapshot();
    let m = measured(&base).expect("admitted");
    let report = snapshot_report(&m, 3);
    assert_eq!(report.policy, "cc-sloc-clones-form/rust/1");
    assert_eq!(report.summary.counts.callables, 5);
    assert_eq!(report.summary.counts.high_cc, 2);
    assert_eq!(report.summary.counts.max_cc, 30);
    assert_eq!(
        report.summary.loc,
        100 + 50 + 20 + 10 + 5 + 4 + 4,
        "file sloc: callable sloc plus four top-level lines per file"
    );
    assert_eq!(report.summary.files, 2);
    let names: Vec<&str> = report.hotspots.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["monster", "big", "grows"]);

    let text = snapshot_text(&report);
    let line = text
        .lines()
        .find(|l| l.starts_with("erosion:"))
        .expect("erosion line");
    assert!(line.contains("over cc 10: 2"), "{line}");
    assert!(line.contains("max cc 30"), "{line}");
    assert!(
        text.contains("src/a.rs:1-102"),
        "hotspot carries file and span"
    );
    assert!(
        text.contains("every cluster is one callable"),
        "a snapshot without calls edges says its clusters are degenerate"
    );
}

#[test]
fn per_file_and_per_cluster_erosion_put_the_heaviest_first() {
    let base = base_snapshot();
    let m = measured(&base).expect("admitted");
    let files = per_file(&m);
    assert_eq!(files.len(), 1, "src/b.rs holds no callable in the base");
    assert_eq!(files[0].key, "src/a.rs");
    assert_eq!(files[0].counts.high_cc, 2);

    let clusters = per_cluster(&m);
    assert!(!clusters.is_empty());
    let total: usize = clusters.iter().map(|c| c.counts.callables).sum();
    assert_eq!(total, 5, "every callable sits in exactly one cluster");
    for pair in clusters.windows(2) {
        assert!(pair[0].counts.mass_above_cutoff >= pair[1].counts.mass_above_cutoff);
    }
}

#[test]
fn the_gate_breaches_on_a_new_high_cc_callable_and_passes_within_budget() {
    let base = base_snapshot();
    let head = head_snapshot(30);
    let (b, h) = measured_pair(&base, &head).expect("pair");
    let report = change_report(&b, &h, 10);

    let strict = parse_budget("max_new_high_cc_callables = 0\n").expect("budget");
    let breaches = evaluate(&report, &strict);
    assert_eq!(breaches.len(), 1);
    assert_eq!(breaches[0].key, "max_new_high_cc_callables");
    assert!((breaches[0].actual - 1.0).abs() < f64::EPSILON);

    let loose = Budget {
        max_erosion_delta: Some(0.0),
        max_loc_delta: Some(100),
        max_new_high_cc_callables: Some(1),
        exclude: Vec::new(),
        targets: Vec::new(),
    };
    assert!(
        evaluate(&report, &loose).is_empty(),
        "erosion fell, loc grew by less than 100"
    );

    let tight_loc = parse_budget("max_loc_delta = 0\n").expect("budget");
    let breaches = evaluate(&report, &tight_loc);
    assert_eq!(breaches.len(), 1);
    assert_eq!(breaches[0].key, "max_loc_delta");
    assert!(breaches[0].actual > 0.0);
}

#[test]
fn the_constants_are_the_plan_values() {
    assert_eq!(policy::HIGH_CC_CUTOFF, 10);
    assert_eq!(policy::HUB_FAN_IN, 8);
    assert_eq!(policy::RATIO_NEVER_ALONE, "ratio-never-alone");
    assert_eq!(
        policy::COUPLING_VERIFIED_NEVER_ALONE,
        "coupling-verified-never-alone"
    );
}

#[test]
fn verified_calls_print_beside_the_sites_the_resolver_left_unverified() {
    let head = common::head_with_call_sites();
    let m = measured(&head).expect("a head with call sites is admitted");
    let sites = call_sites(&head);

    let grows = &sites[&common::fn_id("src/a.rs", "grows")];
    assert_eq!(grows.verified_out, Some(1), "the calls edge into small");
    assert_eq!(grows.verified_in, Some(0));
    assert_eq!(
        grows.sites,
        SiteCounts {
            candidate: 1,
            external: 1,
            unresolved: 1,
            observed: 0,
            ..SiteCounts::default()
        },
        "the resolved site is the edge, so it is not counted twice"
    );

    let small = &sites[&common::fn_id("src/a.rs", "small")];
    assert_eq!(small.verified_in, Some(1), "called by grows");
    assert_eq!(small.sites.candidate, 1);

    let total: usize = sites
        .values()
        .map(|c| c.sites.candidate + c.sites.external + c.sites.unresolved)
        .sum();
    assert_eq!(
        total, 4,
        "the file-level site at line 240 belongs to no callable"
    );

    let report = snapshot_report(&m, 10);
    assert_eq!(report.verified_calls, Some(1));
    let text = snapshot_text(&report);
    assert!(text.contains("coupling: 1 verified calls edges"), "{text}");
    let line = text
        .lines()
        .find(|l| l.contains("  grows  "))
        .expect("grows hotspot line");
    assert!(
        line.contains("calls in 0 out 1 verified, 1 candidate, 1 external, 1 unresolved"),
        "{line}"
    );
}

#[test]
fn a_syntax_snapshot_prints_coupling_as_unavailable_never_zero() {
    let mut snapshot = common::ruby_snapshot(common::BASE_COMMIT, 12);
    let start_byte = 2 * 40;
    snapshot.call_sites.push(CallSite {
        rel_path: "app/example.rb".into(),
        span: Span {
            start_byte,
            end_byte: start_byte + 8,
            start_line: 3,
            start_col: 2,
            end_line: 3,
            end_col: 10,
        },
        raw_name: "puts".into(),
        category: CallSiteCategory::Observed,
    });
    snapshot
        .report
        .call_sites_by_category
        .insert("Observed".into(), 1);
    let m = measured(&snapshot).expect("admitted");
    let report = snapshot_report(&m, 10);
    assert_eq!(report.verified_calls, None);
    let example = &report.hotspots[0];
    assert_eq!(example.calls.verified_in, None);
    assert_eq!(example.calls.verified_out, None);
    assert_eq!(example.calls.sites.observed, 1);

    let text = snapshot_text(&report);
    assert!(
        text.contains("coupling: syntax snapshot, verified calls unavailable"),
        "{text}"
    );
    assert!(text.contains("calls n/a verified, 1 observed"), "{text}");
    assert!(
        !text.contains("calls in 0 out 0"),
        "a syntax snapshot never claims zero coupling: {text}"
    );

    let (b, h) = measured_pair(&snapshot, &snapshot).expect("pair");
    let change = change_report(&b, &h, 10);
    assert_eq!(change.verified_calls.base, None);
    let change_text = codemap_slop::render::change_text(&change);
    assert!(
        change_text.contains("coupling: verified calls edges n/a -> n/a"),
        "{change_text}"
    );
}

#[test]
fn the_report_prints_verbosity_clone_with_its_counts() {
    let head = common::head_with_clone();
    let m = measured(&head).expect("admitted");
    let report = snapshot_report(&m, 3);
    assert_eq!(report.summary.clones.unwrap().groups, 1);
    assert_eq!(report.summary.clones.unwrap().clone_sloc, 32);
    let text = snapshot_text(&report);
    let line = text
        .lines()
        .find(|l| l.starts_with("clones:"))
        .expect("clones line");
    assert!(line.contains("verbosity_clone 0."), "{line}");
    assert!(line.contains("clone groups 1"), "{line}");
    assert!(line.contains("clone lines 32 of"), "{line}");
    assert!(line.contains("largest group 2 occurrences"), "{line}");
}

#[test]
fn ruby_reports_complexity_size_and_clone_figures_under_its_own_policy() {
    let base = common::ruby_snapshot(common::BASE_COMMIT, 3);
    let head = common::ruby_snapshot(common::HEAD_COMMIT, 12);
    let (b, h) = measured_pair(&base, &head).expect("Ruby pair");
    assert_eq!(h.policy, "cc-sloc-clones-form/ruby/1");
    let report = snapshot_report(&h, 3);
    assert_eq!(report.summary.counts.max_cc, 12);
    assert_eq!(report.summary.loc, 16);
    let clones = report
        .summary
        .clones
        .expect("Ruby carries clone figures since its lexer binding landed");
    assert_eq!(clones.groups, 0, "the fixture holds no clone group");
    assert_eq!(
        report.summary.verbosity_clone,
        Some(0.0),
        "zero clone lines is a measured zero, not an absent figure"
    );
    assert!(snapshot_text(&report).contains("verbosity_clone 0.000"));

    let change = change_report(&b, &h, 3);
    assert!(
        change
            .clone_groups_introduced
            .as_ref()
            .is_some_and(Vec::is_empty)
    );
    assert_eq!(change.clone_groups_removed, Some(0));
    assert!(codemap_slop::render::change_text(&change).contains("clones: groups 0 -> 0"));
}
