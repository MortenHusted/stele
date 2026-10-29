//! The projection over a hand-built snapshot: which facts leave, with which
//! provenance, and what each shipped checker finds in them.

use codemap_lemmaspec::{Checker, Options, project};

mod common;

const fn options(checker: Checker) -> Options {
    Options {
        checker,
        min_call_coverage_per_mille: 1000,
        spec_name: None,
        allow_dirty: false,
        core_files: Vec::new(),
        presentation_files: Vec::new(),
        entry_points: Vec::new(),
        public_api: Vec::new(),
    }
}

fn architecture_options() -> Options {
    Options {
        core_files: vec!["src/core.rs".into()],
        presentation_files: vec!["src/view.rs".into()],
        ..options(Checker::Architecture)
    }
}

fn fact_lines<'a>(evidence: &'a str, relation: &str) -> Vec<&'a str> {
    evidence
        .lines()
        .filter(|line| line.contains(&format!("relation: {relation} ")))
        .collect()
}

#[test]
fn shipped_callables_are_defined_with_their_source_location() {
    let evidence = project(&common::rust_snapshot(false), &options(Checker::Symbols))
        .expect("projection succeeds");
    let defines = fact_lines(&evidence, "defines");
    assert_eq!(defines.len(), 9, "{evidence}");
    assert!(defines.iter().any(|line| {
        line.contains(&format!(
            "args: [\"{}\", \"src/core.rs\"] provenance: [\"src/core.rs:5\"]",
            common::helper_id()
        ))
    }));
    for excluded in ["importref", "type:", "#tests", "module:"] {
        assert!(
            !defines.iter().any(|line| line.contains(excluded)),
            "{excluded} must not be a shipped definition:\n{evidence}"
        );
    }
}

#[test]
fn files_are_classified_by_policy_and_unclassified_files_get_no_layout_fact() {
    let evidence =
        project(&common::rust_snapshot(false), &architecture_options()).expect("projection");
    let production = fact_lines(&evidence, "production_file");
    assert_eq!(production.len(), 5, "{evidence}");
    assert!(
        production
            .iter()
            .all(|line| line.ends_with("provenance: [\"policy:file-layout/rust\"] }"))
    );
    assert_eq!(fact_lines(&evidence, "test_file").len(), 1);
    assert!(!evidence.contains("args: [\"build.rs\"]"), "{evidence}");
}

#[test]
fn calls_are_deduplicated_at_their_first_site_and_test_module_calls_are_dropped() {
    let evidence =
        project(&common::rust_snapshot(false), &options(Checker::Symbols)).expect("projection");
    let calls = fact_lines(&evidence, "calls");
    assert_eq!(calls.len(), 5, "{evidence}");
    let run_calls_helper = format!(
        "args: [\"{}\", \"{}\"] provenance: [\"src/core.rs:2\"]",
        common::run_id(),
        common::helper_id()
    );
    assert!(calls.iter().any(|line| line.contains(&run_calls_helper)));
    let recursion = format!("args: [\"{0}\", \"{0}\"]", common::helper_id());
    assert!(calls.iter().any(|line| line.contains(&recursion)));
    assert!(!evidence.contains("#tests::"), "{evidence}");
}

#[test]
fn review_decisions_carry_no_provenance_and_default_entry_points_cite_the_policy() {
    let evidence =
        project(&common::rust_snapshot(false), &architecture_options()).expect("projection");
    assert!(evidence.contains("fact core_file_0 { relation: core_file args: [\"src/core.rs\"] }"));
    assert!(evidence.contains(
        "fact presentation_file_0 { relation: presentation_file args: [\"src/view.rs\"] }"
    ));
    let entries = fact_lines(&evidence, "entry_point");
    assert_eq!(
        entries.len(),
        1,
        "build.rs#main is not a binary root:\n{evidence}"
    );
    assert!(entries[0].contains(&format!(
        "args: [\"{}\"] provenance: [\"policy:entry-point/rust\"]",
        common::main_id()
    )));
}

#[test]
fn an_explicit_entry_point_is_a_review_decision_unless_the_policy_already_found_it() {
    let snapshot = common::rust_snapshot(false);
    let evidence = project(
        &snapshot,
        &Options {
            entry_points: vec![
                "rust:src/main.rs#main".into(),
                "rust:tests/it.rs#it_probes".into(),
            ],
            ..options(Checker::Symbols)
        },
    )
    .expect("projection");
    let entries = fact_lines(&evidence, "entry_point");
    assert_eq!(entries.len(), 2, "{evidence}");
    assert!(
        entries
            .iter()
            .any(|line| line.contains(&common::main_id()) && line.contains("policy:"))
    );
    assert!(
        entries
            .iter()
            .any(|line| { line.contains(&common::it_probes_id()) && !line.contains("provenance") })
    );
}

#[test]
fn only_the_checkers_vocabulary_is_asserted_and_its_claims_are_appended() {
    let snapshot = common::rust_snapshot(false);
    let symbols = project(&snapshot, &options(Checker::Symbols)).expect("projection");
    assert!(!symbols.contains("relation: test_file"));
    assert!(symbols.contains("expect no_observed_unreached_symbols {"));
    assert!(symbols.contains("query: \"unused_production_symbol(S, _)\""));
    assert_eq!(symbols.matches("\n  expect ").count(), 2);

    let architecture = project(&snapshot, &architecture_options()).expect("projection");
    assert!(architecture.contains("relation: test_file"));
    assert_eq!(architecture.matches("\n  expect ").count(), 4);
    assert!(architecture.starts_with("// Evidence for the `architecture` checker"));
    assert!(architecture.contains("\nspec https___github_com_example_fixture_git {\n"));
}

#[test]
fn the_projection_is_byte_stable() {
    let snapshot = common::rust_snapshot(false);
    let first = project(&snapshot, &architecture_options()).expect("projection");
    let second = project(&snapshot, &architecture_options()).expect("projection");
    assert_eq!(first, second);
}

#[test]
fn absence_checkers_refuse_a_cpp_snapshot() {
    let snapshot = common::cpp_snapshot();
    for checker in [Checker::Architecture, Checker::Symbols] {
        let error = project(&snapshot, &options(checker)).expect_err("C++ is refused");
        assert!(error.to_string().contains("coverage contract"), "{error}");
    }
}

#[test]
fn absence_checkers_refuse_a_js_ts_snapshot() {
    let snapshot = common::js_ts_snapshot();
    for checker in [Checker::Architecture, Checker::Symbols] {
        let error = project(&snapshot, &options(checker)).expect_err("JS/TS absence is refused");
        assert!(error.to_string().contains("coverage contract"), "{error}");
    }
}

#[test]
fn absence_checkers_refuse_python_even_with_native_call_evidence() {
    let snapshot = common::python_snapshot();
    codemap_graph::validate(&snapshot).unwrap();
    for checker in [Checker::Architecture, Checker::Symbols] {
        let error = project(&snapshot, &options(checker)).unwrap_err();
        assert!(error.to_string().contains("coverage contract"));
    }
    assert!(
        project(&snapshot, &options(Checker::Hotspots))
            .unwrap_err()
            .to_string()
            .contains("no measures")
    );
}

#[test]
fn a_dirty_snapshot_is_refused_unless_allowed() {
    let snapshot = common::rust_snapshot(true);
    let error = project(&snapshot, &options(Checker::Symbols)).expect_err("dirty is refused");
    assert!(error.to_string().contains("--allow-dirty"), "{error}");
    project(
        &snapshot,
        &Options {
            allow_dirty: true,
            ..options(Checker::Symbols)
        },
    )
    .expect("allowed");
}

#[test]
fn a_decision_the_checker_cannot_use_or_the_snapshot_cannot_locate_is_an_error() {
    let snapshot = common::rust_snapshot(false);
    let error = project(
        &snapshot,
        &Options {
            core_files: vec!["src/core.rs".into()],
            ..options(Checker::Symbols)
        },
    )
    .expect_err("symbols does not declare core_file");
    assert!(error.to_string().contains("--core"), "{error}");

    let error = project(
        &snapshot,
        &Options {
            presentation_files: vec!["src/nope.rs".into()],
            ..options(Checker::Architecture)
        },
    )
    .expect_err("unknown file");
    assert!(error.to_string().contains("src/nope.rs"), "{error}");

    let error = project(
        &snapshot,
        &Options {
            public_api: vec!["rust:src/core.rs#nope".into()],
            ..options(Checker::Symbols)
        },
    )
    .expect_err("unknown symbol");
    assert!(error.to_string().contains("--public-api"), "{error}");
}

#[test]
fn complexity_facts_cite_each_measured_shipped_callable() {
    let snapshot = common::measured_rust_snapshot(12);
    let evidence = project(&snapshot, &options(Checker::Hotspots)).expect("projection");
    let complexity = fact_lines(&evidence, "complexity");
    let defines = fact_lines(&evidence, "defines");
    assert_eq!(
        complexity.len(),
        defines.len(),
        "one complexity per shipped definition, none for the test-module callable:\n{evidence}"
    );
    let main_line = complexity
        .iter()
        .find(|line| line.contains(&format!("\"{}\", 12]", common::main_id())))
        .unwrap_or_else(|| panic!("main at cc 12 as a bare integer:\n{evidence}"));
    assert!(
        main_line.contains("provenance: [\"src/main.rs:1\"]"),
        "cited at the callable: {main_line}"
    );
    assert!(
        evidence.contains("complexity facts counted under `cc-sloc-clones-form/rust/1`"),
        "{evidence}"
    );
    assert!(
        !project(&snapshot, &options(Checker::Symbols))
            .expect("projection")
            .contains("relation: complexity"),
        "a checker that does not declare complexity never sees it"
    );
}

#[test]
fn the_hotspots_checker_refuses_an_unmeasured_snapshot() {
    let error = project(&common::rust_snapshot(false), &options(Checker::Hotspots))
        .expect_err("unmeasured");
    let text = error.to_string();
    assert!(text.contains("hotspots checker reads complexity"), "{text}");
    assert!(text.contains("--measures"), "{text}");
}

#[test]
fn a_rails_action_is_an_entry_point_by_policy_and_a_model_method_is_not() {
    let snapshot = common::measured_ruby_snapshot(12, 30);
    let evidence = project(&snapshot, &options(Checker::Hotspots)).expect("projection");
    let entries = fact_lines(&evidence, "entry_point");
    assert_eq!(entries.len(), 1, "{evidence}");
    assert!(
        entries[0].contains(&common::cards_index_id())
            && entries[0].contains("provenance: [\"policy:entry-point/ruby\"]"),
        "{}",
        entries[0]
    );
    let complexity = fact_lines(&evidence, "complexity");
    assert_eq!(complexity.len(), 2, "{evidence}");
    assert!(
        complexity
            .iter()
            .any(|line| line.contains(&common::card_title_id()) && line.contains(", 30]")),
        "the model method is measured, and the rule leaves it alone:\n{evidence}"
    );
}

#[test]
fn the_checker_carries_the_named_hotspot_cutoff() {
    // codemap-slop holds this constant equal to its own HIGH_CC_CUTOFF from
    // its side (crates/codemap-slop/tests/hotspot_cutoff.rs), so no Cargo
    // edge runs from this crate to that one.
    assert!(
        Checker::Hotspots.source().contains(&format!(
            "\"Cc > {}\"",
            codemap_lemmaspec::policy::HOTSPOT_CUTOFF
        )),
        "the checker's rule must compare against the named cutoff"
    );
}

fn check(checker: Checker, evidence: &str) -> (i32, serde_json::Value) {
    let dir = tempfile::tempdir().expect("tempdir");
    let checker_path = dir.path().join(format!("{}.lemmaspec", checker.name()));
    let evidence_path = dir.path().join("evidence.lemmaspec");
    std::fs::write(&checker_path, checker.source()).expect("write checker");
    std::fs::write(&evidence_path, evidence).expect("write evidence");
    let (code, stdout) = common::run_lemmaspec(&[
        "check",
        checker_path.to_str().expect("utf8 path"),
        evidence_path.to_str().expect("utf8 path"),
        "--json",
    ]);
    let report = serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!("lemmaspec check printed no JSON report ({error}):\n{stdout}")
    });
    (code, report)
}

fn found(report: &serde_json::Value, expectation: &str) -> Vec<String> {
    report["expectations"]
        .as_array()
        .expect("expectations array")
        .iter()
        .find(|entry| entry["id"] == expectation)
        .unwrap_or_else(|| panic!("no expectation {expectation} in {report}"))["found"]
        .as_array()
        .expect("found array")
        .iter()
        .map(|row| row.as_str().expect("row text").to_string())
        .collect()
}

#[test]
fn the_architecture_checker_finds_each_planted_defect() {
    let evidence =
        project(&common::rust_snapshot(false), &architecture_options()).expect("projection");
    let (code, report) = check(Checker::Architecture, &evidence);
    assert_eq!(code, 1, "three claims must be open:\n{report}");
    assert_eq!(
        found(&report, "core_stays_out_of_presentation"),
        vec![format!("S={}", common::render_id())]
    );
    assert_eq!(
        found(&report, "no_observed_test_only_production_files"),
        vec!["F=src/probe.rs".to_string()]
    );
    assert_eq!(
        found(&report, "no_observed_unreferenced_production_files"),
        vec!["F=src/dead.rs".to_string()]
    );
}

#[test]
fn the_symbols_checker_finds_each_unreachable_callable_and_honours_public_api() {
    let snapshot = common::rust_snapshot(false);
    let evidence = project(&snapshot, &options(Checker::Symbols)).expect("projection");
    let (code, report) = check(Checker::Symbols, &evidence);
    assert_eq!(code, 1, "{report}");
    let mut unreachable = found(&report, "no_observed_unreached_symbols");
    unreachable.sort();
    let mut expected = vec![
        format!("S={}", common::dead_fn_id()),
        format!("S={}", common::probe_target_id()),
        format!("S={}", common::unused_id()),
    ];
    expected.sort();
    assert_eq!(unreachable, expected);

    let evidence = project(
        &snapshot,
        &Options {
            public_api: vec!["rust:src/core.rs#dead_fn".into()],
            ..options(Checker::Symbols)
        },
    )
    .expect("projection");
    let (_, report) = check(Checker::Symbols, &evidence);
    assert_eq!(
        found(&report, "no_observed_unreached_symbols").len(),
        2,
        "{report}"
    );
}

#[test]
fn the_hotspots_checker_names_the_entry_point_over_the_cutoff_and_passes_under_it() {
    let evidence = project(
        &common::measured_rust_snapshot(12),
        &options(Checker::Hotspots),
    )
    .expect("projection");
    let (code, report) = check(Checker::Hotspots, &evidence);
    assert_eq!(code, 1, "{report}");
    assert_eq!(
        found(&report, "no_hotspot_entry_points"),
        vec![format!("S={}", common::main_id())]
    );

    let evidence = project(
        &common::measured_rust_snapshot(9),
        &options(Checker::Hotspots),
    )
    .expect("projection");
    let (code, report) = check(Checker::Hotspots, &evidence);
    assert_eq!(code, 0, "main at cc 9 is under the cutoff:\n{report}");

    let evidence = project(
        &common::measured_ruby_snapshot(12, 30),
        &options(Checker::Hotspots),
    )
    .expect("projection");
    let (code, report) = check(Checker::Hotspots, &evidence);
    assert_eq!(code, 1, "{report}");
    assert_eq!(
        found(&report, "no_hotspot_entry_points"),
        vec![format!("S={}", common::cards_index_id())],
        "the action is the finding; the model method at cc 30 is not an entry point"
    );
}

#[test]
fn a_clean_codebase_passes_every_claim() {
    // Declare the dead file and the test-only file as test code by renaming
    // nothing: instead assert the claims over a snapshot whose only
    // production files are the live chain.
    let mut snapshot = common::rust_snapshot(false);
    snapshot.nodes.retain(|node| {
        !["src/dead.rs", "src/probe.rs", "tests/it.rs"].contains(&node.provenance.rel_path.as_str())
    });
    snapshot
        .edges
        .retain(|edge| !edge.provenance.rel_path.starts_with("tests/"));
    snapshot.files.retain(|file| {
        !["src/dead.rs", "src/probe.rs", "tests/it.rs"].contains(&file.rel_path.as_str())
    });
    snapshot
        .call_sites
        .retain(|site| !site.rel_path.starts_with("tests/"));
    snapshot.report = common::report_for(&snapshot);
    let evidence = project(
        &snapshot,
        &Options {
            core_files: vec!["src/core.rs".into()],
            ..options(Checker::Architecture)
        },
    )
    .expect("projection");
    let (code, report) = check(Checker::Architecture, &evidence);
    assert_eq!(code, 0, "{report}");
}

#[test]
fn absence_requires_positive_admission_even_when_the_census_is_empty() {
    let mut snapshot = common::rust_snapshot(false);
    snapshot.call_sites.clear();
    snapshot.report = common::report_for(&snapshot);
    for checker in [Checker::Architecture, Checker::Symbols] {
        let evidence = project(&snapshot, &options(checker)).expect("projection");
        assert!(
            evidence.contains("expect coverage_is_admitted {"),
            "{evidence}"
        );
        assert!(
            evidence.contains("Resolved sites / all recognized sites: 0/0"),
            "{evidence}"
        );
        assert!(
            evidence.contains("Unsupported observations: unknown"),
            "{evidence}"
        );
    }
}

#[test]
fn census_counts_sites_including_external_targets_and_is_never_a_completeness_claim() {
    use codemap_graph::schema::CallSiteCategory;
    let mut snapshot = common::dispatcher_snapshot();
    for category in [
        CallSiteCategory::Candidate,
        CallSiteCategory::Ambiguous,
        CallSiteCategory::ExternalCrate,
        CallSiteCategory::StandardLibrary,
    ] {
        let mut site = snapshot.call_sites[1].clone();
        site.category = category;
        snapshot.call_sites.push(site);
    }
    snapshot.report = common::report_for(&snapshot);
    let coverage = codemap_lemmaspec::coverage::Coverage::from_snapshot(&snapshot);
    assert_eq!(
        (coverage.resolved, coverage.recognized, coverage.per_mille),
        (1, 6, Some(166))
    );
    assert!(!coverage.admitted(167));
    assert!(coverage.admitted(166));
    for checker in [Checker::Architecture, Checker::Symbols, Checker::Hotspots] {
        let source = if checker == Checker::Hotspots {
            common::measured_rust_snapshot(9)
        } else {
            snapshot.clone()
        };
        let evidence = project(&source, &options(checker)).expect("projection");
        assert_eq!(fact_lines(&evidence, "call_census").len(), 8);
        assert_eq!(fact_lines(&evidence, "snapshot_identity").len(), 1);
        assert!(evidence.contains(&source.identity.snapshot_id));
        assert!(evidence.contains("Scope file: \"src/main.rs\""));
        assert!(evidence.contains("never a proof of extraction completeness"));
        assert!(evidence.contains("Unsupported observations: unknown"));
    }
}

#[test]
fn unresolved_dispatcher_and_missing_or_rejected_admission_never_pass_absence_checks() {
    for checker in [Checker::Architecture, Checker::Symbols] {
        let snapshot = common::dispatcher_snapshot();
        let evidence = project(&snapshot, &options(checker)).expect("projection");
        let absence = if checker == Checker::Architecture {
            "no_observed_unreferenced_production_files"
        } else {
            "no_observed_unreached_symbols"
        };
        let assert_inconclusive = |evidence: &str| {
            let (code, report) = check(checker, evidence);
            assert_ne!(code, 0, "{report}");
            assert_eq!(report["status"], "incomplete", "{report}");
            assert!(
                found(&report, "coverage_is_admitted").is_empty(),
                "{report}"
            );
            assert!(
                found(&report, absence).is_empty(),
                "absence must be guarded: {report}"
            );
        };
        assert_inconclusive(&evidence);
        let missing = evidence
            .lines()
            .filter(|line| !line.contains("relation: coverage_input "))
            .collect::<Vec<_>>()
            .join("\n");
        assert_inconclusive(&missing);

        let mut empty = snapshot.clone();
        empty.call_sites.clear();
        empty.report = common::report_for(&empty);
        assert_inconclusive(
            &project(
                &empty,
                &Options {
                    min_call_coverage_per_mille: 0,
                    ..options(checker)
                },
            )
            .expect("empty census"),
        );

        let mut zero = snapshot.clone();
        zero.call_sites
            .retain(|site| site.category != codemap_graph::schema::CallSiteCategory::Resolved);
        zero.report = common::report_for(&zero);
        assert_inconclusive(&project(&zero, &options(checker)).expect("zero resolution"));

        let mut parse_error = snapshot.clone();
        parse_error.report.parse_errors = 1;
        assert_inconclusive(
            &project(
                &parse_error,
                &Options {
                    min_call_coverage_per_mille: 500,
                    ..options(checker)
                },
            )
            .expect("parse errors"),
        );

        let mut syntax = common::measured_ruby_snapshot(9, 3);
        let node = &syntax.nodes[1];
        syntax.call_sites.push(codemap_graph::schema::CallSite {
            rel_path: node.provenance.rel_path.clone(),
            span: node.provenance.span,
            raw_name: "callback".into(),
            category: codemap_graph::schema::CallSiteCategory::Observed,
        });
        syntax.report = common::report_for(&syntax);
        assert_inconclusive(
            &project(
                &syntax,
                &Options {
                    min_call_coverage_per_mille: 0,
                    ..options(checker)
                },
            )
            .expect("syntax"),
        );

        let admitted = project(
            &snapshot,
            &Options {
                min_call_coverage_per_mille: 500,
                ..options(checker)
            },
        )
        .expect("explicit lower threshold");
        let (code, report) = check(checker, &admitted);
        assert_ne!(
            code, 0,
            "observed missing path remains a conditional finding: {report}"
        );
        assert_eq!(found(&report, "coverage_is_admitted").len(), 1, "{report}");
        assert_eq!(found(&report, absence).len(), 1, "{report}");
        assert!(
            checker
                .source()
                .contains("no path in observed graph under entry-point policy {entry_policy}")
        );
    }
}

#[test]
fn nested_test_callers_remain_evidence_without_becoming_production_findings() {
    for path in ["src/service/tests/check.rs", "src/service/check_tests.rs"] {
        let snapshot = common::rust_snapshot_with_test_file(false, path);
        let evidence = project(&snapshot, &architecture_options()).unwrap();
        assert!(
            fact_lines(&evidence, "test_file")
                .iter()
                .any(|line| line.contains(path))
        );
        assert!(
            !fact_lines(&evidence, "production_file")
                .iter()
                .any(|line| line.contains(path))
        );
        let (_, report) = check(Checker::Architecture, &evidence);
        assert_eq!(
            found(&report, "no_observed_test_only_production_files"),
            vec!["F=src/probe.rs".to_string()]
        );
        let evidence = project(&snapshot, &options(Checker::Symbols)).unwrap();
        let (_, report) = check(Checker::Symbols, &evidence);
        assert!(
            !found(&report, "no_observed_unreached_symbols")
                .iter()
                .any(|row| row.contains(path))
        );
    }
}
