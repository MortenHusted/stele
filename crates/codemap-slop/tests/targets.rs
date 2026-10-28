//! Shape targets: ceilings under a prefix, the statistics behind them,
//! and the ratchet that breaches on the change and prints the backlog.

mod common;

use codemap_slop::diff::change_report;
use codemap_slop::gate::{evaluate as evaluate_budget, parse_budget};
use codemap_slop::policy::{ALL_FILES, Scope, TARGET_RATCHET};
use codemap_slop::render::{gate_text, snapshot_report, snapshot_text, targets_text};
use codemap_slop::targets::{Stat, Status, Target, TargetMeasure, TargetResult, evaluate};
use codemap_slop::{measured, measured_in, measured_pair};
use serde_json::Value;

use common::{
    BASE_COMMIT, HEAD_COMMIT, go_snapshot, ruby_snapshot_with_form, snapshot_with_classes,
    snapshot_with_classes_and_cc, snapshot_with_run_once_code,
};

fn target(name: &str, prefix: &str, measure: TargetMeasure, stat: Stat, max: f64) -> Target {
    Target {
        name: name.into(),
        prefix: prefix.into(),
        measure,
        stat,
        max,
    }
}

#[test]
fn per_callable_and_per_class_measures_read_the_snapshot_facts() {
    let snapshot = snapshot_with_classes(BASE_COMMIT, true);
    let m = measured(&snapshot).expect("admitted");
    let targets = [
        target("cc", "app/", TargetMeasure::Cc, Stat::Each, 10.0),
        target("sloc_p90", "app/", TargetMeasure::Sloc, Stat::P90, 100.0),
        target("sloc_p50", "app/", TargetMeasure::Sloc, Stat::P50, 100.0),
        target(
            "methods",
            "app/",
            TargetMeasure::MethodsPerClass,
            Stat::Max,
            25.0,
        ),
        target(
            "class_loc",
            "app/",
            TargetMeasure::ClassLoc,
            Stat::Each,
            50.0,
        ),
        target(
            "depth",
            "app/",
            TargetMeasure::InheritsDepth,
            Stat::Max,
            3.0,
        ),
        target("twins", "app/", TargetMeasure::TwinsPer1k, Stat::Value, 2.0),
        target(
            "mig_methods",
            "db/",
            TargetMeasure::MethodsPerClass,
            Stat::Each,
            0.0,
        ),
    ];
    let results = evaluate(&targets, &m, None).expect("prefixes match");
    let by_name = |name: &str| {
        results
            .iter()
            .find(|r| r.target.name == name)
            .expect("present")
    };

    let cc = by_name("cc");
    assert_eq!(cc.sample, 3, "big, grows, small");
    assert_eq!(cc.head, Some(1.0), "big is over 10");
    assert_eq!(cc.backlog.len(), 1);
    assert_eq!(cc.backlog[0].name, "big");
    assert!((cc.backlog[0].value - 15.0).abs() < f64::EPSILON);
    assert!(cc.breached.is_empty(), "no base, so nothing is a breach");
    assert_eq!(cc.status, Status::OverTarget);

    assert_eq!(
        by_name("sloc_p90").head,
        Some(50.0),
        "nearest rank over [5, 20, 50]: ceil(0.9 * 3) = 3rd"
    );
    assert_eq!(by_name("sloc_p50").head, Some(20.0));
    assert_eq!(by_name("sloc_p50").status, Status::Met);

    let methods = by_name("methods");
    assert_eq!(methods.sample, 2, "Core and CoreBase");
    assert_eq!(methods.head, Some(3.0), "Core holds three methods");
    assert_eq!(methods.status, Status::Met);

    let class_loc = by_name("class_loc");
    assert_eq!(class_loc.head, Some(1.0), "Core spans 95 lines");
    assert_eq!(class_loc.backlog[0].name, "Core");
    assert!((class_loc.backlog[0].value - 95.0).abs() < f64::EPSILON);

    assert_eq!(
        by_name("depth").head,
        Some(1.0),
        "Core inherits CoreBase, which inherits nothing in the repository"
    );

    let twins = by_name("twins");
    assert_eq!(twins.sample, 3);
    let per_1k = twins.head.expect("twins computed");
    assert!(
        (per_1k - 1000.0 / 3.0).abs() < 1e-9,
        "one pair with both sides under app/ (big, grows) over three callables: {per_1k}"
    );
    assert_eq!(
        twins.status,
        Status::OverTarget,
        "over 2 per 1k, no base to be worse than"
    );

    let mig = by_name("mig_methods");
    assert_eq!(mig.head, Some(1.0), "Drop holds up, over a ceiling of 0");
}

#[test]
fn form_measures_are_targets_under_a_policy_that_carries_them_and_n_a_otherwise() {
    // index: 1 param, nesting 1, chain 2, 1 ivar; show: 5 params, nesting
    // 4, chain 5, 3 ivars; build: 2 params, nesting 2, chain 1, 0 ivars.
    let base = ruby_snapshot_with_form(
        BASE_COMMIT,
        &[
            ("index", (1, 1, 2, 1)),
            ("show", (5, 4, 5, 3)),
            ("build", (2, 2, 1, 0)),
        ],
    );
    let m = measured(&base).expect("admitted");
    let targets = [
        target("params", "app/", TargetMeasure::Params, Stat::Each, 4.0),
        target(
            "ivars",
            "app/controllers/",
            TargetMeasure::Ivars,
            Stat::Each,
            1.0,
        ),
        target("nesting", "app/", TargetMeasure::Nesting, Stat::Max, 3.0),
        target("chain", "app/", TargetMeasure::Chain, Stat::P50, 2.0),
    ];
    let results = evaluate(&targets, &m, None).expect("evaluated");
    let by_name = |name: &str| {
        results
            .iter()
            .find(|r| r.target.name == name)
            .expect("present")
    };
    let params = by_name("params");
    assert_eq!(params.sample, 3);
    assert_eq!(params.head, Some(1.0), "show has five parameters");
    assert_eq!(params.backlog[0].name, "show");
    let ivars = by_name("ivars");
    assert_eq!(ivars.sample, 2, "controller actions only");
    assert_eq!(ivars.backlog[0].name, "show");
    assert!((ivars.backlog[0].value - 3.0).abs() < f64::EPSILON);
    assert_eq!(by_name("nesting").head, Some(4.0));
    assert_eq!(by_name("nesting").status, Status::OverTarget);
    assert_eq!(by_name("chain").head, Some(2.0), "median of 2, 5, 1");
    assert_eq!(by_name("chain").status, Status::Met);

    // The ratchet: show gains a sixth parameter, index stays as it was.
    let head = ruby_snapshot_with_form(
        HEAD_COMMIT,
        &[
            ("index", (1, 1, 2, 1)),
            ("show", (6, 4, 5, 3)),
            ("build", (2, 2, 1, 0)),
        ],
    );
    let (b, h) = measured_pair(&base, &head).expect("pair");
    let result = &evaluate(&targets[..1], &h, Some(&b)).expect("evaluated")[0];
    assert_eq!(result.status, Status::Breached);
    assert_eq!(result.breached[0].name, "show");
    assert_eq!(result.breached[0].base, Some(5.0));

    // Every language carries the three form measures now; `ivars` is
    // Ruby's alone, and on a Go snapshot it is n/a, never met.
    let go = go_snapshot(BASE_COMMIT, 3);
    let m = measured(&go).expect("admitted");
    let targets = [
        target("params", "app/", TargetMeasure::Params, Stat::Each, 4.0),
        target("ivars", "app/", TargetMeasure::Ivars, Stat::Each, 1.0),
    ];
    let results = evaluate(&targets, &m, None).expect("evaluated");
    assert_eq!(results[0].status, Status::Met, "params is a real figure");
    assert_eq!(results[0].sample, 1);
    assert_eq!(results[1].status, Status::Unavailable);
    let reason = results[1].reason.clone().expect("a reason");
    assert!(reason.contains("go has no instance variables"), "{reason}");
    let text = targets_text(&results, 5);
    assert!(
        text.contains("n/a (go has no instance variables to count)"),
        "{text}"
    );
}

#[test]
fn a_target_without_twins_is_unavailable_never_met() {
    let snapshot = snapshot_with_classes(BASE_COMMIT, false);
    let m = measured(&snapshot).expect("admitted");
    let targets = [target(
        "twins",
        "app/",
        TargetMeasure::TwinsPer1k,
        Stat::Value,
        2.0,
    )];
    let result = &evaluate(&targets, &m, None).expect("evaluated")[0];
    assert_eq!(result.status, Status::Unavailable);
    assert_eq!(result.head, None);
    assert!(
        result.reason.as_deref().unwrap_or("").contains("--twins"),
        "{:?}",
        result.reason
    );
    let text = targets_text(&evaluate(&targets, &m, None).expect("evaluated"), 10);
    assert!(
        text.contains("n/a (not computed; extract with --measures --twins)"),
        "{text}"
    );
}

#[test]
fn a_prefix_matching_no_file_is_refused_by_name() {
    let snapshot = snapshot_with_classes(BASE_COMMIT, false);
    let m = measured(&snapshot).expect("admitted");
    let targets = [target(
        "typo",
        "app/controlers/",
        TargetMeasure::Cc,
        Stat::Each,
        6.0,
    )];
    let error = evaluate(&targets, &m, None).expect_err("refused");
    assert!(error.to_string().contains("`typo`"), "{error}");
    assert!(error.to_string().contains("app/controlers/"), "{error}");
}

#[test]
fn a_target_is_held_inside_the_scope() {
    let snapshot = snapshot_with_classes(BASE_COMMIT, false);
    let scope = Scope::excluding(["db/migrate/"]).expect("valid");
    let m = measured_in(&snapshot, &scope).expect("admitted");
    // `db/` matches a file, but the scope leaves the migration out.
    let targets = [target("db_cc", "db/", TargetMeasure::Cc, Stat::Max, 5.0)];
    let result = &evaluate(&targets, &m, None).expect("evaluated")[0];
    assert_eq!(result.status, Status::Unavailable);
    assert_eq!(result.sample, 0);
}

#[test]
fn the_ratchet_breaches_on_what_the_change_did_and_prints_the_rest_as_backlog() {
    let base = snapshot_with_classes(BASE_COMMIT, false);
    let each = [target("cc", "app/", TargetMeasure::Cc, Stat::Each, 6.0)];
    let max = [target("cc_max", "app/", TargetMeasure::Cc, Stat::Max, 6.0)];

    // Nothing changed: big (15) and grows (8) are over 6 in both, so both
    // are backlog and neither target is breached.
    let same = snapshot_with_classes(HEAD_COMMIT, false);
    let (b, h) = measured_pair(&base, &same).expect("pair");
    let result = &evaluate(&each, &h, Some(&b)).expect("evaluated")[0];
    assert_eq!(result.status, Status::OverTarget);
    assert_eq!(result.head, Some(2.0));
    assert_eq!(result.base, Some(2.0));
    let names: Vec<&str> = result.backlog.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["big", "grows"], "largest first");
    assert_eq!(result.breached, [] as [codemap_slop::targets::Item; 0]);
    let result = &evaluate(&max, &h, Some(&b)).expect("evaluated")[0];
    assert_eq!(
        result.status,
        Status::OverTarget,
        "15 over 6, but not worse than the base"
    );

    // big got worse: over the ceiling and worse than the base is a breach,
    // and grows stays backlog.
    let worse = snapshot_with_classes_and_cc(HEAD_COMMIT, "big", 16);
    let (b, h) = measured_pair(&base, &worse).expect("pair");
    let result = &evaluate(&each, &h, Some(&b)).expect("evaluated")[0];
    assert_eq!(result.status, Status::Breached);
    assert_eq!(result.breached.len(), 1);
    assert_eq!(result.breached[0].name, "big");
    assert_eq!(result.breached[0].base, Some(15.0));
    assert_eq!(result.backlog.len(), 1);
    assert_eq!(result.backlog[0].name, "grows");
    let result = &evaluate(&max, &h, Some(&b)).expect("evaluated")[0];
    assert_eq!(result.status, Status::Breached);
    assert_eq!(result.head, Some(16.0));
    assert_eq!(result.base, Some(15.0));

    // big improved but is still over: backlog, never a breach.
    let better = snapshot_with_classes_and_cc(HEAD_COMMIT, "big", 12);
    let (b, h) = measured_pair(&base, &better).expect("pair");
    let result = &evaluate(&each, &h, Some(&b)).expect("evaluated")[0];
    assert_eq!(result.status, Status::OverTarget);
    assert_eq!(result.backlog[0].base, Some(15.0));

    // small rose from 1 to 7: over the ceiling and worse, so a breach even
    // though it never crossed the erosion cutoff.
    let crept = snapshot_with_classes_and_cc(HEAD_COMMIT, "small", 7);
    let (b, h) = measured_pair(&base, &crept).expect("pair");
    let result = &evaluate(&each, &h, Some(&b)).expect("evaluated")[0];
    assert_eq!(result.status, Status::Breached);
    assert_eq!(result.breached[0].name, "small");
    assert_eq!(result.head, Some(3.0));
    assert_eq!(result.base, Some(2.0));
}

#[test]
fn a_callable_new_to_the_prefix_breaches_when_over_the_ceiling() {
    // The plain run-once fixture as the base, the same code with classes
    // as the head: the callables are the same by id, so the classes are
    // the new items.
    let base = snapshot_with_run_once_code(BASE_COMMIT);
    let head = snapshot_with_classes(HEAD_COMMIT, false);
    let (b, h) = measured_pair(&base, &head).expect("pair");
    let targets = [target(
        "class_loc",
        "app/",
        TargetMeasure::ClassLoc,
        Stat::Each,
        50.0,
    )];
    let result = &evaluate(&targets, &h, Some(&b)).expect("evaluated")[0];
    assert_eq!(result.status, Status::Breached);
    assert_eq!(result.breached[0].name, "Core");
    assert_eq!(result.breached[0].base, None, "new to the prefix");
    assert_eq!(result.base, Some(0.0));
}

const SEED: &str = "\
max_new_high_cc_callables = 0
exclude = \"db/migrate/\"

[target.app_cc]          # every callable under app/
prefix = \"app/\"
measure = \"cc\"
stat = \"each\"
max = 6

[target.app_cc_p90]
prefix = \"app/\"
measure = \"cc\"
stat = \"p90\"
max = 3

[target.app_twins]
prefix = \"app/\"
measure = \"twins_per_1k\"
stat = \"value\"
max = 2
";

#[test]
fn the_report_holds_one_snapshot_to_a_budget_with_no_breach_and_json_carries_targets() {
    let budget = parse_budget(SEED).expect("parses");
    let snapshot = snapshot_with_classes(BASE_COMMIT, true);
    let scope = Scope::excluding(&budget.exclude).expect("valid");
    let m = measured_in(&snapshot, &scope).expect("admitted");
    let mut report = snapshot_report(&m, 10);
    let plain = serde_json::to_value(&report).expect("json");
    assert!(
        plain.get("targets").is_none(),
        "without --budget the document keeps its bytes"
    );
    report.targets = Some(evaluate(&budget.targets, &m, None).expect("evaluated"));
    let json = serde_json::to_value(&report).expect("json");
    let targets = json["targets"].as_array().expect("array");
    assert_eq!(targets.len(), 3);
    assert_eq!(targets[0]["name"], "app_cc");
    assert_eq!(targets[0]["measure"], "cc");
    assert_eq!(targets[0]["stat"], "each");
    assert_eq!(targets[0]["status"], "over_target");
    assert_eq!(targets[2]["measure"], "twins_per_1k");
    assert!(
        targets
            .iter()
            .all(|t| t["status"] != "breached" && t.get("base").is_some_and(Value::is_null)),
        "one snapshot has no base: {targets:?}"
    );

    let text = snapshot_text(&report);
    assert!(
        text.contains(&format!("targets ({TARGET_RATCHET}:")),
        "{text}"
    );
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("app_cc "))
        .expect("app_cc line");
    assert!(line.contains("cc each <= 6"), "{line}");
    assert!(
        line.contains("over the ceiling 2 of 3 callables, base n/a"),
        "{line}"
    );
    assert!(line.ends_with("over target"), "{line}");
    assert!(
        text.contains("    cc    15  big  app/core.rs:1-52"),
        "{text}"
    );
    let p90 = text
        .lines()
        .find(|l| l.trim_start().starts_with("app_cc_p90 "))
        .expect("p90 line");
    assert!(p90.contains("head 15  base n/a  over 3 callables"), "{p90}");
    assert!(!text.contains("BREACHED"), "{text}");
}

#[test]
fn the_gate_breaches_on_a_target_and_says_which_item_the_change_made_worse() {
    let budget = parse_budget(SEED).expect("parses");
    let scope = Scope::excluding(&budget.exclude).expect("valid");
    let base = snapshot_with_classes(BASE_COMMIT, true);
    let head = snapshot_with_classes_and_cc(HEAD_COMMIT, "small", 7);
    let b = measured_in(&base, &scope).expect("base");
    let h = measured_in(&head, &scope).expect("head");
    let report = change_report(&b, &h, 10);
    let breaches = evaluate_budget(&report, &budget);
    assert!(
        breaches.is_empty(),
        "small at cc 7 crosses no erosion cutoff: {breaches:?}"
    );
    let targets = evaluate(&budget.targets, &h, Some(&b)).expect("evaluated");
    assert!(targets.iter().any(TargetResult::is_breached));
    let text = gate_text(&budget, &breaches, &targets, 10);
    assert!(text.contains("gate: BREACHED"), "{text}");
    assert!(
        text.contains(
            "target app_cc: 1 callables over cc each <= 6 added or made worse by this change"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "    cc     7  small  app/core.rs:90-96  (was 1)  <- added or made worse by this change"
        ),
        "{text}"
    );
    assert!(
        text.contains("app_twins")
            && text.contains("n/a (not computed; extract with --measures --twins)"),
        "the head lost its twins, so the twins target is n/a, never met: {text}"
    );

    // The same head against itself breaches nothing: the backlog is
    // printed and the gate is within budget.
    let (b, h) = measured_pair(&head, &head).expect("pair");
    let report = change_report(&b, &h, 10);
    let targets = evaluate(&budget.targets, &h, Some(&b)).expect("evaluated");
    let text = gate_text(&budget, &evaluate_budget(&report, &budget), &targets, 10);
    assert!(text.contains("gate: within budget"), "{text}");
    assert!(
        text.contains("over the ceiling 3 of 3 callables, base 3  over target"),
        "{text}"
    );
}

#[test]
fn the_backlog_list_is_bounded_and_says_how_many_more() {
    let snapshot = snapshot_with_classes(BASE_COMMIT, false);
    let m = measured_in(&snapshot, &ALL_FILES).expect("admitted");
    let targets = [target("cc", "app/", TargetMeasure::Cc, Stat::Each, 0.0)];
    let results = evaluate(&targets, &m, None).expect("evaluated");
    let text = targets_text(&results, 1);
    assert!(text.contains("cc    15  big"), "{text}");
    assert!(!text.contains("grows"), "{text}");
    assert!(
        text.contains("... and 2 more callables over the ceiling"),
        "{text}"
    );
}
