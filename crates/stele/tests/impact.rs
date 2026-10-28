//! `stele intent impact` lists the statements whose `realized_by` code a
//! change touches, with their standing and examples, then the changed code
//! no statement maps and the examples whose test changed.

#![allow(clippy::doc_markdown)] // Example tags name catalog IDs, not code.

use std::path::Path;
use std::process::{Command, Output};

const CATALOG: &str = r#"catalog greeter {
  intent greets_people {
    text: "The greeter greets people by name."
  }
  definition greeting_names_the_person {
    refines: greets_people
    realized_by: ["rust:src/lib.rs#greet"]
    text: "A greeting is Hello, the person's name, and an exclamation mark."
  }
  channel cli {
    text: "The greet command line."
  }
}
"#;

/// `greet` on lines 1-3, mapped; `farewell` on lines 5-7, not.
const GREETER: &str = "pub fn greet(name: &str) -> String {
    format!(\"Hello, {name}!\")
}

pub fn farewell(name: &str) -> String {
    format!(\"Goodbye, {name}!\")
}
";

const GREETER_TEST: &str = r#"/// intent: example hello_grace exemplifies greeting_names_the_person on cli
#[test]
fn greets_grace() {
    assert_eq!(greeter::greet("Grace"), "Hello, Grace!");
}
"#;

fn stele(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run stele")
}

fn write(dir: &Path, path: &str, source: &str) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, source).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.email=stele@example.com",
            "-c",
            "user.name=stele",
            "-c",
            "core.hooksPath=no-hooks",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The impact's Markdown; it always exits 0.
fn impact(dir: &Path, args: &[&str]) -> String {
    let mut all = vec!["intent", "impact"];
    all.extend(args);
    let output = stele(dir, &all);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A committed scratch crate whose catalog maps `greet`.
fn greeter() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (path, source) in [
        ("intent/catalog.stele", CATALOG),
        (
            "Cargo.toml",
            "[package]\nname = \"greeter\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", GREETER),
        ("tests/greeting.rs", GREETER_TEST),
    ] {
        write(dir.path(), path, source);
    }
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "--no-verify", "-m", "start"]);
    dir
}

const NOTHING_UNMAPPED: &str =
    "## Changed code no statement maps\n\nNone.\n\n## Examples whose test changed\n\nNone.\n";

/// intent: example impact_lists_touched_and_unmapped exemplifies impact_lists_touched_statements on cli
#[test]
fn an_uncommitted_change_lists_the_statements_its_code_realizes() {
    let dir = greeter();
    assert_eq!(
        impact(dir.path(), &["HEAD"]),
        [
            "# Intent impact\n\nChanges from `HEAD` to the working tree.\n\n",
            "## Touched statements\n\nNone.\n\n",
            NOTHING_UNMAPPED,
        ]
        .concat()
    );

    write(dir.path(), "src/lib.rs", &GREETER.replace("Hello", "Hi"));
    assert_eq!(
        impact(dir.path(), &["HEAD"]),
        [
            "# Intent impact\n\nChanges from `HEAD` to the working tree.\n\n",
            "## Touched statements\n\n",
            "### `greeting_names_the_person` (definition)\n\n",
            "- Changed: `rust:src/lib.rs#greet` at `src/lib.rs:1-3`\n",
            "- Examples: `hello_grace`\n\n",
            NOTHING_UNMAPPED,
        ]
        .concat()
    );

    write(dir.path(), "src/lib.rs", &GREETER.replace("Goodbye", "Bye"));
    let unmapped = impact(dir.path(), &["HEAD"]);
    assert!(
        unmapped.contains(
            "## Touched statements\n\nNone.\n\n## Changed code no statement maps\n\n- `rust:src/lib.rs#farewell`\n"
        ),
        "{unmapped}"
    );

    write(
        dir.path(),
        "src/lib.rs",
        &GREETER[GREETER.find("\n\n").unwrap() + 2..],
    );
    let removed = impact(dir.path(), &["HEAD"]);
    assert!(
        removed.contains("- Removed: `rust:src/lib.rs#greet` at `src/lib.rs:1-3`\n"),
        "{removed}"
    );
}

/// intent: example impact_lists_changed_tests exemplifies impact_lists_touched_statements on cli
#[test]
fn a_changed_test_lists_its_example_and_json_carries_the_same_impact() {
    let dir = greeter();
    write(
        dir.path(),
        "tests/greeting.rs",
        &GREETER_TEST.replace("Grace\"),", "Grace\"), \"greets by name\","),
    );
    let text = impact(dir.path(), &["HEAD"]);
    assert!(
        text.ends_with(
            "## Touched statements\n\nNone.\n\n## Changed code no statement maps\n\nNone.\n\n## Examples whose test changed\n\n- `hello_grace` of `greeting_names_the_person` in `tests/greeting.rs`\n"
        ),
        "{text}"
    );

    write(dir.path(), "src/lib.rs", &GREETER.replace("Hello", "Hi"));
    let json: serde_json::Value =
        serde_json::from_str(&impact(dir.path(), &["HEAD", "--json"])).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "statements": [{
                "id": "greeting_names_the_person",
                "kind": "definition",
                "standing": null,
                "anchors": [{
                    "anchor": "rust:src/lib.rs#greet",
                    "change": "changed",
                    "location": "src/lib.rs:1-3",
                }],
                "examples": [{"id": "hello_grace", "test_changed": true}],
            }],
            "unmapped": [],
            "examples": [{
                "id": "hello_grace",
                "statement": "greeting_names_the_person",
                "file": "tests/greeting.rs",
            }],
        })
    );
}

/// intent: example impact_shows_standings exemplifies impact_lists_touched_statements on cli
#[test]
fn two_commits_compare_without_the_working_tree_and_standings_show_once_approved() {
    let dir = greeter();
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
    assert!(
        approve.status.success(),
        "{}",
        String::from_utf8_lossy(&approve.stderr)
    );
    write(dir.path(), "src/lib.rs", &GREETER.replace("Hello", "Hi"));
    git(
        dir.path(),
        &["commit", "-qam", "greet with Hi", "--no-verify"],
    );
    // Uncommitted, and not part of a comparison of two commits.
    write(dir.path(), "src/lib.rs", &GREETER.replace("Goodbye", "Bye"));

    let text = impact(dir.path(), &["HEAD~1", "HEAD"]);
    assert!(
        text.starts_with(
            "# Intent impact\n\nChanges from `HEAD~1` to `HEAD`.\n\n## Touched statements\n\n### `greeting_names_the_person` (definition): unverified\n\n- Changed: `rust:src/lib.rs#greet` at `src/lib.rs:1-3`\n"
        ),
        "{text}"
    );
    assert!(text.contains(NOTHING_UNMAPPED), "{text}");
}
