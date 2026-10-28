//! The shipped checkers and the adapter agree on one vocabulary, and each
//! checker's self-test passes under lemmaspec.

use std::collections::BTreeSet;

use codemap_lemmaspec::{Checker, EMITTED_RELATIONS};

mod common;

/// Every relation the checker declares, by scanning `relation <name> {`.
fn declared(source: &str) -> BTreeSet<&str> {
    source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("relation "))
        .filter_map(|rest| rest.split_whitespace().next())
        .collect()
}

/// Every relation a rule derives, by scanning `derive: "<name>(`.
fn derived(source: &str) -> BTreeSet<&str> {
    source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("derive: \""))
        .filter_map(|rest| rest.split('(').next())
        .collect()
}

#[test]
fn each_checker_is_named_after_its_spec() {
    for checker in Checker::ALL {
        assert!(
            checker
                .source()
                .contains(&format!("\nspec {} {{", checker.name())),
            "{} checker must declare `spec {}`",
            checker.name(),
            checker.name()
        );
    }
}

#[test]
fn the_adapter_asserts_exactly_the_relations_each_checker_leaves_to_evidence() {
    for checker in Checker::ALL {
        let source = checker.source();
        let base: BTreeSet<&str> = declared(source)
            .difference(&derived(source))
            .copied()
            .collect();
        let adapter: BTreeSet<&str> = checker.base_relations().iter().copied().collect();
        assert_eq!(
            adapter,
            base,
            "{}: base_relations() must equal the checker's declared minus derived relations",
            checker.name()
        );
        for relation in &base {
            assert!(
                EMITTED_RELATIONS.contains(relation),
                "{}: the adapter never emits `{relation}`, so the checker could never see it",
                checker.name()
            );
        }
    }
}

#[test]
fn each_claim_queries_a_relation_the_checker_derives() {
    for checker in Checker::ALL {
        let derived = derived(checker.source());
        for expectation in checker.expectations() {
            let relation = expectation.query.split('(').next().expect("atom");
            assert!(
                derived.contains(relation),
                "{}: claim `{}` queries `{relation}`, which no rule derives",
                checker.name(),
                expectation.id
            );
            assert_eq!(
                expectation.count,
                usize::from(expectation.id == "coverage_is_admitted")
            );
        }
    }
}

#[test]
fn each_checker_walks_clean_and_survives_no_mutant() {
    let dir = tempfile::tempdir().expect("tempdir");
    for checker in Checker::ALL {
        let path = dir.path().join(format!("{}.lemmaspec", checker.name()));
        std::fs::write(&path, checker.source()).expect("write checker");
        let path = path.to_str().expect("utf8 path");
        let (code, stdout) = common::run_lemmaspec(&["walk", path]);
        assert_eq!(code, 0, "{} walk:\n{stdout}", checker.name());
        let (code, stdout) = common::run_lemmaspec(&["mutate", path]);
        assert_eq!(code, 0, "{} mutate:\n{stdout}", checker.name());
    }
}
