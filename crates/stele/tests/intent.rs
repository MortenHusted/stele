//! `stele intent` checks a catalog, appends approvals of its statements and
//! examples to the ledger beside the root, writing nothing when an ID fails,
//! records the examples' runs through `cargo test`, each in its own package,
//! and reports the standing of every statement and the code that realizes
//! it.

use std::path::Path;
use std::process::{Command, Output};

const CATALOG: &str = r#"catalog greeter {
  intent greets_people {
    text: "The greeter greets people by name."
  }
  definition greeting_names_the_person {
    refines: greets_people
    text: "A greeting is Hello, the person's name, and an exclamation mark."
  }
  channel cli {
    text: "The greet command line."
  }
  channel library {
    text: "The greeter Rust library."
  }
}
"#;

const GREETING_TEST: &str = r#"/// intent: example hello_grace exemplifies greeting_names_the_person on cli
#[test]
fn greets_grace() {
    assert_eq!(format!("Hello, {}!", "Grace"), "Hello, Grace!");
}
"#;

const LEDGER: &str = "intent/approvals.stele";

fn stele(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run stele")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A directory holding the catalog, outside any repository unless the
/// caller makes it one.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "intent/catalog.stele", CATALOG);
    dir
}

fn write(dir: &Path, path: &str, source: &str) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, source).unwrap();
}

/// The ledger's bytes; empty while it does not exist.
fn ledger(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(LEDGER)).unwrap_or_default()
}

#[test]
fn check_exits_zero_on_a_valid_catalog_and_two_with_the_reason() {
    let dir = project();
    for args in [
        &["intent", "check"][..],
        &["intent", "check", "--catalog", "intent/catalog.stele"],
    ] {
        let output = stele(dir.path(), args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            stderr(&output)
        );
    }

    write(
        dir.path(),
        "plans/next.stele",
        "plan next {\n  use \"../intent/catalog.stele\" as intent\n}\n",
    );
    let plan = stele(
        dir.path(),
        &["intent", "check", "--catalog", "plans/next.stele"],
    );
    assert_eq!(plan.status.code(), Some(2));
    assert_eq!(
        stderr(&plan),
        "stele intent: plans/next.stele is a plan, not a catalog\n"
    );

    let missing = stele(
        dir.path(),
        &["intent", "check", "--catalog", "intent/none.stele"],
    );
    assert_eq!(missing.status.code(), Some(2));
    assert!(
        stderr(&missing).starts_with("stele intent: read intent/none.stele"),
        "{}",
        stderr(&missing)
    );

    write(
        dir.path(),
        "intent/catalog.stele",
        &CATALOG.replace("refines: greets_people", "refines: greets"),
    );
    let broken = stele(dir.path(), &["intent", "check"]);
    assert_eq!(broken.status.code(), Some(2));
    assert_eq!(
        stderr(&broken),
        "stele intent: intent/catalog.stele:6: unknown reference greets in refines of greeting_names_the_person\n"
    );
}

#[test]
fn the_default_catalog_sits_at_the_top_of_the_repository() {
    let dir = project();
    write(dir.path(), "src/lib.rs", "");
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("run git");
    assert!(status.success());

    let src = dir.path().join("src");
    let check = stele(&src, &["intent", "check"]);
    assert_eq!(check.status.code(), Some(0), "{}", stderr(&check));
    let approve = stele(
        &src,
        &["intent", "approve", "greets_people", "--by", "owner"],
    );
    assert_eq!(approve.status.code(), Some(0), "{}", stderr(&approve));
    assert!(ledger(dir.path()).contains("item: greets_people\n"));
    assert!(!src.join(LEDGER).exists());
}

#[test]
fn approve_appends_an_entry_and_approving_unchanged_text_again_appends_the_same_entry() {
    let dir = project();
    let approve = [
        "intent",
        "approve",
        "greeting_names_the_person",
        "--by",
        "owner",
    ];
    let first = stele(dir.path(), &approve);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert_eq!(String::from_utf8_lossy(&first.stdout), "");
    let catalog = stele_plan::load_catalog(
        &stele_plan::DirReader(dir.path().into()),
        "intent/catalog.stele",
    )
    .unwrap();
    let digest = |id: &str| stele_plan::digest::statement_digest(&catalog[id]);
    let entry = ledger(dir.path());
    assert_eq!(
        entry,
        format!(
            r#"approval {{
  item: greeting_names_the_person
  kind: definition
  action: approve
  by: "owner"
  digest: "{}"
  digest_version: 1
  links: [greets_people: "{}"]
  text: "A greeting is Hello, the person's name, and an exclamation mark."
}}
"#,
            digest("greeting_names_the_person"),
            digest("greets_people")
        )
    );

    let second = stele(dir.path(), &approve);
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert_eq!(ledger(dir.path()), format!("{entry}{entry}"));
}

#[test]
fn an_unknown_id_or_a_missing_approver_writes_nothing_and_exits_two() {
    let dir = project();
    let unknown = stele(
        dir.path(),
        &[
            "intent",
            "approve",
            "greets_people",
            "ghost",
            "--by",
            "owner",
        ],
    );
    assert_eq!(unknown.status.code(), Some(2));
    assert_eq!(stderr(&unknown), "stele intent: unknown item ghost\n");

    let blank = stele(
        dir.path(),
        &["intent", "approve", "greets_people", "--by", " "],
    );
    assert_eq!(blank.status.code(), Some(2));
    assert_eq!(
        stderr(&blank),
        "stele intent: an approval needs the approver's name\n"
    );

    for args in [
        &["intent", "approve", "greets_people"][..],
        &["intent", "approve", "--by", "owner"],
        &["intent", "reconfirm", "greets_people", "--by", "owner"],
        &["intent", "remove", "greets_people", "--by", "owner"],
    ] {
        let refused = stele(dir.path(), args);
        assert_eq!(refused.status.code(), Some(2), "{args:?}");
    }
    assert!(!dir.path().join(LEDGER).exists());
}

#[test]
fn remove_is_refused_while_an_approved_statement_links_to_it() {
    let dir = project();
    let approve = stele(
        dir.path(),
        &[
            "intent",
            "approve",
            "greets_people",
            "greeting_names_the_person",
            "--by",
            "owner",
        ],
    );
    assert_eq!(approve.status.code(), Some(0), "{}", stderr(&approve));
    let before = ledger(dir.path());

    let linked = stele(
        dir.path(),
        &["intent", "remove", "greets_people", "--by", "owner"],
    );
    assert_eq!(linked.status.code(), Some(2));
    assert_eq!(
        stderr(&linked),
        "stele intent: cannot remove greets_people: approved items still link to it: greeting_names_the_person\n"
    );
    assert_eq!(ledger(dir.path()), before);

    write(
        dir.path(),
        "intent/catalog.stele",
        &CATALOG.replace("the person's name", "their name"),
    );
    let reconfirm = stele(
        dir.path(),
        &["intent", "reconfirm", "greets_people", "--by", "agent"],
    );
    assert_eq!(reconfirm.status.code(), Some(0), "{}", stderr(&reconfirm));
    let changed = stele(
        dir.path(),
        &[
            "intent",
            "reconfirm",
            "greeting_names_the_person",
            "--by",
            "agent",
        ],
    );
    assert_eq!(changed.status.code(), Some(2));
    assert_eq!(
        stderr(&changed),
        "stele intent: greeting_names_the_person changed since its latest approval; approve it instead\n"
    );

    let removed = stele(
        dir.path(),
        &[
            "intent",
            "remove",
            "greets_people",
            "greeting_names_the_person",
            "--by",
            "agent",
        ],
    );
    assert_eq!(removed.status.code(), Some(0), "{}", stderr(&removed));
    let log = ledger(dir.path());
    assert!(log.starts_with(&before));
    assert_eq!(log[before.len()..].matches("by: \"agent\"").count(), 3);
    assert_eq!(log[before.len()..].matches("action: remove").count(), 2);
}

#[test]
fn approve_takes_the_id_of_an_example_in_the_package_tests() {
    let dir = project();
    write(
        dir.path(),
        "Cargo.toml",
        "[package]\nname = \"greeter\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(dir.path(), "src/lib.rs", "");
    write(dir.path(), "tests/greeting.rs", GREETING_TEST);
    let approve = stele(
        dir.path(),
        &["intent", "approve", "hello_grace", "--by", "owner"],
    );
    assert_eq!(approve.status.code(), Some(0), "{}", stderr(&approve));
    let entry = ledger(dir.path());
    for line in [
        "  item: hello_grace\n",
        "  kind: example\n",
        "  links: [greeting_names_the_person: \"sha256:",
        "  channels: [cli]\n",
    ] {
        assert!(entry.contains(line), "{line:?} not in {entry}");
    }
}

const ALPHA_CHECKS: &str = r#"/// intent: example alpha_greets exemplifies greeting_names_the_person on cli
#[test]
fn greets() {
    assert_eq!(alpha::greeting(), "Hello, Grace!");
}
"#;

const BETA_CHECKS: &str = r#"/// intent: example beta_greets exemplifies greeting_names_the_person on cli
#[test]
fn greets() {
    assert_eq!(beta::greeting(), "Hello, Grace!");
}

/// intent: example beta_on_cli exemplifies greeting_names_the_person on cli, library
#[test]
fn runs_on_the_command_line() {
    assert_eq!(std::env::var("STELE_CHANNEL").as_deref(), Ok("cli"));
}
"#;

fn run(dir: &Path, program: &str, args: &[&str]) {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("run {program}: {e}"));
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        stderr(&output)
    );
}

/// A committed git repository holding the catalog and a workspace of two
/// packages whose test files share the name `checks.rs` and the test name
/// `greets`. Only alpha's greeting is wrong, so a run that reached the other
/// package's test would come out the other way.
fn workspace() -> tempfile::TempDir {
    let dir = project();
    let package = |name: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
    };
    for (path, source) in [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/alpha\", \"crates/beta\"]\nresolver = \"2\"\n"
                .to_string(),
        ),
        (".gitignore", "target/\n".to_string()),
        ("crates/alpha/Cargo.toml", package("alpha")),
        (
            "crates/alpha/src/lib.rs",
            "pub fn greeting() -> &'static str {\n    \"Hi\"\n}\n".to_string(),
        ),
        ("crates/alpha/tests/checks.rs", ALPHA_CHECKS.to_string()),
        ("crates/beta/Cargo.toml", package("beta")),
        (
            "crates/beta/src/lib.rs",
            "pub fn greeting() -> &'static str {\n    \"Hello, Grace!\"\n}\n".to_string(),
        ),
        ("crates/beta/tests/checks.rs", BETA_CHECKS.to_string()),
    ] {
        write(dir.path(), path, &source);
    }
    run(dir.path(), "cargo", &["generate-lockfile", "--offline"]);
    commit_all(dir.path());
    dir
}

/// Make `dir` a git repository with everything in it committed.
fn commit_all(dir: &Path) {
    let git = |args: &[&str]| {
        let mut all = vec![
            "-c",
            "user.email=stele@example.com",
            "-c",
            "user.name=stele",
            "-c",
            "core.hooksPath=no-hooks",
        ];
        all.extend(args);
        run(dir, "git", &all);
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "--no-verify", "-m", "start"]);
}

#[test]
fn record_runs_each_example_in_its_own_package_on_each_of_its_channels() {
    let dir = workspace();
    let record = stele(dir.path(), &["intent", "record"]);
    assert_eq!(record.status.code(), Some(0), "{}", stderr(&record));
    assert_eq!(
        String::from_utf8_lossy(&record.stdout),
        "alpha_greets on cli: fail\nbeta_greets on cli: pass\nbeta_on_cli on cli: pass\nbeta_on_cli on library: fail\n"
    );
    let ledger = std::fs::read_to_string(dir.path().join("intent/runs.stele")).unwrap();
    let runs = stele_plan::runs::parse(&ledger, "intent/runs.stele").unwrap();
    assert_eq!(runs.len(), 4);
    let source = &runs[0].source;
    assert!(source.starts_with("sha256:"), "{source}");
    for run in &runs {
        assert_eq!(&run.source, source);
        assert_eq!(
            run.diff.as_deref(),
            (run.outcome == stele_plan::runs::Outcome::Fail).then_some(""),
            "a clean tree has no diff"
        );
    }

    let one = stele(
        dir.path(),
        &[
            "intent",
            "record",
            "--example",
            "beta_on_cli",
            "--channel",
            "cli",
        ],
    );
    assert_eq!(one.status.code(), Some(0), "{}", stderr(&one));
    assert_eq!(
        String::from_utf8_lossy(&one.stdout),
        "beta_on_cli on cli: pass\n"
    );
    let grown = std::fs::read_to_string(dir.path().join("intent/runs.stele")).unwrap();
    assert!(grown.starts_with(&ledger));
    assert_eq!(grown[ledger.len()..].matches("run {").count(), 1);
}

#[test]
fn source_baseline_recording_checks_the_expected_identity_and_records_real_outcomes() {
    let dir = workspace();
    let repo = stele_plan::identity::Repo::of_catalog(&dir.path().join("intent")).unwrap();
    let baseline = repo.commit_identity("HEAD").unwrap();
    let ledger = dir.path().join("intent/runs.stele");
    for invalid in [
        "sha256:abc".to_string(),
        format!("sha256:{}", "0".repeat(64)),
    ] {
        let output = stele(
            dir.path(),
            &["intent", "record", "--source-baseline", &invalid],
        );
        assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
        assert!(!ledger.exists());
    }
    write(
        dir.path(),
        "crates/alpha/src/lib.rs",
        "pub fn greeting() -> &'static str {\n    \"Broken\"\n}\n",
    );
    let broken = repo.working_tree_identity().unwrap();
    let observed = repo.diff("HEAD").unwrap();
    let red = stele(
        dir.path(),
        &[
            "intent",
            "record",
            "--source-baseline",
            &baseline,
            "--example",
            "alpha_greets",
        ],
    );
    assert_eq!(red.status.code(), Some(0), "{}", stderr(&red));
    assert_eq!(
        String::from_utf8_lossy(&red.stdout),
        "alpha_greets on cli: fail\n"
    );
    run(dir.path(), "git", &["restore", "crates/alpha/src/lib.rs"]);
    let green = stele(
        dir.path(),
        &[
            "intent",
            "record",
            "--source-baseline",
            &baseline,
            "--example",
            "beta_greets",
        ],
    );
    assert_eq!(green.status.code(), Some(0), "{}", stderr(&green));
    assert_eq!(
        String::from_utf8_lossy(&green.stdout),
        "beta_greets on cli: pass\n"
    );
    let runs = stele_plan::runs::parse(
        &std::fs::read_to_string(ledger).unwrap(),
        "intent/runs.stele",
    )
    .unwrap();
    assert_eq!(runs.len(), 2);
    assert!(
        runs.iter()
            .all(|run| run.base == format!("source:{baseline}"))
    );
    assert_eq!(runs[0].source, broken);
    assert_eq!(runs[0].diff.as_deref(), Some(observed.as_str()));
    assert_eq!(runs[1].source, baseline);
    assert_eq!(runs[1].diff, None);
}

#[test]
fn record_refuses_an_unknown_selection_and_records_nothing_without_examples() {
    let dir = workspace();
    for (args, error) in [
        (
            &["intent", "record", "--example", "ghost"][..],
            "stele intent: unknown example ghost\n",
        ),
        (
            &["intent", "record", "--channel", "web"],
            "stele intent: no example is tagged for channel web\n",
        ),
    ] {
        let refused = stele(dir.path(), args);
        assert_eq!(refused.status.code(), Some(2), "{args:?}");
        assert_eq!(stderr(&refused), error);
    }
    assert!(!dir.path().join("intent/runs.stele").exists());

    let plain = project();
    run(plain.path(), "git", &["init", "-q"]);
    let nothing = stele(plain.path(), &["intent", "record"]);
    assert_eq!(nothing.status.code(), Some(0), "{}", stderr(&nothing));
    assert_eq!(String::from_utf8_lossy(&nothing.stdout), "");
    assert!(!plain.path().join("intent/runs.stele").exists());
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

const STATEMENTS: [&str; 4] = [
    "greets_people",
    "greeting_names_the_person",
    "cli",
    "library",
];

#[test]
fn report_exits_one_while_anything_awaits_approval_and_zero_once_approved_even_if_unverified() {
    let dir = project();
    commit_all(dir.path());
    let pending = stele(dir.path(), &["intent", "report"]);
    assert_eq!(pending.status.code(), Some(1), "{}", stderr(&pending));
    let report = stdout(&pending);
    assert!(report.starts_with("# Intent report\n"), "{report}");
    assert!(
        report.contains(
            "### Definitions\n\n- `greeting_names_the_person`: never approved, or re-added after an approved removal\n"
        ),
        "{report}"
    );
    assert!(
        report.contains("- File: `intent/catalog.stele:5`"),
        "{report}"
    );

    let mut approve = vec!["intent", "approve"];
    approve.extend(STATEMENTS);
    approve.extend(["--by", "owner"]);
    let approved = stele(dir.path(), &approve);
    assert!(approved.status.success(), "{}", stderr(&approved));
    let unverified = stele(dir.path(), &["intent", "report"]);
    assert_eq!(unverified.status.code(), Some(0), "{}", stderr(&unverified));
    assert!(
        stdout(&unverified).contains("### `greeting_names_the_person` (definition): unverified"),
        "{}",
        stdout(&unverified)
    );
}

#[test]
fn report_writes_markdown_html_and_the_bound_evidence_to_the_paths_given() {
    let dir = project();
    commit_all(dir.path());
    let mut approve = vec!["intent", "approve"];
    approve.extend(STATEMENTS);
    approve.extend(["--by", "owner"]);
    assert!(stele(dir.path(), &approve).status.success());
    std::fs::create_dir_all(dir.path().join("out")).unwrap();
    let html = stele(
        dir.path(),
        &[
            "intent",
            "report",
            "--format",
            "html",
            "-o",
            "out/report.html",
            "--bound",
            "out/bound.stele",
        ],
    );
    assert_eq!(html.status.code(), Some(0), "{}", stderr(&html));
    assert_eq!(stdout(&html), "");
    let page = std::fs::read_to_string(dir.path().join("out/report.html")).unwrap();
    assert!(page.contains("<html"), "{page}");
    assert!(page.contains("href=\"../intent/catalog.stele#L"), "{page}");
    let bound = lemmaspec::walk_artifact(
        &std::fs::read_to_string(dir.path().join("out/bound.stele")).unwrap(),
    )
    .unwrap();
    assert_eq!(bound.status, "clean");
    assert!(bound.facts.iter().any(|fact| fact.relation == "standing"
        && fact.args == ["greeting_names_the_person", "unverified"]));

    let markdown = stele(dir.path(), &["intent", "report", "--out", "out/report.md"]);
    assert_eq!(markdown.status.code(), Some(0), "{}", stderr(&markdown));
    assert!(
        std::fs::read_to_string(dir.path().join("out/report.md"))
            .unwrap()
            .contains("### `greeting_names_the_person` (definition): unverified")
    );
}

#[test]
fn a_report_that_cannot_be_built_leaves_existing_output_files_untouched() {
    let dir = project();
    commit_all(dir.path());
    write(dir.path(), "out.md", "previous report\n");
    write(dir.path(), "bound.stele", "previous bound\n");
    let outputs = ["intent", "report", "-o", "out.md", "--bound", "bound.stele"];
    for (extra, ours) in [
        (&["--format", "pdf"][..], false),
        (&["--frobnicate"], false),
        (&["-o", "other.md"], false),
        (&["--base", "no-such-commit"], true),
        (&["--catalog", "missing.stele"], true),
    ] {
        let mut args = outputs.to_vec();
        args.extend(extra);
        let run = stele(dir.path(), &args);
        assert_eq!(run.status.code(), Some(2), "{args:?}: {}", stderr(&run));
        assert_eq!(
            stderr(&run).starts_with("stele intent: "),
            ours,
            "{args:?}: {}",
            stderr(&run)
        );
        let read = |path: &str| std::fs::read_to_string(dir.path().join(path)).unwrap();
        assert_eq!(read("out.md"), "previous report\n", "{args:?}");
        assert_eq!(read("bound.stele"), "previous bound\n", "{args:?}");
    }
}

#[test]
fn checker_prints_the_shipped_intent_checker() {
    let dir = project();
    let printed = stele(dir.path(), &["intent", "checker"]);
    assert!(printed.status.success(), "{}", stderr(&printed));
    assert_eq!(stdout(&printed), stele_plan::INTENT_CHECKER);
    let written = stele(dir.path(), &["intent", "checker", "--out", "intent.stele"]);
    assert!(written.status.success(), "{}", stderr(&written));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("intent.stele")).unwrap(),
        stele_plan::INTENT_CHECKER
    );
}

const GREETER_CATALOG: &str = r#"catalog greeter {
  intent greets_people {
    text: "The greeter greets people by name."
  }
  definition greeting_names_the_person {
    refines: greets_people
    text: "A greeting is Hello, the person's name, and an exclamation mark."
  }
  channel cli {
    text: "The greet command line."
  }
}
"#;

const GREETER: &str = "pub fn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\n";

const GREETER_TEST: &str = r#"/// intent: example hello_grace exemplifies greeting_names_the_person on cli
#[test]
fn greets_grace() {
    assert_eq!(greeter::greet("Grace"), "Hello, Grace!");
}
"#;

#[test]
fn red_then_green_then_approval_verifies_the_definition_and_its_intent() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (path, source) in [
        ("intent/catalog.stele", GREETER_CATALOG),
        (
            "Cargo.toml",
            "[package]\nname = \"greeter\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (".gitignore", "target/\n"),
        ("src/lib.rs", GREETER),
        ("tests/greeting.rs", GREETER_TEST),
    ] {
        write(dir.path(), path, source);
    }
    run(dir.path(), "cargo", &["generate-lockfile", "--offline"]);
    commit_all(dir.path());

    // A deliberate break in a tracked file: the example must fail on it.
    write(dir.path(), "src/lib.rs", &GREETER.replace("Hello", "Hi"));
    let red = stele(
        dir.path(),
        &["intent", "record", "--example", "hello_grace"],
    );
    assert_eq!(red.status.code(), Some(0), "{}", stderr(&red));
    assert_eq!(stdout(&red), "hello_grace on cli: fail\n");
    write(dir.path(), "src/lib.rs", GREETER);
    let green = stele(dir.path(), &["intent", "record"]);
    assert_eq!(green.status.code(), Some(0), "{}", stderr(&green));
    assert_eq!(stdout(&green), "hello_grace on cli: pass\n");

    let unapproved = stele(dir.path(), &["intent", "report"]);
    assert_eq!(unapproved.status.code(), Some(1), "{}", stdout(&unapproved));
    let approve = stele(
        dir.path(),
        &[
            "intent",
            "approve",
            "greets_people",
            "greeting_names_the_person",
            "cli",
            "hello_grace",
            "--by",
            "owner",
        ],
    );
    assert!(approve.status.success(), "{}", stderr(&approve));

    let report = stele(dir.path(), &["intent", "report"]);
    let text = stdout(&report);
    assert_eq!(report.status.code(), Some(0), "{text}{}", stderr(&report));
    for line in [
        "matches the tree of HEAD commit",
        "## Awaiting approval\n\nNothing awaits approval.\n",
        "### `greeting_names_the_person` (definition): verified\n",
        "- Examples:\n  - `hello_grace` on `cli`: red fail, green pass at `sha256:",
        "### `greets_people` (intent): verified\n",
    ] {
        assert!(text.contains(line), "{line:?} not in\n{text}");
    }
}

/// A committed scratch crate whose definition names `greet` in
/// `src/lib.rs` as the code that realizes it, with every item approved.
fn mapped_greeter() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = GREETER_CATALOG.replace(
        "    refines: greets_people\n",
        "    refines: greets_people\n    realized_by: [\"rust:src/lib.rs#greet\", \"src/lib.rs\"]\n",
    );
    for (path, source) in [
        ("intent/catalog.stele", catalog.as_str()),
        (
            "Cargo.toml",
            "[package]\nname = \"greeter\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", GREETER),
        ("tests/greeting.rs", GREETER_TEST),
    ] {
        write(dir.path(), path, source);
    }
    commit_all(dir.path());
    let approve = stele(
        dir.path(),
        &[
            "intent",
            "approve",
            "greets_people",
            "greeting_names_the_person",
            "cli",
            "hello_grace",
            "--by",
            "owner",
        ],
    );
    assert!(approve.status.success(), "{}", stderr(&approve));
    dir
}

#[test]
fn renaming_the_code_a_statement_names_makes_report_exit_one_naming_the_anchor() {
    let dir = mapped_greeter();
    let mapped = stele(dir.path(), &["intent", "report"]);
    let text = stdout(&mapped);
    assert_eq!(mapped.status.code(), Some(0), "{text}{}", stderr(&mapped));
    assert!(
        text.contains(
            "## Map\n\n- `greeting_names_the_person` (definition) is realized by `rust:src/lib.rs#greet`, `src/lib.rs`\n"
        ),
        "{text}"
    );

    write(
        dir.path(),
        "src/lib.rs",
        &GREETER.replace("fn greet(", "fn salute("),
    );
    let renamed = stele(dir.path(), &["intent", "report"]);
    let text = stdout(&renamed);
    assert_eq!(renamed.status.code(), Some(1), "{text}{}", stderr(&renamed));
    assert!(text.contains("Nothing awaits approval."), "{text}");
    assert!(
        text.contains("; missing from the code: `rust:src/lib.rs#greet`\n"),
        "{text}"
    );
}

#[test]
fn report_reads_the_code_only_for_a_mapped_catalog_and_takes_a_snapshot_instead() {
    let unmapped = project();
    commit_all(unmapped.path());
    let mut approve = vec!["intent", "approve"];
    approve.extend(STATEMENTS);
    approve.extend(["--by", "owner"]);
    assert!(stele(unmapped.path(), &approve).status.success());
    let never_read = stele(
        unmapped.path(),
        &["intent", "report", "--snapshot", "no-such-snapshot.json"],
    );
    assert_eq!(never_read.status.code(), Some(0), "{}", stderr(&never_read));
    assert!(
        stdout(&never_read)
            .contains("### Unmapped\n\n- `greeting_names_the_person` (definition)\n"),
        "{}",
        stdout(&never_read)
    );

    let dir = mapped_greeter();
    let missing = stele(
        dir.path(),
        &["intent", "report", "--snapshot", "no-such-snapshot.json"],
    );
    assert_eq!(missing.status.code(), Some(2));
    assert!(
        stderr(&missing).starts_with("stele intent: read snapshot no-such-snapshot.json"),
        "{}",
        stderr(&missing)
    );
    // The synthetic snapshot has src/main.rs, not src/lib.rs, which the
    // working tree still holds.
    let snapshot =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../codemap-topology/tests/fixtures/rust.json");
    let elsewhere = stele(
        dir.path(),
        &["intent", "report", "--snapshot", snapshot.to_str().unwrap()],
    );
    assert_eq!(elsewhere.status.code(), Some(1), "{}", stderr(&elsewhere));
    assert!(
        stdout(&elsewhere).contains("; missing from the code: `rust:src/lib.rs#greet`\n"),
        "{}",
        stdout(&elsewhere)
    );
}

#[test]
fn stele_checks_its_own_catalog() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = stele(&repository, &["intent", "check"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}
