//! Plans and catalogs anchor code in every language their symbol anchors
//! name, and any file of the working tree: `stele plan check`, `stele intent
//! report` and `stele intent impact` extract one snapshot per language and
//! find documentation and manifests in the working tree.

use std::path::Path;
use std::process::{Command, Output};

const CATALOG: &str = r#"catalog greeter {
  intent greets_people { text: "The greeter greets people by name." }
}
"#;

/// A plan that anchors Rust, Go and the README.
const PLAN: &str = r#"plan next {
  use "../intent/catalog.stele" as intent
  change greet_everywhere {
    serves: [intent.greets_people]
    touches: ["rust:src/lib.rs#greet", "go:greet/greet.go#greet::Hello", "README.md"]
    text: "Greet from the Rust crate, the Go module and the README."
  }
}
"#;

const LIB: &str = "pub fn greet() -> &'static str {\n    \"hello\"\n}\n";

const GREET_GO: &str = "package greet\n\nfunc Hello() string {\n\treturn \"hello\"\n}\n";

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

fn write(dir: &Path, path: &str, text: &str) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
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

/// A committed repository holding a Rust crate, a Go module, a README and
/// `catalog` with `plan`.
fn repository(catalog: &str, plan: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (path, text) in [
        (
            "Cargo.toml",
            "[package]\nname = \"greeter\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", LIB),
        ("go.mod", "module example.com/greeter\n\ngo 1.22\n"),
        ("greet/greet.go", GREET_GO),
        ("README.md", "# Greeter\n"),
        ("intent/catalog.stele", catalog),
        ("plans/next.stele", plan),
    ] {
        write(dir.path(), path, text);
    }
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-qm", "start"]);
    dir
}

#[test]
fn a_plan_checks_rust_go_and_readme_anchors_and_names_each_one_missing() {
    let dir = repository(CATALOG, PLAN);
    let check = || stele(dir.path(), &["plan", "check", "plans/next.stele"]);
    let clean = check();
    assert_eq!(
        clean.status.code(),
        Some(0),
        "{}{}",
        stdout(&clean),
        stderr(&clean)
    );
    assert!(
        stdout(&clean).starts_with("next: clean"),
        "{}",
        stdout(&clean)
    );

    for (path, from, to, anchor) in [
        (
            "src/lib.rs",
            "fn greet(",
            "fn salute(",
            "rust:src/lib.rs#greet",
        ),
        (
            "greet/greet.go",
            "func Hello(",
            "func Hi(",
            "go:greet/greet.go#greet::Hello",
        ),
    ] {
        let original = std::fs::read_to_string(dir.path().join(path)).unwrap();
        write(dir.path(), path, &original.replace(from, to));
        let renamed = check();
        assert_eq!(renamed.status.code(), Some(1), "{}", stderr(&renamed));
        assert!(
            stdout(&renamed).contains(&format!("C=greet_everywhere, A={anchor}")),
            "{}",
            stdout(&renamed)
        );
        write(dir.path(), path, &original);
    }

    std::fs::rename(dir.path().join("README.md"), dir.path().join("README.txt")).unwrap();
    let renamed = check();
    assert_eq!(renamed.status.code(), Some(1), "{}", stderr(&renamed));
    assert!(
        stdout(&renamed).contains("C=greet_everywhere, A=README.md"),
        "{}",
        stdout(&renamed)
    );
    std::fs::rename(dir.path().join("README.txt"), dir.path().join("README.md")).unwrap();
    assert_eq!(check().status.code(), Some(0));
}

#[test]
fn an_untracked_file_counts_and_an_ignored_one_does_not() {
    let plan = PLAN.replace("\"README.md\"]", "\"README.md\", \"docs/next.md\"]");
    let dir = repository(CATALOG, &plan);
    let check = || stele(dir.path(), &["plan", "check", "plans/next.stele"]);
    let missing = check();
    assert_eq!(missing.status.code(), Some(1), "{}", stderr(&missing));
    assert!(
        stdout(&missing).contains("A=docs/next.md"),
        "{}",
        stdout(&missing)
    );

    write(dir.path(), "docs/next.md", "# Next\n");
    let present = check();
    assert_eq!(present.status.code(), Some(0), "{}", stdout(&present));

    write(dir.path(), ".gitignore", "docs/\n");
    let ignored = check();
    assert_eq!(ignored.status.code(), Some(1), "{}", stderr(&ignored));
    assert!(
        stdout(&ignored).contains("A=docs/next.md"),
        "{}",
        stdout(&ignored)
    );
}

#[test]
fn language_overrides_the_derived_set_and_a_profile_language_is_refused() {
    let dir = repository(CATALOG, PLAN);
    let rust_only = stele(
        dir.path(),
        &["plan", "check", "plans/next.stele", "--language", "rust"],
    );
    assert_eq!(rust_only.status.code(), Some(2));
    assert!(
        stderr(&rust_only)
            .contains("anchor go:greet/greet.go#greet::Hello is go code, but the snapshot is rust"),
        "{}",
        stderr(&rust_only)
    );
    let both = stele(
        dir.path(),
        &[
            "plan",
            "check",
            "plans/next.stele",
            "--language",
            "go",
            "--language",
            "rust",
        ],
    );
    assert_eq!(both.status.code(), Some(0), "{}", stderr(&both));

    let cpp = repository(
        CATALOG,
        &PLAN.replace(
            "\"README.md\"]",
            "\"README.md\", \"cpp:src/main.cpp#main()\"]",
        ),
    );
    let refused = stele(cpp.path(), &["plan", "check", "plans/next.stele"]);
    assert_eq!(refused.status.code(), Some(2));
    let text = stderr(&refused);
    assert!(text.contains("cpp code"), "{text}");
    assert!(text.contains("stele code extract --help"), "{text}");
}

/// A catalog whose definition is realized by the README and a function in
/// each language.
const MAPPED_CATALOG: &str = r#"catalog greeter {
  intent greets_people { text: "The greeter greets people by name." }
  definition greeting_says_hello {
    refines: greets_people
    realized_by: ["README.md", "go:greet/greet.go#greet::Hello", "rust:src/lib.rs#greet"]
    text: "A greeting says hello."
  }
}
"#;

#[test]
fn intent_report_finds_a_readme_that_realizes_a_statement() {
    let dir = repository(MAPPED_CATALOG, PLAN);
    let approve = stele(
        dir.path(),
        &[
            "intent",
            "approve",
            "greets_people",
            "greeting_says_hello",
            "--by",
            "owner",
        ],
    );
    assert!(approve.status.success(), "{}", stderr(&approve));
    let report = || stele(dir.path(), &["intent", "report"]);
    let mapped = report();
    let text = stdout(&mapped);
    assert_eq!(mapped.status.code(), Some(0), "{text}{}", stderr(&mapped));
    assert!(
        text.contains(
            "- `greeting_says_hello` (definition) is realized by `README.md`, `go:greet/greet.go#greet::Hello`, `rust:src/lib.rs#greet`\n"
        ),
        "{text}"
    );

    std::fs::remove_file(dir.path().join("README.md")).unwrap();
    let missing = report();
    let text = stdout(&missing);
    assert_eq!(missing.status.code(), Some(1), "{text}{}", stderr(&missing));
    assert!(
        text.contains("; missing from the code: `README.md`\n"),
        "{text}"
    );
}

#[test]
fn intent_impact_reads_every_language_the_map_names() {
    let dir = repository(MAPPED_CATALOG, PLAN);
    write(
        dir.path(),
        "greet/greet.go",
        &GREET_GO.replace("\"hello\"", "\"hi\""),
    );
    let impact = stele(dir.path(), &["intent", "impact", "HEAD"]);
    let text = stdout(&impact);
    assert_eq!(impact.status.code(), Some(0), "{text}{}", stderr(&impact));
    assert!(
        text.contains(
            "### `greeting_says_hello` (definition)\n\n- Changed: `go:greet/greet.go#greet::Hello` at `greet/greet.go:3-5`\n"
        ),
        "{text}"
    );
}
