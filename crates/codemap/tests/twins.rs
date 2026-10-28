//! Twin gate: `extract --measures --twins` finds the pairs a hand-derived
//! oracle names and nothing else, validates, stays deterministic across
//! checkouts, fails closed without measures, and leaves the measured
//! output without the flag untouched.

use std::collections::BTreeMap;
use std::path::PathBuf;

use codemap::schema::{
    Language, NodeKind, Snapshot, TWIN_MIN_CONTAINMENT_PERCENT, TWIN_MIN_TOKENS,
    TWIN_SHINGLE_TOKENS, TwinPair, node_id, twins_policy,
};
use codemap::{ExtractOptions, extract, to_json, validate};
use serde::Deserialize;

mod common;

#[derive(Deserialize)]
struct Expected {
    policy: String,
    pairs: Vec<ExpectedPair>,
    no_pair: Vec<[[String; 2]; 2]>,
    #[serde(default)]
    observed: BTreeMap<String, Observed>,
}

#[derive(Deserialize)]
struct ExpectedPair {
    a: [String; 2],
    b: [String; 2],
    exact: bool,
    clone_group: bool,
    inserted_tokens: u32,
}

#[derive(Deserialize)]
struct Observed {
    shared: u32,
    shingles_a: u32,
    shingles_b: u32,
}

fn expected_for(language: Language) -> Expected {
    let text = match language {
        Language::Rust => include_str!("fixtures/twins/rust/expected.json"),
        Language::Ruby => include_str!("fixtures/twins/ruby/expected.json"),
        Language::Php | Language::Go | Language::Cpp | Language::JsTs | Language::Python => {
            panic!("no twin oracle for {}", language.tag())
        }
    };
    serde_json::from_str(text).expect("parse the hand-derived twin oracle")
}

fn options(
    repo: &std::path::Path,
    language: Language,
    measures: bool,
    twins: bool,
) -> ExtractOptions {
    ExtractOptions {
        path: PathBuf::from(repo),
        calls_required: false,
        language,
        measures,
        twins,
    }
}

const fn callable_kind(language: Language) -> NodeKind {
    match language {
        Language::Ruby => NodeKind::Method,
        Language::Rust
        | Language::Php
        | Language::Go
        | Language::Cpp
        | Language::JsTs
        | Language::Python => NodeKind::Function,
    }
}

fn id_of(language: Language, side: &[String; 2]) -> String {
    node_id(language, callable_kind(language), &side[0], &[], &side[1])
}

fn twin_snapshot(language: Language) -> Snapshot {
    let repo = common::make_twins_repo_for(language).expect("create twins repo");
    extract(&options(&repo, language, true, true)).expect("measured extraction with twins")
}

fn find<'a>(snapshot: &'a Snapshot, a: &str, b: &str) -> Option<&'a TwinPair> {
    snapshot
        .twin_pairs
        .iter()
        .find(|pair| (pair.a == a && pair.b == b) || (pair.a == b && pair.b == a))
}

fn share_a_clone_group(snapshot: &Snapshot, a: &str, b: &str) -> bool {
    let groups = |id: &str| -> Vec<String> {
        snapshot
            .measures
            .iter()
            .find(|measure| measure.node_id == id)
            .map(|measure| measure.clone_group_ids.clone())
            .unwrap_or_default()
    };
    let (ga, gb) = (groups(a), groups(b));
    ga.iter().any(|group| gb.contains(group))
}

fn check_oracle(language: Language) {
    let expected = expected_for(language);
    let snapshot = twin_snapshot(language);
    assert_eq!(
        snapshot.identity.twins_policy.as_deref(),
        Some(expected.policy.as_str())
    );
    assert_eq!(
        expected.policy,
        twins_policy(language).expect("oracle language has a twin policy")
    );
    assert!(snapshot.identity.measures_policy.is_some());
    validate(&snapshot).expect("a snapshot with twins validates");

    assert_eq!(
        snapshot.twin_pairs.len(),
        expected.pairs.len(),
        "exactly the oracle's pairs: {:#?}",
        snapshot.twin_pairs
    );
    let floor = u32::try_from(TWIN_MIN_TOKENS - TWIN_SHINGLE_TOKENS + 1).unwrap();
    for want in &expected.pairs {
        let (a, b) = (id_of(language, &want.a), id_of(language, &want.b));
        let got = find(&snapshot, &a, &b).unwrap_or_else(|| {
            panic!(
                "no twin pair {} / {}: {:#?}",
                want.a[1], want.b[1], snapshot.twin_pairs
            )
        });
        assert!(got.a < got.b, "ordered by node id");
        let (small, large) = if got.a == a {
            (got.shingles_a, got.shingles_b)
        } else {
            (got.shingles_b, got.shingles_a)
        };
        assert!(
            small >= floor && large >= floor,
            "{}: both sides over the token floor",
            want.a[1]
        );
        assert_eq!(
            large - small,
            want.inserted_tokens,
            "{}: the inserted tokens are the shingle difference",
            want.b[1]
        );
        assert!(
            u64::from(got.shared) * 100
                >= u64::from(TWIN_MIN_CONTAINMENT_PERCENT) * u64::from(small),
            "{}: over the containment threshold",
            want.a[1]
        );
        if want.exact {
            assert!(
                got.is_exact(),
                "{}: verbatim twins share every shingle",
                want.a[1]
            );
            assert!((got.containment() - 1.0).abs() < f64::EPSILON);
        } else {
            assert!(got.shared < small, "{}: an edit costs shingles", want.a[1]);
        }
        assert_eq!(
            share_a_clone_group(&snapshot, &a, &b),
            want.clone_group,
            "{}: clone group membership",
            want.a[1]
        );
        if let Some(observed) = expected.observed.get(&want.a[1]) {
            assert_eq!(
                (got.shared, got.shingles_a, got.shingles_b),
                (observed.shared, observed.shingles_a, observed.shingles_b),
                "{}: pinned integers",
                want.a[1]
            );
        }
    }
    for [a, b] in &expected.no_pair {
        assert!(
            find(&snapshot, &id_of(language, a), &id_of(language, b)).is_none(),
            "{} / {} must not pair",
            a[1],
            b[1]
        );
    }
}

#[test]
fn rust_twin_pairs_match_the_hand_derived_oracle() {
    check_oracle(Language::Rust);
}

#[test]
fn ruby_twin_pairs_match_the_hand_derived_oracle() {
    check_oracle(Language::Ruby);
}

#[test]
fn twins_need_measures_and_leave_the_measured_output_untouched() {
    let repo = common::make_twins_repo_for(Language::Rust).expect("create twins repo");
    let error = extract(&options(&repo, Language::Rust, false, true)).expect_err("refused");
    assert!(
        format!("{error:#}").contains("--twins needs --measures"),
        "{error:#}"
    );

    let measured = extract(&options(&repo, Language::Rust, true, false)).expect("measured");
    assert_eq!(measured.twin_pairs, [] as [codemap::schema::TwinPair; 0]);
    assert!(measured.identity.twins_policy.is_none());
    let json = to_json(&measured).expect("serialize");
    assert!(!json.contains("twin_pairs"), "absent fields are absent");
    assert!(!json.contains("twins_policy"));

    let with_twins = twin_snapshot(Language::Rust);
    assert_ne!(
        measured.identity.snapshot_id,
        with_twins.identity.snapshot_id
    );
    let again = twin_snapshot(Language::Rust);
    assert_eq!(
        to_json(&with_twins).expect("first"),
        to_json(&again).expect("second"),
        "byte-identical across independent checkouts"
    );
}

type Mutant = (&'static str, Box<dyn Fn(&mut Snapshot)>);

#[test]
fn the_validator_refuses_inconsistent_twins() {
    let good = twin_snapshot(Language::Rust);
    assert!(good.twin_pairs.len() >= 2);
    let mutants: Vec<Mutant> = vec![
        (
            "no twins_policy",
            Box::new(|s| s.identity.twins_policy = None),
        ),
        (
            "wrong twins_policy",
            Box::new(|s| s.identity.twins_policy = Some("twins/ruby/1".into())),
        ),
        (
            "unknown callable",
            Box::new(|s| s.twin_pairs[0].a = "rust:function:src/a.rs#ghost".into()),
        ),
        (
            "unordered pair",
            Box::new(|s| {
                let pair = &mut s.twin_pairs[0];
                std::mem::swap(&mut pair.a, &mut pair.b);
            }),
        ),
        (
            "duplicate pair",
            Box::new(|s| {
                let first = s.twin_pairs[0].clone();
                s.twin_pairs.push(first);
            }),
        ),
        (
            "shared above the smaller side",
            Box::new(|s| {
                s.twin_pairs[0].shared =
                    s.twin_pairs[0].shingles_a.min(s.twin_pairs[0].shingles_b) + 1;
            }),
        ),
        (
            "under the containment threshold",
            Box::new(|s| s.twin_pairs[0].shared = 1),
        ),
        (
            "under the token floor",
            Box::new(|s| {
                s.twin_pairs[0].shingles_a = 10;
                s.twin_pairs[0].shared = 10;
            }),
        ),
        (
            "policy without pairs is fine, pairs without measures are not",
            Box::new(|s| {
                s.identity.measures_policy = None;
                s.measures.clear();
                s.file_measures.clear();
                s.clone_groups.clear();
            }),
        ),
    ];
    for (name, mutate) in mutants {
        let mut bad = good.clone();
        mutate(&mut bad);
        bad.identity.snapshot_id = bad.identity.compute_snapshot_id();
        assert!(validate(&bad).is_err(), "validator must refuse: {name}");
    }
    let mut policy_only = good;
    policy_only.twin_pairs.clear();
    policy_only.identity.snapshot_id = policy_only.identity.compute_snapshot_id();
    validate(&policy_only).expect("a twin policy over a repository with no twins is a true zero");
}
