//! A hand-built, validator-clean Rust snapshot with every shape the
//! projection has to handle: an entry point, a live chain with a repeated
//! and a self-recursive call, an inline test module that calls into
//! production, a presentation file the core reaches, a dead file, a file
//! only the test tree reaches, a type, an import placeholder, and an
//! unclassified `build.rs`.

#![allow(dead_code)]

use std::collections::BTreeMap;

use codemap_graph::schema::{
    CPP_SEMANTIC_POLICY, CallSite, CallSiteCategory, CppSemanticInputs, Edge, EdgeKind, FactSource,
    FileRecord, Language, Measure, Node, NodeKind, Provenance, RepoIdentity, Report, Snapshot,
    SnapshotIdentity, Span, ToolVersions, TypeKind, edge_id, measures_policy, node_id,
    node_id_disambiguated, symbol_key,
};
use codemap_lemmaspec::positive_path;

pub const REPOSITORY: &str = "https://github.com/example/fixture.git";
pub const COMMIT: &str = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";

pub fn main_id() -> String {
    id(NodeKind::Function, "src/main.rs", &[], "main")
}
pub fn run_id() -> String {
    id(NodeKind::Function, "src/core.rs", &[], "run")
}
pub fn helper_id() -> String {
    id(NodeKind::Function, "src/core.rs", &[], "helper")
}
pub fn dead_fn_id() -> String {
    id(NodeKind::Function, "src/core.rs", &[], "dead_fn")
}
pub fn render_id() -> String {
    id(NodeKind::Function, "src/view.rs", &[], "render")
}
pub fn unused_id() -> String {
    id(NodeKind::Function, "src/dead.rs", &[], "unused")
}
pub fn probe_target_id() -> String {
    id(NodeKind::Function, "src/probe.rs", &[], "probe_target")
}
pub fn it_probes_id() -> String {
    id(NodeKind::Function, "tests/it.rs", &[], "it_probes")
}
pub fn checks_run_id() -> String {
    id(NodeKind::Function, "src/core.rs", &["tests"], "checks_run")
}

fn id(kind: NodeKind, rel_path: &str, scope: &[&str], name: &str) -> String {
    let scope: Vec<String> = scope.iter().map(ToString::to_string).collect();
    node_id(Language::Rust, kind, rel_path, &scope, name)
}

/// Build the fixture snapshot. `dirty` marks the identity as a
/// working-tree extraction.
pub fn rust_snapshot(dirty: bool) -> Snapshot {
    rust_snapshot_with_test_file(dirty, "tests/it.rs")
}

pub fn rust_snapshot_with_test_file(dirty: bool, test_file: &str) -> Snapshot {
    let identity = SnapshotIdentity::build(
        Language::Rust,
        RepoIdentity {
            id: REPOSITORY.to_string(),
            origin: Some(REPOSITORY.to_string()),
            is_canonical: true,
        },
        COMMIT.to_string(),
        "tree0000".to_string(),
        ToolVersions::semantic(
            "rust-analyzer 1".into(),
            "rustc 1".into(),
            "cargo 1".into(),
            "triple".into(),
            None,
        ),
        dirty,
    );
    let mut fixture = Fixture {
        language: Language::Rust,
        identity,
        nodes: Vec::new(),
        edges: Vec::new(),
        call_sites: Vec::new(),
    };

    let f = NodeKind::Function;
    fixture.node(f, "src/main.rs", &[], "main", 1);
    fixture.node(NodeKind::ImportRef, "src/main.rs", &[], "run", 2);
    fixture.node(f, "src/core.rs", &[], "run", 1);
    fixture.node(f, "src/core.rs", &[], "helper", 5);
    fixture.node(f, "src/core.rs", &[], "dead_fn", 9);
    fixture.node(
        NodeKind::Type(TypeKind::Struct),
        "src/core.rs",
        &[],
        "Config",
        12,
    );
    fixture.node(NodeKind::Module, "src/core.rs", &[], "tests", 20);
    fixture.node(f, "src/core.rs", &["tests"], "checks_run", 21);
    fixture.node(f, "src/view.rs", &[], "render", 1);
    fixture.node(f, "src/dead.rs", &[], "unused", 1);
    fixture.node(f, "src/probe.rs", &[], "probe_target", 1);
    fixture.node(f, test_file, &[], "it_probes", 1);
    fixture.node(f, "build.rs", &[], "main", 1);

    fixture.call(&main_id(), &run_id(), "src/main.rs", 3);
    fixture.call(&run_id(), &helper_id(), "src/core.rs", 3);
    fixture.call(&run_id(), &helper_id(), "src/core.rs", 2);
    fixture.call(&helper_id(), &render_id(), "src/core.rs", 6);
    fixture.call(&helper_id(), &helper_id(), "src/core.rs", 7);
    fixture.call(&checks_run_id(), &run_id(), "src/core.rs", 22);
    fixture.call(&checks_run_id(), &dead_fn_id(), "src/core.rs", 23);
    fixture.call(
        &id(f, test_file, &[], "it_probes"),
        &probe_target_id(),
        test_file,
        2,
    );

    fixture.finish()
}

/// The Rust fixture with a measure on every callable: `main` at the
/// given complexity, every other callable at 3, under the Rust counting
/// policy.
pub fn measured_rust_snapshot(main_cc: u32) -> Snapshot {
    let mut snapshot = rust_snapshot(false);
    measure_every_callable(&mut snapshot, |node| {
        if node.id == main_id() { main_cc } else { 3 }
    });
    snapshot
}

pub fn cards_index_id() -> String {
    node_id(
        Language::Ruby,
        NodeKind::Method,
        "app/controllers/cards_controller.rb",
        &["CardsController".to_string()],
        "index",
    )
}

pub fn card_title_id() -> String {
    node_id(
        Language::Ruby,
        NodeKind::Method,
        "app/models/card.rb",
        &["Card".to_string()],
        "title",
    )
}

/// A syntax C++ snapshot holding `main` and one unreached function.
pub fn cpp_snapshot() -> Snapshot {
    let identity = SnapshotIdentity::build(
        Language::Cpp,
        RepoIdentity {
            id: REPOSITORY.to_string(),
            origin: Some(REPOSITORY.to_string()),
            is_canonical: true,
        },
        COMMIT.to_string(),
        "tree0000".to_string(),
        ToolVersions::syntax_cpp(),
        false,
    );
    let mut fixture = Fixture {
        language: Language::Cpp,
        identity,
        nodes: Vec::new(),
        edges: Vec::new(),
        call_sites: Vec::new(),
    };
    fixture.node(NodeKind::Function, "src/main.cpp", &[], "main()", 1);
    fixture.node(NodeKind::Function, "src/dead.cpp", &[], "unused()", 1);
    fixture.finish()
}

/// A semantic C++ snapshot with a proven path `main()` → `hop()` → the
/// target, the options selecting it, and the ID of a decoy that shares
/// `hop()`'s symbol key but nothing reaches.
pub fn positive_path_fixture() -> (Snapshot, positive_path::Options, String) {
    let mut snapshot = cpp_snapshot();
    snapshot.identity = snapshot.identity.with_cpp_semantic(CppSemanticInputs {
        profile_sha256: "a".repeat(64),
        inputs_sha256: "b".repeat(64),
        runtime_sha256: "c".repeat(64),
        clangd_sha256: "d".repeat(64),
        policy: CPP_SEMANTIC_POLICY.into(),
    });
    let entry = snapshot.nodes[0].id.clone();
    let target = snapshot.nodes[1].id.clone();
    let mut intermediate = snapshot.nodes[1].clone();
    intermediate.id = node_id(
        Language::Cpp,
        NodeKind::Function,
        "src/core.cpp",
        &[],
        "hop()",
    );
    intermediate.symbol_key = symbol_key(Language::Cpp, "src/core.cpp", &[], "hop()");
    intermediate.name = "hop()".into();
    intermediate.provenance.rel_path = "src/core.cpp".into();
    let mut file = snapshot.files[1].clone();
    file.rel_path = "src/core.cpp".into();
    snapshot.files.push(file);
    let mut decoy = intermediate.clone();
    decoy.id = node_id_disambiguated(
        Language::Cpp,
        NodeKind::Function,
        "src/core.cpp",
        &[],
        "hop()",
        200,
    );
    decoy.provenance.span = Span::from_ts(200, 220, (4, 0), (4, 20));
    let decoy_id = decoy.id.clone();
    let intermediate_id = intermediate.id.clone();
    snapshot.nodes.extend([intermediate, decoy]);
    for (from, to) in [(&entry, &intermediate_id), (&intermediate_id, &target)] {
        let mut provenance = snapshot
            .nodes
            .iter()
            .find(|node| node.id == *from)
            .unwrap()
            .provenance
            .clone();
        provenance.span = Span::from_ts(45, 48, (0, 5), (0, 8));
        provenance.fact_source = FactSource::Clangd;
        snapshot.call_sites.push(CallSite {
            rel_path: provenance.rel_path.clone(),
            span: provenance.span,
            raw_name: "call".into(),
            category: CallSiteCategory::Resolved,
        });
        snapshot.edges.push(Edge {
            id: edge_id(EdgeKind::Calls, from, to, 45),
            kind: EdgeKind::Calls,
            from: from.clone(),
            to: to.clone(),
            provenance,
        });
    }
    snapshot.report = report_for(&snapshot);
    codemap_graph::validate(&snapshot).unwrap();
    (
        snapshot,
        positive_path::Options {
            entry_node: entry,
            target_node: target,
            allow_dirty: false,
        },
        decoy_id,
    )
}

/// A JS/TS snapshot with an explicitly incomplete call graph.
pub fn js_ts_snapshot() -> Snapshot {
    let identity = SnapshotIdentity::build(
        Language::JsTs,
        RepoIdentity {
            id: REPOSITORY.to_string(),
            origin: Some(REPOSITORY.to_string()),
            is_canonical: true,
        },
        COMMIT.to_string(),
        "tree0000".to_string(),
        ToolVersions::syntax_js_ts(),
        false,
    );
    let mut fixture = Fixture {
        language: Language::JsTs,
        identity,
        nodes: Vec::new(),
        edges: Vec::new(),
        call_sites: Vec::new(),
    };
    fixture.node(NodeKind::Function, "src/entry.tsx", &[], "entry", 1);
    fixture.node(NodeKind::Function, "src/unused.ts", &[], "unused", 1);
    fixture.finish()
}

/// A handbuilt Python native graph for consumer contracts, not parser attestation.
pub fn python_snapshot() -> Snapshot {
    use codemap_graph::schema::{
        PYTHON_NATIVE_POLICY, PYTHON_PARSER_POLICY, PythonDefinition, PythonInputs,
        PythonNativeInputs,
    };
    let mut tools = ToolVersions::syntax_python();
    tools.mypyc = Some("2.3.1".into());
    let identity = SnapshotIdentity::build(
        Language::Python,
        RepoIdentity {
            id: REPOSITORY.into(),
            origin: Some(REPOSITORY.into()),
            is_canonical: true,
        },
        COMMIT.into(),
        "tree0000".into(),
        tools,
        false,
    )
    .with_python(PythonInputs {
        parser_profile_sha256: "a".repeat(64),
        policy: PYTHON_PARSER_POLICY.into(),
        native: Some(PythonNativeInputs {
            profile_sha256: "b".repeat(64),
            evidence_sha256: "c".repeat(64),
            policy: PYTHON_NATIVE_POLICY.into(),
        }),
    });
    let mut fixture = Fixture {
        language: Language::Python,
        identity,
        nodes: vec![],
        edges: vec![],
        call_sites: vec![],
    };
    let path = "src/main.py";
    fixture.node(NodeKind::File, path, &[], "main.py", 0);
    fixture.node(NodeKind::Function, path, &[], "caller", 1);
    fixture.node(NodeKind::Function, path, &[], "target", 2);
    fixture.nodes[0].provenance.span = Span::from_ts(0, 4096, (0, 0), (99, 0));
    let file_id = fixture.nodes[0].id.clone();
    for node in &mut fixture.nodes[1..] {
        node.python = Some(PythonDefinition {
            is_async: false,
            ordinal: 0,
            ambiguous: false,
            decorators: vec![],
            bases: vec![],
        });
        fixture.edges.push(Edge {
            id: edge_id(
                EdgeKind::Contains,
                &file_id,
                &node.id,
                node.provenance.span.start_byte,
            ),
            kind: EdgeKind::Contains,
            from: file_id.clone(),
            to: node.id.clone(),
            provenance: node.provenance.clone(),
        });
    }
    let from = fixture.nodes[1].id.clone();
    let to = fixture.nodes[2].id.clone();
    fixture.call(&from, &to, path, 1);
    fixture.edges.last_mut().unwrap().provenance.fact_source = FactSource::Mypyc;
    fixture.call_sites.push(CallSite {
        rel_path: path.into(),
        span: Span::from_ts(120, 130, (2, 0), (2, 10)),
        raw_name: "unknown".into(),
        category: CallSiteCategory::Unsupported,
    });
    fixture.finish()
}

/// A measured Rails snapshot: one controller action and one model method,
/// each at the given complexity, syntax mode (no calls).
pub fn measured_ruby_snapshot(index_cc: u32, title_cc: u32) -> Snapshot {
    let identity = SnapshotIdentity::build(
        Language::Ruby,
        RepoIdentity {
            id: REPOSITORY.to_string(),
            origin: Some(REPOSITORY.to_string()),
            is_canonical: true,
        },
        COMMIT.to_string(),
        "tree0000".to_string(),
        ToolVersions::syntax_ruby(),
        false,
    );
    let mut fixture = Fixture {
        language: Language::Ruby,
        identity,
        nodes: Vec::new(),
        edges: Vec::new(),
        call_sites: Vec::new(),
    };
    fixture.node(
        NodeKind::Type(TypeKind::Struct),
        "app/controllers/cards_controller.rb",
        &[],
        "CardsController",
        1,
    );
    fixture.node(
        NodeKind::Method,
        "app/controllers/cards_controller.rb",
        &["CardsController"],
        "index",
        2,
    );
    fixture.node(
        NodeKind::Type(TypeKind::Struct),
        "app/models/card.rb",
        &[],
        "Card",
        1,
    );
    fixture.node(
        NodeKind::Method,
        "app/models/card.rb",
        &["Card"],
        "title",
        2,
    );
    let mut snapshot = fixture.finish();
    measure_every_callable(&mut snapshot, |node| {
        if node.id == cards_index_id() {
            index_cc
        } else {
            title_cc
        }
    });
    snapshot
}

fn measure_every_callable(snapshot: &mut Snapshot, cc: impl Fn(&Node) -> u32) {
    let form = codemap_graph::schema::form_measures_available(snapshot.identity.language);
    let measures: Vec<Measure> = snapshot
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Macro
            )
        })
        .map(|node| Measure {
            node_id: node.id.clone(),
            cc: cc(node),
            sloc: 10,
            loc: 12,
            clone_group_ids: Vec::new(),
            // The form measures travel with the language's policy.
            params: form.then_some(1),
            nesting: form.then_some(0),
            chain: form.then_some(0),
            ivars: (snapshot.identity.language == Language::Ruby).then_some(0),
        })
        .collect();
    snapshot.measures = measures;
    snapshot.identity = snapshot
        .identity
        .clone()
        .with_measures_policy(measures_policy(snapshot.identity.language).expect("policy"));
}

struct Fixture {
    language: Language,
    identity: SnapshotIdentity,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    call_sites: Vec<CallSite>,
}

impl Fixture {
    fn provenance(&self, rel_path: &str, line: u32, fact_source: FactSource) -> Provenance {
        let start_byte = usize::try_from(line).expect("line fits") * 40;
        Provenance {
            repository: self.identity.repository.id.clone(),
            commit_sha: self.identity.commit_sha.clone(),
            rel_path: rel_path.to_string(),
            span: Span {
                start_byte,
                end_byte: start_byte + 20,
                start_line: line,
                start_col: 0,
                end_line: line,
                end_col: 20,
            },
            file_hash: file_hash(rel_path),
            extractor_version: self.identity.extractor_version.clone(),
            fact_source,
            confidence: u8::MAX,
        }
    }

    fn node(&mut self, kind: NodeKind, rel_path: &str, scope: &[&str], name: &str, line: u32) {
        let scope: Vec<String> = scope.iter().map(ToString::to_string).collect();
        self.nodes.push(Node {
            python: None,
            id: node_id(self.language, kind, rel_path, &scope, name),
            kind,
            name: name.to_string(),
            symbol_key: symbol_key(self.language, rel_path, &scope, name),
            provenance: self.provenance(rel_path, line, self.language.structural_fact_source()),
        });
    }

    fn call(&mut self, from: &str, to: &str, rel_path: &str, line: u32) {
        let provenance = self.provenance(rel_path, line, FactSource::RustAnalyzer);
        self.call_sites.push(CallSite {
            rel_path: rel_path.to_string(),
            span: provenance.span,
            raw_name: to.rsplit('#').next().expect("fixture symbol").to_string(),
            category: CallSiteCategory::Resolved,
        });
        self.edges.push(Edge {
            id: edge_id(EdgeKind::Calls, from, to, provenance.span.start_byte),
            kind: EdgeKind::Calls,
            from: from.to_string(),
            to: to.to_string(),
            provenance,
        });
    }

    fn finish(self) -> Snapshot {
        let mut paths: Vec<String> = self
            .nodes
            .iter()
            .map(|node| node.provenance.rel_path.clone())
            .collect();
        paths.sort();
        paths.dedup();
        let files: Vec<FileRecord> = paths
            .iter()
            .map(|rel_path| FileRecord {
                rel_path: rel_path.clone(),
                sha256: file_hash(rel_path),
                byte_len: 4096,
            })
            .collect();
        let mut node_counts = BTreeMap::new();
        for node in &self.nodes {
            *node_counts.entry(node.kind.to_string()).or_insert(0) += 1;
        }
        let mut edge_counts = BTreeMap::new();
        for edge in &self.edges {
            *edge_counts.entry(edge.kind.tag().to_string()).or_insert(0) += 1;
        }
        let import_ref_nodes = self
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::ImportRef)
            .count();
        let mut snapshot = Snapshot {
            identity: self.identity,
            report: Report {
                files_scanned: files.len(),
                files_skipped: 0,
                parse_errors: 0,
                parse_error_files: BTreeMap::new(),
                first_parse_errors: BTreeMap::new(),
                node_counts,
                edge_counts,
                call_sites_by_category: BTreeMap::new(),
                import_ref_nodes,
                observations: BTreeMap::new(),
            },
            files,
            nodes: self.nodes,
            edges: self.edges,
            call_sites: self.call_sites,
            measures: Vec::new(),
            file_measures: Vec::new(),
            clone_groups: Vec::new(),
            twin_pairs: Vec::new(),
        };
        snapshot.report = report_for(&snapshot);
        snapshot
    }
}

fn file_hash(rel_path: &str) -> String {
    format!("{:0>64}", rel_path.len())
}

/// Run the workspace's `LemmaSpec` engine in-process on the files a
/// `lemmaspec walk|mutate|check|bind` command line names. `walk`, `mutate`
/// and `check` return their exit status (0 when the report is clean, 1 when
/// it is not, 2 on an error) with the JSON report; `bind` returns the bound
/// artifact.
pub fn run_lemmaspec(args: &[&str]) -> (i32, String) {
    let read = |path: &str| std::fs::read_to_string(path).expect("read artifact");
    let reported = |status: &str, json: serde_json::Result<String>| {
        (
            i32::from(status != "clean"),
            json.expect("serialize report"),
        )
    };
    let outcome = match args {
        ["walk", path, ..] => lemmaspec::walk_artifact(&read(path))
            .map(|report| reported(&report.status, serde_json::to_string_pretty(&report))),
        ["mutate", path, ..] => lemmaspec::mutate_artifact(&read(path))
            .map(|report| reported(&report.status, serde_json::to_string_pretty(&report))),
        ["check", checker, evidence, ..] => {
            lemmaspec::check_artifact(&read(checker), &read(evidence))
                .map(|report| reported(&report.status, serde_json::to_string_pretty(&report)))
        }
        ["bind", checker, evidence, ..] => {
            lemmaspec::bind_artifact(&read(checker), &read(evidence)).map(|bound| (0, bound))
        }
        _ => panic!("unsupported lemmaspec command {args:?}"),
    };
    outcome.unwrap_or_else(|error| (2, error.to_string()))
}

/// Recompute the report for a snapshot whose nodes or edges were trimmed.
pub fn report_for(snapshot: &Snapshot) -> Report {
    let mut node_counts = BTreeMap::new();
    for node in &snapshot.nodes {
        *node_counts.entry(node.kind.to_string()).or_insert(0) += 1;
    }
    let mut edge_counts = BTreeMap::new();
    for edge in &snapshot.edges {
        *edge_counts.entry(edge.kind.tag().to_string()).or_insert(0) += 1;
    }
    Report {
        files_scanned: snapshot.files.len(),
        files_skipped: 0,
        parse_errors: 0,
        parse_error_files: BTreeMap::new(),
        first_parse_errors: BTreeMap::new(),
        node_counts,
        edge_counts,
        call_sites_by_category: snapshot.call_sites.iter().fold(
            BTreeMap::new(),
            |mut counts, site| {
                *counts
                    .entry(site.category.report_key().to_string())
                    .or_insert(0) += 1;
                counts
            },
        ),
        import_ref_nodes: snapshot
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::ImportRef)
            .count(),
        observations: BTreeMap::new(),
    }
}

/// Synthetic call observations for `fixtures/unresolved_dispatcher`: the
/// callback is passed as a value, then invoked through an unresolved parameter.
/// These are explicit test inputs, not claims about an extraction run.
pub fn dispatcher_snapshot() -> Snapshot {
    let mut fixture = Fixture {
        language: Language::Rust,
        identity: rust_snapshot(false).identity,
        nodes: Vec::new(),
        edges: Vec::new(),
        call_sites: Vec::new(),
    };
    fixture.node(NodeKind::Function, "src/main.rs", &[], "main", 3);
    fixture.node(NodeKind::Function, "src/main.rs", &[], "dispatch", 7);
    fixture.node(NodeKind::Function, "src/service.rs", &[], "handle", 1);
    let dispatch = id(NodeKind::Function, "src/main.rs", &[], "dispatch");
    fixture.call(&main_id(), &dispatch, "src/main.rs", 4);
    fixture.call_sites.push(CallSite {
        rel_path: "src/main.rs".into(),
        span: fixture
            .provenance("src/main.rs", 8, Language::Rust.structural_fact_source())
            .span,
        raw_name: "callback".into(),
        category: CallSiteCategory::Unmapped,
    });
    fixture.finish()
}
