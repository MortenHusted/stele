//! The approvals ledger: what `approve`, `reconfirm` and `remove` append,
//! what they refuse, and how the ledger reads back.

use std::{collections::BTreeMap, fs, path::Path};

use stele_plan::{
    DirReader, Statement,
    approvals::{self, Action, Entry, ItemKind, Log},
    digest::{DIGEST_VERSION, statement_digest},
    examples::{Example, packages, scan},
    load_catalog,
};

const CATALOG: &str = "intent/catalog.stele";
const LEDGER: &str = "intent/approvals.stele";
const TESTS: &str = "tests/walk.rs";

const CATALOG_SOURCE: &str = r#"catalog fixture {
  intent results_are_honest {
    text: "Stele never reports a spec as passing while one of its claims is open."
  }
  definition walk_exit_status {
    refines: results_are_honest
    text: "walk exits 0 when every expectation holds, 1 when one is open, and 2 when the spec is invalid."
  }
  exception library_returns_report {
    narrows: walk_exit_status
    channels: [library]
    kind: holds_instead
    text: "The library returns the report, and its status carries the same outcomes."
  }
  channel cli {
    text: "The stele command line."
  }
  channel library {
    text: "The engine as a Rust library."
  }
}
"#;

const INTENT_TEXT: &str = "Stele never reports a spec as passing while one of its claims is open.";
const DEFINITION_TEXT: &str = "walk exits 0 when every expectation holds, 1 when one is open, and 2 when the spec is invalid.";
const CHANGED_TEXT: &str = "walk exits 0 when every expectation holds and 1 otherwise.";
const EXCEPTION_TEXT: &str =
    "The library returns the report, and its status carries the same outcomes.";

const DEFINITION: &str = r#"  definition walk_exit_status {
    refines: results_are_honest
    text: "walk exits 0 when every expectation holds, 1 when one is open, and 2 when the spec is invalid."
  }
"#;

const EXCEPTION: &str = r#"  exception library_returns_report {
    narrows: walk_exit_status
    channels: [library]
    kind: holds_instead
    text: "The library returns the report, and its status carries the same outcomes."
  }
"#;

const OPEN_EXITS_ONE: &str = r"/// intent: example open_exits_one exemplifies walk_exit_status on library, cli
#[test]
fn open_expectation_exits_one() {
    assert_eq!(1, 1);
}
";

const INVALID_EXITS_TWO: &str = r"/// intent: example invalid_exits_two exemplifies walk_exit_status on cli
#[test]
fn invalid_artifact_exits_two() {
    assert_eq!(2, 2);
}
";

/// A scratch Cargo package holding the fixture catalog, its tagged examples
/// in `tests/walk.rs`, and its ledger once something is approved.
struct Project(tempfile::TempDir);

impl Project {
    fn new() -> Self {
        let project = Self(tempfile::tempdir().expect("tempdir"));
        project.write(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        project.write("src/lib.rs", "");
        project.write(CATALOG, CATALOG_SOURCE);
        project.write(TESTS, &format!("{OPEN_EXITS_ONE}\n{INVALID_EXITS_TWO}"));
        project
    }

    fn root(&self) -> &Path {
        self.0.path()
    }

    fn write(&self, path: &str, source: &str) {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    /// Replace `from` with `to` in the project file at `path`.
    fn edit(&self, path: &str, from: &str, to: &str) {
        let source = fs::read_to_string(self.root().join(path)).unwrap();
        assert!(source.contains(from), "{from:?} is not in {path}");
        self.write(path, &source.replace(from, to));
    }

    fn catalog(&self) -> BTreeMap<String, Statement> {
        load_catalog(&DirReader(self.root().into()), CATALOG).unwrap()
    }

    fn examples(&self) -> Vec<Example> {
        let packages = packages(self.root()).unwrap();
        scan(self.root(), &packages, &self.catalog())
            .unwrap()
            .examples
    }

    fn statement_digest(&self, id: &str) -> String {
        statement_digest(&self.catalog()[id])
    }

    fn example_digest(&self, id: &str) -> String {
        let examples = self.examples();
        examples.into_iter().find(|e| e.id == id).unwrap().digest
    }

    /// The ledger's bytes; empty while it does not exist.
    fn log_source(&self) -> String {
        fs::read_to_string(self.root().join(LEDGER)).unwrap_or_default()
    }

    fn log(&self) -> Log {
        Log::read(&DirReader(self.root().into()), LEDGER).unwrap()
    }

    fn append(
        &self,
        change: impl FnOnce(&Log, &BTreeMap<String, Statement>, &[Example]) -> Result<String, String>,
    ) -> Result<(), String> {
        approvals::append(self.root(), CATALOG, change)
    }

    fn approve(&self, ids: &[&str]) -> Result<(), String> {
        self.append(|_, catalog, examples| approvals::approve(catalog, examples, ids, "owner"))
    }

    fn reconfirm(&self, ids: &[&str]) -> Result<(), String> {
        self.append(|log, catalog, examples| {
            approvals::reconfirm(log, catalog, examples, ids, "owner")
        })
    }

    fn remove(&self, ids: &[&str]) -> Result<(), String> {
        self.append(|log, catalog, examples| {
            approvals::remove(log, catalog, examples, ids, "owner")
        })
    }
}

fn links(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(target, digest)| ((*target).to_string(), (*digest).to_string()))
        .collect()
}

#[test]
fn approving_a_definition_appends_its_digest_refined_intent_prose_and_approver_last() {
    let project = Project::new();
    project.approve(&["results_are_honest"]).unwrap();
    project.approve(&["walk_exit_status"]).unwrap();
    let log = project.log();
    assert_eq!(log.entries.len(), 2, "{log:?}");
    assert_eq!(log.entries[0].item, "results_are_honest");
    assert_eq!(
        log.entries[1],
        Entry {
            item: "walk_exit_status".into(),
            kind: ItemKind::Definition,
            action: Action::Approve,
            by: "owner".into(),
            digest: project.statement_digest("walk_exit_status"),
            digest_version: DIGEST_VERSION,
            links: links(&[(
                "results_are_honest",
                &project.statement_digest("results_are_honest")
            )]),
            channels: vec![],
            text: Some(DEFINITION_TEXT.into()),
        }
    );
}

#[test]
fn approving_an_example_stores_its_statement_channels_and_digest() {
    let project = Project::new();
    project.approve(&["open_exits_one"]).unwrap();
    assert_eq!(
        project.log().entries,
        [Entry {
            item: "open_exits_one".into(),
            kind: ItemKind::Example,
            action: Action::Approve,
            by: "owner".into(),
            digest: project.example_digest("open_exits_one"),
            digest_version: DIGEST_VERSION,
            links: links(&[(
                "walk_exit_status",
                &project.statement_digest("walk_exit_status")
            )]),
            channels: vec!["cli".into(), "library".into()],
            text: None,
        }]
    );
}

#[test]
fn an_entry_is_one_approval_block_with_one_field_per_line_and_escaped_prose() {
    let project = Project::new();
    project.edit(
        CATALOG,
        "Stele never reports",
        r#"Stele \"never\" reports\n\t\\"#,
    );
    project
        .approve(&["results_are_honest", "walk_exit_status", "open_exits_one"])
        .unwrap();
    let intent = project.statement_digest("results_are_honest");
    let definition = project.statement_digest("walk_exit_status");
    let example = project.example_digest("open_exits_one");
    assert_eq!(
        project.log_source(),
        format!(
            r#"approval {{
  item: results_are_honest
  kind: intent
  action: approve
  by: "owner"
  digest: "{intent}"
  digest_version: 1
  text: "Stele \"never\" reports\n\t\\ a spec as passing while one of its claims is open."
}}
approval {{
  item: walk_exit_status
  kind: definition
  action: approve
  by: "owner"
  digest: "{definition}"
  digest_version: 1
  links: [results_are_honest: "{intent}"]
  text: "{DEFINITION_TEXT}"
}}
approval {{
  item: open_exits_one
  kind: example
  action: approve
  by: "owner"
  digest: "{example}"
  digest_version: 1
  links: [walk_exit_status: "{definition}"]
  channels: [cli, library]
}}
"#
        )
    );
    assert_eq!(
        project.log().entries[0].text.as_deref(),
        Some("Stele \"never\" reports\n\t\\ a spec as passing while one of its claims is open.")
    );
}

#[test]
fn only_the_latest_approval_counts_so_text_reverted_to_an_earlier_approval_cannot_be_reconfirmed() {
    let project = Project::new();
    project.approve(&["walk_exit_status"]).unwrap();
    let text_a = project.statement_digest("walk_exit_status");
    project.edit(CATALOG, DEFINITION_TEXT, CHANGED_TEXT);
    project.approve(&["walk_exit_status"]).unwrap();
    let text_b = project.statement_digest("walk_exit_status");
    project.edit(CATALOG, CHANGED_TEXT, DEFINITION_TEXT);

    let log = project.log();
    let latest = log.latest()["walk_exit_status"];
    assert_eq!(latest.digest, text_b);
    assert_eq!(latest.text.as_deref(), Some(CHANGED_TEXT));
    assert_eq!(project.statement_digest("walk_exit_status"), text_a);
    assert_ne!(latest.digest, text_a);
    assert_eq!(
        project.reconfirm(&["walk_exit_status"]).unwrap_err(),
        "walk_exit_status changed since its latest approval; approve it instead"
    );
}

#[test]
fn reconfirming_an_unchanged_exception_records_its_definitions_new_digest() {
    let project = Project::new();
    project
        .approve(&["walk_exit_status", "library_returns_report"])
        .unwrap();
    let before = project.statement_digest("walk_exit_status");
    project.edit(CATALOG, DEFINITION_TEXT, CHANGED_TEXT);
    let after = project.statement_digest("walk_exit_status");
    assert_ne!(before, after);

    project.reconfirm(&["library_returns_report"]).unwrap();
    let log = project.log();
    let [_, approved, reconfirmed] = log.entries.as_slice() else {
        panic!("{log:?}")
    };
    assert_eq!(approved.links, links(&[("walk_exit_status", &before)]));
    assert_eq!(
        reconfirmed,
        &Entry {
            links: links(&[("walk_exit_status", &after)]),
            ..approved.clone()
        }
    );
}

#[test]
fn reconfirming_an_unchanged_example_records_its_definitions_new_digest() {
    let project = Project::new();
    project
        .approve(&["walk_exit_status", "open_exits_one"])
        .unwrap();
    let before = project.statement_digest("walk_exit_status");
    project.edit(CATALOG, DEFINITION_TEXT, CHANGED_TEXT);
    let after = project.statement_digest("walk_exit_status");

    project.reconfirm(&["open_exits_one"]).unwrap();
    let log = project.log();
    let [_, approved, reconfirmed] = log.entries.as_slice() else {
        panic!("{log:?}")
    };
    assert_eq!(approved.links, links(&[("walk_exit_status", &before)]));
    assert_eq!(
        reconfirmed,
        &Entry {
            links: links(&[("walk_exit_status", &after)]),
            ..approved.clone()
        }
    );

    project.edit(TESTS, "assert_eq!(1, 1)", "assert_eq!(1, 2)");
    assert_eq!(
        project.reconfirm(&["open_exits_one"]).unwrap_err(),
        "open_exits_one changed since its latest approval; approve it instead"
    );
}

#[test]
fn mapping_a_statement_to_code_leaves_its_approval_as_it_was() {
    let project = Project::new();
    project.approve(&["walk_exit_status"]).unwrap();
    let approved = project.log_source();
    project.edit(
        CATALOG,
        "    refines: results_are_honest\n",
        "    refines: results_are_honest\n    realized_by: [\"rust:src/lib.rs#walk\"]\n",
    );
    project.reconfirm(&["walk_exit_status"]).unwrap();
    assert_eq!(project.log_source(), approved.repeat(2));
}

#[test]
fn reconfirm_refuses_unknown_never_approved_and_changed_items_and_writes_nothing() {
    let project = Project::new();
    project.approve(&["walk_exit_status"]).unwrap();
    project.edit(CATALOG, DEFINITION_TEXT, CHANGED_TEXT);
    let before = project.log_source();
    for (ids, error) in [
        (["no_such_item"], "unknown item no_such_item"),
        (["cli"], "cli is not approved; approve it instead"),
        (
            ["invalid_exits_two"],
            "invalid_exits_two is not approved; approve it instead",
        ),
        (
            ["walk_exit_status"],
            "walk_exit_status changed since its latest approval; approve it instead",
        ),
    ] {
        assert_eq!(project.reconfirm(&ids).unwrap_err(), error);
        assert_eq!(project.log_source(), before);
    }
}

#[test]
fn remove_is_refused_while_approved_items_link_to_the_target_and_names_them() {
    let project = Project::new();
    project
        .approve(&[
            "results_are_honest",
            "walk_exit_status",
            "library_returns_report",
            "open_exits_one",
        ])
        .unwrap();
    let approved = project.log().latest()["walk_exit_status"].clone();
    let before = project.log_source();
    for (ids, error) in [
        (
            ["walk_exit_status"],
            "cannot remove walk_exit_status: approved items still link to it: library_returns_report, open_exits_one",
        ),
        (
            ["results_are_honest"],
            "cannot remove results_are_honest: approved items still link to it: walk_exit_status",
        ),
        (["no_such_item"], "unknown item no_such_item"),
        (
            ["invalid_exits_two"],
            "invalid_exits_two is not approved, so there is nothing to remove",
        ),
    ] {
        assert_eq!(project.remove(&ids).unwrap_err(), error);
        assert_eq!(project.log_source(), before);
    }

    // A deleted example that is still approved still links to its definition.
    project.edit(CATALOG, EXCEPTION, "");
    project.edit(TESTS, OPEN_EXITS_ONE, "");
    project.remove(&["library_returns_report"]).unwrap();
    assert_eq!(
        project.remove(&["walk_exit_status"]).unwrap_err(),
        "cannot remove walk_exit_status: approved items still link to it: open_exits_one"
    );

    // Removing the dependent in the same command clears the way.
    project.edit(CATALOG, DEFINITION, "");
    project
        .remove(&["walk_exit_status", "open_exits_one"])
        .unwrap();
    let log = project.log();
    assert_eq!(
        log.latest()["walk_exit_status"],
        &Entry {
            action: Action::Remove,
            links: BTreeMap::new(),
            text: None,
            ..approved
        }
    );
    let removed = log.latest()["open_exits_one"];
    assert_eq!(
        (removed.action, removed.kind, &removed.channels),
        (Action::Remove, ItemKind::Example, &vec![])
    );
}

#[test]
fn approving_an_unknown_id_or_while_the_catalog_has_a_link_error_writes_nothing() {
    let project = Project::new();
    project.approve(&["results_are_honest"]).unwrap();
    let before = project.log_source();
    assert_eq!(
        project
            .approve(&["walk_exit_status", "no_such_item"])
            .unwrap_err(),
        "unknown item no_such_item"
    );
    assert_eq!(project.log_source(), before);

    project.edit(
        CATALOG,
        "refines: results_are_honest",
        "refines: results_are_honst",
    );
    for refused in [
        project.approve(&["results_are_honest"]),
        project.reconfirm(&["results_are_honest"]),
        project.remove(&["results_are_honest"]),
    ] {
        assert_eq!(
            refused.unwrap_err(),
            "intent/catalog.stele:6: unknown reference results_are_honst in refines of walk_exit_status"
        );
        assert_eq!(project.log_source(), before);
    }
}

#[test]
fn approving_needs_an_approver_and_an_id_that_names_one_item() {
    let project = Project::new();
    assert_eq!(
        project
            .append(|_, catalog, examples| {
                approvals::approve(catalog, examples, &["walk_exit_status"], " ")
            })
            .unwrap_err(),
        "an approval needs the approver's name"
    );
    assert_eq!(
        project
            .append(|_, catalog, examples| {
                approvals::approve(catalog, examples, &["walk_exit_status"], "owner\n<!--")
            })
            .unwrap_err(),
        "the approver's name \"owner\\n<!--\" has a control character"
    );
    project.write(
        "tests/clash.rs",
        "/// intent: example walk_exit_status exemplifies walk_exit_status on cli\n#[test]\nfn clash() {}\n",
    );
    assert_eq!(
        project.approve(&["walk_exit_status"]).unwrap_err(),
        "walk_exit_status names both a statement and an example"
    );
    assert_eq!(project.log_source(), "");
}

#[test]
fn without_a_cargo_package_there_are_no_examples_to_approve() {
    let project = Project::new();
    std::fs::remove_file(project.root().join("Cargo.toml")).unwrap();
    assert_eq!(
        project.approve(&["open_exits_one"]).unwrap_err(),
        "unknown item open_exits_one"
    );
    project.approve(&["walk_exit_status"]).unwrap();
}

#[test]
fn every_write_leaves_the_existing_log_bytes_as_an_unchanged_prefix() {
    let project = Project::new();
    // Kept by hand, ending in a comment with no final newline.
    project.write(LEDGER, "// The approval log.\n// Entries follow");
    let mut written = project.log_source();
    let mut grows = |result: Result<(), String>| {
        result.unwrap();
        let now = project.log_source();
        assert!(
            now.len() > written.len() && now.starts_with(&written),
            "{now}"
        );
        written = now;
    };
    grows(project.approve(&["walk_exit_status", "library_returns_report"]));
    project.edit(CATALOG, DEFINITION_TEXT, CHANGED_TEXT);
    grows(project.reconfirm(&["library_returns_report"]));
    grows(project.approve(&["walk_exit_status"]));
    grows(project.remove(&["library_returns_report"]));
    assert_eq!(project.log().entries.len(), 5);
}

#[test]
fn entries_appended_on_two_branches_load_in_either_merge_order_and_the_later_one_wins() {
    let project = Project::new();
    project.approve(&["results_are_honest"]).unwrap();
    let base = project.log_source();
    let ours = approvals::approve(
        &project.catalog(),
        &project.examples(),
        &[
            "walk_exit_status",
            "library_returns_report",
            "open_exits_one",
        ],
        "alice",
    )
    .unwrap();
    project.edit(CATALOG, DEFINITION_TEXT, CHANGED_TEXT);
    let theirs = approvals::approve(
        &project.catalog(),
        &project.examples(),
        &["walk_exit_status"],
        "bob",
    )
    .unwrap();

    for (first, second, winner, text) in [
        (&ours, &theirs, "bob", CHANGED_TEXT),
        (&theirs, &ours, "alice", DEFINITION_TEXT),
    ] {
        let log = Log::parse(&format!("{base}{first}{second}"), LEDGER).unwrap();
        assert_eq!(log.entries.len(), 5);
        let latest = log.latest();
        assert_eq!(latest.len(), 4);
        assert_eq!(latest["walk_exit_status"].by, winner);
        assert_eq!(latest["walk_exit_status"].text.as_deref(), Some(text));
        assert_eq!(latest["library_returns_report"].by, "alice");
        assert_eq!(latest["open_exits_one"].channels, ["cli", "library"]);
        assert_eq!(
            latest["library_returns_report"].text.as_deref(),
            Some(EXCEPTION_TEXT)
        );
        assert_eq!(latest["results_are_honest"].by, "owner");
        assert_eq!(
            latest["results_are_honest"].text.as_deref(),
            Some(INTENT_TEXT)
        );
    }
}

#[test]
fn approving_a_changed_item_again_appends_an_entry_that_differs_only_where_it_changed() {
    let project = Project::new();
    project.approve(&["walk_exit_status"]).unwrap();
    project.edit(CATALOG, DEFINITION_TEXT, CHANGED_TEXT);
    project.approve(&["walk_exit_status"]).unwrap();

    let source = project.log_source();
    let lines: Vec<&str> = source.lines().collect();
    let (first, second) = lines.split_at(lines.len() / 2);
    let changed: Vec<_> = first
        .iter()
        .zip(second)
        .filter(|(a, b)| a != b)
        .map(|(a, _)| a.split(':').next().unwrap().trim())
        .collect();
    assert_eq!(changed, ["digest", "text"], "{source}");

    let log = project.log();
    let [first, second] = log.entries.as_slice() else {
        panic!("{log:?}")
    };
    assert_eq!(second.text.as_deref(), Some(CHANGED_TEXT));
    assert_eq!(
        first,
        &Entry {
            digest: first.digest.clone(),
            text: first.text.clone(),
            ..second.clone()
        }
    );
}

#[test]
fn a_missing_log_is_empty_and_malformed_entries_are_rejected_with_their_line() {
    assert_eq!(Log::read(&BTreeMap::new(), LEDGER).unwrap(), Log::default());
    let entry = "approval {\n  item: x\n  kind: intent\n  action: approve\n  by: \"owner\"\n  digest: \"sha256:a\"\n  digest_version: 1\n}\n";
    assert_eq!(Log::parse(entry, LEDGER).unwrap().entries.len(), 1);
    for (source, error) in [
        (
            entry.replace("kind: intent", "kind: statement"),
            "intent/approvals.stele:3: approval of x has unknown kind statement; expected intent, definition, exception, channel or example",
        ),
        (
            entry.replace("action: approve", "action: revoke"),
            "intent/approvals.stele:4: approval of x has unknown action revoke; expected approve or remove",
        ),
        (
            entry.replace("  digest: \"sha256:a\"\n", ""),
            "intent/approvals.stele:1: approval of x needs digest",
        ),
        (
            entry.replace("  item: x\n", ""),
            "intent/approvals.stele:1: approval needs item",
        ),
        (
            entry.replace("item: x", "item: \"x\""),
            "intent/approvals.stele:2: item of an approval must be an item ID",
        ),
        (
            entry.replace("  by:", "  approver: \"owner\"\n  by:"),
            "intent/approvals.stele:5: approval of x has no field approver",
        ),
        (
            entry.replace("  by:", "  by: \"owner\"\n  by:"),
            "intent/approvals.stele:6: duplicate field by in approval of x",
        ),
        (
            entry.replace("by: \"owner\"", "by: owner"),
            "intent/approvals.stele:5: by of x must be a string",
        ),
        (
            entry.replace("digest_version: 1", "digest_version: \"1\""),
            "intent/approvals.stele:7: digest_version of x must be an integer",
        ),
        (
            entry.replace(
                "  digest_version: 1\n",
                "  digest_version: 1\n  links: [y]\n",
            ),
            "intent/approvals.stele:8: links of x must be a list of target: \"digest\" entries",
        ),
        (
            entry.replace(
                "  digest_version: 1\n",
                "  digest_version: 1\n  links: [y: \"sha256:b\", y: \"sha256:c\"]\n",
            ),
            "intent/approvals.stele:8: links of x name y twice",
        ),
        (
            entry.replace(
                "  digest_version: 1\n",
                "  digest_version: 1\n  channels: [\"cli\"]\n",
            ),
            "intent/approvals.stele:8: channels of x must be a list of channel IDs",
        ),
        (
            format!("{entry}run {{\n}}\n"),
            "intent/approvals.stele:9: expected an approval entry, found run",
        ),
        (
            "catalog log {\n}\n".to_string(),
            "intent/approvals.stele:1: expected '{', found log",
        ),
    ] {
        assert_eq!(Log::parse(&source, LEDGER).unwrap_err(), error, "{source}");
    }
}
