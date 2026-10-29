//! Original standalone fixtures. No Rails boot or application dependencies.
use std::{collections::BTreeMap, fs, path::Path, process::Command, time::Duration};
use stele_plan::{
    DirReader, Statement,
    examples::{self, Example, Execution},
    identity::Repo,
    load_catalog,
    minitest::Minitest,
    recorder::{self, CargoTest, Runner, Selection},
    runs::{self, Outcome},
};

const CATALOG: &str = r#"catalog fixture {
 intent truth { text: "Results reflect the library." }
 definition value { refines: truth text: "The library returns seven." }
 channel cli { text: "Command line." }
 channel library { text: "Library." }
 exception alternate { narrows: value kind: holds_instead channels: [cli] text: "Alternate." }
}"#;
const TEST: &str = "require 'minitest/autorun'\nrequire_relative '../lib/value'\nmodule Demo\n  class ValueTest < Minitest::Test\n    # intent: example ruby_value exemplifies value on library, cli\n    def test_value\n      assert_equal 7, value\n    end\n    def test_value_suffix\n      raise 'suffix selected'\n    end\n    def test_value_prefix\n      raise 'prefix selected'\n    end\n  end\n  class OtherTest < Minitest::Test\n    def test_value\n      raise 'other class selected'\n    end\n  end\nend\n";

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new(source: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        for name in ["intent", "test", "lib"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        fs::write(dir.path().join("intent/catalog.stele"), CATALOG).unwrap();
        fs::write(dir.path().join("lib/value.rb"), "def value; 7; end\n").unwrap();
        fs::write(dir.path().join("test/value_test.rb"), source).unwrap();
        git(dir.path(), &["add", "."]);
        git(
            dir.path(),
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "standalone fixture",
            ],
        );
        Self { dir }
    }
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn catalog(&self) -> BTreeMap<String, Statement> {
        load_catalog(&DirReader(self.root().into()), "intent/catalog.stele").unwrap()
    }
    fn scan(&self) -> Result<examples::Scan, String> {
        examples::scan(self.root(), &[], &self.catalog())
    }
    fn example(&self) -> Example {
        let scan = self.scan().unwrap();
        assert!(scan.issues.is_empty(), "{scan:?}");
        scan.examples[0].clone()
    }
    fn runner(&self) -> Minitest {
        Minitest::new(self.root().into())
    }
    fn write_test(&self, source: &str) {
        fs::write(self.root().join("test/value_test.rb"), source).unwrap();
    }
}

#[test]
fn static_discovery_is_located_sorted_and_does_not_execute_test_source() {
    let fixture = Fixture::new(&TEST.replace(
        "require 'minitest/autorun'",
        "raise 'discovery executed Ruby'",
    ));
    let example = fixture.example();
    assert_eq!(example.channels, ["cli", "library"]);
    assert_eq!(example.file, "test/value_test.rb");
    assert_eq!(example.line, 5);
    assert_eq!(example.test, "Demo::ValueTest#test_value");
    assert_eq!(
        example.execution,
        Execution::Minitest {
            class: "Demo::ValueTest".into(),
            method: "test_value".into(),
            definition_line: 6
        }
    );
    assert_eq!(fixture.scan().unwrap(), fixture.scan().unwrap());
    assert!(fs::read_to_string(fixture.root().join("intent/approvals.stele")).is_err());
}

#[test]
fn ruby_digest_covers_complete_file_context_and_normalizes_crlf() {
    let fixture = Fixture::new(TEST);
    let original = fixture.example().digest;
    fixture.write_test(&TEST.replace('\n', "\r\n"));
    assert_eq!(original, fixture.example().digest);
    for changed in [
        TEST.replace("assert_equal 7", "assert_equal 8"),
        TEST.replace("Demo", "Other"),
        TEST.replace("library, cli", "cli, library"),
        TEST.replace(
            "    def test_value_suffix",
            "    def setup; @value = 1; end\n    def test_value_suffix",
        ),
        format!("{TEST}\ndef helper; 1; end\n"),
    ] {
        fixture.write_test(&changed);
        assert_ne!(original, fixture.example().digest);
    }
}

#[test]
fn unsupported_forms_are_located_issues_and_parse_errors_fail() {
    for changed in [
        TEST.replace("< Minitest::Test", "< BaseTest"),
        TEST.replace("def test_value\n", "def self.test_value\n"),
        TEST.replace(
            "    def test_value\n      assert_equal 7, value\n    end",
            "    test('value') { assert_equal 7, value }",
        ),
        TEST.replace(
            "    def test_value\n      assert_equal 7, value\n    end",
            "    if true\n      def test_value; end\n    end",
        ),
        format!("{TEST}\nmodule Demo\n class ValueTest; end\nend\n"),
        TEST.replace(
            "    def test_value_suffix",
            "    def test_value; end\n    def test_value_suffix",
        ),
        TEST.replace(
            "    def test_value_suffix",
            "    define_method(:extra) {}\n    def test_value_suffix",
        ),
    ] {
        let fixture = Fixture::new(&changed);
        let scan = fixture.scan().unwrap();
        assert!(scan.examples.is_empty(), "{changed}: {scan:?}");
        assert_eq!(scan.issues.len(), 1);
        assert_eq!(scan.issues[0].line, 5);
    }
    let fixture = Fixture::new("class Bad <\n");
    assert!(fixture.scan().unwrap_err().contains("invalid Ruby"));
}

#[test]
fn ruby_tags_share_catalog_rules_and_duplicate_namespace_with_cargo() {
    for (from, to, reason) in [
        (
            "exemplifies value",
            "exemplifies unknown",
            "unknown statement",
        ),
        ("on library, cli", "on missing", "unknown channel"),
        ("exemplifies value", "exemplifies truth", "an intent"),
        ("exemplifies value", "exemplifies alternate", "applies only"),
        ("example ruby_value", "example", "malformed"),
    ] {
        let fixture = Fixture::new(&TEST.replace(from, to));
        let scan = fixture.scan().unwrap();
        assert_eq!(scan.examples, []);
        assert!(scan.issues[0].reason.contains(reason), "{scan:?}");
    }
    let fixture = Fixture::new(TEST);
    fs::create_dir(fixture.root().join("tests")).unwrap();
    fs::write(
        fixture.root().join("tests/rust.rs"),
        "/// intent: example ruby_value exemplifies value on cli\n#[test]\nfn value() {}\n",
    )
    .unwrap();
    let result = examples::scan(
        fixture.root(),
        &[examples::Package {
            name: "fixture".into(),
            dir: String::new(),
        }],
        &fixture.catalog(),
    );
    assert!(result.unwrap_err().contains("duplicate example id"));
    let fixture = Fixture::new(&format!(
        "{TEST}\n# intent: example ruby_value exemplifies value on cli\ndef test_other; end\n"
    ));
    assert!(fixture.scan().unwrap_err().contains("duplicate example id"));
}

#[test]
fn strings_do_not_create_tags_and_untracked_or_symlink_evidence_is_refused() {
    let fixture = Fixture::new("TEXT = '# intent: example fake exemplifies value on cli'\n");
    assert_eq!(fixture.scan().unwrap().examples, []);
    assert_eq!(fixture.scan().unwrap().issues, []);
    fs::write(fixture.root().join("test/untracked_test.rb"), TEST).unwrap();
    assert!(fixture.scan().unwrap_err().contains("must be tracked"));
    fs::remove_file(fixture.root().join("test/untracked_test.rb")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            fixture.root().join("lib/value.rb"),
            fixture.root().join("test/escape_test.rb"),
        )
        .unwrap();
        git(fixture.root(), &["add", "test/escape_test.rb"]);
        assert!(fixture.scan().unwrap_err().contains("symlinks"));
    }
}

#[test]
#[cfg(unix)]
fn exact_command_and_provider_mismatch_rails_and_stale_discovery_are_checked_before_spawn() {
    let fixture = Fixture::new(TEST);
    let example = fixture.example();
    let runner = fixture.runner();
    let command = runner
        .command(
            &example,
            "cli",
            Path::new("/private/reporter.rb"),
            Path::new("/private/receipt.json"),
        )
        .unwrap();
    assert_eq!(command.get_program(), "ruby");
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "-Ilib",
            "-Itest",
            "-r",
            "/private/reporter.rb",
            "test/value_test.rb",
            "--name",
            "Demo::ValueTest#test_value"
        ]
    );
    assert_eq!(command.get_current_dir(), Some(fixture.root()));
    assert!(
        command
            .get_envs()
            .any(|(k, v)| k == "STELE_CHANNEL" && v.unwrap() == "cli")
    );
    let mut cargo = CargoTest {
        dir: fixture.root().into(),
    };
    assert!(
        cargo
            .run(&example, "cli")
            .unwrap_err()
            .contains("requires --runner minitest")
    );
    fixture.write_test(&TEST.replace("assert_equal 7", "assert_equal 8"));
    assert!(
        runner
            .validate(&example)
            .unwrap_err()
            .contains("changed since discovery")
    );
    fs::create_dir(fixture.root().join("config")).unwrap();
    fs::write(
        fixture.root().join("config/application.rb"),
        "raise 'must never boot'\n",
    )
    .unwrap();
    assert!(
        fixture
            .runner()
            .run(&example, "cli")
            .unwrap_err()
            .contains("refuses Rails roots")
    );
}

#[test]
#[ignore = "requires Ruby and locally installed reviewed Minitest 5.27.0 or 6.0.6"]
fn native_runtime_outcomes_and_source_location_guards() {
    let cases = [
        ("assert_equal 7, value", Outcome::Pass),
        ("assert_equal 8, value", Outcome::Fail),
        ("raise 'runtime error'", Outcome::Error),
        ("skip 'not available'", Outcome::Skip),
        (
            "puts 'test result: ok. 1 passed; 0 failed; 0 ignored'; exit! 0",
            Outcome::Error,
        ),
        ("exit! 1", Outcome::Error),
        ("Process.kill('TERM', Process.pid)", Outcome::Error),
    ];
    for (body, expected) in cases {
        let fixture = Fixture::new(&TEST.replace("assert_equal 7, value", body));
        let result = fixture.runner().run(&fixture.example(), "cli").unwrap();
        assert_eq!(result.outcome, expected, "{body}: {result:?}");
    }
    for (addition, expected) in [
        ("def setup; flunk 'setup'; end", Outcome::Fail),
        ("def setup; raise 'setup'; end", Outcome::Error),
        ("def teardown; flunk 'teardown'; end", Outcome::Fail),
        ("def teardown; raise 'teardown'; end", Outcome::Error),
    ] {
        let source = TEST.replace(
            "    def test_value_suffix",
            &format!("    {addition}\n    def test_value_suffix"),
        );
        let fixture = Fixture::new(&source);
        assert_eq!(
            fixture
                .runner()
                .run(&fixture.example(), "cli")
                .unwrap()
                .outcome,
            expected,
            "{addition}"
        );
    }
    let fixture = Fixture::new(
        &TEST
            .replace("assert_equal 7, value", "flunk 'assertion'")
            .replace(
                "    def test_value_suffix",
                "    def teardown; raise 'teardown'; end\n    def test_value_suffix",
            ),
    );
    assert_eq!(
        fixture
            .runner()
            .run(&fixture.example(), "cli")
            .unwrap()
            .outcome,
        Outcome::Error
    );
    for source in [
        TEST.replace("require 'minitest/autorun'", "require 'minitest'"),
        format!("raise 'boot failure'\n{TEST}"),
        format!("exit! 0\n{TEST}"),
        format!("exit! 1\n{TEST}"),
        format!(
            "{TEST}\nMinitest.after_run {{ File.write(ENV.fetch('STELE_MINITEST_RECEIPT'), '{{}}{{}}') }}\n"
        ),
        format!(
            "{TEST}\nMinitest.after_run {{ path = ENV.fetch('STELE_MINITEST_RECEIPT'); receipt = JSON.parse(File.read(path)); receipt['results'] *= 2; File.write(path, JSON.generate(receipt)) }}\n"
        ),
        format!("{TEST}\nMinitest.after_run {{ exit! 1 }}\n"),
        format!(
            "{}\nMinitest.after_run {{ exit! 0 }}\n",
            TEST.replace("assert_equal 7, value", "flunk 'failure'")
        ),
        format!(
            "{TEST}\nDemo::ValueTest.class_eval {{ define_method(:test_value) {{ raise 'redefined body executed' }} }}\n"
        ),
        format!("{TEST}\nDemo::ValueTest.class_eval {{ remove_method(:test_value) }}\n"),
    ] {
        let fixture = Fixture::new(&source);
        assert_eq!(
            fixture
                .runner()
                .run(&fixture.example(), "cli")
                .unwrap()
                .outcome,
            Outcome::Error
        );
    }
    let fixture = Fixture::new(&TEST.replace("assert_equal 7, value", "sleep 10"));
    let mut runner = fixture.runner();
    runner.timeout = Duration::from_millis(50);
    assert_eq!(
        runner.run(&fixture.example(), "cli").unwrap().outcome,
        Outcome::Error
    );
}

#[test]
#[ignore = "requires Ruby and locally installed reviewed Minitest 5.27.0 or 6.0.6"]
#[allow(clippy::too_many_lines)]
fn production_mutation_records_real_red_and_restore_green_without_test_or_digest_change() {
    let fixture = Fixture::new(TEST);
    let repo = Repo::of_catalog(&fixture.root().join("intent")).unwrap();
    let example = fixture.example();
    let ledger = fixture.root().join("intent/runs.stele");
    let selection = Selection {
        example: Some(example.id.clone()),
        channel: Some("cli".into()),
    };
    let baseline = repo.working_tree_identity().unwrap();
    let record = |runner: &mut Minitest| {
        recorder::record_catalog(
            fixture.root(),
            "intent/catalog.stele",
            &selection,
            runner,
            &repo,
            &ledger,
        )
        .unwrap()
    };
    let green = record(&mut fixture.runner());
    assert_eq!(green[0].outcome, Outcome::Pass);
    let prefix = fs::read(&ledger).unwrap();
    fs::write(fixture.root().join("lib/value.rb"), "def value; 8; end\n").unwrap();
    let red = record(&mut fixture.runner());
    eprintln!("production-change red receipt: {:?}", red[0]);
    assert_eq!(red[0].outcome, Outcome::Fail);
    assert_ne!(red[0].source, baseline);
    assert_eq!(red[0].digest, example.digest);
    assert_eq!(fixture.example().digest, example.digest);
    assert_eq!(red[0].base, repo.head().unwrap());
    assert!(red[0].diff.as_ref().unwrap().contains("lib/value.rb"));
    assert!(red[0].diff.as_ref().unwrap().contains("+def value; 8; end"));
    git(fixture.root(), &["restore", "lib/value.rb"]);
    let restored = record(&mut fixture.runner());
    assert_eq!(restored[0].outcome, Outcome::Pass);
    assert_eq!(restored[0].source, baseline);
    assert!(fs::read(&ledger).unwrap().starts_with(&prefix));
    let runs = runs::parse(&fs::read_to_string(&ledger).unwrap(), "runs.stele").unwrap();
    assert_eq!(runs.len(), 3);
    assert!(
        runs.iter()
            .all(|run| run.runner.as_ref().unwrap().starts_with("minitest/1 ruby="))
    );
    git(fixture.root(), &["add", "intent/runs.stele"]);
    git(
        fixture.root(),
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "ledger only",
        ],
    );
    assert_eq!(repo.working_tree_identity().unwrap(), baseline);
    let source_baseline_identity = recorder::SourceBaseline::new(&repo, &baseline).unwrap();
    fs::write(fixture.root().join("lib/value.rb"), "def value; 9; end\n").unwrap();
    let tagged_red = recorder::record_catalog(
        fixture.root(),
        "intent/catalog.stele",
        &selection,
        &mut fixture.runner(),
        &source_baseline_identity,
        &ledger,
    )
    .unwrap();
    assert_eq!(tagged_red[0].outcome, Outcome::Fail);
    assert_eq!(tagged_red[0].base, format!("source:{baseline}"));
    assert!(
        tagged_red[0]
            .diff
            .as_ref()
            .unwrap()
            .contains("+def value; 9; end")
    );
    git(fixture.root(), &["restore", "lib/value.rb"]);
    let tagged_green = recorder::record_catalog(
        fixture.root(),
        "intent/catalog.stele",
        &selection,
        &mut fixture.runner(),
        &source_baseline_identity,
        &ledger,
    )
    .unwrap();
    assert_eq!(tagged_green[0].outcome, Outcome::Pass);
    assert_eq!(tagged_green[0].source, baseline);
    git(fixture.root(), &["reset", "--soft", "HEAD~1"]);
    git(fixture.root(), &["add", "intent/runs.stele"]);
    git(
        fixture.root(),
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "squashed fixture ledger",
        ],
    );
    assert_eq!(repo.working_tree_identity().unwrap(), baseline);
    assert!(recorder::SourceBaseline::new(&repo, &baseline).is_ok());
}

#[test]
#[ignore = "requires Ruby and locally installed reviewed Minitest 5.27.0 or 6.0.6"]
fn runtime_source_mutation_and_late_selected_test_mutation_leave_ledger_untouched() {
    let fixture = Fixture::new(&TEST.replace(
        "assert_equal 7, value",
        "File.write('lib/value.rb', 'def value; 8; end'); assert true",
    ));
    let repo = Repo::of_catalog(&fixture.root().join("intent")).unwrap();
    let ledger = fixture.root().join("intent/runs.stele");
    assert!(
        recorder::record_catalog(
            fixture.root(),
            "intent/catalog.stele",
            &Selection::default(),
            &mut fixture.runner(),
            &repo,
            &ledger
        )
        .unwrap_err()
        .contains("source changed")
    );
    assert!(!ledger.exists());
    let examples = vec![fixture.example()];
    fixture.write_test(&TEST.replace("assert_equal 7", "assert_equal 8"));
    assert!(
        recorder::record(
            &examples,
            &Selection::default(),
            &mut fixture.runner(),
            &repo,
            &ledger
        )
        .unwrap_err()
        .contains("changed since discovery")
    );
    assert!(!ledger.exists());
}

fn approved_report_fixture() -> Fixture {
    let fixture = Fixture::new(&TEST.replace("on library, cli", "on cli"));
    fs::write(
        fixture.root().join("intent/catalog.stele"),
        r#"catalog fixture {
 intent truth { text: "Results reflect the library." }
 definition value { refines: truth text: "The library returns seven." }
 channel cli { text: "Command line." }
}"#,
    )
    .unwrap();
    stele_plan::approvals::append(
        fixture.root(),
        "intent/catalog.stele",
        |_, statements, examples| {
            stele_plan::approvals::approve(
                statements,
                examples,
                &["truth", "value", "cli", "ruby_value"],
                "synthetic-fixture-owner",
            )
        },
    )
    .unwrap();
    fixture
}

fn report_runs(fixture: &Fixture) -> [runs::Run; 2] {
    let example = fixture.example();
    let repo = Repo::of_catalog(&fixture.root().join("intent")).unwrap();
    let run = runs::Run {
        example: example.id,
        digest: example.digest,
        digest_version: 1,
        channel: "cli".into(),
        source: repo.working_tree_identity().unwrap(),
        base: repo.head().unwrap(),
        outcome: Outcome::Pass,
        diff: None,
        runner: Some("minitest/1 ruby=ruby/4.0.9/test-platform gem=6.0.6 minitest=6.0.6".into()),
    };
    let mut red = run.clone();
    red.source = "sha256:break".into();
    red.outcome = Outcome::Fail;
    red.diff = Some("synthetic production diff".into());
    [red, run]
}

fn report_fixture(fixture: &Fixture) -> stele_plan::report::Report {
    stele_plan::report::report(
        &stele_plan::report::Options {
            root: fixture.root().into(),
            catalog: "intent/catalog.stele".into(),
            base: None,
        },
        None,
    )
    .unwrap()
}

#[test]
fn report_rejects_only_the_runner_digest_or_source_varied_against_a_verified_control() {
    for (field, changed_run) in [
        ("missing runner", 0),
        ("missing runner", 1),
        ("wrong runner", 0),
        ("wrong runner", 1),
        ("wrong digest", 0),
        ("wrong digest", 1),
        ("wrong passing source", 1),
    ] {
        let fixture = approved_report_fixture();
        let mut evidence = report_runs(&fixture);
        let source = evidence[1].source.clone();
        let ledger = fixture.root().join("intent/runs.stele");
        runs::append(&ledger, &evidence).unwrap();
        let control = report_fixture(&fixture);
        assert_eq!(
            control.standings["value"], "verified",
            "{field}/{changed_run} control"
        );
        assert!(
            control.markdown.lines().any(|line| line
                == format!("  - `ruby_value` on `cli`: red fail, green pass at `{source}`"))
        );
        match field {
            "missing runner" => evidence[changed_run].runner = None,
            "wrong runner" => {
                evidence[changed_run].runner = Some(
                    "minitest/2 ruby=ruby/4.0.9/test-platform gem=6.0.6 minitest=6.0.6".into(),
                );
            }
            "wrong digest" => evidence[changed_run].digest = "sha256:old-digest".into(),
            "wrong passing source" => evidence[changed_run].source = "sha256:old-source".into(),
            _ => unreachable!(),
        }
        // Each case owns a fresh fixture. Replace its control ledger with
        // evidence differing in exactly the field under test.
        fs::remove_file(&ledger).unwrap();
        runs::append(&ledger, &evidence).unwrap();
        let rejected = report_fixture(&fixture);
        assert_eq!(
            rejected.standings["value"], "unverified",
            "{field}/{changed_run}"
        );
        let row = match (field, changed_run) {
            ("wrong passing source", _) => "red fail, green pass at `sha256:old-source`".into(),
            (_, 0) => format!("no red run, green pass at `{source}`"),
            (_, 1) => "red fail, no green run".into(),
            _ => unreachable!(),
        };
        assert!(
            rejected
                .markdown
                .lines()
                .any(|line| line == format!("  - `ruby_value` on `cli`: {row}")),
            "{field}/{changed_run}: {}",
            rejected.markdown
        );
    }
}

#[test]
fn report_errors_and_skips_cannot_supply_red_or_green_and_errors_spoil_current_green() {
    for (outcome, changed_run) in [
        (Outcome::Error, 0),
        (Outcome::Skip, 0),
        (Outcome::Error, 1),
        (Outcome::Skip, 1),
    ] {
        let fixture = approved_report_fixture();
        let mut evidence = report_runs(&fixture);
        let source = evidence[1].source.clone();
        let ledger = fixture.root().join("intent/runs.stele");
        runs::append(&ledger, &evidence).unwrap();
        assert_eq!(report_fixture(&fixture).standings["value"], "verified");
        evidence[changed_run].outcome = outcome;
        evidence[changed_run].diff = None;
        fs::remove_file(&ledger).unwrap();
        runs::append(&ledger, &evidence).unwrap();
        let rejected = report_fixture(&fixture);
        assert_eq!(
            rejected.standings["value"], "unverified",
            "{outcome:?}/{changed_run}"
        );
        let row = if changed_run == 0 {
            format!("no red run, green pass at `{source}`")
        } else {
            "red fail, no green run".into()
        };
        assert!(
            rejected
                .markdown
                .lines()
                .any(|line| line == format!("  - `ruby_value` on `cli`: {row}"))
        );
    }
    for stamp in [
        Some("minitest/1 unavailable"),
        Some("minitest/1 ruby=ruby/4.0.9/test-platform gem=6.0.6 minitest=6.0.6"),
    ] {
        let fixture = approved_report_fixture();
        let evidence = report_runs(&fixture);
        let ledger = fixture.root().join("intent/runs.stele");
        runs::append(&ledger, &evidence).unwrap();
        assert_eq!(report_fixture(&fixture).standings["value"], "verified");
        let mut error = evidence[1].clone();
        error.outcome = Outcome::Error;
        error.runner = stamp.map(str::to_owned);
        runs::append(&ledger, &[error]).unwrap();
        assert_eq!(report_fixture(&fixture).standings["value"], "unverified");
    }
}

#[test]
fn new_minitest_examples_start_unapproved() {
    let fixture = Fixture::new(TEST);
    let pending = report_fixture(&fixture);
    assert!(pending.pending);
    assert_eq!(pending.standings["value"], "unapproved");
    assert!(pending.markdown.contains("ruby_value"));
}

#[test]
fn source_change_during_discovery_prevents_execution_and_append() {
    use std::cell::Cell;
    use stele_plan::recorder::Source;
    struct ChangingSource {
        repo: Repo,
        reads: Cell<usize>,
    }
    impl Source for ChangingSource {
        fn identity(&self) -> Result<String, String> {
            let reads = self.reads.get();
            self.reads.set(reads + 1);
            if reads == 1 {
                fs::write(self.repo.root.join("lib/value.rb"), "def value; 8; end\n").unwrap();
            }
            self.repo.working_tree_identity()
        }
        fn base(&self) -> Result<String, String> {
            panic!("must stop before execution")
        }
        fn diff_against(&self, _: &str) -> Result<String, String> {
            panic!("must not record")
        }
    }
    let fixture = Fixture::new(TEST);
    let source = ChangingSource {
        repo: Repo::of_catalog(&fixture.root().join("intent")).unwrap(),
        reads: Cell::new(0),
    };
    let ledger = fixture.root().join("intent/runs.stele");
    let error = recorder::record_catalog(
        fixture.root(),
        "intent/catalog.stele",
        &Selection::default(),
        &mut fixture.runner(),
        &source,
        &ledger,
    )
    .unwrap_err();
    assert!(error.contains("source changed since discovery"));
    assert!(!ledger.exists());
}

#[test]
#[cfg(unix)]
#[ignore = "requires Ruby and locally installed reviewed Minitest 5.27.0 or 6.0.6"]
fn timeout_terminates_owned_workers_and_preserves_unrelated_processes() {
    struct Process(std::process::Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut unrelated = Process(
        Command::new("ruby")
            .args(["-e", "sleep 60"])
            .spawn()
            .unwrap(),
    );
    let fixture = Fixture::new(&TEST.replace("assert_equal 7, value", "worker = Process.spawn(RbConfig.ruby, '-e', 'sleep 60'); File.write('worker.pid', worker.to_s); File.write('parent.pid', Process.pid.to_s); sleep 60"));
    let mut runner = fixture.runner();
    runner.timeout = Duration::from_secs(2);
    assert_eq!(
        runner.run(&fixture.example(), "cli").unwrap().outcome,
        Outcome::Error
    );
    let worker: i32 = fs::read_to_string(fixture.root().join("worker.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let parent: i32 = fs::read_to_string(fixture.root().join("parent.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let alive = |pid| {
        // SAFETY: kill with signal zero probes only the fixture-owned process ID.
        unsafe { libc::kill(pid, 0) == 0 }
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while alive(worker) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let worker_survived = alive(worker);
    if worker_survived {
        // SAFETY: this PID came directly from the original synthetic fixture.
        unsafe {
            libc::kill(worker, libc::SIGKILL);
        }
    }
    assert!(
        unrelated.0.try_wait().unwrap().is_none(),
        "timeout killed an unrelated process"
    );
    assert!(!alive(parent), "direct Ruby child was not reaped");
    assert!(
        !worker_survived,
        "timeout left its spawned worker alive (cleaned up by the test)"
    );
}
