//! Hand-built, validator-clean measured Rust snapshots: a base and a head
//! that differ in exactly the ways the change report has to explain.
//!
//! Base: `monster` (cc 30), `big` (cc 15), `grows` (cc 8), `moved` (cc 3,
//! in `src/a.rs`), `small` (cc 1). Head: `monster` (cc 30, or what the
//! caller asks), `big` split into `big_a`/`big_b`/`big_c` (cc 5 each),
//! `grows` at cc 12 (crossed the cutoff), `moved` relocated to `src/b.rs`
//! with the same name (paired by `infer_moves`), `small` unchanged. The
//! head also gains a call from `grows` into `small` so a coupling delta
//! exists.

#![allow(dead_code)]

use std::collections::BTreeMap;

use codemap_graph::schema::{
    CallSite, CallSiteCategory, CloneGroup, CloneOccurrence, Edge, EdgeKind, FactSource,
    FileMeasure, FileRecord, Language, Measure, Node, NodeKind, Provenance, RepoIdentity, Report,
    Snapshot, SnapshotIdentity, Span, ToolVersions, TwinPair, TypeKind, node_id, symbol_key,
};

pub const REPOSITORY: &str = "https://github.com/example/slop-fixture.git";
pub const BASE_COMMIT: &str = "b0000000b0000000b0000000b0000000b0000000";
pub const HEAD_COMMIT: &str = "a1111111a1111111a1111111a1111111a1111111";

/// A callable to place in a file: name, start line, cc, sloc, loc.
#[derive(Debug, Clone, Copy)]
pub struct Fn {
    pub name: &'static str,
    pub line: u32,
    pub cc: u32,
    pub sloc: u32,
    pub loc: u32,
}

const fn f(name: &'static str, line: u32, cc: u32, sloc: u32) -> Fn {
    Fn {
        name,
        line,
        cc,
        sloc,
        loc: sloc + 2,
    }
}

pub fn fn_id(rel_path: &str, name: &str) -> String {
    node_id(Language::Rust, NodeKind::Function, rel_path, &[], name)
}

pub fn base_snapshot() -> Snapshot {
    build(
        BASE_COMMIT,
        &[
            (
                "src/a.rs",
                vec![
                    f("monster", 1, 30, 100),
                    f("big", 120, 15, 50),
                    f("grows", 180, 8, 20),
                    f("moved", 210, 3, 10),
                    f("small", 230, 1, 5),
                ],
            ),
            ("src/b.rs", vec![]),
        ],
        &[],
    )
}

/// The head; `monster_cc` lets a test make the untouched hotspot worse
/// (31) or leave it alone (30).
pub fn head_snapshot(monster_cc: u32) -> Snapshot {
    build(
        HEAD_COMMIT,
        &[
            (
                "src/a.rs",
                vec![
                    f("monster", 1, monster_cc, 100),
                    f("big_a", 120, 5, 16),
                    f("big_b", 140, 5, 16),
                    f("big_c", 160, 5, 16),
                    f("grows", 180, 12, 24),
                    f("small", 230, 1, 5),
                ],
            ),
            ("src/b.rs", vec![f("moved", 1, 3, 10)]),
        ],
        &[("src/a.rs", "grows", "small")],
    )
}

/// The head, with `big_a` and `big_b` reported as one clone group of two
/// occurrences (sixteen source lines each) in `src/a.rs`.
pub fn head_with_clone() -> Snapshot {
    let mut snapshot = head_snapshot(30);
    let id = "clone:0123456789abcdef01234567".to_string();
    snapshot.clone_groups.push(CloneGroup {
        id: id.clone(),
        occurrences: vec![
            CloneOccurrence {
                rel_path: "src/a.rs".into(),
                start_line: 120,
                end_line: 137,
                sloc: 16,
            },
            CloneOccurrence {
                rel_path: "src/a.rs".into(),
                start_line: 140,
                end_line: 157,
                sloc: 16,
            },
        ],
        tokens: 60,
        lines: 18,
    });
    let a_file = node_id(Language::Rust, NodeKind::File, "src/a.rs", &[], "");
    for file in &mut snapshot.file_measures {
        if file.file_id == a_file {
            file.clone_sloc = 32;
        }
    }
    for name in ["big_a", "big_b"] {
        let target = fn_id("src/a.rs", name);
        for measure in &mut snapshot.measures {
            if measure.node_id == target {
                measure.clone_group_ids = vec![id.clone()];
            }
        }
    }
    snapshot
}

/// The head with the call sites a resolver would leave behind: inside
/// `grows`, the resolved site behind its `calls` edge plus one in-repository
/// candidate, one external target, and one unmapped name; inside `small`,
/// one candidate; and one candidate at file level that belongs to no
/// callable.
pub fn head_with_call_sites() -> Snapshot {
    let mut snapshot = head_snapshot(30);
    let site = |line: u32, raw_name: &str, category: CallSiteCategory| {
        let start_byte = usize::try_from(line - 1).expect("fits") * 40;
        CallSite {
            rel_path: "src/a.rs".into(),
            span: Span {
                start_byte,
                end_byte: start_byte + 10,
                start_line: line,
                start_col: 4,
                end_line: line,
                end_col: 14,
            },
            raw_name: raw_name.to_string(),
            category,
        }
    };
    snapshot.call_sites = vec![
        site(181, "small", CallSiteCategory::Resolved),
        site(182, "helper", CallSiteCategory::Candidate),
        site(183, "println", CallSiteCategory::StandardLibrary),
        site(184, "mystery", CallSiteCategory::Unmapped),
        site(231, "helper", CallSiteCategory::Candidate),
        site(240, "helper", CallSiteCategory::Candidate),
    ];
    let mut by_category = BTreeMap::new();
    for site in &snapshot.call_sites {
        *by_category
            .entry(site.category.report_key().to_string())
            .or_insert(0) += 1;
    }
    snapshot.report.call_sites_by_category = by_category;
    snapshot
}

/// A repository with run-once code beside its application: `app/core.rs`
/// holds `big` (cc 15), `grows` (cc 8), and `small` (cc 1);
/// `db/migrate/001_drop.rs` holds `up` (cc 30, the repository's max);
/// `script/fix.rs` holds `fix` (cc 12). Every file is Rust so the fixture
/// stays under the one counting policy the crate reads.
pub fn snapshot_with_run_once_code(commit: &str) -> Snapshot {
    build(
        commit,
        &[
            (
                "app/core.rs",
                vec![
                    f("big", 1, 15, 50),
                    f("grows", 60, 8, 20),
                    f("small", 90, 1, 5),
                ],
            ),
            ("db/migrate/001_drop.rs", vec![f("up", 1, 30, 40)]),
            ("script/fix.rs", vec![f("fix", 1, 12, 30)]),
        ],
        &[],
    )
}

/// The run-once fixture with twin pairs: `big` and the migration's `up`
/// are exact twins (a pair that a scope excluding `db/migrate/` drops),
/// `big` and `grows` are near twins (an edited copy).
pub fn snapshot_with_twins(commit: &str) -> Snapshot {
    let mut snapshot = snapshot_with_run_once_code(commit);
    snapshot.twin_pairs = vec![
        TwinPair {
            a: fn_id("app/core.rs", "big"),
            b: fn_id("db/migrate/001_drop.rs", "up"),
            shared: 100,
            shingles_a: 100,
            shingles_b: 100,
        },
        TwinPair {
            a: fn_id("app/core.rs", "big"),
            b: fn_id("app/core.rs", "grows"),
            shared: 90,
            shingles_a: 100,
            shingles_b: 120,
        },
    ];
    snapshot.identity = snapshot.identity.clone().with_twins_policy(
        codemap_graph::schema::twins_policy(Language::Rust).expect("rust has a twin policy"),
    );
    snapshot
}

/// The head with a clone group and the same two callables as an exact twin
/// pair, so the pair is not new information to a reader of the clone list.
pub fn head_with_clone_and_twins() -> Snapshot {
    let mut snapshot = head_with_clone();
    snapshot.twin_pairs = vec![TwinPair {
        a: fn_id("src/a.rs", "big_a"),
        b: fn_id("src/a.rs", "big_b"),
        shared: 80,
        shingles_a: 80,
        shingles_b: 80,
    }];
    snapshot.identity = snapshot.identity.clone().with_twins_policy(
        codemap_graph::schema::twins_policy(Language::Rust).expect("rust has a twin policy"),
    );
    snapshot
}

/// A class-like definition to place in a file: name, first and last line,
/// the callables it contains, and the class it inherits from, if any.
#[derive(Debug, Clone, Copy)]
pub struct Class {
    pub rel_path: &'static str,
    pub name: &'static str,
    pub start: u32,
    pub end: u32,
    pub methods: &'static [&'static str],
    pub parent: Option<&'static str>,
}

pub fn class_id(rel_path: &str, name: &str) -> String {
    node_id(
        Language::Rust,
        NodeKind::Type(TypeKind::Struct),
        rel_path,
        &[],
        name,
    )
}

/// Add class-like nodes with their `contains` and `inherits` edges to a
/// snapshot built here, keeping the report counts the validator checks.
pub fn with_classes(mut snapshot: Snapshot, classes: &[Class]) -> Snapshot {
    let identity = snapshot.identity.clone();
    for class in classes {
        let id = class_id(class.rel_path, class.name);
        snapshot.nodes.push(Node {
            python: None,
            id: id.clone(),
            kind: NodeKind::Type(TypeKind::Struct),
            name: class.name.to_string(),
            symbol_key: symbol_key(Language::Rust, class.rel_path, &[], class.name),
            provenance: provenance(&identity, class.rel_path, class.start, class.end),
        });
        for method in class.methods {
            let to = fn_id(class.rel_path, method);
            let prov = provenance(&identity, class.rel_path, class.start, class.start);
            snapshot.edges.push(Edge {
                id: codemap_graph::schema::edge_id(
                    EdgeKind::Contains,
                    &id,
                    &to,
                    prov.span.start_byte,
                ),
                kind: EdgeKind::Contains,
                from: id.clone(),
                to,
                provenance: prov,
            });
        }
        if let Some(parent) = class.parent {
            let parent_class = classes
                .iter()
                .find(|c| c.name == parent)
                .expect("the parent is one of the classes");
            let to = class_id(parent_class.rel_path, parent);
            let prov = provenance(&identity, class.rel_path, class.start, class.start);
            snapshot.edges.push(Edge {
                id: codemap_graph::schema::edge_id(
                    EdgeKind::Inherits,
                    &id,
                    &to,
                    prov.span.start_byte + 1,
                ),
                kind: EdgeKind::Inherits,
                from: id,
                to,
                provenance: prov,
            });
        }
    }
    snapshot.report.node_counts.clear();
    for node in &snapshot.nodes {
        *snapshot
            .report
            .node_counts
            .entry(node.kind.to_string())
            .or_insert(0) += 1;
    }
    snapshot.report.edge_counts.clear();
    for edge in &snapshot.edges {
        *snapshot
            .report
            .edge_counts
            .entry(edge.kind.tag().to_string())
            .or_insert(0) += 1;
    }
    snapshot
}

/// The run-once fixture with its classes: `Core` in `app/core.rs` (lines
/// 1–95) holds `big`, `grows`, and `small` and inherits from `CoreBase`
/// (lines 97–100, no methods); `Drop` in the migration holds `up`. With
/// `twins`, the twin pairs of [`snapshot_with_twins`] too.
pub fn snapshot_with_classes(commit: &str, twins: bool) -> Snapshot {
    let base = if twins {
        snapshot_with_twins(commit)
    } else {
        snapshot_with_run_once_code(commit)
    };
    with_classes(
        base,
        &[
            Class {
                rel_path: "app/core.rs",
                name: "Core",
                start: 1,
                end: 95,
                methods: &["big", "grows", "small"],
                parent: Some("CoreBase"),
            },
            Class {
                rel_path: "app/core.rs",
                name: "CoreBase",
                start: 97,
                end: 100,
                methods: &[],
                parent: None,
            },
            Class {
                rel_path: "db/migrate/001_drop.rs",
                name: "Drop",
                start: 1,
                end: 41,
                methods: &["up"],
                parent: None,
            },
        ],
    )
}

/// [`snapshot_with_classes`] with one callable's `cc` changed, so a head
/// can differ from a base in exactly one measure.
pub fn snapshot_with_classes_and_cc(commit: &str, name: &str, cc: u32) -> Snapshot {
    let mut snapshot = snapshot_with_classes(commit, false);
    let target = fn_id("app/core.rs", name);
    for measure in &mut snapshot.measures {
        if measure.node_id == target {
            measure.cc = cc;
        }
    }
    snapshot
}

/// A snapshot measured under a policy name this crate does not read.
pub fn foreign_policy_snapshot() -> Snapshot {
    let mut snapshot = base_snapshot();
    snapshot.identity.measures_policy = Some("cc-sloc/rust/99".to_string());
    snapshot.identity.snapshot_id = snapshot.identity.compute_snapshot_id();
    snapshot
}

/// The base without any measures: a default extraction.
pub fn unmeasured_snapshot() -> Snapshot {
    let mut snapshot = base_snapshot();
    snapshot.measures.clear();
    snapshot.file_measures.clear();
    snapshot.identity.measures_policy = None;
    snapshot.identity.snapshot_id = snapshot.identity.compute_snapshot_id();
    snapshot
}

/// A measured snapshot with no files at all.
pub fn empty_snapshot() -> Snapshot {
    build(BASE_COMMIT, &[], &[])
}

fn provenance(identity: &SnapshotIdentity, rel_path: &str, start: u32, end: u32) -> Provenance {
    let start_byte = usize::try_from(start - 1).expect("fits") * 40;
    let end_byte = usize::try_from(end).expect("fits") * 40;
    Provenance {
        repository: identity.repository.id.clone(),
        commit_sha: identity.commit_sha.clone(),
        rel_path: rel_path.to_string(),
        span: Span {
            start_byte,
            end_byte,
            start_line: start,
            start_col: 0,
            end_line: end,
            end_col: 20,
        },
        file_hash: file_hash(rel_path),
        extractor_version: identity.extractor_version.clone(),
        fact_source: identity.language.structural_fact_source(),
        confidence: u8::MAX,
    }
}

fn file_hash(rel_path: &str) -> String {
    format!("{:0>64}", rel_path.len())
}

fn call_edges(
    identity: &SnapshotIdentity,
    files: &[(&str, Vec<Fn>)],
    calls: &[(&str, &str, &str)],
) -> Vec<Edge> {
    let mut edges = Vec::new();
    for (rel_path, from, to) in calls {
        let from_id = fn_id(rel_path, from);
        let to_id = fn_id(rel_path, to);
        let line = files
            .iter()
            .find(|(path, _)| path == rel_path)
            .and_then(|(_, fns)| fns.iter().find(|f| f.name == *from))
            .map_or(1, |f| f.line + 1);
        let provenance = provenance(identity, rel_path, line, line);
        edges.push(Edge {
            id: codemap_graph::schema::edge_id(
                EdgeKind::Calls,
                &from_id,
                &to_id,
                provenance.span.start_byte,
            ),
            kind: EdgeKind::Calls,
            from: from_id,
            to: to_id,
            provenance: Provenance {
                fact_source: FactSource::RustAnalyzer,
                ..provenance
            },
        });
    }
    edges
}

fn build(commit: &str, files: &[(&str, Vec<Fn>)], calls: &[(&str, &str, &str)]) -> Snapshot {
    build_for(Language::Rust, commit, files, calls)
}

/// A Go snapshot with one function under `app/`: a language whose policy
/// carries no form measures.
pub fn go_snapshot(commit: &str, cc: u32) -> Snapshot {
    build_for(
        Language::Go,
        commit,
        &[("app/example.go", vec![f("example", 1, cc, 12)])],
        &[],
    )
}

pub fn ruby_snapshot(commit: &str, cc: u32) -> Snapshot {
    build_for(
        Language::Ruby,
        commit,
        &[("app/example.rb", vec![f("example", 1, cc, 12)])],
        &[],
    )
}

/// `(params, nesting, chain, ivars)` of one callable.
pub type FormValues = (u32, u32, u32, u32);

/// A Ruby snapshot with two controller actions and one model method, each
/// with its form measures set.
pub fn ruby_snapshot_with_form(commit: &str, forms: &[(&str, FormValues)]) -> Snapshot {
    let mut snapshot = build_for(
        Language::Ruby,
        commit,
        &[
            (
                "app/controllers/widgets_controller.rb",
                vec![f("index", 1, 2, 6), f("show", 10, 3, 8)],
            ),
            ("app/models/widget.rb", vec![f("build", 1, 4, 20)]),
        ],
        &[],
    );
    for measure in &mut snapshot.measures {
        let name = measure.node_id.rsplit('#').next().unwrap_or("");
        let (params, nesting, chain, ivars) = forms
            .iter()
            .find(|(n, _)| *n == name)
            .map_or((0, 0, 0, 0), |(_, form)| *form);
        measure.params = Some(params);
        measure.nesting = Some(nesting);
        measure.chain = Some(chain);
        measure.ivars = Some(ivars);
    }
    snapshot
}

fn build_for(
    language: Language,
    commit: &str,
    files: &[(&str, Vec<Fn>)],
    calls: &[(&str, &str, &str)],
) -> Snapshot {
    let kind = if language == Language::Ruby {
        NodeKind::Method
    } else {
        NodeKind::Function
    };
    let identity = identity_for(language, commit);
    let form = codemap_graph::schema::form_measures_available(language);

    let mut nodes = Vec::new();
    let mut measures = Vec::new();
    let mut file_measures = Vec::new();
    let mut records = Vec::new();
    for (rel_path, fns) in files {
        // Six trailing lines after the last callable; four top-level source
        // lines, one blank, the rest comment, so file sloc exceeds the sum
        // of callable sloc and the classes add up.
        let all_lines = fns.iter().map(|f| f.line + f.loc - 1).max().unwrap_or(0) + 6;
        let source_lines = fns.iter().map(|f| f.sloc).sum::<u32>() + 4;
        let file_id = node_id(language, NodeKind::File, rel_path, &[], "");
        nodes.push(Node {
            python: None,
            id: file_id.clone(),
            kind: NodeKind::File,
            name: (*rel_path).to_string(),
            symbol_key: format!("{}:file:{rel_path}", language.tag()),
            provenance: provenance(&identity, rel_path, 1, all_lines),
        });
        file_measures.push(FileMeasure {
            file_id,
            loc: all_lines,
            sloc: source_lines,
            blank: 1,
            comment: all_lines - source_lines - 1,
            clone_sloc: 0,
        });
        records.push(FileRecord {
            rel_path: (*rel_path).to_string(),
            sha256: file_hash(rel_path),
            byte_len: usize::try_from(all_lines).expect("fits") * 40,
        });
        for fun in fns {
            let id = node_id(language, kind, rel_path, &[], fun.name);
            nodes.push(Node {
                python: None,
                id: id.clone(),
                kind,
                name: fun.name.to_string(),
                symbol_key: symbol_key(language, rel_path, &[], fun.name),
                provenance: provenance(&identity, rel_path, fun.line, fun.line + fun.loc - 1),
            });
            measures.push(Measure {
                node_id: id,
                cc: fun.cc,
                sloc: fun.sloc,
                loc: fun.loc,
                clone_group_ids: Vec::new(),
                params: form.then_some(0),
                nesting: form.then_some(0),
                chain: form.then_some(0),
                ivars: (language == Language::Ruby).then_some(0),
            });
        }
    }
    let edges = call_edges(&identity, files, calls);

    let mut node_counts = BTreeMap::new();
    for node in &nodes {
        *node_counts.entry(node.kind.to_string()).or_insert(0) += 1;
    }
    let mut edge_counts = BTreeMap::new();
    for edge in &edges {
        *edge_counts.entry(edge.kind.tag().to_string()).or_insert(0) += 1;
    }
    Snapshot {
        identity,
        report: Report {
            files_scanned: records.len(),
            files_skipped: 0,
            parse_errors: 0,
            parse_error_files: BTreeMap::new(),
            first_parse_errors: BTreeMap::new(),
            node_counts,
            edge_counts,
            call_sites_by_category: BTreeMap::new(),
            import_ref_nodes: 0,
            observations: BTreeMap::new(),
        },
        files: records,
        nodes,
        edges,
        call_sites: Vec::new(),
        measures,
        file_measures,
        clone_groups: Vec::new(),
        twin_pairs: Vec::new(),
    }
}

fn identity_for(language: Language, commit: &str) -> SnapshotIdentity {
    SnapshotIdentity::build(
        language,
        RepoIdentity {
            id: REPOSITORY.to_string(),
            origin: Some(REPOSITORY.to_string()),
            is_canonical: true,
        },
        commit.to_string(),
        format!("tree-{commit}"),
        // A semantic identity, so the head may carry a calls edge.
        match language {
            Language::Ruby => ToolVersions::syntax_ruby(),
            Language::Go => ToolVersions::syntax_go(),
            Language::Rust | Language::Php => ToolVersions::semantic(
                "rust-analyzer 1".into(),
                "rustc 1".into(),
                "cargo 1".into(),
                "triple".into(),
                None,
            ),
            Language::Cpp | Language::JsTs | Language::Python => {
                panic!("cpp has no measures identity")
            }
        },
        false,
    )
    .with_measures_policy(codemap_graph::schema::measures_policy(language).unwrap())
}
