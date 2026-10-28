use std::collections::BTreeSet;

use lemmaspec::{project_artifact, GraphNodeData};

const EXAMPLE: &str = include_str!("../examples/release_readiness.stele");

#[test]
fn projects_the_complete_internal_graph() {
    let projection = project_artifact(EXAMPLE).expect("project example");

    projection
        .validate_closed()
        .expect("projected graph must be internally closed");

    let relations: BTreeSet<_> = projection
        .edges
        .iter()
        .map(|edge| edge.rel.as_str())
        .collect();
    assert_eq!(
        relations,
        BTreeSet::from([
            "asserts",
            "depends_on",
            "derives",
            "expects",
            "proves",
            "references_symbol",
        ])
    );

    let node_ids: BTreeSet<_> = projection
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect();
    assert!(projection.edges.iter().all(|edge| {
        node_ids.contains(edge.from.as_str()) && node_ids.contains(edge.to.as_str())
    }));
    assert!(projection
        .nodes
        .iter()
        .all(|node| node.id.starts_with("lemmaspec:release_readiness:")));
    assert!(projection
        .edges
        .iter()
        .all(|edge| edge.id.starts_with("lemmaspec:release_readiness:")));

    assert!(projection.edges.iter().any(|edge| {
        edge.rel == "depends_on"
            && edge.basis.as_deref() == Some("proof_witness")
            && projection
                .nodes
                .iter()
                .find(|node| node.id == edge.from)
                .is_some_and(|node| node.node_type() == "fact")
    }));
    assert_eq!(
        projection
            .edges
            .iter()
            .filter(|edge| {
                edge.rel == "depends_on" && edge.basis.as_deref() == Some("proof_witness")
            })
            .count(),
        2,
        "one deterministic witness contributes the two concrete prerequisites"
    );
    assert!(projection.edges.iter().any(|edge| {
        edge.rel == "references_symbol"
            && projection
                .nodes
                .iter()
                .find(|node| node.id == edge.to)
                .is_some_and(|node| node.node_type() == "symbol")
    }));

    let serialized = serde_json::to_value(&projection).expect("serialize graph");
    assert!(serialized["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .filter(|node| node["type"] == "fact")
        .all(|node| node.get("why").is_none()));
}

#[test]
fn rejects_an_internal_edge_with_the_wrong_endpoint_types() {
    let mut projection = project_artifact(EXAMPLE).expect("project example");
    let relation_id = projection
        .nodes
        .iter()
        .find(|node| node.node_type() == "relation")
        .expect("relation node")
        .id
        .clone();
    let edge = projection
        .edges
        .iter_mut()
        .find(|edge| edge.rel == "asserts")
        .expect("asserts edge");
    edge.from = relation_id;

    let error = projection
        .validate_closed()
        .expect_err("edge relations have typed endpoint constraints");
    assert!(
        error.to_string().contains("invalid `asserts` edge"),
        "{error}"
    );
}

#[test]
fn rejects_an_edge_that_escapes_the_projection() {
    let mut projection = project_artifact(EXAMPLE).expect("project example");
    projection.edges[0].to = "external_graph:external:node".to_string();

    let error = projection
        .validate_closed()
        .expect_err("external endpoints require a later adapter");
    assert!(error.to_string().contains("missing target node"), "{error}");
}

#[test]
fn repeated_projection_is_byte_stable() {
    let first = serde_json::to_vec_pretty(&project_artifact(EXAMPLE).unwrap()).unwrap();
    let second = serde_json::to_vec_pretty(&project_artifact(EXAMPLE).unwrap()).unwrap();

    assert_eq!(first, second);
}

#[test]
fn projects_mutation_targets_and_named_oracles() {
    let source = r#"
spec mutation_projection {
  relation source { args: [symbol] }
  relation result { args: [symbol] }
  fact source_item { relation: source args: [item] }
  rule derive_result {
    derive: "result(Item)"
    when: { source_exists: "source(Item)" }
  }
  expect result_exists { query: "result(item)" count: 1 }
  mutation rule_coverage {
    operator: drop_rule
    must_fail: result_exists
  }
}
"#;
    let projection = project_artifact(source).expect("project mutation policy");
    let mutation_id = projection
        .nodes
        .iter()
        .find(|node| node.node_type() == "mutation")
        .expect("mutation node")
        .id
        .as_str();
    let rule_id = projection
        .nodes
        .iter()
        .find(|node| node.node_type() == "rule")
        .expect("rule node")
        .id
        .as_str();
    let condition_ids = projection
        .nodes
        .iter()
        .find_map(|node| match &node.data {
            GraphNodeData::Rule {
                name,
                condition_ids,
                ..
            } if name == "derive_result" => Some(condition_ids.as_slice()),
            _ => None,
        })
        .expect("named rule conditions");
    let expectation_id = projection
        .nodes
        .iter()
        .find(|node| node.node_type() == "expectation")
        .expect("expectation node")
        .id
        .as_str();

    assert!(projection
        .edges
        .iter()
        .any(|edge| { edge.rel == "targets" && edge.from == mutation_id && edge.to == rule_id }));
    assert!(projection.edges.iter().any(|edge| {
        edge.rel == "must_fail" && edge.from == mutation_id && edge.to == expectation_id
    }));
    assert_eq!(condition_ids, ["source_exists"]);
    projection.validate_closed().expect("closed mutation graph");
}

#[test]
fn projects_symbol_constants_from_rule_comparisons() {
    let source = r#"
spec comparison_symbols {
  relation named { args: [symbol] }
  relation selected { args: [symbol] }
  fact release_name { relation: named args: [release] }
  rule choose {
    derive: "selected(Name)"
    when: ["named(Name)", "Name = release"]
  }
}
"#;
    let projection = project_artifact(source).expect("project comparison");
    let rule_id = projection
        .nodes
        .iter()
        .find(|node| node.node_type() == "rule")
        .expect("rule node")
        .id
        .as_str();
    let release_symbol_id = projection
        .nodes
        .iter()
        .find(|node| {
            matches!(
                &node.data,
                lemmaspec::GraphNodeData::Symbol { value, .. } if value == "release"
            )
        })
        .expect("release symbol")
        .id
        .as_str();

    assert!(projection.edges.iter().any(|edge| {
        edge.rel == "references_symbol"
            && edge.basis.as_deref() == Some("rule_comparison")
            && edge.from == rule_id
            && edge.to == release_symbol_id
    }));
}

#[test]
fn projects_every_distinct_rule_that_produces_a_fact() {
    let source = r#"
spec multiple_producers {
  relation source { args: [symbol] }
  relation result { args: [symbol] }
  fact source_item { relation: source args: [item] }
  rule one { derive: "result(Item)" when: ["source(Item)"] }
  rule two { derive: "result(Item)" when: ["source(Item)"] }
  rule three { derive: "result(Item)" when: ["source(Item)"] }
  rule four { derive: "result(Item)" when: ["source(Item)"] }
  rule five { derive: "result(Item)" when: ["source(Item)"] }
}
"#;
    let projection = project_artifact(source).expect("project producers");
    let producers: BTreeSet<_> = projection
        .edges
        .iter()
        .filter(|edge| edge.rel == "derives" && edge.basis.as_deref() == Some("proof_witness"))
        .filter_map(|edge| edge.witness.as_deref())
        .collect();

    assert_eq!(
        producers,
        BTreeSet::from(["five", "four", "one", "three", "two"])
    );
}

#[test]
fn aggregate_proofs_resolve_to_projected_facts_and_link_constant_symbols() {
    let source = r#"
spec aggregate_projection {
  relation bought { args: [symbol, symbol] }
  relation kit_count { args: [symbol, integer] }
  fact bought_spitfire { relation: bought args: [alice, spitfire] }
  fact bought_hurricane { relation: bought args: [alice, hurricane] }
  rule count_spitfires {
    derive: "kit_count(Person, count(spitfire))"
    when: ["bought(Person, Kit)"]
  }
  expect one_spitfire { query: "kit_count(alice, 1)" count: 1 }
}
"#;
    let projection = project_artifact(source).expect("project aggregate");
    projection
        .validate_closed()
        .expect("closed aggregate graph");

    let aggregate_fact_id = projection
        .nodes
        .iter()
        .find_map(|node| match &node.data {
            GraphNodeData::Fact { relation, .. } if relation == "kit_count" => {
                Some(node.id.as_str())
            }
            _ => None,
        })
        .expect("aggregate fact");
    let source_fact_ids: BTreeSet<_> = projection
        .nodes
        .iter()
        .filter_map(|node| match &node.data {
            GraphNodeData::Fact { relation, .. } if relation == "bought" => Some(node.id.as_str()),
            _ => None,
        })
        .collect();
    assert!(projection.edges.iter().any(|edge| {
        edge.rel == "depends_on"
            && edge.basis.as_deref() == Some("proof_witness")
            && edge.from == aggregate_fact_id
            && source_fact_ids.contains(edge.to.as_str())
    }));

    let rule_id = projection
        .nodes
        .iter()
        .find_map(|node| match &node.data {
            GraphNodeData::Rule { name, .. } if name == "count_spitfires" => Some(node.id.as_str()),
            _ => None,
        })
        .expect("aggregate rule");
    let symbol_id = projection
        .nodes
        .iter()
        .find_map(|node| match &node.data {
            GraphNodeData::Symbol { value, .. } if value == "spitfire" => Some(node.id.as_str()),
            _ => None,
        })
        .expect("aggregate symbol");
    assert!(projection.edges.iter().any(|edge| {
        edge.rel == "references_symbol"
            && edge.basis.as_deref() == Some("rule_head")
            && edge.position == Some(1)
            && edge.from == rule_id
            && edge.to == symbol_id
    }));

    let serialized = serde_json::to_string(&projection).expect("serialize aggregate graph");
    assert!(!serialized.contains("__agg"), "{serialized}");
}

#[test]
fn open_claims_link_to_the_facts_found_instead() {
    let source = r#"
spec open_claim {
  relation loose_end { args: [symbol] }
  fact one { relation: loose_end args: [popup] }
  fact two { relation: loose_end args: [prompt] }
  expect no_loose_ends { query: "loose_end(X)" count: 0 }
}
"#;
    let projection = project_artifact(source).expect("project open claim");
    projection.validate_closed().expect("closed");
    let matches: Vec<_> = projection
        .edges
        .iter()
        .filter(|edge| edge.rel == "matches")
        .collect();
    assert_eq!(matches.len(), 2);
    assert!(matches
        .iter()
        .all(|edge| edge.basis.as_deref() == Some("unsatisfied_query")
            && edge.to.ends_with(":expectation:no_loose_ends")));
    assert!(projection.edges.iter().all(|edge| edge.rel != "proves"));
}

#[test]
fn projects_authored_prose_roles_and_readings() {
    let source = r#"
// Which mutants does each policy govern?

spec prose {
  // Ties a mutant to the policy that judges it.
  relation governed_by {
    args: [symbol, symbol]
    roles: [mutation, policy]
    reads: "{mutation} is governed by {policy}"
  }
  // Recorded from the fixture manifest.
  fact m { relation: governed_by args: [rule_deletion_caught, rule_coverage] }
  expect one { query: "governed_by(M, P)" count: 1 }
}
"#;
    let projection = project_artifact(source).expect("project prose");
    let json = serde_json::to_value(&projection).expect("serialize");
    let nodes = json["nodes"].as_array().expect("nodes");
    let node = |kind: &str| {
        nodes
            .iter()
            .find(|node| node["type"] == kind)
            .unwrap_or_else(|| panic!("{kind} node"))
    };

    assert_eq!(
        node("spec")["doc"],
        "Which mutants does each policy govern?"
    );
    assert_eq!(
        node("relation")["roles"],
        serde_json::json!(["mutation", "policy"])
    );
    assert_eq!(
        node("relation")["reads"],
        "{mutation} is governed by {policy}"
    );
    assert_eq!(
        node("relation")["doc"],
        "Ties a mutant to the policy that judges it."
    );
    assert_eq!(
        node("fact")["reading"],
        "rule_deletion_caught is governed by rule_coverage"
    );
    assert_eq!(node("fact")["doc"], "Recorded from the fixture manifest.");
    assert!(node("expectation").get("doc").is_none());

    let bare = project_artifact(
        "spec bare { relation input { args: [symbol] } fact one { relation: input args: [a] } expect one_input { query: \"input(X)\" count: 1 } }",
    )
    .expect("project bare artifact");
    let bare = serde_json::to_value(&bare).expect("serialize");
    assert!(bare["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .filter(|node| node["type"] == "relation")
        .all(|node| node.get("roles").is_none() && node.get("reads").is_none()));
}
