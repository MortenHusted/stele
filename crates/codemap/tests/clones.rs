//! P2 clone gate: `extract --measures` finds the clone groups a hand-derived
//! oracle names and nothing else, attaches them to the right callables and
//! files under the one line rule, validates, stays deterministic across
//! checkouts, and leaves the default output untouched.

use std::collections::BTreeMap;
use std::path::PathBuf;

use codemap::schema::{
    CLONE_MIN_LINES, CLONE_MIN_TOKENS, CloneGroup, Language, NodeKind, Snapshot, measures_policy,
};
use codemap::{ExtractOptions, extract, to_json, validate};
use serde::Deserialize;

mod common;

#[derive(Deserialize)]
struct Expected {
    policy: String,
    groups: Vec<ExpectedGroup>,
    file_clone_sloc: BTreeMap<String, u32>,
    callable_groups: BTreeMap<String, BTreeMap<String, Option<String>>>,
}

#[derive(Deserialize)]
struct ExpectedGroup {
    name: String,
    occurrences: Vec<ExpectedOccurrence>,
    lines: u32,
}

#[derive(Deserialize, PartialEq, Eq, Debug)]
struct ExpectedOccurrence {
    rel_path: String,
    start_line: u32,
    end_line: u32,
    sloc: u32,
}

fn expected_for(language: Language) -> Expected {
    let text = match language {
        Language::Rust => include_str!("fixtures/clones/rust/expected.json"),
        Language::Php => include_str!("fixtures/clones/php/expected.json"),
        Language::Go => include_str!("fixtures/clones/go/expected.json"),
        Language::Ruby => include_str!("fixtures/clones/ruby/expected.json"),
        Language::Cpp | Language::JsTs | Language::Python => panic!("cpp has no clone oracle"),
    };
    serde_json::from_str(text).expect("parse the hand-derived clone oracle")
}

fn options(repo: &std::path::Path, language: Language, measures: bool) -> ExtractOptions {
    ExtractOptions {
        path: PathBuf::from(repo),
        calls_required: false,
        language,
        measures,
        twins: false,
    }
}

fn measured_snapshot_for(language: Language) -> Snapshot {
    let repo = common::make_clones_repo_for(language).expect("create clones repo");
    extract(&options(&repo, language, true)).expect("measured extraction")
}

fn measured_snapshot() -> Snapshot {
    measured_snapshot_for(Language::Rust)
}

/// The oracle group that has exactly these occurrences.
fn find<'a>(snapshot: &'a Snapshot, group: &ExpectedGroup) -> &'a CloneGroup {
    snapshot
        .clone_groups
        .iter()
        .find(|g| {
            let got: Vec<ExpectedOccurrence> = g
                .occurrences
                .iter()
                .map(|o| ExpectedOccurrence {
                    rel_path: o.rel_path.clone(),
                    start_line: o.start_line,
                    end_line: o.end_line,
                    sloc: o.sloc,
                })
                .collect();
            got == group.occurrences
        })
        .unwrap_or_else(|| {
            panic!(
                "no clone group with the occurrences the oracle names for {}; got {:#?}",
                group.name, snapshot.clone_groups
            )
        })
}

#[test]
fn rust_clone_groups_match_the_hand_derived_oracle() {
    check_oracle(Language::Rust);
}

#[test]
fn php_clone_groups_match_the_hand_derived_oracle() {
    check_oracle(Language::Php);
}

#[test]
fn go_clone_groups_match_the_hand_derived_oracle() {
    check_oracle(Language::Go);
}

#[test]
fn ruby_clone_groups_match_the_hand_derived_oracle() {
    check_oracle(Language::Ruby);
}

#[test]
fn ruby_measured_output_is_deterministic_across_checkouts() {
    let first = measured_snapshot_for(Language::Ruby);
    let second = measured_snapshot_for(Language::Ruby);
    assert_eq!(
        to_json(&first).expect("serialize first"),
        to_json(&second).expect("serialize second")
    );
    assert_ne!(first.clone_groups, [] as [codemap::schema::CloneGroup; 0]);
}

/// The detector finds exactly the groups the sidecar names for one
/// language's fixture, attaches them to the right files and callables,
/// and the snapshot validates.
fn check_oracle(language: Language) {
    let expected = expected_for(language);
    let snapshot = measured_snapshot_for(language);
    assert_eq!(
        snapshot.identity.measures_policy.as_deref(),
        Some(expected.policy.as_str())
    );
    assert_eq!(Some(expected.policy.as_str()), measures_policy(language));
    validate(&snapshot).expect("a measured snapshot with clones validates");

    assert_eq!(
        snapshot.clone_groups.len(),
        expected.groups.len(),
        "exactly the oracle's groups, no near miss admitted: {:#?}",
        snapshot.clone_groups
    );
    let mut ids_by_name: BTreeMap<&str, &str> = BTreeMap::new();
    for group in &expected.groups {
        let got = find(&snapshot, group);
        assert_eq!(
            got.lines, group.lines,
            "{}: shortest occurrence",
            group.name
        );
        assert!(
            got.tokens as usize >= CLONE_MIN_TOKENS,
            "{}: {} tokens is under the threshold",
            group.name,
            got.tokens
        );
        assert!(got.lines >= CLONE_MIN_LINES);
        assert!(got.id.starts_with("clone:"), "{}", got.id);
        ids_by_name.insert(&group.name, &got.id);
    }
    assert_eq!(ids_by_name.len(), 2, "the two groups have distinct ids");

    for file in &snapshot.file_measures {
        let rel_path = snapshot
            .nodes
            .iter()
            .find(|n| n.id == file.file_id)
            .map(|n| n.provenance.rel_path.clone())
            .expect("file node");
        assert_eq!(
            file.clone_sloc, expected.file_clone_sloc[&rel_path],
            "{rel_path}: source lines under a clone"
        );
        assert!(file.clone_sloc <= file.sloc);
    }

    let names: BTreeMap<&str, (&str, &str)> = snapshot
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Macro
            )
        })
        .map(|n| {
            (
                n.id.as_str(),
                (n.provenance.rel_path.as_str(), n.name.as_str()),
            )
        })
        .collect();
    let mut checked = 0;
    for measure in &snapshot.measures {
        let (rel_path, name) = names[measure.node_id.as_str()];
        let want = expected.callable_groups[rel_path]
            .get(name)
            .unwrap_or_else(|| panic!("oracle names no expectation for {rel_path} {name}"));
        let got: Vec<&str> = measure.clone_group_ids.iter().map(String::as_str).collect();
        match want {
            Some(group) => assert_eq!(got, vec![ids_by_name[group.as_str()]], "{rel_path} {name}"),
            None => assert!(got.is_empty(), "{rel_path} {name} is in {got:?}"),
        }
        checked += 1;
    }
    assert_eq!(
        checked, 10,
        "every callable the fixture defines was checked"
    );
}

#[test]
fn the_default_output_carries_no_clones_and_the_measured_output_is_deterministic() {
    let repo = common::make_clones_repo().expect("create clones repo");
    let plain = extract(&options(&repo, Language::Rust, false)).expect("default extraction");
    assert_eq!(plain.clone_groups, [] as [codemap::schema::CloneGroup; 0]);
    assert!(plain.identity.measures_policy.is_none());
    let json = to_json(&plain).expect("serialize");
    assert!(!json.contains("clone_groups"), "absent fields are absent");
    assert!(!json.contains("clone_sloc"));

    let first = measured_snapshot();
    let second = measured_snapshot();
    assert_eq!(
        to_json(&first).expect("serialize first"),
        to_json(&second).expect("serialize second"),
        "measured extraction is byte-identical across independent checkouts"
    );
    assert_ne!(first.identity.snapshot_id, plain.identity.snapshot_id);
}

type Mutant = (&'static str, Box<dyn Fn(&mut Snapshot)>);

#[test]
fn the_validator_refuses_inconsistent_clones() {
    let good = measured_snapshot();
    let mutants: Vec<Mutant> = vec![
        (
            "clone_sloc above sloc",
            Box::new(|s| s.file_measures[0].clone_sloc = s.file_measures[0].sloc + 1),
        ),
        (
            "a group with one occurrence",
            Box::new(|s| s.clone_groups[0].occurrences.truncate(1)),
        ),
        (
            "a group under the token threshold",
            Box::new(|s| s.clone_groups[0].tokens = 10),
        ),
        (
            "a group whose declared lines disagree with its occurrences",
            Box::new(|s| s.clone_groups[0].lines += 1),
        ),
        (
            "an occurrence in a file the inventory lacks",
            Box::new(|s| s.clone_groups[0].occurrences[0].rel_path = "src/ghost.rs".into()),
        ),
        (
            "an occurrence counting more source lines than it spans",
            Box::new(|s| s.clone_groups[0].occurrences[0].sloc = 1000),
        ),
        (
            "a duplicate group id",
            Box::new(|s| {
                let dup = s.clone_groups[0].clone();
                s.clone_groups.push(dup);
            }),
        ),
        (
            "a measure naming a group the snapshot lacks",
            Box::new(|s| s.measures[0].clone_group_ids = vec!["clone:nope".into()]),
        ),
        (
            "a measure naming a group that does not overlap it",
            Box::new(|s| {
                let id = s.clone_groups[0].id.clone();
                let uninvolved = s
                    .measures
                    .iter()
                    .position(|m| m.clone_group_ids.is_empty())
                    .expect("a callable outside every clone");
                s.measures[uninvolved].clone_group_ids = vec![id];
            }),
        ),
        (
            "clone groups without measures",
            Box::new(|s| {
                s.measures.clear();
                s.file_measures.clear();
            }),
        ),
    ];
    for (what, mutate) in mutants {
        let mut bad = good.clone();
        mutate(&mut bad);
        assert!(validate(&bad).is_err(), "the validator accepted {what}");
    }
}
