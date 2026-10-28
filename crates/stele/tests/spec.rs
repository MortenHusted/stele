#![allow(clippy::doc_markdown)] // Example tags name catalog IDs, not code.

use std::collections::BTreeSet;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// `stele spec`, run from the engine crate so its example paths resolve.
fn spec() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stele"));
    command
        .arg("spec")
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../lemmaspec"));
    command
}

#[test]
fn version_flags_report_the_package_version_globally() {
    for args in [
        &["--version"][..],
        &["-V"],
        &["walk", "--version"],
        &["syntax", "-V"],
    ] {
        let output = spec().args(args).output().expect("run lemmaspec");

        assert!(output.status.success(), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("lemmaspec {}\n", lemmaspec::VERSION),
            "{args:?}"
        );
        assert!(output.stderr.is_empty(), "{args:?}");
    }
}

/// intent: example clean_walk_exits_zero exemplifies exit_status_contract on cli
#[test]
fn walk_command_emits_machine_readable_report() {
    let output = spec()
        .args(["walk", "examples/release_readiness.stele", "--json"])
        .output()
        .expect("run lemmaspec");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["spec"], "release_readiness");
    assert_eq!(report["status"], "clean");
    assert_eq!(report["asserted"], 2);
    assert_eq!(report["derived"], 1);
}

#[test]
fn mutate_command_emits_machine_readable_report() {
    let output = spec()
        .args(["mutate", "examples/mutation_analysis.stele", "--json"])
        .output()
        .expect("run lemmaspec");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["spec"], "mutation_analysis");
    assert_eq!(report["status"], "clean");
    assert_eq!(report["summary"]["total"], 9);
    assert_eq!(report["summary"]["executed"], 9);
    assert_eq!(report["summary"]["killed"], 9);
    assert_eq!(report["summary"]["survived"], 0);
    assert_eq!(report["policies"][0]["status"], "clean");
}

#[test]
fn mutate_command_prints_named_condition_targets() {
    let source = r#"
spec named_condition_cli {
  relation source { args: [symbol] }
  relation allowed { args: [symbol] }
  relation result { args: [symbol] }
  fact source_release { relation: source args: [release] }
  fact source_draft { relation: source args: [draft] }
  fact allowed_release { relation: allowed args: [release] }
  rule derive_result {
    derive: "result(Item)"
    when: {
      source_exists: "source(Item)"
      item_is_allowed: "allowed(Item)"
    }
  }
  expect exactly_one_result { query: "result(Item)" count: 1 }
  mutation condition_coverage {
    operator: drop_condition
    except: ["derive_result.source_exists"]
    must_fail: exactly_one_result
  }
}
"#;
    let directory = temporary_directory("named-condition-output");
    let input = directory.join("named.lemmaspec");
    std::fs::write(&input, source).expect("write artifact");

    let output = spec()
        .args(["mutate", input.to_str().unwrap()])
        .output()
        .expect("run lemmaspec");
    std::fs::remove_dir_all(directory).expect("remove temporary directory");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("condition source_exists of rule derive_result: source(Item)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("condition item_is_allowed of rule derive_result: allowed(Item)"),
        "{stdout}"
    );
}

/// intent: example surviving_mutant_is_reported exemplifies rules_are_load_bearing on cli
#[test]
fn surviving_mutant_exits_one() {
    let source = r#"
spec surviving_mutant {
  relation pair { args: [symbol, symbol] }
  relation related { args: [symbol, symbol] }
  fact pair_ab { relation: pair args: [a, b] }
  rule relate {
    derive: "related(A, B)"
    when: ["pair(A, B)", "A \\= B"]
  }
  expect one_pair { query: "related(A, B)" count: 1 }
  mutation condition_coverage { operator: drop_condition }
}
"#;
    let directory = temporary_directory("mutation-survivor");
    let input = directory.join("survivor.lemmaspec");
    std::fs::write(&input, source).expect("write artifact");

    let output = spec()
        .args(["mutate", input.to_str().unwrap(), "--json"])
        .output()
        .expect("run lemmaspec");
    std::fs::remove_dir_all(directory).expect("remove temporary directory");

    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["status"], "survived");
    assert_eq!(report["summary"]["survived"], 1);
    assert_eq!(report["summary"]["rejected"], 1);
}

#[test]
fn vacuous_mutation_policy_exits_one() {
    let source = r#"
spec vacuous_mutation {
  relation input { args: [symbol] }
  relation output { args: [symbol] }
  fact input_exists { relation: input args: [value] }
  rule derive_output {
    derive: "output(Value)"
    when: ["input(Value)"]
  }
  expect output_exists { query: "output(value)" count: 1 }
  mutation condition_coverage { operator: drop_condition }
}
"#;
    let directory = temporary_directory("mutation-vacuity");
    let input = directory.join("vacuous.lemmaspec");
    std::fs::write(&input, source).expect("write artifact");

    let output = spec()
        .args(["mutate", input.to_str().unwrap(), "--json"])
        .output()
        .expect("run lemmaspec");
    std::fs::remove_dir_all(directory).expect("remove temporary directory");

    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["status"], "vacuous");
    assert_eq!(report["summary"]["executed"], 0);
    assert_eq!(report["policies"][0]["status"], "vacuous");
}

#[test]
fn project_command_emits_a_closed_native_graph_shape() {
    let output = spec()
        .args(["project", "examples/release_readiness.stele", "--json"])
        .output()
        .expect("run lemmaspec");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let graph: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON graph");
    let nodes = graph["nodes"].as_array().expect("nodes");
    let edges = graph["edges"].as_array().expect("edges");
    let node_ids: BTreeSet<_> = nodes
        .iter()
        .filter_map(|node| node["id"].as_str())
        .collect();

    assert!(nodes.iter().all(|node| node["kind"] == "node"));
    assert!(edges.iter().all(|edge| {
        edge["kind"] == "edge"
            && edge["from"]
                .as_str()
                .is_some_and(|id| node_ids.contains(id))
            && edge["to"].as_str().is_some_and(|id| node_ids.contains(id))
    }));
}

#[test]
fn help_describes_machine_output_and_exit_statuses() {
    let output = spec().arg("--help").output().expect("run lemmaspec");

    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("syntax"), "{help}");
    assert!(
        help.contains("machine-readable JSON for walk, mutate, check, or project"),
        "{help}"
    );
    assert!(
        help.contains("1  Artifact valid, but an expectation or mutation policy failed"),
        "{help}"
    );
    assert!(
        help.contains("2  Usage, read, parse, validation, or evaluation error"),
        "{help}"
    );
}

#[test]
fn syntax_command_prints_the_supported_artifact_language() {
    let output = spec().arg("syntax").output().expect("run lemmaspec");

    assert!(output.status.success());
    let syntax = String::from_utf8_lossy(&output.stdout);
    assert!(syntax.contains("spec NAME"), "{syntax}");
    assert!(syntax.contains("relation NAME"), "{syntax}");
    assert!(syntax.contains("mutation ID"), "{syntax}");
    assert!(syntax.contains("drop_condition"), "{syntax}");
    assert!(syntax.contains("named condition map"), "{syntax}");
    assert!(syntax.contains("quoted rule.condition ids"), "{syntax}");
    assert!(syntax.contains("count(X)"), "{syntax}");
    assert!(syntax.contains("sum and now are unavailable"), "{syntax}");
    assert!(
        syntax
            .lines()
            .any(|line| line.ends_with("and <, =<, >, >=, =, \\=")),
        "{syntax}"
    );
}

#[test]
fn render_command_writes_html_beside_the_artifact_by_default() {
    let directory = temporary_directory("render-clean");
    let input = directory.join("release.lemmaspec");
    let output = directory.join("release.html");
    std::fs::write(
        &input,
        include_str!("../../lemmaspec/examples/release_readiness.stele"),
    )
    .expect("write artifact");

    let result = spec()
        .args(["render", input.to_str().unwrap()])
        .output()
        .expect("run lemmaspec");

    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(output.is_file());
    assert!(String::from_utf8_lossy(&result.stdout).contains(output.to_str().unwrap()));
    let html = std::fs::read_to_string(output).expect("read HTML");
    assert!(
        html.starts_with("<!doctype html>\n<html lang=\"en\" data-status=\"clean\">"),
        "{html}"
    );
    std::fs::remove_dir_all(directory).expect("remove temporary directory");
}

#[test]
fn incomplete_render_writes_html_and_exits_one() {
    let directory = temporary_directory("render-incomplete");
    let input = directory.join("release.lemmaspec");
    let output = directory.join("report.html");
    let source = include_str!("../../lemmaspec/examples/release_readiness.stele")
        .replace("count: 1", "count: 0");
    std::fs::write(&input, source).expect("write artifact");

    let result = spec()
        .args([
            "render",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("run lemmaspec");

    assert_eq!(result.status.code(), Some(1));
    let html = std::fs::read_to_string(&output).expect("read HTML");
    assert!(
        html.starts_with("<!doctype html>\n<html lang=\"en\" data-status=\"incomplete\">"),
        "{html}"
    );
    assert!(html.contains("1 expectation needs attention"), "{html}");
    assert!(html.contains("data-status=\"failed\""), "{html}");
    std::fs::remove_dir_all(directory).expect("remove temporary directory");
}

#[test]
fn invalid_render_does_not_write_html() {
    let directory = temporary_directory("render-invalid");
    let input = directory.join("invalid.lemmaspec");
    let output = directory.join("invalid.html");
    std::fs::write(&input, "spec invalid { fact }").expect("write invalid artifact");

    let result = spec()
        .args(["render", input.to_str().unwrap()])
        .output()
        .expect("run lemmaspec");

    assert_eq!(result.status.code(), Some(2));
    assert!(!output.exists());
    let stderr = String::from_utf8_lossy(&result.stderr);
    let prefix = format!("stele spec: render `{}`: ", input.display());
    let diagnostic = stderr
        .strip_prefix(&prefix)
        .expect("render-specific diagnostic prefix");
    assert!(diagnostic.contains("expected block name"), "{diagnostic}");
    std::fs::remove_dir_all(directory).expect("remove temporary directory");
}

#[test]
fn render_rejects_invalid_cli_shapes_without_writing_output() {
    let directory = temporary_directory("render-cli-errors");
    let input = directory.join("release.lemmaspec");
    let wrong_extension = directory.join("release.txt");
    std::fs::write(
        &input,
        include_str!("../../lemmaspec/examples/release_readiness.stele"),
    )
    .expect("write artifact");
    std::fs::write(&wrong_extension, "not a LemmaSpec artifact").expect("write text input");

    let default_output = directory.join("release.html");
    let first_output = directory.join("first.html");
    let second_output = directory.join("second.html");
    let non_html_output = directory.join("report.txt");
    let cases = [
        (
            "missing input",
            vec!["render".to_string()],
            "render requires a .stele path",
            vec![],
        ),
        (
            "non-.lemmaspec input",
            vec!["render".to_string(), wrong_extension.display().to_string()],
            "render expects a .stele file",
            vec![default_output.clone()],
        ),
        (
            "missing output value",
            vec![
                "render".to_string(),
                input.display().to_string(),
                "--output".to_string(),
            ],
            "--output requires a path",
            vec![default_output.clone()],
        ),
        (
            "duplicate output",
            vec![
                "render".to_string(),
                input.display().to_string(),
                "--output".to_string(),
                first_output.display().to_string(),
                "-o".to_string(),
                second_output.display().to_string(),
            ],
            "render accepts only one output path",
            vec![first_output.clone(), second_output.clone()],
        ),
        (
            "unexpected argument",
            vec![
                "render".to_string(),
                input.display().to_string(),
                "unexpected".to_string(),
            ],
            "unexpected argument `unexpected`",
            vec![default_output],
        ),
        (
            "non-.html output",
            vec![
                "render".to_string(),
                input.display().to_string(),
                "--output".to_string(),
                non_html_output.display().to_string(),
            ],
            "render output must be an .html file",
            vec![non_html_output.clone()],
        ),
    ];

    for (label, args, expected_error, output_paths) in cases {
        let result = spec().args(args).output().expect("run lemmaspec");

        assert_eq!(result.status.code(), Some(2), "{label}");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.starts_with("stele spec: ") && stderr.contains(expected_error),
            "{label}: {stderr}"
        );
        for output_path in output_paths {
            assert!(!output_path.exists(), "{label}: {}", output_path.display());
        }
    }

    std::fs::remove_dir_all(directory).expect("remove temporary directory");
}

#[test]
fn render_output_is_byte_stable_across_processes() {
    let directory = temporary_directory("render-stable");
    let input = directory.join("release.lemmaspec");
    let first = directory.join("first.html");
    let second = directory.join("second.html");
    std::fs::write(
        &input,
        include_str!("../../lemmaspec/examples/release_readiness.stele"),
    )
    .expect("write artifact");

    for output in [&first, &second] {
        let result = spec()
            .args([
                "render",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ])
            .output()
            .expect("run lemmaspec");
        assert!(result.status.success());
    }

    assert_eq!(
        std::fs::read(&first).expect("read first HTML"),
        std::fs::read(&second).expect("read second HTML")
    );
    std::fs::remove_dir_all(directory).expect("remove temporary directory");
}

#[test]
fn bare_invocation_prints_help_and_fails() {
    let output = spec().output().expect("run lemmaspec");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
}

/// intent: example open_walk_is_incomplete exemplifies open_claim_is_reported on cli
#[test]
fn incomplete_walk_exits_one_and_reports_failure() {
    let source = include_str!("../../lemmaspec/examples/release_readiness.stele")
        .replace("count: 1", "count: 0");
    let path = std::env::temp_dir().join(format!(
        "lemmaspec-incomplete-{}.lemmaspec",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary artifact");

    let output = spec()
        .args(["walk", path.to_str().unwrap(), "--json"])
        .output()
        .expect("run lemmaspec");
    std::fs::remove_file(&path).expect("remove temporary artifact");

    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["status"], "incomplete");
    assert_eq!(report["expectations"][0]["satisfied"], false);
}

#[test]
fn check_command_evaluates_evidence_with_the_checker_rules() {
    let evidence = r#"
        spec schema_under_review {
          fact flag { relation: boolean_column args: [webhook, active] provenance: ["db/schema.rb"] }
          fact companion { relation: companion_column args: [webhook, active, first_failure_at] }
          expect nothing_is_flattened { query: "violation(M, C)" count: 0 }
        }
    "#;
    let path = std::env::temp_dir().join(format!(
        "lemmaspec-evidence-{}.lemmaspec",
        std::process::id()
    ));
    std::fs::write(&path, evidence).expect("write temporary evidence");

    let output = spec()
        .args([
            "check",
            "examples/state_as_records.stele",
            path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run lemmaspec");
    std::fs::remove_file(&path).expect("remove temporary evidence");

    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["spec"], "schema_under_review");
    assert_eq!(report["status"], "incomplete");
    assert_eq!(report["asserted"], 2);
    assert_eq!(
        report["expectations"]
            .as_array()
            .expect("expectations")
            .len(),
        1
    );
    assert_eq!(
        report["expectations"][0]["found"],
        serde_json::json!(["M=webhook, C=active"])
    );
    let violation = report["facts"]
        .as_array()
        .expect("facts")
        .iter()
        .find(|fact| fact["relation"] == "violation")
        .expect("the flattened flag is a violation");
    assert!(
        violation["why"]
            .as_str()
            .expect("why")
            .contains("db/schema.rb")
    );
}

#[test]
fn bind_command_writes_an_artifact_that_walks_like_the_check() {
    let evidence = r#"
        // Is the real schema clean?
        spec schema_under_review {
          fact flag { relation: boolean_column args: [webhook, active] provenance: ["db/schema.rb"] }
          fact companion { relation: companion_column args: [webhook, active, first_failure_at] }
          expect nothing_is_flattened { query: "violation(M, C)" count: 0 }
        }
    "#;
    let directory = temporary_directory("bind");
    let evidence_path = directory.join("evidence.lemmaspec");
    std::fs::write(&evidence_path, evidence).expect("write temporary evidence");

    let bind = spec()
        .args([
            "bind",
            "examples/state_as_records.stele",
            evidence_path.to_str().unwrap(),
        ])
        .output()
        .expect("run lemmaspec");
    assert!(
        bind.status.success(),
        "{}",
        String::from_utf8_lossy(&bind.stderr)
    );
    let bound_path = directory.join("evidence.bound.stele");
    assert_eq!(
        String::from_utf8_lossy(&bind.stdout),
        format!("wrote {}\n", bound_path.display())
    );
    let bound = std::fs::read_to_string(&bound_path).expect("read bound artifact");
    assert!(
        bound.starts_with(
            "// Bound from state_as_records over schema_under_review.\n//\n// Is the real schema clean?\n"
        ),
        "{bound}"
    );

    let walk = spec()
        .args(["walk", bound_path.to_str().unwrap(), "--json"])
        .output()
        .expect("run lemmaspec");
    let check = spec()
        .args([
            "check",
            "examples/state_as_records.stele",
            evidence_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run lemmaspec");
    std::fs::remove_dir_all(&directory).expect("clean up temporary directory");

    assert_eq!(walk.status.code(), Some(1));
    assert_eq!(check.status.code(), Some(1));
    assert_eq!(
        walk.stdout, check.stdout,
        "walk of the bound artifact differs from check"
    );
}

#[test]
fn bind_command_rejects_evidence_that_redefines_the_checker() {
    let directory = temporary_directory("bind-rule");
    let evidence_path = directory.join("evidence.lemmaspec");
    std::fs::write(
        &evidence_path,
        r#"spec e { rule r { derive: "violation(M, C)" when: ["boolean_column(M, C)"] } }"#,
    )
    .expect("write temporary evidence");

    let output = spec()
        .args([
            "bind",
            "examples/state_as_records.stele",
            evidence_path.to_str().unwrap(),
        ])
        .output()
        .expect("run lemmaspec");
    let bound_exists = directory.join("evidence.bound.stele").exists();
    std::fs::remove_dir_all(&directory).expect("clean up temporary directory");

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("declares a rule"));
    assert!(!bound_exists, "no bound artifact is written on failure");
}

#[test]
fn bind_command_requires_two_artifact_paths_and_protects_its_inputs() {
    let output = spec()
        .args(["bind", "examples/state_as_records.stele"])
        .output()
        .expect("run lemmaspec");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("checker and an evidence"));

    let output = spec()
        .args([
            "bind",
            "examples/state_as_records.stele",
            "examples/release_readiness.stele",
            "-o",
            "examples/release_readiness.stele",
        ])
        .output()
        .expect("run lemmaspec");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("refuses to overwrite"));
}

#[test]
fn check_command_rejects_evidence_that_redefines_the_checker() {
    let path = std::env::temp_dir().join(format!(
        "lemmaspec-evidence-rule-{}.lemmaspec",
        std::process::id()
    ));
    std::fs::write(
        &path,
        r#"spec e { rule r { derive: "violation(M, C)" when: ["boolean_column(M, C)"] } }"#,
    )
    .expect("write temporary evidence");

    let output = spec()
        .args([
            "check",
            "examples/state_as_records.stele",
            path.to_str().unwrap(),
        ])
        .output()
        .expect("run lemmaspec");
    std::fs::remove_file(&path).expect("remove temporary evidence");

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("declares a rule"));
}

#[test]
fn check_command_requires_two_artifact_paths() {
    let output = spec()
        .args(["check", "examples/state_as_records.stele"])
        .output()
        .expect("run lemmaspec");

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("checker and an evidence"));
}

#[test]
fn incomplete_project_exits_one_and_omits_proof_edges() {
    let source = include_str!("../../lemmaspec/examples/release_readiness.stele")
        .replace("count: 1", "count: 0");
    let path = std::env::temp_dir().join(format!(
        "lemmaspec-incomplete-project-{}.lemmaspec",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary artifact");

    let output = spec()
        .args(["project", path.to_str().unwrap(), "--json"])
        .output()
        .expect("run lemmaspec");
    std::fs::remove_file(&path).expect("remove temporary artifact");

    assert_eq!(output.status.code(), Some(1));
    let graph: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON graph");
    assert_eq!(graph["status"], "incomplete");
    assert!(
        graph["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .any(|node| { node["type"] == "expectation" && node["satisfied"] == false })
    );
    assert!(
        graph["edges"]
            .as_array()
            .expect("edges")
            .iter()
            .all(|edge| edge["rel"] != "proves")
    );
}

/// intent: example missing_spec_exits_two exemplifies exit_status_contract on cli
#[test]
fn missing_artifact_exits_two() {
    let output = spec()
        .args(["walk", "does-not-exist.lemmaspec"])
        .output()
        .expect("run lemmaspec");

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("read `does-not-exist.lemmaspec`"));
}

fn temporary_directory(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("lemmaspec-{label}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create temporary directory");
    path
}

#[test]
fn a_lemmaspec_file_from_before_stele_still_walks() {
    let directory = temporary_directory("legacy-extension");
    let input = directory.join("release_readiness.lemmaspec");
    std::fs::write(
        &input,
        include_str!("../../lemmaspec/examples/release_readiness.stele"),
    )
    .expect("write legacy artifact");
    let output = spec()
        .args(["walk", input.to_str().unwrap()])
        .output()
        .expect("walk legacy artifact");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    std::fs::remove_dir_all(directory).expect("remove temporary directory");
}
