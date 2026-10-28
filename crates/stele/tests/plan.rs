//! `stele plan` checks a plan against its catalog and the code in one step,
//! the same as writing the evidence and the checker and running
//! `stele spec check`, and lists the changes that are ready and blocked.

#![allow(clippy::doc_markdown)] // Example tags name catalog IDs, not code.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CATALOG: &str = r#"catalog greeter {
  intent greets_people { text: "The greeter greets people by name." }
}
"#;

/// A plan over the topology fixture snapshot's `src/main.rs`.
const PLAN: &str = r#"plan next {
  use "../intent/catalog.stele" as intent

  definition greeting_is_polite {
    refines: intent.greets_people
    text: "A greeting says hello and the name of the person."
  }
  decision keep_entry {
    governs: [greeting_is_polite]
    text: "Greet from the entry function."
  }
  change write_work {
    serves: [intent.greets_people]
    touches: ["rust:src/main.rs#work"]
    state: done
    text: "Write work."
  }
  change polish_entry {
    after: [write_work]
    follows: [keep_entry]
    serves: [greeting_is_polite]
    touches: ["rust:src/main.rs#entry", "src/main.rs"]
    text: "Polish entry."
  }
  change add_farewell {
    after: [polish_entry]
    serves: [intent.greets_people]
    creates: ["rust:src/main.rs#farewell"]
    text: "Add farewell."
  }
}
"#;

fn stele(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run stele")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn snapshot() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../codemap-topology/tests/fixtures/rust.json")
}

/// A directory holding the catalog and `plan`, outside any repository
/// unless the caller makes it one.
fn project(plan: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (path, source) in [
        ("intent/catalog.stele", CATALOG),
        ("plans/next.stele", plan),
    ] {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.email=stele@example.com",
            "-c",
            "user.name=stele",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

/// intent: example plan_lists_ready_and_blocked exemplifies plan_shows_what_is_next on cli
#[test]
fn check_reports_findings_and_the_order_of_the_changes() {
    let dir = project(PLAN);
    let snapshot = snapshot();
    let output = stele(
        dir.path(),
        &[
            "plan",
            "check",
            "plans/next.stele",
            "--snapshot",
            snapshot.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.starts_with("next: clean"), "{text}");
    assert!(text.contains("ready: polish_entry\n"), "{text}");
    assert!(
        text.contains("blocked: add_farewell (after polish_entry)\n"),
        "{text}"
    );

    let broken = project(&PLAN.replace("src/main.rs#entry", "src/main.rs#gone"));
    let output = stele(
        broken.path(),
        &[
            "plan",
            "check",
            "plans/next.stele",
            "--snapshot",
            snapshot.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("[FAIL] touched_code_exists"), "{text}");
    assert!(
        text.contains("C=polish_entry, A=rust:src/main.rs#gone"),
        "{text}"
    );
}

#[test]
fn one_step_check_matches_the_manual_sequence() {
    let dir = project(&PLAN.replace("state: done", "state: planned"));
    let snapshot = snapshot();
    let snapshot = snapshot.to_str().unwrap();
    let evidence = stele(
        dir.path(),
        &[
            "plan",
            "evidence",
            "plans/next.stele",
            "--snapshot",
            snapshot,
            "--out",
            "evidence.stele",
        ],
    );
    assert!(evidence.status.success(), "{}", stderr(&evidence));
    let checker = stele(dir.path(), &["plan", "checker", "--out", "plan.stele"]);
    assert!(checker.status.success(), "{}", stderr(&checker));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("plan.stele")).unwrap(),
        stele_plan::CHECKER
    );
    for json in [false, true] {
        let mut manual = vec!["spec", "check", "plan.stele", "evidence.stele"];
        let mut one_step = vec!["plan", "check", "plans/next.stele", "--snapshot", snapshot];
        if json {
            manual.push("--json");
            one_step.push("--json");
        }
        let manual = stele(dir.path(), &manual);
        let one_step = stele(dir.path(), &one_step);
        assert_eq!(one_step.status.code(), manual.status.code());
        let one_step_text = stdout(&one_step);
        let report = if json {
            one_step_text.as_str()
        } else {
            one_step_text
                .split("ready: ")
                .next()
                .expect("report before the order")
        };
        assert_eq!(report, stdout(&manual), "json={json}");
    }
}

/// intent: example missing_anchor_fails_the_plan exemplifies plan_checked_against_code on cli
#[test]
fn check_reads_the_working_tree_of_the_repository_holding_the_plan() {
    let plan = r#"plan next {
  intent says_goodbye { text: "The greeter says goodbye." }
  change add_farewell {
    serves: [says_goodbye]
    touches: ["rust:src/lib.rs#farewell"]
    text: "Add farewell."
  }
}
"#;
    let dir = project(plan);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"greeter\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn greet() -> &'static str {\n    \"hello\"\n}\n",
    )
    .unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-qm", "start"]);

    // Run from a subdirectory: imports and anchors resolve from the root.
    let plans = dir.path().join("plans");
    let missing = stele(&plans, &["plan", "check", "next.stele"]);
    assert_eq!(missing.status.code(), Some(1), "{}", stderr(&missing));
    assert!(stdout(&missing).contains("A=rust:src/lib.rs#farewell"));

    // Uncommitted code counts: plans are written beside the change.
    let mut lib = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    lib.push_str("\npub fn farewell() -> &'static str {\n    \"bye\"\n}\n");
    std::fs::write(dir.path().join("src/lib.rs"), lib).unwrap();
    let present = stele(&plans, &["plan", "check", "next.stele"]);
    assert_eq!(present.status.code(), Some(0), "{}", stdout(&present));
}

#[test]
fn a_plan_without_anchors_needs_no_code() {
    let plan = r#"plan next {
  use "../intent/catalog.stele" as intent
  change explain {
    serves: [intent.greets_people]
    text: "Explain the greeting."
  }
}
"#;
    let dir = project(plan);
    let output = stele(dir.path(), &["plan", "check", "plans/next.stele"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("ready: explain\n"));
}

/// intent: example plan_error_exits_two exemplifies exit_status_contract on cli
#[test]
fn errors_exit_two_with_the_reason() {
    let dir = project(&PLAN.replace(
        "[greeting_is_polite]\n    text: \"Greet",
        "[nothing]\n    text: \"Greet",
    ));
    let output = stele(dir.path(), &["plan", "check", "plans/next.stele"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr(&output),
        "stele plan: plans/next.stele:9: unknown reference nothing in governs of keep_entry\n"
    );

    let ruby = project(&PLAN.replace("rust:src/main.rs#work", "ruby:app/work.rb#Work"));
    let snapshot = snapshot();
    let output = stele(
        ruby.path(),
        &[
            "plan",
            "check",
            "plans/next.stele",
            "--snapshot",
            snapshot.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output)
            .contains("anchor ruby:app/work.rb#Work is ruby code, but the snapshot is rust"),
        "{}",
        stderr(&output)
    );

    let output = stele(dir.path(), &["plan", "check", "plans/missing.stele"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).starts_with("stele plan: read plans/missing.stele"));
}

#[test]
fn spec_points_a_catalog_or_plan_to_stele_plan() {
    let dir = project(PLAN);
    let output = stele(dir.path(), &["spec", "walk", "intent/catalog.stele"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("a catalog or plan is checked with `stele plan check`"),
        "{}",
        stderr(&output)
    );
}

const GREETER_CATALOG: &str = r#"catalog greeter {
  intent greets_people { text: "The greeter greets people by name." }
  definition greeting_names_the_person {
    refines: greets_people
    text: "A greeting is Hello, the person's name, and an exclamation mark."
  }
  channel cli { text: "The greet command line." }
}
"#;

const GREETER: &str = "pub fn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\n";

const GREETER_TEST: &str = r#"/// intent: example hello_grace exemplifies greeting_names_the_person on cli
#[test]
fn greets_grace() {
    assert_eq!(greeter::greet("Grace"), "Hello, Grace!");
}
"#;

const DONE_PLAN: &str = r#"plan next {
  use "../intent/catalog.stele" as intent
  change greet {
    serves: [intent.greeting_names_the_person]
    state: done
    text: "Greet by name."
  }
}
"#;

/// intent: example done_needs_verified_statements exemplifies done_backed_by_runs on cli
#[test]
fn a_done_change_needs_its_statements_verified_once_the_catalog_has_approvals() {
    let dir = tempfile::tempdir().expect("tempdir");
    let write = |path: &str, source: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    };
    for (path, source) in [
        ("intent/catalog.stele", GREETER_CATALOG),
        ("plans/next.stele", DONE_PLAN),
        (
            "Cargo.toml",
            "[package]\nname = \"greeter\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (".gitignore", "target/\n"),
        ("src/lib.rs", GREETER),
        ("tests/greeting.rs", GREETER_TEST),
    ] {
        write(path, source);
    }
    let lockfile = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(dir.path())
        .output()
        .expect("run cargo");
    assert!(lockfile.status.success(), "{}", stderr(&lockfile));
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-qm", "start"]);
    let check = || stele(dir.path(), &["plan", "check", "plans/next.stele"]);
    let run = |args: &[&str]| {
        let output = stele(dir.path(), args);
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
        stdout(&output)
    };

    // Without approvals the plan is checked as before.
    let unapproved = check();
    assert_eq!(unapproved.status.code(), Some(0), "{}", stdout(&unapproved));

    // Approved but never seen failing and passing, so not verified.
    run(&[
        "intent",
        "approve",
        "greets_people",
        "greeting_names_the_person",
        "cli",
        "hello_grace",
        "--by",
        "owner",
    ]);
    let unverified = check();
    assert_eq!(unverified.status.code(), Some(1), "{}", stdout(&unverified));
    let text = stdout(&unverified);
    assert!(
        text.contains(
            "[FAIL] done_changes_serve_verified_statements: unverified_done(C, S) (expected 0, found 1)"
        ),
        "{text}"
    );
    assert!(text.contains("greeting_names_the_person"), "{text}");

    // Red on a deliberate break, then green: verified, so the plan passes.
    write("src/lib.rs", &GREETER.replace("Hello", "Hi"));
    assert_eq!(
        run(&["intent", "record", "--example", "hello_grace"]),
        "hello_grace on cli: fail\n"
    );
    write("src/lib.rs", GREETER);
    assert_eq!(run(&["intent", "record"]), "hello_grace on cli: pass\n");
    let verified = check();
    assert_eq!(verified.status.code(), Some(0), "{}", stdout(&verified));

    // Standings are taken at HEAD, so editing the next change leaves the
    // committed work verified.
    write("src/lib.rs", &GREETER.replace("Hello", "Hi"));
    let dirty = check();
    assert_eq!(dirty.status.code(), Some(0), "{}", stdout(&dirty));
}

/// Stele's own plan holds together against its catalog and its code. Whether
/// its done changes serve verified statements depends on green runs recorded
/// at HEAD, so it is checked before merging, not here: every commit would
/// otherwise fail this test until the examples are recorded again.
#[test]
fn stele_plans_itself_against_its_own_code() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = stele(
        &repository,
        &["plan", "check", "plans/stele-plans.stele", "--json"],
    );
    assert_ne!(output.status.code(), Some(2), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    let open: Vec<_> = report["expectations"]
        .as_array()
        .expect("expectations")
        .iter()
        .filter(|expectation| expectation["satisfied"] != true)
        .filter(|expectation| expectation["id"] != "done_changes_serve_verified_statements")
        .collect();
    assert!(open.is_empty(), "{open:#?}");
}
