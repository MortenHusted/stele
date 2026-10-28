//! The intent report: the evidence it projects, the standing of every
//! statement with its reasons, what awaits approval, the code that realizes
//! each statement, and what changed since a base commit.

use std::{collections::BTreeMap, path::Path, process::Command};

use codemap_graph::Snapshot;
use lemmaspec::{EvidenceKind, FactValue, parse_artifact, print_artifact};
use stele_plan::{
    Code, DirReader, approvals,
    digest::DIGEST_VERSION,
    examples::{packages, scan},
    identity::Repo,
    load_catalog,
    project::{self as projection, Inputs},
    report::{self as intent_report, Options, Report, report},
    runs::{self, Outcome, Run},
};

const CATALOG: &str = "intent/catalog.stele";
const LEDGER: &str = "intent/runs.stele";

const CHANNELS: &str = r#"catalog channels {
  channel cli {
    text: "The stele command line."
  }
  channel library {
    text: "The engine as a Rust library."
  }
}
"#;

/// The statements of the root catalog, which imports the channels as `ch`.
const STATEMENTS: &str = r#"  intent results_are_honest {
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
"#;

const EXAMPLES: &str = r"/// intent: example open_exits_one exemplifies walk_exit_status on cli
#[test]
fn open_expectation_exits_one() {}

/// intent: example library_report exemplifies library_returns_report on library
#[test]
fn library_returns_the_report() {}
";

/// Every statement and example of the project.
const ITEMS: [&str; 7] = [
    "cli",
    "library",
    "results_are_honest",
    "walk_exit_status",
    "library_returns_report",
    "open_exits_one",
    "library_report",
];

fn root_catalog(body: &str) -> String {
    format!("catalog fixture {{\n  use \"./channels.stele\" as ch\n\n{body}}}\n")
}

/// A small Cargo package in a temporary git repository, with a catalog in
/// `intent/`, tagged examples in `tests/` and a source file in `src/`.
struct Project(tempfile::TempDir);

impl Project {
    fn new() -> Self {
        let project = Self(tempfile::tempdir().expect("tempdir"));
        project.git(&["init", "-q"]);
        for (key, value) in [
            ("user.email", "stele@example.com"),
            ("user.name", "Stele Test"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
            ("core.hooksPath", "no-hooks"),
        ] {
            project.git(&["config", key, value]);
        }
        project.write(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        project.write("src/lib.rs", "pub fn open_status() -> u8 {\n    1\n}\n");
        project.write(CATALOG, &root_catalog(STATEMENTS));
        project.write("intent/channels.stele", CHANNELS);
        project.write("tests/walk.rs", EXAMPLES);
        project.commit("initial");
        project
    }

    fn dir(&self) -> &Path {
        self.0.path()
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(self.dir())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.dir().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir().join(path)).unwrap()
    }

    /// Replace the first occurrence of `from`, which must be there.
    fn edit(&self, path: &str, from: &str, to: &str) {
        let text = self.read(path);
        assert!(text.contains(from), "{path} has no {from:?}");
        self.write(path, &text.replacen(from, to, 1));
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&[
            "commit",
            "-q",
            "--no-verify",
            "--allow-empty",
            "-m",
            message,
        ]);
        self.git(&["rev-parse", "HEAD"]).trim().to_string()
    }

    fn repo(&self) -> Repo {
        Repo::of_catalog(&self.dir().join("intent")).unwrap()
    }

    fn identity(&self) -> String {
        self.repo().working_tree_identity().unwrap()
    }

    fn approve(&self, ids: &[&str], by: &str) {
        approvals::append(self.dir(), CATALOG, |_, catalog, examples| {
            approvals::approve(catalog, examples, ids, by)
        })
        .unwrap();
    }

    fn approve_all(&self) {
        self.approve(&ITEMS, "owner");
    }

    /// Append one run of every example on each of its channels to the
    /// ledger.
    fn record(&self, outcome: Outcome, source: &str, diff: &str) {
        let catalog = load_catalog(&DirReader(self.dir().into()), CATALOG).unwrap();
        let packages = packages(self.dir()).unwrap();
        let examples = scan(self.dir(), &packages, &catalog).unwrap().examples;
        let runs: Vec<Run> = examples
            .iter()
            .flat_map(|example| {
                example.channels.iter().map(|channel| Run {
                    example: example.id.clone(),
                    digest: example.digest.clone(),
                    digest_version: DIGEST_VERSION,
                    channel: channel.clone(),
                    source: source.into(),
                    base: "0000000".into(),
                    outcome,
                    diff: (outcome == Outcome::Fail).then(|| diff.into()),
                })
            })
            .collect();
        runs::append(&self.dir().join(LEDGER), &runs).unwrap();
    }

    /// Every example failed on a scratch break.
    fn record_red(&self) {
        self.record(Outcome::Fail, "sha256:scratch", "a scratch break");
    }

    /// Every example passes at the current identity.
    fn record_green(&self) {
        self.record(Outcome::Pass, &self.identity(), "");
    }

    fn report(&self, base: Option<&str>) -> Report {
        self.report_on(base, None).unwrap()
    }

    fn report_on(&self, base: Option<&str>, snapshot: Option<Snapshot>) -> Result<Report, String> {
        let code = snapshot.map(|snapshot| Code {
            snapshots: vec![snapshot],
            working_tree: None,
        });
        report(
            &Options {
                root: self.dir().into(),
                catalog: CATALOG.into(),
                base: base.map(Into::into),
            },
            code.as_ref(),
        )
    }
}

fn verified() -> Project {
    let project = Project::new();
    project.approve_all();
    project.record_red();
    project
}

fn standings(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(id, standing)| ((*id).to_string(), (*standing).to_string()))
        .collect()
}

fn section<'a>(markdown: &'a str, heading: &str) -> &'a str {
    let start = markdown
        .find(&format!("\n## {heading}"))
        .unwrap_or_else(|| panic!("no {heading} section in\n{markdown}"));
    let rest = &markdown[start + 1..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |end| end + 3);
    &rest[..end]
}

#[test]
fn projecting_the_same_inputs_twice_gives_byte_identical_evidence_with_bare_ids_and_snapshot_bases()
{
    let project = verified();
    project.record_green();
    let identity = project.identity();
    let evidence = || {
        let inputs = Inputs::read(project.dir(), CATALOG).unwrap();
        projection::project(&inputs, &identity, None).unwrap()
    };
    let text = print_artifact(&evidence());
    assert_eq!(text, print_artifact(&evidence()));
    let evidence = evidence();
    assert_eq!(parse_artifact(&text).unwrap(), evidence);

    let mut counts = BTreeMap::new();
    for fact in &evidence.facts {
        let index = counts.entry(fact.relation.as_str()).or_insert(0);
        assert_eq!(fact.id, format!("{}_{index}", fact.relation));
        *index += 1;
        let basis = fact.basis.as_ref().unwrap();
        assert_eq!(basis.kind, EvidenceKind::Snapshot, "{fact:?}");
        assert!(basis.identity.as_ref().unwrap().starts_with("sha256:"));
    }
    assert!(evidence.facts.iter().any(|fact| {
        fact.args
            .iter()
            .any(|arg| matches!(arg, FactValue::Symbol(digest) if digest.starts_with("sha256:")))
    }));
    let scoped = evidence
        .facts
        .iter()
        .find(|fact| fact.relation == "scoped_to")
        .unwrap();
    assert_eq!(scoped.args[1], FactValue::Symbol("library".into()));
    assert_eq!(
        scoped.basis.as_ref().unwrap().source,
        "intent/catalog.stele#L11"
    );
    let expectations: Vec<_> = evidence
        .expectations
        .iter()
        .map(|e| (e.query.as_str(), e.count))
        .collect();
    assert_eq!(
        expectations,
        [
            ("awaiting_approval(I)", 0),
            ("broken_map(S, A)", 0),
            ("standing(\"library_returns_report\", Kind)", 1),
            ("standing(\"results_are_honest\", Kind)", 1),
            ("standing(\"walk_exit_status\", Kind)", 1),
        ]
    );

    let report = project.report(None);
    assert_eq!(report.markdown, project.report(None).markdown);
    let html = report.html(None).unwrap();
    assert!(html.contains(&report.target));
    assert!(!html.contains("Not current"), "{html}");
}

#[test]
fn an_unapproved_definition_makes_the_report_pending_and_is_listed_first() {
    let project = Project::new();
    project.approve(
        &[
            "cli",
            "library",
            "results_are_honest",
            "library_returns_report",
            "open_exits_one",
            "library_report",
        ],
        "owner",
    );
    let report = project.report(None);
    assert!(report.pending);
    assert_eq!(report.standings["walk_exit_status"], "unapproved");
    let awaiting = section(&report.markdown, "Awaiting approval");
    assert_eq!(
        awaiting,
        "## Awaiting approval\n\n### Definitions\n\n- `walk_exit_status`: never approved, or re-added after an approved removal\n"
    );
    assert!(report.markdown.find("## Awaiting approval") < report.markdown.find("## Statements"));
}

#[test]
fn a_fully_approved_catalog_with_unverified_definitions_is_not_pending_and_shows_reasons() {
    let project = Project::new();
    project.approve_all();
    let report = project.report(None);
    assert!(!report.pending, "{}", report.markdown);
    assert_eq!(
        report.standings,
        standings(&[
            ("library_returns_report", "unverified"),
            ("results_are_honest", "unverified"),
            ("walk_exit_status", "unverified"),
        ])
    );
    assert!(
        report
            .markdown
            .contains("## Awaiting approval\n\nNothing awaits approval.\n")
    );
    let statements = section(&report.markdown, "Statements");
    for reason in [
        "- Reason: an example has no failing run with its current text on this channel (`cli`)",
        "- Reason: an example does not pass cleanly at the target on this channel (`cli`)",
        "- Reason: a definition or exception it requires is not verified (`walk_exit_status`)",
    ] {
        assert!(statements.contains(reason), "{reason}\n{statements}");
    }
}

#[test]
fn html_and_markdown_show_each_statements_prose_links_and_standing() {
    let project = verified();
    project.record_green();
    let report = project.report(None);
    let statements = section(&report.markdown, "Statements");
    let green = &report.target;
    assert!(
        statements.contains(&format!(
            "### `walk_exit_status` (definition): verified\n\n> walk exits 0 when every expectation holds, 1 when one is open, and 2 when the spec is invalid.\n\n- File: `intent/catalog.stele:7`\n- Refines: `results_are_honest`\n- Examples:\n  - `open_exits_one` on `cli`: red fail, green pass at `{green}`\n"
        )),
        "{statements}"
    );
    assert!(
        statements.contains(&format!(
            "- Narrows: `walk_exit_status`\n- Kind: holds_instead on `library`\n- Examples:\n  - `library_report` on `library`: red fail, green pass at `{green}`\n"
        )),
        "{statements}"
    );
    let html = report.html(None).unwrap();
    for prose in [
        "Stele never reports a spec as passing while one of its claims is open.",
        "The library returns the report, and its status carries the same outcomes.",
        "stands verified",
        "refines",
    ] {
        assert!(html.contains(prose), "{prose}");
    }
}

#[test]
fn committing_green_runs_keeps_the_definition_verified_at_that_commit() {
    let project = verified();
    project.commit("approves and records red");
    let red = project.report(None);
    assert_eq!(red.standings["walk_exit_status"], "unverified");
    project.record_green();
    let green = project.report(None);
    assert_eq!(
        green.standings,
        standings(&[
            ("library_returns_report", "verified"),
            ("results_are_honest", "verified"),
            ("walk_exit_status", "verified"),
        ])
    );
    assert!(!green.pending);
    let head = project.commit("records green");
    let committed = project.report(None);
    assert_eq!(committed.target, green.target);
    assert_eq!(committed.standings, green.standings);
    assert!(
        committed.markdown.starts_with(&format!(
            "# Intent report\n\nSource identity `{}` matches the tree of HEAD commit `{head}`.\n",
            committed.target
        )),
        "{}",
        committed.markdown
    );
}

#[test]
fn the_header_names_the_evaluated_identity_and_whether_it_matches_head() {
    let project = Project::new();
    let head = project.commit("head");
    let clean = project.report(None);
    assert!(clean.markdown.starts_with(&format!(
        "# Intent report\n\nSource identity `{}` matches the tree of HEAD commit `{head}`.\n",
        clean.target
    )));
    project.edit("src/lib.rs", "    1", "    0");
    let dirty = project.report(None);
    assert_ne!(dirty.target, clean.target);
    assert_eq!(dirty.target, project.identity());
    assert!(
        dirty.markdown.starts_with(&format!(
            "# Intent report\n\nSource identity `{}` is the working tree, which does not match the tree of HEAD commit `{head}`, so these standings do not hold for that commit.\n",
            dirty.target
        )),
        "{}",
        dirty.markdown
    );
}

#[test]
fn approvals_added_since_the_base_are_listed_with_their_approver_and_an_edited_log_is_altered() {
    let project = Project::new();
    project.approve_all();
    let base = project.commit("approved");
    project.edit(CATALOG, "when one is open", "when an expectation is open");
    project.approve(&["walk_exit_status"], "agent");
    project.commit("changes the definition and approves it");
    let report = project.report(Some(&base));
    let since = section(&report.markdown, &format!("Since base `{base}`"));
    assert!(
        since.contains(
            "### Approvals\n\n- Entry 8: `agent` approved definition `walk_exit_status`\n"
        ),
        "{since}"
    );
    assert!(!since.contains("Entry 7"), "{since}");

    project.edit(
        "intent/approvals.stele",
        "by: \"owner\"",
        "by: \"someone else\"",
    );
    let report = project.report(Some(&base));
    let since = section(&report.markdown, &format!("Since base `{base}`"));
    assert!(since.contains("The approval log was altered"), "{since}");
    assert!(!since.contains("Entry 8"), "{since}");
}

#[test]
fn a_base_without_an_approval_log_lists_every_entry_as_added() {
    let project = Project::new();
    let base = project.commit("no approvals yet");
    project.approve_all();
    let report = project.report(Some(&base));
    let since = section(&report.markdown, &format!("Since base `{base}`"));
    assert_eq!(since.matches("\n- Entry ").count(), 7, "{since}");
    assert!(since.contains("- Entry 1: `owner` approved channel `cli`\n"));
    assert!(since.contains("- Entry 7: `owner` approved example `library_report`\n"));
    assert!(since.contains("### Failing runs\n\nNone.\n"), "{since}");
}

#[test]
fn an_approver_name_from_the_log_cannot_start_a_markdown_block() {
    let project = Project::new();
    let base = project.commit("no approvals yet");
    project.approve_all();
    project.edit(
        "intent/approvals.stele",
        "by: \"owner\"",
        "by: \"alice `x`\\n\\n<!--\"",
    );
    let report = project.report(Some(&base));
    let since = section(&report.markdown, &format!("Since base `{base}`"));
    assert!(
        since.contains("- Entry 1: ``alice `x`  <!--`` approved channel `cli`\n"),
        "{since}"
    );
    assert!(!report.markdown.contains("\n<!--"), "{}", report.markdown);
}

#[test]
fn a_failing_run_recorded_since_the_base_is_listed_with_the_diff_of_its_break() {
    let project = Project::new();
    project.approve_all();
    let base = project.commit("approved");
    project.edit("src/lib.rs", "    1", "    0");
    let scratch = project.identity();
    let diff = project.repo().diff(&base).unwrap();
    assert!(diff.contains("+    0"), "{diff}");
    project.record(Outcome::Fail, &scratch, &diff);
    project.edit("src/lib.rs", "    0", "    1");
    project.commit("records red on a scratch break");
    let report = project.report(Some(&base));
    let since = section(&report.markdown, &format!("Since base `{base}`"));
    assert!(
        since.contains(&format!(
            "`open_exits_one` failed on `cli` at source `{scratch}`, based on `0000000`:\n\n```diff\n{diff}```\n"
        )),
        "{since}"
    );
}

#[test]
fn a_source_edit_after_the_green_run_leaves_every_statement_unverified() {
    let project = verified();
    project.record_green();
    project.edit("src/lib.rs", "    1", "    0");
    let report = project.report(None);
    assert!(!report.pending, "{}", report.markdown);
    assert_eq!(
        report.standings,
        standings(&[
            ("library_returns_report", "unverified"),
            ("results_are_honest", "unverified"),
            ("walk_exit_status", "unverified"),
        ])
    );
    let statements = section(&report.markdown, "Statements");
    assert!(
        statements.contains(
            "- Reason: an example does not pass cleanly at the target on this channel (`cli`)"
        ),
        "{statements}"
    );
}

#[test]
fn editing_an_approved_example_awaits_approval_and_drops_its_red_run() {
    let project = verified();
    project.edit(
        "tests/walk.rs",
        "fn open_expectation_exits_one() {}",
        "fn open_expectation_exits_one() {\n    assert!(true);\n}",
    );
    project.record_green();
    let report = project.report(None);
    assert!(report.pending, "{}", report.markdown);
    assert_eq!(report.standings["walk_exit_status"], "unverified");
    assert_eq!(report.standings["results_are_honest"], "unverified");
    assert_eq!(report.standings["library_returns_report"], "verified");
    let awaiting = section(&report.markdown, "Awaiting approval");
    assert!(
        awaiting.contains("### Examples\n\n- `open_exits_one`: "),
        "{awaiting}"
    );
}

#[test]
fn a_decoy_test_in_place_of_a_compiled_out_example_does_not_keep_it_verified() {
    let project = verified();
    project.write(
        "tests/walk.rs",
        &format!(
            "#[cfg(any())]\nmod m {{\n{EXAMPLES}}}\nmod m {{\n    #[test]\n    fn open_expectation_exits_one() {{}}\n}}\n"
        ),
    );
    project.record_green();
    let report = project.report(None);
    assert!(report.pending, "{}", report.markdown);
    assert_eq!(report.standings["walk_exit_status"], "unverified");
    let ignored = section(&report.markdown, "Ignored examples");
    assert!(
        ignored
            .contains("another test in tests/walk.rs has the path m::open_expectation_exits_one"),
        "{ignored}"
    );
}

#[test]
fn moving_statements_into_an_imported_file_leaves_every_standing_unchanged_and_nothing_pending() {
    let project = verified();
    project.record_green();
    let before = project.report(None);
    assert!(!before.pending, "{}", before.markdown);
    assert_eq!(
        before.standings,
        standings(&[
            ("library_returns_report", "verified"),
            ("results_are_honest", "verified"),
            ("walk_exit_status", "verified"),
        ])
    );
    project.write(
        CATALOG,
        "catalog fixture {\n  use \"./channels.stele\" as channels\n  use \"./walk.stele\" as walk\n}\n",
    );
    project.write(
        "intent/walk.stele",
        &root_catalog(STATEMENTS).replace("fixture", "walk"),
    );
    // The source changed, so green is recorded again at the new identity.
    project.record_green();
    let after = project.report(None);
    assert_ne!(after.target, before.target);
    assert_eq!(after.standings, before.standings);
    assert!(!after.pending, "{}", after.markdown);
    assert!(after.markdown.contains("- File: `intent/walk.stele:7`"));
}

#[test]
fn a_test_tagged_with_an_unknown_statement_is_listed_as_ignored_and_the_report_still_renders() {
    let project = verified();
    project.write(
        "tests/ghost.rs",
        "/// intent: example ghost exemplifies no_such_statement on cli\n#[test]\nfn ghost() {}\n",
    );
    project.record_green();
    let report = project.report(None);
    let ignored = section(&report.markdown, "Ignored examples");
    assert_eq!(
        ignored,
        "## Ignored examples\n\n- `ghost` at `tests/ghost.rs:1` (`ghost`): unknown statement no_such_statement\n"
    );
    assert_eq!(report.standings["walk_exit_status"], "verified");
    assert!(!report.pending);
    assert!(
        !report
            .bound
            .facts
            .iter()
            .any(|fact| fact.args.contains(&FactValue::Symbol("ghost".into())))
    );
    assert!(report.html(None).is_ok());
}

#[test]
fn approvals_and_runs_recorded_under_another_digest_version_are_listed_and_not_evaluated() {
    let project = verified();
    let log = project.read("intent/approvals.stele");
    project.write(
        "intent/approvals.stele",
        &format!(
            "{log}approval {{\n  item: walk_exit_status\n  kind: definition\n  action: remove\n  by: \"old tool\"\n  digest: \"sha256:old\"\n  digest_version: 0\n}}\n"
        ),
    );
    let current = project.report(None);
    let digest = current
        .bound
        .facts
        .iter()
        .find(|fact| {
            fact.relation == "current_digest"
                && fact.args[0] == FactValue::Symbol("open_exits_one".into())
        })
        .map(|fact| fact.args[1].clone())
        .unwrap();
    let FactValue::Symbol(digest) = digest else {
        unreachable!()
    };
    runs::append(
        &project.dir().join(LEDGER),
        &[Run {
            example: "open_exits_one".into(),
            digest,
            digest_version: DIGEST_VERSION - 1,
            channel: "cli".into(),
            source: project.identity(),
            base: "0000000".into(),
            outcome: Outcome::Pass,
            diff: None,
        }],
    )
    .unwrap();
    let report = project.report(None);
    assert_eq!(report.standings["walk_exit_status"], "unverified");
    assert!(!report.pending, "{}", report.markdown);
    let skipped = section(&report.markdown, "Not evaluated");
    assert!(
        skipped.contains(
            "- Approval entry 8 for `walk_exit_status` by `old tool`: recorded under digest version 0\n"
        ),
        "{skipped}"
    );
    assert!(
        skipped.contains(&format!(
            "- Run of `open_exits_one` on `cli` in `{LEDGER}`: recorded under digest version 0\n"
        )),
        "{skipped}"
    );
    assert!(
        !report
            .bound
            .facts
            .iter()
            .any(|fact| fact.relation == "run" && fact.args[4] == FactValue::Symbol("pass".into()))
    );
}

#[test]
fn an_approved_statement_deleted_from_the_catalog_keeps_one_unapproved_standing() {
    let project = verified();
    project.write(
        CATALOG,
        &root_catalog(&STATEMENTS.replace(
            "  exception library_returns_report {\n    narrows: walk_exit_status\n    channels: [ch.library]\n    kind: holds_instead\n    text: \"The library returns the report, and its status carries the same outcomes.\"\n  }\n",
            "",
        )),
    );
    let report = project.report(None);
    assert!(report.pending);
    assert_eq!(report.standings["library_returns_report"], "unapproved");
    assert!(
        report
            .bound
            .expectations
            .iter()
            .any(|e| e.query == "standing(\"library_returns_report\", Kind)" && e.count == 1)
    );
    let statements = section(&report.markdown, "Statements");
    assert!(
        statements.contains(
            "### `library_returns_report` (exception): unapproved\n\n> The library returns the report, and its status carries the same outcomes.\n\n- File: removed from the catalog; its approval stands until a removal is approved\n- Narrows: `walk_exit_status`\n- Reason: removed without an approved removal\n"
        ),
        "{statements}"
    );
}

#[test]
fn a_ledger_at_the_base_that_does_not_parse_names_the_base() {
    let project = Project::new();
    let run = "run {\n  example: open_exits_one\n  digest: \"sha256:d\"\n  digest_version: 1\n  channel: cli\n  source: \"sha256:s\"\n  base: \"0000000\"\n  outcome: pass\n}\n";
    let (cut, _) = run.split_at(run.find("  channel").unwrap());
    project.write(LEDGER, cut);
    let base = project.commit("half a run");
    project.write(LEDGER, run);
    let error = project.report_on(Some(&base), None).err().unwrap();
    assert!(error.starts_with(&format!("{base}:{LEDGER}:")), "{error}");
}

/// The synthetic Rust snapshot of a `src/main.rs` with the functions
/// `entry`, `work`, `demo` and `unused`.
fn snapshot() -> Snapshot {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../codemap-topology/tests/fixtures/rust.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Map `walk_exit_status` to `anchors` in the catalog.
fn map_definition(project: &Project, anchors: &str) {
    project.edit(
        CATALOG,
        "    refines: results_are_honest\n",
        &format!("    refines: results_are_honest\n    realized_by: {anchors}\n"),
    );
}

#[test]
fn a_catalog_without_a_map_lists_every_definition_and_exception_as_unmapped() {
    let project = Project::new();
    project.approve_all();
    let report = project.report(None);
    assert!(!report.pending, "{}", report.markdown);
    assert_eq!(
        section(&report.markdown, "Map"),
        "## Map\n\nNo statement names the code that realizes it.\n\n### Unmapped\n\n- `library_returns_report` (exception)\n- `walk_exit_status` (definition)\n"
    );
}

#[test]
fn a_statement_mapped_to_code_the_snapshot_contains_stays_approved_and_lists_its_anchors() {
    let project = Project::new();
    project.approve_all();
    let before = project.report(None);
    map_definition(&project, "[\"src/main.rs\", \"rust:src/main.rs#entry\"]");
    let report = project.report_on(None, Some(snapshot())).unwrap();
    assert!(!report.pending, "{}", report.markdown);
    assert_eq!(report.standings, before.standings);
    assert!(
        report
            .markdown
            .contains("## Awaiting approval\n\nNothing awaits approval.\n")
    );
    assert_eq!(
        section(&report.markdown, "Map"),
        "## Map\n\n- `walk_exit_status` (definition) is realized by `rust:src/main.rs#entry`, `src/main.rs`\n\n### Unmapped\n\n- `library_returns_report` (exception)\n"
    );
    let realized: Vec<_> = report
        .bound
        .facts
        .iter()
        .filter(|fact| fact.relation == "realized" || fact.relation == "anchor_present")
        .map(|fact| {
            (
                fact.relation.as_str(),
                fact.args.clone(),
                fact.basis.as_ref().unwrap().source.as_str(),
            )
        })
        .collect();
    let symbol = |value: &str| FactValue::Symbol(value.into());
    assert_eq!(
        realized,
        [
            (
                "anchor_present",
                vec![symbol("rust:src/main.rs#entry")],
                "src/main.rs"
            ),
            ("anchor_present", vec![symbol("src/main.rs")], "src/main.rs"),
            (
                "realized",
                vec![symbol("walk_exit_status"), symbol("rust:src/main.rs#entry")],
                "intent/catalog.stele#L7"
            ),
            (
                "realized",
                vec![symbol("walk_exit_status"), symbol("src/main.rs")],
                "intent/catalog.stele#L7"
            ),
        ]
    );
}

#[test]
fn a_mapped_anchor_the_code_lacks_makes_the_report_pending_and_changes_no_standing() {
    let project = Project::new();
    project.approve_all();
    map_definition(&project, "[\"rust:src/main.rs#entry\"]");
    let present = project.report_on(None, Some(snapshot())).unwrap();
    assert!(!present.pending, "{}", present.markdown);

    project.edit(CATALOG, "#entry\"]", "#entry\", \"rust:src/main.rs#gone\"]");
    let report = project.report_on(None, Some(snapshot())).unwrap();
    assert!(report.pending, "{}", report.markdown);
    assert_eq!(report.standings, present.standings);
    assert!(
        report
            .markdown
            .contains("## Awaiting approval\n\nNothing awaits approval.\n")
    );
    assert!(
        section(&report.markdown, "Map").contains(
            "- `walk_exit_status` (definition) is realized by `rust:src/main.rs#entry`, `rust:src/main.rs#gone`; missing from the code: `rust:src/main.rs#gone`\n"
        ),
        "{}",
        report.markdown
    );
}

#[test]
fn a_mapped_catalog_needs_a_snapshot_in_the_language_of_its_symbols() {
    let project = Project::new();
    map_definition(&project, "[\"src/main.rs\"]");
    assert_eq!(
        project.report_on(None, None).err().unwrap(),
        "the catalog maps statements to code, so it needs a code snapshot"
    );
    project.edit(
        CATALOG,
        "[\"src/main.rs\"]",
        "[\"ruby:app/greeter.rb#Greeter\"]",
    );
    let error = project.report_on(None, Some(snapshot())).err().unwrap();
    assert!(
        error.contains("anchor ruby:app/greeter.rb#Greeter is ruby code, but the snapshot is rust"),
        "{error}"
    );
}

#[test]
fn a_plans_standings_need_approvals_and_are_taken_at_head() {
    let project = Project::new();
    assert_eq!(
        intent_report::standings(project.dir(), CATALOG, None),
        Ok(None)
    );
    project.approve_all();
    project.record_red();
    project.record_green();
    let at_head = intent_report::standings(project.dir(), CATALOG, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        at_head.target,
        project.repo().commit_identity("HEAD").unwrap()
    );
    assert_eq!(
        at_head.verified().collect::<Vec<_>>(),
        [
            "library_returns_report",
            "results_are_honest",
            "walk_exit_status"
        ]
    );

    // A dirty tree leaves the committed work verified for the plan, while
    // the report, which evaluates the working tree, finds it unverified.
    project.edit("src/lib.rs", "    1", "    0");
    assert_eq!(
        intent_report::standings(project.dir(), CATALOG, None)
            .unwrap()
            .unwrap(),
        at_head
    );
    assert_eq!(
        project.report(None).standings["walk_exit_status"],
        "unverified"
    );

    // Without a HEAD there is no committed work, so the working tree counts.
    project.git(&["checkout", "-q", "--orphan", "unborn"]);
    let unborn = intent_report::standings(project.dir(), CATALOG, None)
        .unwrap()
        .unwrap();
    assert_eq!(unborn.target, project.identity());
    assert!(unborn.verified().next().is_none(), "{unborn:?}");
}

#[test]
fn what_awaits_approval_is_grouped_by_kind() {
    let report = Project::new().report(None);
    let awaiting = section(&report.markdown, "Awaiting approval");
    let headings: Vec<&str> = awaiting
        .lines()
        .filter(|line| line.starts_with("### "))
        .collect();
    assert_eq!(
        headings,
        [
            "### Channels",
            "### Intents",
            "### Definitions",
            "### Exceptions",
            "### Examples"
        ]
    );
    assert!(
        awaiting.contains("### Channels\n\n- `cli`: never approved, or re-added after an approved removal\n- `library`: "),
        "{awaiting}"
    );
}

#[test]
fn each_example_shows_its_latest_red_and_green_runs_with_its_current_text() {
    let project = Project::new();
    project.approve_all();
    let line = |report: &Report| {
        section(&report.markdown, "Statements")
            .lines()
            .find(|line| line.starts_with("  - `open_exits_one` on `cli`: "))
            .unwrap()
            .to_string()
    };
    assert_eq!(
        line(&project.report(None)),
        "  - `open_exits_one` on `cli`: no red run, no green run"
    );
    project.record_red();
    assert_eq!(
        line(&project.report(None)),
        "  - `open_exits_one` on `cli`: red fail, no green run"
    );
    project.record_green();
    let first = project.identity();
    project.edit("src/lib.rs", "    1", "    2");
    project.record_green();
    let report = project.report(None);
    assert_ne!(report.target, first);
    assert_eq!(
        line(&report),
        format!(
            "  - `open_exits_one` on `cli`: red fail, green pass at `{}`",
            report.target
        )
    );

    // Runs recorded for the example's earlier text no longer show.
    project.edit(
        "tests/walk.rs",
        "fn open_expectation_exits_one() {}",
        "fn open_expectation_exits_one() {\n    assert!(true);\n}",
    );
    assert_eq!(
        line(&project.report(None)),
        "  - `open_exits_one` on `cli`: no red run, no green run"
    );
}
