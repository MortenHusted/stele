//! Examples: tagged tests in each Cargo package's `tests/*.rs`, the tags
//! that are not examples and why, and what an example's digest covers.

use std::{collections::BTreeMap, path::Path};

use stele_plan::{
    Statement,
    digest::example_digest,
    examples::{Example, ExampleIssue, Package, Scan, packages, scan},
    load_catalog,
};

const CATALOG: &str = r#"catalog fixture {
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
  exception no_web_exit_status {
    narrows: walk_exit_status
    channels: [web]
    kind: does_not_apply
    text: "The web view has no exit status."
  }
  channel cli {
    text: "The stele command line."
  }
  channel library {
    text: "The engine as a Rust library."
  }
  channel web {
    text: "The web view."
  }
}
"#;

const WALK: &str = r"//! Integration tests tagged as examples of the fixture catalog.

/// Walk exits 1 when an expectation is open.
/// intent: example open_exits_one exemplifies walk_exit_status on library, cli
#[test]
fn open_expectation_exits_one() {
    assert_eq!(1, 1);
}

#[test]
fn untagged_test_is_not_an_example() {}

/// intent: example helper_is_not_a_test exemplifies walk_exit_status on cli
fn helper() {}

mod exit_codes {
    mod invalid {
        /// intent: example invalid_exits_two exemplifies walk_exit_status on cli
        #[test]
        fn invalid_artifact_exits_two() {}
    }
}

/// intent: example library_report exemplifies library_returns_report on library
#[test]
fn library_returns_the_report() {}

/// intent: example undeclared exemplifies undeclared_statement on cli
#[test]
fn names_an_undeclared_statement() {}

/// intent: example honest exemplifies results_are_honest on cli
#[test]
fn names_an_intent() {}

/// intent: example no_web exemplifies no_web_exit_status on web
#[test]
fn names_a_does_not_apply_exception() {}

/// intent: example exemplifies walk_exit_status on cli
#[test]
fn has_a_malformed_tag() {}
";

const OTHER: &str = r"/// intent: example cli_open_exits_one exemplifies walk_exit_status on cli
#[test]
fn cli_open_exits_one() {}
";

const IN_A_SUBDIRECTORY: &str = r"/// intent: example in_a_subdirectory exemplifies walk_exit_status on cli
#[test]
fn in_a_subdirectory() {}
";

fn catalog() -> BTreeMap<String, Statement> {
    let files = BTreeMap::from([("intent/catalog.stele".to_string(), CATALOG.to_string())]);
    load_catalog(&files, "intent/catalog.stele").unwrap()
}

fn package(name: &str, dir: &str) -> Package {
    Package {
        name: name.into(),
        dir: dir.into(),
    }
}

/// A scratch repository holding `files`, by their paths from its root.
fn repository(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (path, source) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    dir
}

/// Scan the files as the tests of one package at the repository root.
fn scan_files(files: &[(&str, &str)]) -> Result<Scan, String> {
    let dir = repository(files);
    scan(dir.path(), &[package("fixture", "")], &catalog())
}

/// Scan `source` as the only test file, `tests/example.rs`.
fn scan_source(source: &str) -> Scan {
    scan_files(&[("tests/example.rs", source)]).unwrap()
}

fn digest(source: &str) -> String {
    let scanned = scan_source(source);
    assert_eq!(scanned.examples.len(), 1, "{scanned:?}");
    scanned.examples[0].digest.clone()
}

fn example(
    id: &str,
    statement: &str,
    channels: &[&str],
    file: &str,
    test: &str,
    line: usize,
) -> Example {
    Example {
        id: id.into(),
        statement: statement.into(),
        channels: channels
            .iter()
            .map(|channel| (*channel).to_string())
            .collect(),
        execution: stele_plan::examples::Execution::Cargo {
            package: "fixture".into(),
            target: std::path::Path::new(file)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        },
        file: file.into(),
        test: test.into(),
        line,
        digest: String::new(),
    }
}

fn issue(example: Option<&str>, file: &str, test: &str, line: usize, reason: &str) -> ExampleIssue {
    ExampleIssue {
        example: example.map(Into::into),
        file: file.into(),
        test: test.into(),
        line,
        reason: reason.into(),
    }
}

fn without_digests(mut examples: Vec<Example>) -> Vec<Example> {
    for example in &mut examples {
        assert!(
            example.digest.starts_with("sha256:") && example.digest.len() == 71,
            "{example:?}"
        );
        example.digest.clear();
    }
    examples
}

fn tagged() -> Scan {
    scan_files(&[
        ("tests/walk.rs", WALK),
        ("tests/other.rs", OTHER),
        ("tests/common/mod.rs", IN_A_SUBDIRECTORY),
        ("src/lib.rs", IN_A_SUBDIRECTORY),
    ])
    .unwrap()
}

#[test]
fn tagged_tests_yield_examples_with_sorted_channels_package_file_and_test_path() {
    let scanned = tagged();
    assert_eq!(
        without_digests(scanned.examples.clone()),
        [
            example(
                "cli_open_exits_one",
                "walk_exit_status",
                &["cli"],
                "tests/other.rs",
                "cli_open_exits_one",
                1
            ),
            example(
                "open_exits_one",
                "walk_exit_status",
                &["cli", "library"],
                "tests/walk.rs",
                "open_expectation_exits_one",
                4
            ),
            example(
                "invalid_exits_two",
                "walk_exit_status",
                &["cli"],
                "tests/walk.rs",
                "exit_codes::invalid::invalid_artifact_exits_two",
                18
            ),
            example(
                "library_report",
                "library_returns_report",
                &["library"],
                "tests/walk.rs",
                "library_returns_the_report",
                24
            ),
        ]
    );
    assert_eq!(scanned.examples[1].target(), "walk");
}

#[test]
fn malformed_tags_and_statements_no_example_can_exemplify_are_example_issues() {
    let rule = "an example exemplifies a definition or a holds_instead exception";
    assert_eq!(
        tagged().issues,
        [
            issue(
                Some("undeclared"),
                "tests/walk.rs",
                "names_an_undeclared_statement",
                28,
                "unknown statement undeclared_statement"
            ),
            issue(
                Some("honest"),
                "tests/walk.rs",
                "names_an_intent",
                32,
                &format!("results_are_honest is an intent; {rule}")
            ),
            issue(
                Some("no_web"),
                "tests/walk.rs",
                "names_a_does_not_apply_exception",
                36,
                &format!("no_web_exit_status is a does_not_apply exception; {rule}")
            ),
            issue(
                None,
                "tests/walk.rs",
                "has_a_malformed_tag",
                40,
                "malformed tag \"intent: example exemplifies walk_exit_status on cli\"; expected intent: example <id> exemplifies <statement> on <channel>[, <channel>...]"
            ),
        ]
    );
}

#[test]
fn every_misshapen_tag_line_is_malformed() {
    for tag in [
        "intent: example open exemplifies walk_exit_status",
        "intent: example open exemplifies walk_exit_status on",
        "intent: example open exemplifies walk_exit_status on cli,",
        "intent: example open exemplifies walk_exit_status on cli library",
        "intent: example open exemplifies walk_exit_status on cli, cli",
        "intent: example open-exits exemplifies walk_exit_status on cli",
        "intent: example open exemplify walk_exit_status on cli",
        "intent:example open exemplifies walk_exit_status on cli",
        "intent: open exemplifies walk_exit_status on cli",
    ] {
        let scanned = scan_source(&format!("/// {tag}\n#[test]\nfn tagged() {{}}\n"));
        assert_eq!(scanned.examples, [], "{tag}");
        assert_eq!(scanned.issues.len(), 1, "{tag}");
        assert_eq!(scanned.issues[0].example, None, "{tag}");
        assert!(
            scanned.issues[0].reason.starts_with("malformed tag"),
            "{tag}: {}",
            scanned.issues[0].reason
        );
    }
}

#[test]
fn a_tag_naming_an_undeclared_statement_is_an_example_issue_not_an_error() {
    let scanned = scan_source(
        "/// intent: example ghost exemplifies no_such_statement on cli\n#[test]\nfn ghost() {}\n",
    );
    assert_eq!(scanned.examples, []);
    assert_eq!(
        scanned.issues,
        [issue(
            Some("ghost"),
            "tests/example.rs",
            "ghost",
            1,
            "unknown statement no_such_statement"
        )]
    );
}

#[test]
fn a_tag_naming_a_channel_its_statement_cannot_apply_on_is_an_example_issue() {
    for (tag, reason) in [
        ("walk_exit_status on cli, ghost", "unknown channel ghost"),
        (
            "walk_exit_status on results_are_honest",
            "results_are_honest is an intent, not a channel",
        ),
        (
            "library_returns_report on cli, library",
            "library_returns_report applies only on library, not on cli",
        ),
    ] {
        let scanned = scan_source(&format!(
            "/// intent: example tagged exemplifies {tag}\n#[test]\nfn tagged() {{}}\n"
        ));
        assert_eq!(scanned.examples, [], "{tag}");
        assert_eq!(
            scanned.issues,
            [issue(
                Some("tagged"),
                "tests/example.rs",
                "tagged",
                1,
                reason
            )],
            "{tag}"
        );
    }
}

#[test]
fn two_tests_claiming_one_example_id_fail_naming_both_locations() {
    let first = "/// intent: example open_exits_one exemplifies walk_exit_status on cli\n#[test]\nfn open_exits_one() {}\n";
    let second = "mod library {\n    /// intent: example open_exits_one exemplifies walk_exit_status on library\n    #[test]\n    fn open_exits_one() {}\n}\n";
    assert_eq!(
        scan_files(&[("tests/a.rs", first), ("tests/b.rs", second)]).unwrap_err(),
        "duplicate example id open_exits_one in tests/a.rs:1 and tests/b.rs:2"
    );

    // Across packages too.
    let dir = repository(&[
        ("crates/alpha/tests/a.rs", first),
        ("crates/beta/tests/a.rs", first),
    ]);
    assert_eq!(
        scan(
            dir.path(),
            &[
                package("alpha", "crates/alpha"),
                package("beta", "crates/beta")
            ],
            &catalog()
        )
        .unwrap_err(),
        "duplicate example id open_exits_one in crates/alpha/tests/a.rs:1 and crates/beta/tests/a.rs:1"
    );
}

#[test]
fn a_parse_error_fails_at_its_file_and_line() {
    assert_eq!(
        scan_files(&[("tests/broken.rs", "#[test]\nfn broken() {\n")])
            .unwrap_err()
            .split(": ")
            .next(),
        Some("tests/broken.rs:2")
    );
}

const EXAMPLE: &str = r"/// intent: example open_exits_one exemplifies walk_exit_status on cli
#[test]
fn open_exits_one() {
    let status = 1; // the open case
    assert_eq!(status, 1);
}
";

const REFORMATTED: &str = r"/// intent: example open_exits_one exemplifies walk_exit_status on cli
#[ test ] fn open_exits_one( ) { /* reformatted */ let status=1;
        // another comment
    assert_eq!( status , 1 ) ;
}
";

#[test]
fn an_example_digest_ignores_formatting_and_comments() {
    assert_eq!(digest(REFORMATTED), digest(EXAMPLE));
}

#[test]
fn an_example_digest_changes_with_its_body_tag_or_ignore_attribute() {
    let original = digest(EXAMPLE);
    for (label, changed) in [
        (
            "body",
            EXAMPLE.replace("assert_eq!(status, 1)", "assert_eq!(status, 2)"),
        ),
        ("tag", EXAMPLE.replace("on cli", "on cli, library")),
        ("ignore", EXAMPLE.replace("#[test]", "#[test]\n#[ignore]")),
        ("file attribute", format!("#![cfg(any())]\n{EXAMPLE}")),
        (
            "module attribute",
            format!("#[cfg(any())]\nmod m {{\n{EXAMPLE}}}\n"),
        ),
    ] {
        assert_ne!(digest(&changed), original, "{label}");
    }
}

const RAW_STRING: &str = r##"/// intent: example raw_report exemplifies walk_exit_status on cli
#[test]
fn raw_report() {
    let expected = r#"line one
line two
"#;
    assert!(expected.ends_with("two\n"));
}
"##;

#[test]
fn lf_and_crlf_copies_of_a_multiline_raw_string_get_the_same_digest() {
    let crlf = RAW_STRING.replace('\n', "\r\n");
    let lf = digest(RAW_STRING);
    assert_eq!(digest(&crlf), lf);
    assert_eq!(
        lf,
        example_digest(&[], &syn::parse_str(RAW_STRING).unwrap())
    );
}

#[test]
fn an_example_in_a_plain_module_keeps_its_digest() {
    assert_eq!(digest(&format!("mod m {{\n{EXAMPLE}}}\n")), digest(EXAMPLE));
}

// A decoy compiled in place of a cfg'd-out example would run under the
// example's `--exact` path.
#[test]
fn a_tagged_test_sharing_its_path_with_another_test_is_an_example_issue() {
    let decoy = format!(
        "#[cfg(any())]\nmod m {{\n{EXAMPLE}}}\nmod m {{\n    #[test]\n    fn open_exits_one() {{}}\n}}\n"
    );
    assert_eq!(
        scan_source(&decoy),
        Scan {
            examples: vec![],
            issues: vec![issue(
                Some("open_exits_one"),
                "tests/example.rs",
                "m::open_exits_one",
                3,
                "another test in tests/example.rs has the path m::open_exits_one, so cargo test --exact cannot tell them apart"
            )],
        }
    );
}

#[test]
fn a_missing_tests_directory_has_no_examples() {
    assert_eq!(scan_files(&[("src/lib.rs", "")]), Ok(Scan::default()));
}

const ALPHA_TEST: &str = r"/// intent: example alpha_open exemplifies walk_exit_status on cli
#[test]
fn open() {}
";

const BETA_TEST: &str = r"/// intent: example beta_open exemplifies walk_exit_status on cli
#[test]
fn open() {}
";

/// A virtual workspace of two packages, each with a `tests/checks.rs`.
fn workspace() -> tempfile::TempDir {
    repository(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/alpha\", \"crates/beta\"]\nresolver = \"2\"\n",
        ),
        (
            "crates/alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("crates/alpha/src/lib.rs", ""),
        ("crates/alpha/tests/checks.rs", ALPHA_TEST),
        (
            "crates/beta/Cargo.toml",
            "[package]\nname = \"beta\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("crates/beta/src/lib.rs", ""),
        ("crates/beta/tests/checks.rs", BETA_TEST),
    ])
}

#[test]
fn the_packages_are_the_cargo_workspace_at_the_root() {
    let dir = workspace();
    assert_eq!(
        packages(dir.path()).unwrap(),
        [
            package("alpha", "crates/alpha"),
            package("beta", "crates/beta")
        ]
    );

    let single = repository(&[
        (
            "Cargo.toml",
            "[package]\nname = \"single\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", ""),
    ]);
    assert_eq!(packages(single.path()).unwrap(), [package("single", "")]);

    let none = repository(&[("README.md", "No Cargo here.\n")]);
    assert_eq!(packages(none.path()).unwrap(), []);

    let broken = repository(&[("Cargo.toml", "[package\n")]);
    let error = packages(broken.path()).unwrap_err();
    assert!(error.starts_with("`cargo metadata` failed in "), "{error}");
}

#[test]
fn each_package_s_tests_yield_examples_of_that_package() {
    let dir = workspace();
    let scanned = scan(dir.path(), &packages(dir.path()).unwrap(), &catalog()).unwrap();
    let found: Vec<_> = scanned
        .examples
        .iter()
        .map(|example| {
            (
                example.id.as_str(),
                match &example.execution {
                    stele_plan::examples::Execution::Cargo { package, .. } => package.as_str(),
                    stele_plan::examples::Execution::Minitest { .. } => unreachable!(),
                },
                example.file.as_str(),
                example.target(),
                example.test.as_str(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            (
                "alpha_open",
                "alpha",
                "crates/alpha/tests/checks.rs",
                "checks",
                "open"
            ),
            (
                "beta_open",
                "beta",
                "crates/beta/tests/checks.rs",
                "checks",
                "open"
            ),
        ]
    );
    assert!(Path::new(&dir.path().join(&scanned.examples[0].file)).is_file());
}

#[test]
fn standalone_minitest_examples_are_discovered() {
    let root = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(root.path())
        .status()
        .unwrap();
    std::fs::create_dir(root.path().join("test")).unwrap();
    std::fs::write(root.path().join("test/value_test.rb"), "class ValueTest < Minitest::Test\n  # intent: example ruby_value exemplifies walk_exit_status on cli\n  def test_value\n    assert true\n  end\nend\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(root.path())
        .status()
        .unwrap();
    let found = scan(root.path(), &[], &catalog()).unwrap();
    assert_eq!(found.examples.len(), 1);
    assert_eq!(found.examples[0].test, "ValueTest#test_value");
}

#[test]
fn cargo_example_keeps_its_historical_digest() {
    assert_eq!(
        digest(EXAMPLE),
        "sha256:3b319e2070432b4b06a9cd6e8d9bac233ba9e0ee4f287fec49005737e498ddda"
    );
}
