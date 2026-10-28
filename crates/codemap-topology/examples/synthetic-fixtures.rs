//! Regenerate the purpose-built, source-free snapshots used by integration tests.
use codemap_graph::schema::{
    CallSite, CallSiteCategory, Edge, EdgeKind, FactSource, FileRecord, Language, Node, NodeKind,
    Provenance, RepoIdentity, Report, Snapshot, SnapshotIdentity, Span, ToolVersions, edge_id,
    node_id, symbol_key,
};
use std::collections::BTreeMap;

const fn span(start: usize, end: usize) -> Span {
    Span {
        start_byte: start,
        end_byte: end,
        start_line: (start / 10 + 1) as u32,
        end_line: (end / 10 + 1) as u32,
        start_col: 0,
        end_col: 0,
    }
}

fn tools(language: Language) -> ToolVersions {
    match language {
        Language::Rust => ToolVersions::semantic(
            "rust-analyzer synthetic".into(),
            "rustc synthetic".into(),
            "cargo synthetic".into(),
            "synthetic-target".into(),
            None,
        ),
        Language::Ruby => ToolVersions::semantic_ruby(
            "ruby-lsp synthetic".into(),
            "ruby synthetic".into(),
            "rbs synthetic".into(),
            "synthetic-bundle".into(),
        ),
        Language::Php => ToolVersions::semantic_php(
            "phpactor synthetic".into(),
            "php synthetic".into(),
            "synthetic-bundle".into(),
        ),
        Language::Go => ToolVersions::semantic_go(
            "gopls synthetic".into(),
            "go synthetic".into(),
            "synthetic-bundle".into(),
        ),
        Language::Cpp | Language::JsTs | Language::Python => {
            panic!("C++ is syntax-only: it has no proven calls to render")
        }
    }
}

fn report(nodes: &[Node], edges: &[Edge]) -> Report {
    let mut node_counts = BTreeMap::new();
    for node in nodes {
        *node_counts.entry(node.kind.to_string()).or_default() += 1;
    }
    let mut edge_counts = BTreeMap::new();
    for edge in edges {
        *edge_counts.entry(edge.kind.tag().to_string()).or_default() += 1;
    }
    Report {
        files_scanned: 1,
        node_counts,
        edge_counts,
        call_sites_by_category: BTreeMap::from([("Resolved".into(), 1), ("Candidate".into(), 1)]),
        ..Report::default()
    }
}

const fn source(language: Language) -> (&'static str, FactSource) {
    match language {
        Language::Rust => ("src/main.rs", FactSource::RustAnalyzer),
        Language::Ruby => ("app/controllers/demo.rb", FactSource::RubyLsp),
        Language::Php => ("src/demo.php", FactSource::Phpactor),
        Language::Go => ("cmd/demo/main.go", FactSource::Gopls),
        Language::Cpp | Language::JsTs | Language::Python => {
            panic!("C++ is syntax-only: it has no proven calls to render")
        }
    }
}

fn main() -> anyhow::Result<()> {
    for language in [Language::Rust, Language::Ruby, Language::Php, Language::Go] {
        let tools = tools(language);
        let (path, semantic) = source(language);
        let identity = SnapshotIdentity::build(
            language,
            RepoIdentity {
                id: "https://example.test/synthetic-topology.git".into(),
                origin: None,
                is_canonical: false,
            },
            "synthetic-commit".into(),
            "synthetic-tree".into(),
            tools,
            false,
        );
        let provenance = |start, end, source| Provenance {
            repository: identity.repository.id.clone(),
            commit_sha: identity.commit_sha.clone(),
            rel_path: path.into(),
            span: span(start, end),
            file_hash: "synthetic-file-hash".into(),
            extractor_version: identity.extractor_version.clone(),
            fact_source: source,
            confidence: 255,
        };
        let mut nodes = Vec::new();
        for (kind, name, start, end) in [
            (NodeKind::File, path, 0, 300),
            (NodeKind::Module, "demo", 0, 300),
            (NodeKind::Function, "entry", 10, 90),
            (NodeKind::Function, "work", 100, 190),
            (NodeKind::Function, "unused", 200, 290),
        ] {
            nodes.push(Node {
                python: None,
                id: node_id(language, kind, path, &[], name),
                kind,
                name: name.into(),
                symbol_key: symbol_key(language, path, &[], name),
                provenance: provenance(start, end, language.structural_fact_source()),
            });
        }
        let mut edges = Vec::new();
        for (from, to) in [(0, 1), (1, 2), (1, 3), (1, 4)] {
            edges.push(Edge {
                id: edge_id(EdgeKind::Contains, &nodes[from].id, &nodes[to].id, 0),
                kind: EdgeKind::Contains,
                from: nodes[from].id.clone(),
                to: nodes[to].id.clone(),
                provenance: provenance(0, 300, language.structural_fact_source()),
            });
        }
        edges.push(Edge {
            id: edge_id(EdgeKind::Calls, &nodes[2].id, &nodes[3].id, 30),
            kind: EdgeKind::Calls,
            from: nodes[2].id.clone(),
            to: nodes[3].id.clone(),
            provenance: provenance(30, 34, semantic),
        });
        let call_sites = vec![
            CallSite {
                rel_path: path.into(),
                span: span(30, 34),
                raw_name: "work".into(),
                category: CallSiteCategory::Resolved,
            },
            CallSite {
                rel_path: path.into(),
                span: span(50, 54),
                raw_name: "dynamic_target".into(),
                category: CallSiteCategory::Candidate,
            },
        ];
        let report = report(&nodes, &edges);
        let snapshot = Snapshot {
            identity,
            files: vec![FileRecord {
                rel_path: path.into(),
                sha256: "synthetic-file-hash".into(),
                byte_len: 300,
            }],
            nodes,
            edges,
            call_sites,
            measures: vec![],
            file_measures: vec![],
            clone_groups: vec![],
            twin_pairs: vec![],
            report,
        };
        codemap_graph::validate(&snapshot)?;
        let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
        std::fs::write(
            format!("{dir}/{}.json", language.tag()),
            serde_json::to_string_pretty(&snapshot)? + "\n",
        )?;
    }
    Ok(())
}
