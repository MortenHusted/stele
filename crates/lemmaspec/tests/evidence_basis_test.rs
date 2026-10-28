use lemmaspec::{parse_artifact, print_artifact, project_artifact, walk_artifact};

#[test]
fn evidence_basis_is_presentation_metadata_and_round_trips() {
    let plain = "spec demo { relation item { args: [symbol] } fact a { relation: item args: [sample] } expect one { query: \"item(X)\" count: 1 } }";
    let typed = plain.replace(
        "args: [sample]",
        "args: [sample] basis: observed source: \"probe:gate\" identity: \"sha256:sample\"",
    );
    assert_eq!(
        serde_json::to_value(walk_artifact(plain).unwrap()).unwrap(),
        serde_json::to_value(walk_artifact(&typed).unwrap()).unwrap()
    );
    let parsed = parse_artifact(&typed).unwrap();
    assert_eq!(parse_artifact(&print_artifact(&parsed)).unwrap(), parsed);
    let projected = serde_json::to_value(project_artifact(&typed).unwrap()).unwrap();
    let fact = projected["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["type"] == "fact")
        .unwrap();
    assert_eq!(fact["bases"][0]["kind"], "observed");
    assert_eq!(fact["bases"][0]["identity"], "sha256:sample");
}

#[test]
fn evidence_basis_cannot_hide_an_untyped_duplicate_declaration() {
    let source = r#"spec demo {
      relation item { args: [symbol] }
      fact a { relation: item args: [sample] basis: snapshot source: "src/sample.rs#L1" identity: "tree:sample" }
      fact b { relation: item args: [sample] provenance: ["snapshot:claimed"] }
    }"#;
    let projected = serde_json::to_value(project_artifact(source).unwrap()).unwrap();
    let fact = projected["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["type"] == "fact")
        .unwrap();
    let bases = fact["bases"].as_array().unwrap();
    assert!(bases.iter().any(|b| b["kind"] == "reviewer_declared"));
    assert!(bases.iter().any(|b| b["kind"] == "snapshot"));
}

#[test]
fn evidence_basis_requires_source_and_observation_identity() {
    for fields in [
        "basis: invented",
        "basis: snapshot source: \"src/a.rs\"",
        "basis: observed source: \"probe:a\"",
        "basis: policy",
        "source: \"src/a.rs\"",
        "identity: \"tree:sample\"",
    ] {
        let source = format!("spec demo {{ relation item {{ args: [symbol] }} fact a {{ relation: item args: [sample] {fields} }} }}");
        assert!(parse_artifact(&source).is_err(), "accepted {fields}");
    }
}
