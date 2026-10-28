//! P0 measures gate: `extract --measures` emits per-callable cyclomatic
//! complexity and line counts and per-file line classes that match a
//! hand-counted oracle, leaves the default output untouched, is
//! deterministic, and fails closed everywhere the plan says it must.

use std::collections::BTreeMap;
use std::path::PathBuf;

use codemap::schema::{
    Language, Measure, NodeKind, Snapshot, form_measures_available, measures_policy,
};
use codemap::{ExtractOptions, extract, to_json, validate};
use serde::Deserialize;

mod common;

#[derive(Deserialize)]
struct Expected {
    policy: String,
    files: BTreeMap<String, ExpectedFile>,
    callables: BTreeMap<String, BTreeMap<String, ExpectedCallable>>,
}

#[derive(Deserialize)]
struct ExpectedFile {
    loc: u32,
    sloc: u32,
    blank: u32,
    comment: u32,
}

#[derive(Deserialize)]
struct ExpectedCallable {
    cc: u32,
    loc: u32,
    sloc: u32,
    rules: String,
    /// The form measures, present in every oracle whose policy carries them.
    form: Option<ExpectedForm>,
}

#[derive(Deserialize)]
struct ExpectedForm {
    params: u32,
    nesting: u32,
    chain: u32,
    ivars: Option<u32>,
    rules: String,
}

fn expected_for(language: Language) -> Expected {
    let text = match language {
        Language::Rust => include_str!("fixtures/measures/rust/expected.json"),
        Language::Php => include_str!("fixtures/measures/php/expected.json"),
        Language::Go => include_str!("fixtures/measures/go/expected.json"),
        Language::Ruby => include_str!("fixtures/measures/ruby/expected.json"),
        Language::Cpp | Language::JsTs | Language::Python => panic!("cpp has no measures oracle"),
    };
    serde_json::from_str(text).expect("parse the hand-counted oracle")
}

fn expected() -> Expected {
    expected_for(Language::Rust)
}

fn options_for(repo: &std::path::Path, language: Language, measures: bool) -> ExtractOptions {
    ExtractOptions {
        path: PathBuf::from(repo),
        calls_required: false,
        language,
        measures,
        twins: false,
    }
}

fn options(repo: &std::path::Path, measures: bool) -> ExtractOptions {
    options_for(repo, Language::Rust, measures)
}

fn measured_snapshot_for(language: Language) -> Snapshot {
    let repo = common::make_measures_repo_for(language).expect("create measures repo");
    extract(&options_for(&repo, language, true)).expect("measured extraction")
}

fn measured_snapshot() -> Snapshot {
    measured_snapshot_for(Language::Rust)
}

const fn is_callable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function | NodeKind::Method | NodeKind::Macro
    )
}

#[test]
fn rust_measures_match_the_hand_counted_oracle() {
    check_oracle(Language::Rust);
    // The file with top-level code proves the denominator must come from
    // files, not from callable spans.
    let expected = expected();
    let snapshot = measured_snapshot();
    let measures: BTreeMap<&str, &Measure> = snapshot
        .measures
        .iter()
        .map(|m| (m.node_id.as_str(), m))
        .collect();
    let toplevel: u32 = snapshot
        .nodes
        .iter()
        .filter(|n| is_callable(n.kind) && n.provenance.rel_path == "toplevel.rs")
        .map(|n| measures[n.id.as_str()].sloc)
        .sum();
    assert!(toplevel < expected.files["toplevel.rs"].sloc);
}

#[test]
fn php_measures_match_the_hand_counted_oracle() {
    check_oracle(Language::Php);
}

#[test]
fn go_measures_match_the_hand_counted_oracle() {
    check_oracle(Language::Go);
}

#[test]
fn ruby_measures_match_the_hand_counted_oracle() {
    check_oracle(Language::Ruby);
}

/// Every callable and file of a language's fixture measures what the
/// hand-counted sidecar says, under that language's policy.
fn check_oracle(language: Language) {
    let expected = expected_for(language);
    let snapshot = measured_snapshot_for(language);
    assert_eq!(
        snapshot.identity.measures_policy.as_deref(),
        Some(expected.policy.as_str())
    );
    assert_eq!(Some(expected.policy.as_str()), measures_policy(language));
    validate(&snapshot).expect("a measured snapshot validates");

    let measures: BTreeMap<&str, &Measure> = snapshot
        .measures
        .iter()
        .map(|m| (m.node_id.as_str(), m))
        .collect();
    let callables: Vec<_> = snapshot
        .nodes
        .iter()
        .filter(|n| is_callable(n.kind))
        .collect();
    assert_eq!(
        callables.len(),
        snapshot.measures.len(),
        "one measure per callable node"
    );
    let expected_count: usize = expected.callables.values().map(BTreeMap::len).sum();
    assert_eq!(
        callables.len(),
        expected_count,
        "the oracle names every callable the fixture defines"
    );

    for (rel_path, by_name) in &expected.callables {
        for (name, want) in by_name {
            let node = callables
                .iter()
                .find(|n| n.provenance.rel_path == *rel_path && n.name == *name)
                .unwrap_or_else(|| panic!("callable {rel_path}#{name} was not extracted"));
            let got = measures
                .get(node.id.as_str())
                .unwrap_or_else(|| panic!("no measure for {}", node.id));
            assert_eq!(
                (got.cc, got.loc, got.sloc),
                (want.cc, want.loc, want.sloc),
                "{rel_path}#{name}: (cc, loc, sloc) expected under rule `{}`",
                want.rules
            );
            check_form(
                language,
                &format!("{rel_path}#{name}"),
                got,
                want.form.as_ref(),
            );
        }
    }

    assert_eq!(
        snapshot.file_measures.len(),
        expected.files.len(),
        "one file measure per fixture file"
    );
    for (rel_path, want) in &expected.files {
        let file_node = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::File && n.name == *rel_path)
            .unwrap_or_else(|| panic!("no file node for {rel_path}"));
        let got = snapshot
            .file_measures
            .iter()
            .find(|f| f.file_id == file_node.id)
            .unwrap_or_else(|| panic!("no file measure for {rel_path}"));
        assert_eq!(
            (got.loc, got.sloc, got.blank, got.comment),
            (want.loc, want.sloc, want.blank, want.comment),
            "{rel_path}: (loc, sloc, blank, comment)"
        );
        let callable_sloc: u32 = snapshot
            .nodes
            .iter()
            .filter(|n| is_callable(n.kind) && n.provenance.rel_path == *rel_path)
            .map(|n| measures[n.id.as_str()].sloc)
            .sum();
        assert!(
            callable_sloc <= got.sloc,
            "{rel_path}: callable sloc {callable_sloc} cannot exceed file sloc {}",
            got.sloc
        );
    }
}

#[test]
fn the_flag_leaves_the_default_output_untouched_and_changes_the_id_when_on() {
    let repo = common::make_repo_with(&common::measures_files()).expect("create measures repo");
    let plain = extract(&options(&repo, false)).expect("default extraction");
    let measured = extract(&options(&repo, true)).expect("measured extraction");

    assert_eq!(plain.identity.measures_policy, None);
    assert!(plain.measures.is_empty() && plain.file_measures.is_empty());
    let plain_json = to_json(&plain).expect("serialize default snapshot");
    assert!(
        !plain_json.contains("measures"),
        "the default snapshot must not mention measures at all"
    );

    assert_ne!(
        plain.identity.snapshot_id, measured.identity.snapshot_id,
        "measures change the snapshot id"
    );
    let mut without_policy = measured.identity.clone();
    without_policy.measures_policy = None;
    without_policy.snapshot_id = without_policy.compute_snapshot_id();
    assert_eq!(
        without_policy, plain.identity,
        "the policy field is the only identity difference"
    );
    assert_eq!(plain.files, measured.files);
    assert_eq!(plain.nodes, measured.nodes);
    assert_eq!(plain.edges, measured.edges);
    assert_eq!(plain.report, measured.report);
}

#[test]
fn measured_snapshots_are_byte_identical_across_checkouts() {
    let first = common::make_measures_repo().expect("first checkout");
    let second = common::make_measures_repo().expect("second checkout");
    let a = to_json(&extract(&options(&first, true)).expect("first")).expect("serialize");
    let b = to_json(&extract(&options(&second, true)).expect("second")).expect("serialize");
    assert_eq!(a, b);
}

#[test]
fn the_validator_refuses_measures_that_do_not_hold_together() {
    let good = measured_snapshot();
    validate(&good).expect("the measured snapshot validates");

    let mut no_policy = good.clone();
    no_policy.identity.measures_policy = None;
    no_policy.identity.snapshot_id = no_policy.identity.compute_snapshot_id();
    let err = validate(&no_policy).expect_err("measures without a policy");
    assert!(format!("{err:#}").contains("measures_policy"), "{err:#}");

    let mut wrong_policy = good.clone();
    wrong_policy.identity.measures_policy = Some("cc-sloc/rust/0".into());
    wrong_policy.identity.snapshot_id = wrong_policy.identity.compute_snapshot_id();
    let err = validate(&wrong_policy).expect_err("an unknown policy version");
    assert!(format!("{err:#}").contains("cc-sloc/rust/0"), "{err:#}");

    let mut sums = good.clone();
    sums.file_measures[0].blank += 1;
    let err = validate(&sums).expect_err("file classes that do not add up");
    assert!(format!("{err:#}").contains("does not add up"), "{err:#}");

    let mut missing = good.clone();
    missing.measures.pop();
    let err = validate(&missing).expect_err("a callable without a measure");
    assert!(format!("{err:#}").contains("has no measure"), "{err:#}");

    let mut inverted = good.clone();
    inverted.measures[0].sloc = inverted.measures[0].loc + 1;
    let err = validate(&inverted).expect_err("sloc above loc");
    assert!(format!("{err:#}").contains("above loc"), "{err:#}");

    let mut stray = good.clone();
    stray.measures[0].node_id = stray
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::File)
        .expect("a file node")
        .id
        .clone();
    let err = validate(&stray).expect_err("a measure naming a non-callable");
    assert!(format!("{err:#}").contains("not a callable"), "{err:#}");

    let mut policy_only = good;
    policy_only.measures.clear();
    policy_only.file_measures.clear();
    let err = validate(&policy_only).expect_err("a policy with nothing measured");
    assert!(
        format!("{err:#}").contains("carries no measures"),
        "{err:#}"
    );
}

/// The form measures match the oracle under a policy that carries them
/// and are absent under one that does not; an oracle out of step with its
/// policy is itself the failure.
fn check_form(language: Language, callable: &str, got: &Measure, want: Option<&ExpectedForm>) {
    let actual = (got.params, got.nesting, got.chain, got.ivars);
    match (want, form_measures_available(language)) {
        (Some(form), true) => assert_eq!(
            actual,
            (
                Some(form.params),
                Some(form.nesting),
                Some(form.chain),
                form.ivars
            ),
            "{callable}: (params, nesting, chain, ivars) expected under rule `{}`",
            form.rules
        ),
        (None, true) => panic!(
            "{callable}: the {} oracle must hand-count the form measures its policy carries",
            language.tag()
        ),
        (Some(_), false) => panic!(
            "{callable}: the {} oracle pins form measures its policy does not carry",
            language.tag()
        ),
        (None, false) => assert_eq!(
            actual,
            (None, None, None, None),
            "{callable}: no form measure under a policy without them"
        ),
    }
}

#[test]
fn form_measures_are_all_or_nothing_per_policy() {
    let ruby = measured_snapshot_for(Language::Ruby);
    validate(&ruby).expect("the measured Ruby snapshot validates");
    let mut lacking = ruby.clone();
    lacking.measures[0].params = None;
    let err = validate(&lacking).expect_err("a Ruby measure without params");
    assert!(format!("{err:#}").contains("lacks `params`"), "{err:#}");
    let mut lacking_ivars = ruby;
    lacking_ivars.measures[0].ivars = None;
    let err = validate(&lacking_ivars).expect_err("a Ruby measure without ivars");
    assert!(format!("{err:#}").contains("lacks `ivars`"), "{err:#}");

    // Every language carries the three form measures now; `ivars` is
    // Ruby's alone.
    let go = measured_snapshot_for(Language::Go);
    let mut carrying = go.clone();
    carrying.measures[0].ivars = Some(1);
    let err = validate(&carrying).expect_err("a Go measure with a form field");
    assert!(
        format!("{err:#}")
            .contains("carries `ivars`, which the go counting policy does not define"),
        "{err:#}"
    );
    assert!(
        !to_json(&go).expect("serialize").contains("\"ivars\""),
        "a language without instance variables never serializes ivars"
    );
}

#[test]
fn ruby_measures_preserve_default_facts_and_repeat_across_checkouts() {
    let first = common::make_measures_repo_for(Language::Ruby).expect("first checkout");
    let second = common::make_measures_repo_for(Language::Ruby).expect("second checkout");
    let plain = extract(&options_for(&first, Language::Ruby, false)).expect("plain");
    let mut measured = extract(&options_for(&first, Language::Ruby, true)).expect("measured");
    let again = extract(&options_for(&second, Language::Ruby, true)).expect("second");
    assert_eq!(to_json(&measured).unwrap(), to_json(&again).unwrap());
    assert_ne!(plain.identity.snapshot_id, measured.identity.snapshot_id);
    assert_eq!(
        measured.clone_groups,
        [] as [codemap::schema::CloneGroup; 0]
    );
    assert!(
        measured
            .file_measures
            .iter()
            .all(|file| file.clone_sloc == 0)
    );
    measured.measures.clear();
    measured.file_measures.clear();
    measured.identity.measures_policy = None;
    measured.identity.snapshot_id = measured.identity.compute_snapshot_id();
    assert_eq!(to_json(&plain).unwrap(), to_json(&measured).unwrap());
    assert!(
        serde_json::to_value(&plain)
            .unwrap()
            .get("measures")
            .is_none()
    );
}
