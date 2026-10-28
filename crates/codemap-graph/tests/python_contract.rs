//! Python schema/refusal contracts; these fixtures do not attest parser execution.
use std::collections::BTreeMap;

use codemap_graph::schema::*;
use codemap_graph::validate;

fn snapshot() -> Snapshot {
    let raw = b"def outer():\n    return 1\n";
    let identity = SnapshotIdentity::build(
        Language::Python,
        RepoIdentity {
            id: "fixture".into(),
            origin: None,
            is_canonical: false,
        },
        "commit".into(),
        "tree".into(),
        ToolVersions::syntax_python(),
        false,
    )
    .with_python(PythonInputs {
        parser_profile_sha256: sha256_hex(b"fixture parser profile"),
        policy: PYTHON_PARSER_POLICY.into(),
        native: None,
    });
    let provenance = |span| Provenance {
        repository: "fixture".into(),
        commit_sha: "commit".into(),
        rel_path: "main.py".into(),
        span,
        file_hash: sha256_hex(raw),
        extractor_version: EXTRACTOR_VERSION.into(),
        fact_source: FactSource::Cpython,
        confidence: u8::MAX,
    };
    let file = Node {
        python: None,
        id: "python:file:main.py".into(),
        kind: NodeKind::File,
        name: "main.py".into(),
        symbol_key: "python:file:main.py".into(),
        provenance: provenance(Span::from_ts(0, raw.len(), (0, 0), (2, 0))),
    };
    let function = Node {
        python: Some(PythonDefinition {
            is_async: false,
            ordinal: 0,
            ambiguous: false,
            decorators: vec![],
            bases: vec![],
        }),
        id: "python:function:main.py#outer@0".into(),
        kind: NodeKind::Function,
        name: "outer".into(),
        symbol_key: "python:main.py#outer".into(),
        provenance: provenance(Span::from_ts(0, raw.len() - 1, (0, 0), (1, 12))),
    };
    let edge = Edge {
        id: edge_id(EdgeKind::Contains, &file.id, &function.id, 0),
        kind: EdgeKind::Contains,
        from: file.id.clone(),
        to: function.id.clone(),
        provenance: function.provenance.clone(),
    };
    Snapshot {
        identity,
        files: vec![FileRecord {
            rel_path: "main.py".into(),
            sha256: sha256_hex(raw),
            byte_len: raw.len(),
        }],
        nodes: vec![file, function],
        edges: vec![edge],
        call_sites: vec![],
        measures: vec![],
        file_measures: vec![],
        clone_groups: vec![],
        twin_pairs: vec![],
        report: Report {
            files_scanned: 1,
            node_counts: BTreeMap::from([("file".into(), 1), ("function".into(), 1)]),
            edge_counts: BTreeMap::from([("contains".into(), 1)]),
            ..Report::default()
        },
    }
}

#[test]
fn parser_identity_does_not_authorize_calls_or_measures() {
    let original = snapshot();
    validate(&original).unwrap();
    assert!(!original.identity.is_semantic());
    assert_eq!(measures_policy(Language::Python), None);
    assert!(!clone_measures_available(Language::Python));
    assert_eq!(twins_policy(Language::Python), None);
    for source in [
        FactSource::Cpython,
        FactSource::Mypyc,
        FactSource::TreeSitter,
    ] {
        let mut invalid = original.clone();
        let id = invalid.nodes[1].id.clone();
        let mut provenance = invalid.nodes[1].provenance.clone();
        provenance.fact_source = source;
        invalid.edges.push(Edge {
            id: edge_id(EdgeKind::Calls, &id, &id, 0),
            kind: EdgeKind::Calls,
            from: id.clone(),
            to: id,
            provenance,
        });
        invalid.report.edge_counts.insert("calls".into(), 1);
        assert!(validate(&invalid).is_err());
    }
}

#[test]
fn syntax_cannot_invent_python_inheritance_relations() {
    for kind in [EdgeKind::Implements, EdgeKind::Inherits, EdgeKind::Extends] {
        let mut invalid = snapshot();
        let function = &invalid.nodes[1];
        invalid.edges.push(Edge {
            id: edge_id(kind, &function.id, &function.id, 0),
            kind,
            from: function.id.clone(),
            to: function.id.clone(),
            provenance: function.provenance.clone(),
        });
        invalid.report.edge_counts.insert(kind.tag().into(), 1);
        let error = validate(&invalid).unwrap_err();
        assert!(error.to_string().contains("no accepted relation policy"));
    }
}

#[test]
fn corrupt_python_profiles_metadata_and_parentage_fail_closed() {
    let original = snapshot();
    for corruption in 0..9 {
        let mut invalid = original.clone();
        match corruption {
            0 => invalid.identity.python = None,
            1 => {
                invalid
                    .identity
                    .python
                    .as_mut()
                    .unwrap()
                    .parser_profile_sha256 = "missing".into();
            }
            2 => invalid.identity.tools.python = Some("3.14.6".into()),
            3 => invalid.identity.tools.mypyc = Some("2.3.1".into()),
            4 => invalid.nodes[1].python = None,
            5 => invalid.nodes[1].python.as_mut().unwrap().ordinal = 1,
            6 => invalid.nodes[1].python.as_mut().unwrap().ambiguous = true,
            7 => invalid.edges[0].from = invalid.nodes[1].id.clone(),
            8 => invalid.nodes[0].python = invalid.nodes[1].python.clone(),
            _ => unreachable!(),
        }
        invalid.identity.snapshot_id = invalid.identity.compute_snapshot_id();
        assert!(validate(&invalid).is_err(), "corruption {corruption}");
    }
}

#[test]
fn native_identity_requires_complete_profile_evidence_and_tool_pins() {
    let mut inputs = snapshot().identity;
    inputs.python.as_mut().unwrap().native = Some(PythonNativeInputs {
        profile_sha256: sha256_hex(b"native profile"),
        evidence_sha256: sha256_hex(b"evidence"),
        policy: PYTHON_NATIVE_POLICY.into(),
    });
    assert!(!inputs.is_semantic());
    inputs.tools.mypyc = Some("2.3.1".into());
    assert!(inputs.is_semantic());
    inputs
        .python
        .as_mut()
        .unwrap()
        .native
        .as_mut()
        .unwrap()
        .evidence_sha256
        .clear();
    assert!(!inputs.is_semantic());
}

#[test]
fn native_calls_require_exact_resolved_census_even_with_consistent_counts() {
    let mut original = snapshot();
    original.identity.tools.mypyc = Some("2.3.1".into());
    original.identity.python.as_mut().unwrap().native = Some(PythonNativeInputs {
        profile_sha256: sha256_hex(b"native profile"),
        evidence_sha256: sha256_hex(b"evidence"),
        policy: PYTHON_NATIVE_POLICY.into(),
    });
    original.identity.snapshot_id = original.identity.compute_snapshot_id();
    let function = &original.nodes[1];
    let mut provenance = function.provenance.clone();
    provenance.fact_source = FactSource::Mypyc;
    original.call_sites.push(CallSite {
        rel_path: provenance.rel_path.clone(),
        span: provenance.span,
        raw_name: "outer".into(),
        category: CallSiteCategory::Resolved,
    });
    original.edges.push(Edge {
        id: edge_id(EdgeKind::Calls, &function.id, &function.id, 0),
        kind: EdgeKind::Calls,
        from: function.id.clone(),
        to: function.id.clone(),
        provenance,
    });
    original.report.edge_counts.insert("calls".into(), 1);
    original
        .report
        .call_sites_by_category
        .insert("Resolved".into(), 1);
    validate(&original).unwrap();
    for corruption in 0..4 {
        let mut invalid = original.clone();
        let expected = match corruption {
            0 => {
                invalid.edges.pop();
                invalid.report.edge_counts.remove("calls");
                "resolved call site has no exact call edge"
            }
            1 => {
                invalid.call_sites[0].category = CallSiteCategory::Unsupported;
                invalid.report.call_sites_by_category = BTreeMap::from([("Unsupported".into(), 1)]);
                "calls edge contradicts its exact resolved call site"
            }
            2 => {
                invalid.call_sites[0].span.end_col -= 1;
                "calls edge contradicts its exact resolved call site"
            }
            3 => {
                invalid.call_sites.clear();
                invalid.report.call_sites_by_category.clear();
                "call edge has no exact resolved call site"
            }
            _ => unreachable!(),
        };
        assert!(
            validate(&invalid)
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
}

#[test]
fn absent_python_fields_preserve_existing_identity_serialization() {
    for tools in [
        ToolVersions::syntax(),
        ToolVersions::syntax_ruby(),
        ToolVersions::syntax_php(),
        ToolVersions::syntax_go(),
        ToolVersions::syntax_cpp(),
        ToolVersions::syntax_js_ts(),
    ] {
        let value = serde_json::to_value(tools).unwrap();
        assert!(value.get("python").is_none());
        assert!(value.get("mypyc").is_none());
    }
    let mut other = snapshot();
    other.identity.language = Language::Rust;
    other.identity.tools = ToolVersions::syntax();
    other.identity.snapshot_id = other.identity.compute_snapshot_id();
    assert!(
        validate(&other)
            .unwrap_err()
            .to_string()
            .contains("Python identity")
    );
}
