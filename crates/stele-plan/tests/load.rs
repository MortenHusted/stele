//! Loading catalogs and plans: every statement and block with its links, and
//! every way a file can be wrong, named at its line.

use std::collections::BTreeMap;

use stele_plan::{ExceptionKind, State, Statement, StatementKind, load, load_catalog};

const ROOT: &str = r#"catalog stele {
  use "./channels.stele" as channels
  use "./walk.stele" as walk
}
"#;

const CHANNELS: &str = r#"catalog channels {
  channel cli {
    text: "The stele command line."
  }
  channel library {
    text: "The engine as a Rust library."
  }
}
"#;

const WALK: &str = r#"catalog walk {
  use "./channels.stele" as ch

  intent results_are_honest {
    text: "Stele never reports a spec as passing while one of its claims is open."
  }
  definition walk_exit_status {
    refines: results_are_honest
    text: "walk exits 0 when every expectation holds, 1 when one is open, and 2 when the spec is invalid."
  }
  exception library_returns_report {
    narrows: walk_exit_status
    channels: [ch.library]
    kind: holds_instead
    text: "The library returns the report, and its status carries the same outcomes."
  }
}
"#;

const PLAN: &str = r#"plan honest_walk {
  use "../intent/catalog.stele" as intent

  definition walk_reports_status {
    refines: intent.results_are_honest
    replaces: intent.walk_exit_status
    text: "walk prints the status it exits with."
  }
  decision status_line_first {
    governs: [walk_reports_status, intent.cli]
    text: "The status line comes before the expectations."
    rejected: "A trailing summary: readers stop at the first screen."
  }
  change share_status {
    serves: [intent.results_are_honest]
    touches: ["rust:crates/lemmaspec/src/cli.rs#report_walk"]
    state: done
    text: "Print the status from one place."
  }
  change print_status_line {
    after: [share_status]
    follows: [status_line_first]
    serves: [walk_reports_status]
    creates: ["crates/lemmaspec/src/status.rs"]
    text: "walk prints its status line first."
  }
}
"#;

fn files(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(path, source)| ((*path).to_string(), (*source).to_string()))
        .collect()
}

fn base() -> BTreeMap<String, String> {
    files(&[
        ("intent/catalog.stele", ROOT),
        ("intent/channels.stele", CHANNELS),
        ("intent/walk.stele", WALK),
        ("plans/honest_walk.stele", PLAN),
    ])
}

fn with(path: &str, source: &str) -> BTreeMap<String, String> {
    let mut files = base();
    files.insert(path.into(), source.into());
    files
}

fn catalog_error(files: &BTreeMap<String, String>) -> String {
    load(files, "intent/catalog.stele").unwrap_err()
}

fn plan_error(source: &str) -> String {
    load(
        &with("plans/honest_walk.stele", source),
        "plans/honest_walk.stele",
    )
    .unwrap_err()
}

/// The plan with one line replaced, to break it in one place.
fn plan_with(from: &str, to: &str) -> String {
    assert!(PLAN.contains(from), "{from}");
    PLAN.replacen(from, to, 1)
}

#[test]
fn catalog_root_loads_every_statement_with_its_links() {
    let document = load(&base(), "intent/catalog.stele").unwrap();
    assert!(document.plan.is_none());
    assert_eq!(
        document
            .catalog
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "cli",
            "library",
            "library_returns_report",
            "results_are_honest",
            "walk_exit_status"
        ]
    );
    assert_eq!(
        document.catalog["results_are_honest"],
        Statement {
            id: "results_are_honest".into(),
            text: "Stele never reports a spec as passing while one of its claims is open.".into(),
            file: "intent/walk.stele".into(),
            line: 4,
            kind: StatementKind::Intent,
            realized_by: Vec::new(),
        }
    );
    assert_eq!(
        document.catalog["library_returns_report"].kind,
        StatementKind::Exception {
            narrows: "walk_exit_status".into(),
            kind: ExceptionKind::HoldsInstead,
            channels: vec!["library".into()],
        }
    );
}

#[test]
fn plan_loads_its_catalog_proposals_decisions_and_changes() {
    let document = load(&base(), "plans/honest_walk.stele").unwrap();
    assert_eq!(document.catalog.len(), 5);
    let plan = document.plan.unwrap();
    assert_eq!(plan.name, "honest_walk");
    assert_eq!(plan.file, "plans/honest_walk.stele");
    assert_eq!(plan.catalog.as_deref(), Some("intent/catalog.stele"));

    let proposal = &plan.proposals["walk_reports_status"];
    assert_eq!(proposal.replaces.as_deref(), Some("walk_exit_status"));
    assert_eq!(
        proposal.statement.kind,
        StatementKind::Definition {
            refines: "results_are_honest".into()
        }
    );

    let decision = &plan.decisions["status_line_first"];
    assert_eq!(decision.governs, ["cli", "walk_reports_status"]);
    assert_eq!(
        decision.rejected.as_deref(),
        Some("A trailing summary: readers stop at the first screen.")
    );

    let done = &plan.changes["share_status"];
    assert_eq!(done.state, State::Done);
    assert_eq!(
        done.touches,
        ["rust:crates/lemmaspec/src/cli.rs#report_walk"]
    );
    let next = &plan.changes["print_status_line"];
    assert_eq!(next.state, State::Planned);
    assert_eq!(next.after, ["share_status"]);
    assert_eq!(next.follows, ["status_line_first"]);
    assert_eq!(next.serves, ["walk_reports_status"]);
    assert_eq!(next.creates, ["crates/lemmaspec/src/status.rs"]);
}

#[test]
fn plan_without_a_catalog_loads_its_own_blocks() {
    let source = r#"plan alone {
  intent works { text: "It works." }
  change make_it_work { serves: [works] text: "Make it work." }
}
"#;
    let document = load(
        &files(&[("plans/alone.stele", source)]),
        "plans/alone.stele",
    )
    .unwrap();
    assert!(document.catalog.is_empty());
    assert_eq!(
        document.plan.unwrap().changes["make_it_work"].serves,
        ["works"]
    );
}

#[test]
fn a_file_imported_along_two_paths_loads_once_and_a_cycle_fails() {
    let root = r#"catalog stele {
  use "./a.stele" as a
  use "./b.stele" as b
}
"#;
    let a = "catalog a {\n  use \"./b.stele\" as b\n}\n";
    let b = "catalog b {\n  intent shared { text: \"Shared.\" }\n}\n";
    let diamond = files(&[
        ("intent/catalog.stele", root),
        ("intent/a.stele", a),
        ("intent/b.stele", b),
    ]);
    assert_eq!(
        load(&diamond, "intent/catalog.stele")
            .unwrap()
            .catalog
            .len(),
        1
    );

    let cyclic = files(&[
        ("intent/catalog.stele", root),
        ("intent/a.stele", a),
        (
            "intent/b.stele",
            "catalog b {\n  use \"./a.stele\" as a\n}\n",
        ),
    ]);
    assert_eq!(
        catalog_error(&cyclic),
        "intent/b.stele:2: import cycle through intent/a.stele"
    );
}

#[test]
fn catalog_files_must_be_found_reached_and_kept_inside_the_catalog() {
    let mut missing = base();
    missing.remove("intent/walk.stele");
    assert_eq!(
        catalog_error(&missing),
        "intent/catalog.stele:3: imported file intent/walk.stele not found"
    );
    assert_eq!(
        load(&base(), "intent/none.stele").unwrap_err(),
        "intent/none.stele not found"
    );
    assert_eq!(
        catalog_error(&with(
            "intent/stray.stele",
            "catalog stray {\n  intent lost { text: \"Lost.\" }\n}\n"
        )),
        "intent/stray.stele: not imported from intent/catalog.stele"
    );
    let escaping = with(
        "intent/catalog.stele",
        "catalog stele {\n  use \"../plans/honest_walk.stele\" as p\n}\n",
    );
    assert_eq!(
        catalog_error(&escaping),
        "intent/catalog.stele:2: import \"../plans/honest_walk.stele\" must be a relative path inside the catalog directory"
    );
    assert_eq!(
        load(&files(&[("catalog.stele", ROOT)]), "catalog.stele").unwrap_err(),
        "catalog.stele:1: a catalog root must sit in a directory of its own"
    );
}

#[test]
fn the_ledgers_beside_the_root_are_read_by_name_never_imported() {
    let ledger = "approval {\n  item: walk_exit_status\n  digest_version: 1\n}\n";
    let mut beside = base();
    beside.insert("intent/approvals.stele".into(), ledger.into());
    beside.insert("intent/runs.stele".into(), "run {\n}\n".into());
    assert_eq!(
        load(&beside, "intent/catalog.stele").unwrap(),
        load(&base(), "intent/catalog.stele").unwrap()
    );
    assert!(load(&beside, "plans/honest_walk.stele").is_ok());
    for nested in ["intent/sub/approvals.stele", "intent/sub/runs.stele"] {
        assert_eq!(
            catalog_error(&with(nested, ledger)),
            format!("{nested}: not imported from intent/catalog.stele")
        );
    }
}

#[test]
fn load_catalog_reads_a_catalog_root_and_refuses_a_plan() {
    assert_eq!(
        load_catalog(&base(), "intent/catalog.stele").unwrap(),
        load(&base(), "intent/catalog.stele").unwrap().catalog
    );
    assert_eq!(
        load_catalog(&base(), "plans/honest_walk.stele").unwrap_err(),
        "plans/honest_walk.stele is a plan, not a catalog"
    );
    assert_eq!(
        load_catalog(&base(), "intent/none.stele").unwrap_err(),
        "intent/none.stele not found"
    );
}

#[test]
fn duplicate_ids_name_both_places() {
    assert_eq!(
        catalog_error(&with(
            "intent/channels.stele",
            &CHANNELS.replace("channel library", "channel results_are_honest")
        )),
        "duplicate id results_are_honest in intent/channels.stele:5 and intent/walk.stele:4"
    );
    assert_eq!(
        plan_error(&plan_with("change share_status", "change cli")),
        "duplicate id cli in intent/channels.stele:2 and plans/honest_walk.stele:14"
    );
}

#[test]
fn references_resolve_by_file_in_a_catalog_and_across_it_from_a_plan() {
    // A bare ID names a block of the same file only.
    assert_eq!(
        catalog_error(&with(
            "intent/walk.stele",
            &WALK.replace("channels: [ch.library]", "channels: [library]")
        )),
        "intent/walk.stele:13: unknown reference library in channels of library_returns_report"
    );
    // From a plan, the catalog alias reaches every catalog file.
    assert!(load(&base(), "plans/honest_walk.stele").is_ok());
    assert_eq!(
        plan_error(&plan_with("intent.cli]", "intent.nowhere]")),
        "plans/honest_walk.stele:10: unknown reference intent.nowhere in governs of status_line_first"
    );
    assert_eq!(
        plan_error(&plan_with("[share_status]", "[later]")),
        "plans/honest_walk.stele:21: unknown reference later in after of print_status_line"
    );
}

#[test]
fn links_must_name_the_right_kind() {
    assert_eq!(
        catalog_error(&with(
            "intent/walk.stele",
            &WALK.replace("narrows: walk_exit_status", "narrows: results_are_honest")
        )),
        "intent/walk.stele:12: narrows of library_returns_report must name a definition, but results_are_honest is an intent"
    );
    assert_eq!(
        plan_error(&plan_with(
            "follows: [status_line_first]",
            "follows: [share_status]"
        )),
        "plans/honest_walk.stele:22: follows of print_status_line must name a decision, but share_status is a change"
    );
    assert_eq!(
        plan_error(&plan_with(
            "serves: [walk_reports_status]",
            "serves: [status_line_first]"
        )),
        "plans/honest_walk.stele:23: serves of print_status_line must name a statement, but status_line_first is a decision"
    );
    assert_eq!(
        plan_error(&plan_with(
            "replaces: intent.walk_exit_status",
            "replaces: intent.results_are_honest"
        )),
        "plans/honest_walk.stele:6: replaces of walk_reports_status must name a definition, but intent.results_are_honest is an intent"
    );
}

#[test]
fn a_proposal_replaces_only_a_catalog_statement() {
    let source = plan_with(
        "  decision status_line_first",
        "  definition second_try {\n    refines: intent.results_are_honest\n    replaces: walk_reports_status\n    text: \"Again.\"\n  }\n  decision status_line_first",
    );
    assert_eq!(
        plan_error(&source),
        "plans/honest_walk.stele:11: replaces of second_try must name a catalog statement"
    );
}

#[test]
fn catalogs_hold_statements_and_plans_import_one_catalog() {
    assert_eq!(
        catalog_error(&with(
            "intent/walk.stele",
            &WALK.replace("  intent results_are_honest", "  decision early {\n    governs: [walk_exit_status]\n    text: \"Early.\"\n  }\n  intent results_are_honest")
        )),
        "intent/walk.stele:4: a catalog holds statements; decision early belongs in a plan"
    );
    assert_eq!(
        plan_error(&plan_with(
            "  use \"../intent/catalog.stele\" as intent\n",
            "  use \"../intent/catalog.stele\" as intent\n  use \"../intent/walk.stele\" as walk\n"
        )),
        "plans/honest_walk.stele:3: a plan imports one catalog"
    );
    assert_eq!(
        plan_error(&plan_with("../intent/catalog.stele", "./other.stele")),
        "plans/honest_walk.stele:2: imported file plans/other.stele not found"
    );
    let importing_a_plan = with(
        "plans/second.stele",
        "plan second {\n  use \"./honest_walk.stele\" as first\n}\n",
    );
    assert_eq!(
        load(&importing_a_plan, "plans/second.stele").unwrap_err(),
        "plans/second.stele:2: plans/honest_walk.stele is not a catalog"
    );
    assert_eq!(
        plan_error(&plan_with("../intent/catalog.stele", "../../outside.stele")),
        "plans/honest_walk.stele:2: import \"../../outside.stele\" must be a relative path that stays inside the repository"
    );
}

#[test]
fn fields_are_checked() {
    assert_eq!(
        plan_error(&plan_with("    state: done\n", "    state: shipped\n")),
        "plans/honest_walk.stele:17: change share_status has unknown state shipped; expected planned or done"
    );
    assert_eq!(
        plan_error(&plan_with(
            "governs: [walk_reports_status, intent.cli]",
            "governs: []"
        )),
        "plans/honest_walk.stele:9: decision status_line_first governs no statement"
    );
    assert_eq!(
        plan_error(&plan_with(
            "    text: \"Print the status from one place.\"\n",
            ""
        )),
        "plans/honest_walk.stele:14: change share_status needs text"
    );
    assert_eq!(
        plan_error(&plan_with("    state: done\n", "    owner: \"someone\"\n")),
        "plans/honest_walk.stele:17: change share_status has no field owner"
    );
    assert_eq!(
        plan_error(&plan_with(
            "serves: [walk_reports_status]",
            "serves: walk_reports_status"
        )),
        "plans/honest_walk.stele:23: serves of print_status_line must be a list"
    );
    assert_eq!(
        plan_error(&plan_with(
            "serves: [walk_reports_status]",
            "serves: [walk_reports_status, walk_reports_status]"
        )),
        "plans/honest_walk.stele:23: serves of print_status_line names walk_reports_status twice"
    );
    assert_eq!(
        catalog_error(&with(
            "intent/walk.stele",
            &WALK.replace("kind: holds_instead", "kind: sometimes")
        )),
        "intent/walk.stele:14: exception library_returns_report has unknown kind sometimes; expected does_not_apply or holds_instead"
    );
}

/// The walk catalog file with `realized_by` given to its definition.
fn walk_realized_by(anchors: &str) -> BTreeMap<String, String> {
    with(
        "intent/walk.stele",
        &WALK.replace(
            "    refines: results_are_honest\n",
            &format!("    refines: results_are_honest\n    realized_by: {anchors}\n"),
        ),
    )
}

#[test]
fn definitions_and_exceptions_name_the_code_that_realizes_them() {
    let files = with(
        "intent/walk.stele",
        &WALK
            .replace(
                "    refines: results_are_honest\n",
                "    refines: results_are_honest\n    realized_by: [\"rust:crates/lemmaspec/src/cli.rs#report_walk\", \"crates/lemmaspec/src/cli.rs\"]\n",
            )
            .replace(
                "    kind: holds_instead\n",
                "    kind: holds_instead\n    realized_by: [\"rust:crates/lemmaspec/src/lib.rs#walk\"]\n",
            ),
    );
    let catalog = load(&files, "intent/catalog.stele").unwrap().catalog;
    assert_eq!(
        catalog["walk_exit_status"].realized_by,
        [
            "crates/lemmaspec/src/cli.rs",
            "rust:crates/lemmaspec/src/cli.rs#report_walk"
        ]
    );
    assert_eq!(
        catalog["library_returns_report"].realized_by,
        ["rust:crates/lemmaspec/src/lib.rs#walk"]
    );
    assert_eq!(catalog["results_are_honest"].realized_by, [""; 0]);
    let empty = load(&walk_realized_by("[]"), "intent/catalog.stele").unwrap();
    assert_eq!(empty.catalog["walk_exit_status"].realized_by, [""; 0]);
}

#[test]
fn realized_by_holds_distinct_anchors_on_catalog_definitions_and_exceptions_only() {
    for (anchors, error) in [
        (
            "[\"/etc/passwd\"]",
            "intent/walk.stele:9: \"/etc/passwd\" in realized_by of walk_exit_status is neither a symbol key (language:path#name) nor a repository-relative path",
        ),
        (
            "[\"src/lib.rs\", \"src/lib.rs\"]",
            "intent/walk.stele:9: realized_by of walk_exit_status names src/lib.rs twice",
        ),
        (
            "\"src/lib.rs\"",
            "intent/walk.stele:9: realized_by of walk_exit_status must be a list",
        ),
        (
            "[cli]",
            "intent/walk.stele:9: realized_by of walk_exit_status must list strings",
        ),
    ] {
        assert_eq!(
            catalog_error(&walk_realized_by(anchors)),
            error,
            "{anchors}"
        );
    }
    assert_eq!(
        catalog_error(&with(
            "intent/walk.stele",
            &WALK.replace(
                "  intent results_are_honest {\n",
                "  intent results_are_honest {\n    realized_by: [\"src/lib.rs\"]\n",
            )
        )),
        "intent/walk.stele:5: intent results_are_honest has no field realized_by"
    );
    assert_eq!(
        catalog_error(&with(
            "intent/channels.stele",
            &CHANNELS.replace(
                "  channel cli {\n",
                "  channel cli {\n    realized_by: [\"src/main.rs\"]\n",
            )
        )),
        "intent/channels.stele:3: channel cli has no field realized_by"
    );
    // A plan proposes what a statement says; the catalog maps it to code.
    assert_eq!(
        plan_error(&plan_with(
            "    replaces: intent.walk_exit_status\n",
            "    replaces: intent.walk_exit_status\n    realized_by: [\"src/lib.rs\"]\n",
        )),
        "plans/honest_walk.stele:7: definition walk_reports_status has no field realized_by"
    );
}

#[test]
fn anchors_are_symbol_keys_or_repository_paths() {
    for anchor in [
        "rust:crates/lemmaspec/src/cli.rs#report_walk",
        "ruby:app/models/user.rb#User::active",
        "js_ts:web/app.ts#App::render",
        "crates/stele/src/plan.rs",
    ] {
        let source = plan_with("crates/lemmaspec/src/status.rs", anchor);
        assert!(
            load(
                &with("plans/honest_walk.stele", &source),
                "plans/honest_walk.stele"
            )
            .is_ok(),
            "{anchor}"
        );
    }
    for anchor in [
        "/etc/passwd",
        "../outside.rs",
        "src/./lib.rs",
        "rust:src/lib.rs",
        "rust:src/lib.rs#",
        "Rust:src/lib.rs#main",
        "src/lib.rs#main",
        "",
    ] {
        assert_eq!(
            plan_error(&plan_with("crates/lemmaspec/src/status.rs", anchor)),
            format!(
                "plans/honest_walk.stele:24: {anchor:?} in creates of print_status_line is neither a symbol key (language:path#name) nor a repository-relative path"
            )
        );
    }
}

#[test]
fn only_catalogs_and_plans_load() {
    assert_eq!(
        load(
            &files(&[("plans/x.stele", "spec release { }\n")]),
            "plans/x.stele"
        )
        .unwrap_err(),
        "plans/x.stele:1: this is a specification, not a catalog or plan; run it with stele spec"
    );
    assert_eq!(
        load(
            &files(&[("plans/x.stele", "intent x { }\n")]),
            "plans/x.stele"
        )
        .unwrap_err(),
        "plans/x.stele:1: expected catalog NAME or plan NAME, found intent"
    );
    assert_eq!(
        plan_error(&plan_with(
            "  decision status_line_first",
            "  milestone status_line_first"
        )),
        "plans/honest_walk.stele:9: unknown kind milestone"
    );
}

#[test]
fn a_directory_on_disk_loads_like_the_same_files_in_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (path, source) in base() {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    let reader = stele_plan::DirReader(dir.path().to_path_buf());
    for path in ["intent/catalog.stele", "plans/honest_walk.stele"] {
        assert_eq!(load(&reader, path).unwrap(), load(&base(), path).unwrap());
    }
    std::fs::write(dir.path().join("intent/nested.stele"), "catalog n {\n}\n").unwrap();
    assert_eq!(
        load(&reader, "intent/catalog.stele").unwrap_err(),
        "intent/nested.stele: not imported from intent/catalog.stele"
    );
}
