use codemap_graph::schema::{
    Edge, EdgeKind, FileRecord, Node, NodeKind, TypeKind, edge_id, node_id, symbol_key,
};
use codemap_graph::{CallSiteCategory, Snapshot};
use codemap_topology::{Policy, Relation, Scope, project, render_html};

fn snapshot(language: &str) -> Snapshot {
    serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/tests/fixtures/{language}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn accepted_language_snapshots_preserve_only_proven_calls_and_annotations() {
    for language in ["rust", "ruby", "php", "go"] {
        let source = snapshot(language);
        let topology = project(&source, Scope::default()).unwrap();
        let value = serde_json::to_value(&topology).unwrap();
        assert_eq!(value["edges"].as_array().unwrap().len(), 1);
        assert_eq!(value["edges"][0]["kind"], "call");
        assert_eq!(
            value["edges"][0]["codemap"]["provenance"]["confidence"],
            255
        );
        assert_eq!(
            value["codemap"]["identity"],
            serde_json::to_value(&source.identity).unwrap()
        );
        assert_eq!(value["codemap"]["version"], "codemap.topology.v2");
        assert_eq!(value["deadEnds"], serde_json::json!([]));
        assert_eq!(value["entryPoints"], serde_json::json!([]));
        assert!(value["codemap"].get("excluded").is_none());
        assert!(value.get("flows").is_none());
        assert!(value.get("observations").is_none());
        let json = serde_json::to_string(&topology).unwrap();
        assert!(json.contains("Candidate"));
        assert!(!json.contains("\"dispatch\""));
        assert_eq!(
            json,
            serde_json::to_string(&project(&source, Scope::default()).unwrap()).unwrap()
        );
        let mut leaves = Vec::new();
        leaf_ids(&value["root"], &mut leaves);
        for edge in value["edges"].as_array().unwrap() {
            assert!(leaves.contains(&edge["source"].as_str().unwrap()));
            assert!(leaves.contains(&edge["target"].as_str().unwrap()));
        }
    }
}

fn leaf_ids<'a>(node: &'a serde_json::Value, ids: &mut Vec<&'a str>) {
    if let Some(children) = node["children"].as_array() {
        for child in children {
            leaf_ids(child, ids);
        }
    } else {
        ids.push(node["id"].as_str().unwrap());
    }
}

#[test]
fn malicious_source_names_cannot_close_the_data_script() {
    let mut source = snapshot("rust");
    source.nodes[1].name = "</script><script>alert('&')</script>".into();
    let topology = project(&source, Scope::default()).unwrap();
    let html = render_html(&topology).unwrap();
    assert!(!html.contains("</script><script>alert"));
    assert!(html.contains("\\u003c/script\\u003e"));
    assert!(html.contains("\\u0026"));
    assert_eq!(html, render_html(&topology).unwrap());
}

#[test]
fn malformed_snapshots_are_rejected_before_projection() {
    let mut source = snapshot("rust");
    source.call_sites[0].category = CallSiteCategory::Candidate;
    assert!(project(&source, Scope::default()).is_err());
}

fn fixture_value(language: &str, suffix: &str) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/tests/fixtures/{language}-{suffix}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap()
}

fn checked(
    source: &Snapshot,
    report: serde_json::Value,
    selection: serde_json::Value,
) -> anyhow::Result<codemap_topology::Topology> {
    project(
        source,
        Scope {
            selection: Some(&serde_json::from_value(selection)?),
            liveness: Some(&serde_json::from_value(report)?),
            ..Scope::default()
        },
    )
}

#[test]
fn admitted_incomplete_reports_have_qualified_witnesses_in_all_accepted_languages() {
    for language in ["rust", "ruby", "php", "go"] {
        let report = fixture_value(language, "check");
        assert_eq!(report["status"], "incomplete");
        let topology = checked(
            &snapshot(language),
            report,
            fixture_value(language, "selection"),
        )
        .unwrap();
        assert_eq!(topology.dead_ends.len(), 1);
        let absence = &topology.codemap.absence[&topology.dead_ends[0]];
        assert!(
            absence
                .qualification
                .starts_with("no path in the observed graph under entry-point policy ")
        );
        assert!(absence.witness.contains("shipped_without_observed_reach"));
        assert!(absence.admission_witness.contains("coverage_input("));
    }
}

#[test]
fn mismatched_or_missing_admission_and_forged_findings_fail_closed() {
    let source = snapshot("rust");
    let original = fixture_value("rust", "check");
    let selection = fixture_value("rust", "selection");
    let mutate_fact = |relation: &str, field: &str, value: serde_json::Value| {
        let mut report = original.clone();
        let fact = report["facts"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|f| f["relation"] == relation)
            .unwrap();
        fact[field] = value;
        assert!(
            checked(&source, report, selection.clone()).is_err(),
            "{relation}.{field}"
        );
    };
    for index in 0..5 {
        let mut report = original.clone();
        let fact = report["facts"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|f| f["relation"] == "snapshot_identity")
            .unwrap();
        fact["args"][index] = "other".into();
        assert!(checked(&source, report, selection.clone()).is_err());
    }
    mutate_fact("coverage_admitted", "origin", "asserted".into());
    mutate_fact("coverage_admitted", "why", "invented".into());
    mutate_fact(
        "coverage_input",
        "args",
        serde_json::json!(["policy:entry-point/rust", "2", "500", "0", "1", "0", "0"]),
    );
    mutate_fact(
        "call_census",
        "args",
        serde_json::json!(["Ambiguous", "999"]),
    );
    mutate_fact("unused_production_symbol", "origin", "asserted".into());
    mutate_fact("unused_production_symbol", "why", "invented".into());
    mutate_fact("calls", "args", serde_json::json!(["invented", "invented"]));
    for relation in [
        "coverage_input",
        "coverage_admitted",
        "call_census",
        "defines",
        "production_file",
    ] {
        let mut report = original.clone();
        report["facts"]
            .as_array_mut()
            .unwrap()
            .retain(|f| f["relation"] != relation);
        assert!(
            checked(&source, report, selection.clone()).is_err(),
            "missing {relation}"
        );
    }
    let mut report = original.clone();
    report["expectations"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["id"] != "coverage_is_admitted");
    assert!(checked(&source, report, selection.clone()).is_err());
    let mut report = original.clone();
    report["expectations"][0]["found"] = serde_json::json!(["P=other"]);
    assert!(checked(&source, report, selection.clone()).is_err());
    let mut report = original.clone();
    report["expectations"][0]["satisfied"] = false.into();
    assert!(checked(&source, report, selection.clone()).is_err());
    // A valid-looking witness cannot make the actual entry point absent.
    let mut forged = serde_json::to_string(&original).unwrap();
    forged = forged.replace("#unused", "#entry");
    let mut report: serde_json::Value = serde_json::from_str(&forged).unwrap();
    let facts = report["facts"].as_array_mut().unwrap();
    let mut seen = std::collections::BTreeSet::new();
    facts.retain(|fact| seen.insert(format!("{}{}", fact["relation"], fact["args"])));
    assert!(checked(&source, report, selection).is_err());
}

#[test]
fn expected_selections_bind_actual_entry_and_public_sets() {
    let source = snapshot("rust");
    for field in ["entry_points", "public_api"] {
        for replacement in [
            serde_json::json!(["unknown"]),
            serde_json::json!([source.nodes[2].id, source.nodes[2].id]),
        ] {
            let mut selection = fixture_value("rust", "selection");
            selection[field] = replacement;
            assert!(checked(&source, fixture_value("rust", "check"), selection).is_err());
        }
    }
    let mut selection = fixture_value("rust", "selection");
    selection["entry_points"] = serde_json::json!([]);
    assert!(checked(&source, fixture_value("rust", "check"), selection).is_err());
}

#[test]
fn source_links_reject_active_schemes_and_browser_normalization() {
    for link in [
        "javascript:alert(1)",
        "JaVaScRiPt:alert(1)",
        "data:text/html,x",
        "//evil.test",
        "\\evil.test",
        "/absolute",
        "https:///evil.test",
        "https://",
        "\njavascript:x",
        " java\nscript:x",
    ] {
        assert!(!codemap_topology::safe_link(link), "{link}");
    }
    for link in [
        "src/main.rs#L1-L4",
        "../src/main.rs#L1",
        "#node",
        "https://example.test/src/main.rs#L1",
    ] {
        assert!(codemap_topology::safe_link(link), "{link}");
    }
}

#[test]
fn unresolved_sites_belong_only_to_unique_innermost_callables() {
    let mut source = snapshot("rust");
    let mut nested = source.nodes[2].clone();
    nested.id.push_str("::nested");
    nested.symbol_key.push_str("::nested");
    nested.name = "nested".into();
    nested.provenance.span.start_byte = 45;
    nested.provenance.span.end_byte = 65;
    nested.provenance.span.start_line = 5;
    nested.provenance.span.end_line = 7;
    source.nodes.push(nested.clone());
    *source.report.node_counts.get_mut("function").unwrap() += 1;
    let topology = project(&source, Scope::default()).unwrap();
    let owner = annotated_nodes(&topology.root);
    assert_eq!(owner, vec![nested.id.clone()]);
    let mut equally_inner = nested.clone();
    equally_inner.id.push_str("_other");
    equally_inner.symbol_key.push_str("_other");
    source.nodes.push(equally_inner);
    *source.report.node_counts.get_mut("function").unwrap() += 1;
    let topology = project(&source, Scope::default()).unwrap();
    assert_eq!(
        annotated_nodes(&topology.root),
        [] as [std::string::String; 0]
    );
    assert_eq!(topology.codemap.unowned_call_sites.len(), 1);
}

fn annotated_nodes(node: &codemap_topology::TreeNode) -> Vec<String> {
    let mut ids = vec![];
    if node
        .codemap
        .as_ref()
        .is_some_and(|e| !e.unresolved_call_sites.is_empty())
    {
        ids.push(node.id.clone());
    }
    for child in &node.children {
        ids.extend(annotated_nodes(child));
    }
    ids
}

#[test]
fn snapshot_order_does_not_change_topology_bytes() {
    let mut source = snapshot("rust");
    let first = serde_json::to_string(&project(&source, Scope::default()).unwrap()).unwrap();
    source.nodes.reverse();
    source.edges.reverse();
    source.call_sites.reverse();
    source.files.reverse();
    assert_eq!(
        first,
        serde_json::to_string(&project(&source, Scope::default()).unwrap()).unwrap()
    );
}

/// The Ruby fixture plus `Child < Base` and `Child` including `Mixin`, with
/// `Base` and `Mixin` in a second file. Built here so the pinned fixtures and
/// the checker reports recorded against them stay as they are.
fn hierarchy() -> Snapshot {
    let mut source = snapshot("ruby");
    let language = source.identity.language;
    let own = source.nodes[0].provenance.rel_path.clone();
    let other = "lib/base.rb";
    let template = source.nodes[1].provenance.clone();
    let mut node = |kind: NodeKind, path: &str, name: &str| {
        let mut provenance = template.clone();
        provenance.rel_path = path.into();
        let node = Node {
            python: None,
            id: node_id(language, kind, path, &[], name),
            kind,
            name: name.into(),
            symbol_key: symbol_key(language, path, &[], name),
            provenance,
        };
        *source
            .report
            .node_counts
            .entry(kind.to_string())
            .or_default() += 1;
        source.nodes.push(node.clone());
        node
    };
    let class = NodeKind::Type(TypeKind::Class);
    let file = node(NodeKind::File, other, other);
    let base = node(class, other, "Base");
    let mixin = node(NodeKind::Module, other, "Mixin");
    let child = node(class, &own, "Child");
    source.files.push(FileRecord {
        rel_path: other.into(),
        sha256: template.file_hash.clone(),
        byte_len: 300,
    });
    source.report.files_scanned += 1;
    for (kind, from, to) in [
        (EdgeKind::Contains, &file, &base),
        (EdgeKind::Contains, &file, &mixin),
        (EdgeKind::Inherits, &child, &base),
        (EdgeKind::Implements, &child, &mixin),
    ] {
        let mut provenance = template.clone();
        provenance.rel_path.clone_from(&from.provenance.rel_path);
        source.edges.push(Edge {
            id: edge_id(kind, &from.id, &to.id, 0),
            kind,
            from: from.id.clone(),
            to: to.id.clone(),
            provenance,
        });
        *source
            .report
            .edge_counts
            .entry(kind.tag().to_string())
            .or_default() += 1;
    }
    source
}

fn edge_kinds(topology: &codemap_topology::Topology) -> Vec<(&str, &str, &str)> {
    topology
        .edges
        .iter()
        .map(|e| (e.kind, e.source.as_str(), e.target.as_str()))
        .collect()
}

fn tree_ids<'a>(node: &'a codemap_topology::TreeNode, ids: &mut Vec<&'a str>) {
    ids.push(&node.id);
    for child in &node.children {
        tree_ids(child, ids);
    }
}

#[test]
fn inheritance_and_implementation_join_the_circles_of_their_types() {
    let source = hierarchy();
    let topology = project(&source, Scope::default()).unwrap();
    let child = "@container:ruby:type:app/controllers/demo.rb#Child";
    assert_eq!(
        edge_kinds(&topology)
            .into_iter()
            .filter(|(kind, _, _)| *kind != "call")
            .collect::<Vec<_>>(),
        [
            (
                "implements",
                child,
                "@container:ruby:module:lib/base.rb#Mixin"
            ),
            ("inherits", child, "@container:ruby:type:lib/base.rb#Base"),
        ]
    );
    let mut ids = Vec::new();
    tree_ids(&topology.root, &mut ids);
    for edge in &topology.edges {
        assert!(ids.contains(&edge.source.as_str()));
        assert!(ids.contains(&edge.target.as_str()));
    }
    assert_eq!(
        topology.edges.iter().filter(|e| e.kind == "call").count(),
        1
    );
}

#[test]
fn an_excluded_prefix_leaves_the_map_and_is_named_on_it() {
    let source = hierarchy();
    let exclude = ["lib/".to_string()];
    let topology = project(
        &source,
        Scope {
            exclude: &exclude,
            ..Scope::default()
        },
    )
    .unwrap();
    let mut ids = Vec::new();
    tree_ids(&topology.root, &mut ids);
    assert!(!ids.iter().any(|id| id.contains("lib")));
    assert_eq!(edge_kinds(&topology).len(), 1);
    let excluded = topology.codemap.excluded.as_ref().unwrap();
    assert_eq!(excluded.prefixes, ["lib"]);
    assert_eq!((excluded.files, excluded.nodes, excluded.edges), (1, 3, 4));
    assert_eq!(
        serde_json::to_value(&topology.codemap.extraction_report).unwrap(),
        serde_json::to_value(&source.report).unwrap()
    );
    for prefix in ["li", "missing", "lib/base"] {
        let exclude = [prefix.to_string()];
        assert!(
            project(
                &source,
                Scope {
                    exclude: &exclude,
                    ..Scope::default()
                }
            )
            .is_err(),
            "{prefix}"
        );
    }
}

#[test]
fn a_selection_alone_marks_entry_points_and_claims_no_absence() {
    let source = snapshot("ruby");
    let selection: codemap_topology::Selection =
        serde_json::from_value(fixture_value("ruby", "selection")).unwrap();
    let scope = Scope {
        selection: Some(&selection),
        ..Scope::default()
    };
    let topology = project(&source, scope).unwrap();
    assert_eq!(topology.entry_points, selection.entry_points);
    assert_eq!(topology.dead_ends, [] as [String; 0]);
    assert_eq!(topology.codemap.absence.len(), 0);

    let mut unknown = selection.clone();
    unknown.entry_points = vec!["ruby:function:app/controllers/demo.rb#missing".into()];
    assert!(
        project(
            &source,
            Scope {
                selection: Some(&unknown),
                ..Scope::default()
            }
        )
        .is_err()
    );
    let report = serde_json::from_value(fixture_value("ruby", "check")).unwrap();
    assert!(
        project(
            &source,
            Scope {
                liveness: Some(&report),
                ..Scope::default()
            }
        )
        .is_err()
    );
}

#[test]
fn html_counts_unowned_sites_and_json_lists_them() {
    let mut source = snapshot("rust");
    let mut twin = source.nodes[2].clone();
    twin.id.push_str("_twin");
    twin.symbol_key.push_str("_twin");
    source.nodes.push(twin);
    *source.report.node_counts.get_mut("function").unwrap() += 1;
    let topology = project(&source, Scope::default()).unwrap();
    assert_eq!(topology.codemap.unowned_call_sites.len(), 1);
    assert_eq!(
        serde_json::to_value(&topology.codemap.unowned_call_site_summary).unwrap(),
        serde_json::json!({
            "total": 1,
            "by_category": { "Candidate": 1 },
            "by_file": { "src/main.rs": 1 },
        })
    );
    let html = render_html(&topology).unwrap();
    assert!(html.contains("\"unowned_call_site_summary\""));
    assert!(!html.contains("\"unowned_call_sites\""));
}

#[test]
fn supplied_policy_relations_are_placed_apart_from_edges_and_checked_against_the_snapshot() {
    let source = hierarchy();
    let child = "ruby:type:app/controllers/demo.rb#Child";
    let relation = |to: &str| Relation {
        from: child.into(),
        to: to.into(),
        rule: "inferred".into(),
        declaration: "belongs_to :base".into(),
        through: None,
        via: None,
        rel_path: "app/controllers/demo.rb".into(),
        lines: (3, 3),
    };
    let policy = |relations| {
        [Policy {
            name: "policy:association/ruby".into(),
            kind: "association".into(),
            relations,
            unresolved: [("no_class".to_string(), 2)].into(),
        }]
    };
    let policies = policy(vec![relation("ruby:type:lib/base.rb#Base")]);
    let topology = project(
        &source,
        Scope {
            policies: &policies,
            ..Scope::default()
        },
    )
    .unwrap();
    let value = serde_json::to_value(&topology).unwrap();
    assert_eq!(
        value["codemap"]["policy_relations"],
        serde_json::json!([{
            "source": format!("@container:{child}"),
            "target": "@container:ruby:type:lib/base.rb#Base",
            "kind": "association",
            "policy": "policy:association/ruby",
            "rule": "inferred",
            "declaration": "belongs_to :base",
            "rel_path": "app/controllers/demo.rb",
            "start_line": 3,
            "end_line": 3,
            "source_link": "app/controllers/demo.rb#L3-L3",
        }])
    );
    assert_eq!(
        value["codemap"]["policies"],
        serde_json::json!([{
            "policy": "policy:association/ruby",
            "relations": 1,
            "unresolved": { "no_class": 2 },
        }])
    );
    assert!(topology.edges.iter().all(|edge| edge.kind != "association"));

    // Hidden with an end that is excluded; still counted.
    let exclude = ["lib".to_string()];
    let scoped = project(
        &source,
        Scope {
            policies: &policies,
            exclude: &exclude,
            ..Scope::default()
        },
    )
    .unwrap();
    assert_eq!(scoped.codemap.policy_relations.len(), 0);
    assert_eq!(scoped.codemap.policies[0].relations, 1);

    let unknown = policy(vec![relation("ruby:type:lib/base.rb#Missing")]);
    assert!(
        project(
            &source,
            Scope {
                policies: &unknown,
                ..Scope::default()
            }
        )
        .is_err()
    );
}
