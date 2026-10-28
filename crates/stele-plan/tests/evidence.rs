//! The shipped checker proves its own rules, and the evidence a plan
//! projects states the plan, its catalog, the code it names and, once the
//! catalog has approvals, which of its statements are verified.

use std::collections::{BTreeMap, BTreeSet};

use lemmaspec::{EvidenceKind, FactValue, check_artifact, parse_artifact, print_artifact};
use stele_plan::{CHECKER, anchors, evidence, load, report::Standings};

#[test]
fn the_checker_proves_every_rule_and_condition_on_its_fixture() {
    assert_eq!(lemmaspec::walk_artifact(CHECKER).unwrap().status, "clean");
    let report = lemmaspec::mutate_artifact(CHECKER).unwrap();
    assert_eq!(report.status, "clean");
    assert_eq!(report.summary.survived, 0);
    assert!(report.summary.killed > 0);
}

fn files() -> BTreeMap<String, String> {
    [
        (
            "intent/catalog.stele",
            "catalog c {\n  intent works { text: \"It works.\" }\n}\n",
        ),
        (
            "plans/p.stele",
            "plan p {\n  use \"../intent/catalog.stele\" as intent\n  change make {\n    serves: [intent.works]\n    touches: [\"src/lib.rs\"]\n    state: done\n    text: \"Make it.\"\n  }\n}\n",
        ),
    ]
    .into_iter()
    .map(|(path, source)| (path.to_string(), source.to_string()))
    .collect()
}

#[test]
fn a_plan_with_anchors_needs_a_snapshot() {
    let files = files();
    let document = load(&files, "plans/p.stele").unwrap();
    assert_eq!(
        anchors(&document).into_iter().collect::<Vec<_>>(),
        ["src/lib.rs"]
    );
    assert_eq!(
        evidence(&files, &document, None, None).unwrap_err(),
        "the plan names code anchors, so it needs a code snapshot"
    );
}

#[test]
fn facts_cite_their_file_and_line_and_done_is_reviewer_declared() {
    let files = files();
    let mut document = load(&files, "plans/p.stele").unwrap();
    document
        .plan
        .as_mut()
        .unwrap()
        .changes
        .get_mut("make")
        .unwrap()
        .touches
        .clear();
    let artifact = evidence(&files, &document, None, None).unwrap();

    let fact = |relation: &str| {
        artifact
            .facts
            .iter()
            .find(|fact| fact.relation == relation)
            .unwrap_or_else(|| panic!("no {relation} fact"))
    };
    let serves = fact("serves");
    assert_eq!(
        serves.args,
        [
            FactValue::Symbol("make".into()),
            FactValue::Symbol("works".into())
        ]
    );
    let basis = serves.basis.as_ref().unwrap();
    assert_eq!(basis.kind, EvidenceKind::Snapshot);
    assert_eq!(basis.source, "plans/p.stele#L3");
    assert!(basis.identity.as_deref().unwrap().starts_with("sha256:"));
    assert_eq!(
        fact("statement").basis.as_ref().unwrap().source,
        "intent/catalog.stele#L2"
    );
    assert_eq!(
        fact("done").basis.as_ref().unwrap().kind,
        EvidenceKind::ReviewerDeclared
    );
    assert_eq!(artifact.expectations.len(), 7);
    assert!(
        artifact
            .expectations
            .iter()
            .all(|expectation| expectation.count == 0)
    );
    assert_eq!(
        artifact
            .symbols
            .iter()
            .find(|symbol| symbol.value == "works")
            .unwrap()
            .label,
        "It works."
    );
}

fn standings(verified: &[&str]) -> Standings {
    Standings {
        target: "sha256:head".into(),
        of: verified
            .iter()
            .map(|id| ((*id).to_string(), "verified".to_string()))
            .collect(),
    }
}

/// The evidence for the plan without anchors, with `standings`.
fn standings_evidence(standings: Option<&Standings>) -> lemmaspec::Artifact {
    let mut files = files();
    let plan = files.get_mut("plans/p.stele").unwrap();
    *plan = plan.replace("    touches: [\"src/lib.rs\"]\n", "");
    let document = load(&files, "plans/p.stele").unwrap();
    evidence(&files, &document, None, standings).unwrap()
}

#[test]
fn standings_add_the_verified_statements_on_the_intent_checkers_word() {
    let artifact = standings_evidence(Some(&standings(&["works"])));
    let facts: Vec<_> = artifact
        .facts
        .iter()
        .filter(|fact| fact.relation == "verified" || fact.relation == "standings_evaluated")
        .collect();
    assert_eq!(facts.len(), 2, "{facts:?}");
    for fact in &facts {
        let basis = fact.basis.as_ref().unwrap();
        assert_eq!(basis.kind, EvidenceKind::Policy);
        assert_eq!(basis.source, "stele intent checker at sha256:head");
    }
    assert_eq!(facts[0].args, [FactValue::Symbol("sha256:head".into())]);
    assert_eq!(facts[1].args, [FactValue::Symbol("works".into())]);
    assert_eq!(
        parse_artifact(&print_artifact(&artifact)).unwrap(),
        artifact
    );

    let none = standings_evidence(None);
    assert!(
        !none
            .facts
            .iter()
            .any(|fact| fact.relation == "verified" || fact.relation == "standings_evaluated")
    );
}

#[test]
fn a_done_change_needs_its_statements_verified_once_standings_are_evaluated() {
    let unmet = |standings: Option<&Standings>| -> BTreeSet<String> {
        let evidence = print_artifact(&standings_evidence(standings));
        check_artifact(CHECKER, &evidence)
            .unwrap()
            .expectations
            .into_iter()
            .filter(|expectation| !expectation.satisfied)
            .map(|expectation| expectation.id)
            .collect()
    };
    assert_eq!(unmet(None), BTreeSet::new());
    assert_eq!(unmet(Some(&standings(&["works"]))), BTreeSet::new());
    assert_eq!(
        unmet(Some(&standings(&[]))),
        BTreeSet::from(["done_changes_serve_verified_statements".to_string()])
    );
}
