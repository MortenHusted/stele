//! G1 Go adapter gates: language purity (bidirectional), the Go
//! toolchain's inventory rule (no `vendor/`, no `testdata/`), structural
//! correctness of package-qualified facts, embedding resolution, syntax-only
//! call discipline, malformed-file reporting, and byte determinism across
//! independent checkouts.

use std::collections::BTreeSet;
use std::path::PathBuf;

use codemap::schema::{CallSiteCategory, EdgeKind, Language, NodeKind, TypeKind};
use codemap::{ExtractOptions, extract, to_json};

mod common;

fn extract_go(repo: &std::path::Path) -> codemap::Snapshot {
    extract(&ExtractOptions {
        path: PathBuf::from(repo),
        calls_required: false,
        measures: false,
        twins: false,
        language: Language::Go,
    })
    .expect("go extraction")
}

#[test]
fn only_go_sources_outside_vendor_and_testdata_produce_facts() {
    let repo = common::make_repo_with(&common::go_corpus_files()).expect("create go repo");
    let snapshot = extract_go(&repo);

    for node in &snapshot.nodes {
        assert!(node.id.starts_with("go:"), "non-go node id: {}", node.id);
    }
    let files: BTreeSet<&str> = snapshot.files.iter().map(|f| f.rel_path.as_str()).collect();
    assert_eq!(
        files,
        BTreeSet::from([
            "broken.go",
            "cmd/tool/main.go",
            "internal/svc/svc.go",
            "main.go",
            "pkg/model/model.go",
            "pkg/model/model_test.go",
        ]),
        "vendor and testdata are outside the Go source tree; other languages never enter"
    );
    for node in &snapshot.nodes {
        assert!(
            !node.provenance.rel_path.ends_with("helper.rs")
                && !node.provenance.rel_path.ends_with("helper.rb")
                && !node.provenance.rel_path.ends_with("helper.php"),
            "cross-language provenance leak: {}",
            node.provenance.rel_path
        );
        assert!(
            !node.provenance.rel_path.starts_with("vendor/")
                && !node.provenance.rel_path.contains("/testdata/"),
            "excluded directory leaked into facts: {}",
            node.provenance.rel_path
        );
    }
    assert_eq!(snapshot.identity.language, Language::Go);
    assert_eq!(snapshot.identity.tools.tree_sitter_go, "0.25.0");
    assert!(snapshot.identity.tools.gopls.is_none());
}

#[test]
#[allow(clippy::too_many_lines)] // a labelled golden fixture asserts as one table
fn go_structure_matches_the_labelled_corpus() {
    let repo = common::make_repo_with(&common::go_corpus_files()).expect("create go repo");
    let snapshot = extract_go(&repo);

    let ids: BTreeSet<&str> = snapshot.nodes.iter().map(|n| n.id.as_str()).collect();
    for expected in [
        "go:file:main.go",
        "go:file:cmd/tool/main.go",
        "go:file:internal/svc/svc.go",
        "go:file:pkg/model/model.go",
        "go:file:pkg/model/model_test.go",
        "go:file:broken.go",
        // One Module per file, named by its package clause.
        "go:module:main.go#main",
        "go:module:cmd/tool/main.go#main",
        "go:module:internal/svc/svc.go#svc",
        "go:module:pkg/model/model.go#model",
        // Definitions are qualified by package; methods by receiver type.
        "go:function:main.go#main::main",
        "go:function:cmd/tool/main.go#main::main",
        "go:function:cmd/tool/main.go#main::run",
        "go:type:internal/svc/svc.go#svc::Named",
        "go:method:internal/svc/svc.go#svc::Named::Name",
        "go:type:internal/svc/svc.go#svc::Runner",
        "go:method:internal/svc/svc.go#svc::Runner::Run",
        "go:type:internal/svc/svc.go#svc::Base",
        "go:method:internal/svc/svc.go#svc::Base::Name",
        "go:type:internal/svc/svc.go#svc::Service",
        "go:function:internal/svc/svc.go#svc::New",
        "go:method:internal/svc/svc.go#svc::Service::Run",
        "go:function:internal/svc/svc.go#svc::Drive",
        "go:type:pkg/model/model.go#model::ID",
        "go:type:pkg/model/model.go#model::Entity",
        "go:type:pkg/model/model.go#model::Alias",
        "go:method:pkg/model/model.go#model::Entity::Key",
        "go:type:pkg/model/model.go#model::List",
        "go:method:pkg/model/model.go#model::List::Push",
        "go:function:pkg/model/model.go#model::Map",
        "go:function:pkg/model/model_test.go#model::TestKey",
    ] {
        assert!(
            ids.contains(expected),
            "missing node {expected}\nhave: {ids:?}"
        );
    }
    // Nothing is invented for what is not a definition.
    for forbidden in ["items", "helper", "name", "Mutex", "Ignored", "Vendored"] {
        assert!(
            !ids.iter().any(|id| id.ends_with(&format!("::{forbidden}"))
                || id.ends_with(&format!("#{forbidden}"))),
            "a node was invented for {forbidden}: {ids:?}"
        );
    }

    let kind_of = |id: &str| {
        snapshot
            .nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("node {id}"))
            .kind
    };
    assert_eq!(
        kind_of("go:type:internal/svc/svc.go#svc::Runner"),
        NodeKind::Type(TypeKind::Interface)
    );
    assert_eq!(
        kind_of("go:type:internal/svc/svc.go#svc::Service"),
        NodeKind::Type(TypeKind::Struct)
    );
    assert_eq!(
        kind_of("go:type:pkg/model/model.go#model::ID"),
        NodeKind::Type(TypeKind::Defined)
    );
    assert_eq!(
        kind_of("go:type:pkg/model/model.go#model::Alias"),
        NodeKind::Type(TypeKind::TypeAlias)
    );

    // Lexical containment: top-level definitions are file children, an
    // interface owns its method declarations.
    let contains: Vec<(&str, &str)> = snapshot
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains)
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect();
    assert!(contains.contains(&(
        "go:file:internal/svc/svc.go",
        "go:method:internal/svc/svc.go#svc::Base::Name",
    )));
    assert!(contains.contains(&(
        "go:type:internal/svc/svc.go#svc::Runner",
        "go:method:internal/svc/svc.go#svc::Runner::Run",
    )));

    // Embeddings resolve in-repo: same package, cross-package through the
    // import path under the root module, and an interface embedding.
    let edges: Vec<(EdgeKind, &str, &str)> = snapshot
        .edges
        .iter()
        .map(|e| (e.kind, e.from.as_str(), e.to.as_str()))
        .collect();
    assert!(
        edges.contains(&(
            EdgeKind::Inherits,
            "go:type:internal/svc/svc.go#svc::Service",
            "go:type:internal/svc/svc.go#svc::Base",
        )),
        "missing inherits Service -> Base"
    );
    assert!(
        edges.contains(&(
            EdgeKind::Inherits,
            "go:type:internal/svc/svc.go#svc::Service",
            "go:type:pkg/model/model.go#model::Entity",
        )),
        "missing inherits Service -> model.Entity"
    );
    assert!(
        edges.contains(&(
            EdgeKind::Implements,
            "go:type:internal/svc/svc.go#svc::Runner",
            "go:type:internal/svc/svc.go#svc::Named",
        )),
        "missing implements Runner -> Named"
    );
    // The standard-library embedding is an observation, and implicit
    // satisfaction (Base has Name(), so it is a Named) is never an edge.
    let unmapped: BTreeSet<&str> = snapshot
        .call_sites
        .iter()
        .filter(|c| c.category == CallSiteCategory::Unmapped)
        .map(|c| c.raw_name.as_str())
        .collect();
    assert!(unmapped.contains("embeds sync.Mutex"), "{unmapped:?}");
    assert!(
        !edges.iter().any(|(kind, from, to)| {
            *kind == EdgeKind::Implements
                && *from == "go:type:internal/svc/svc.go#svc::Base"
                && *to == "go:type:internal/svc/svc.go#svc::Named"
        }),
        "implicit interface satisfaction must never be an edge"
    );

    // Imports become ImportRef placeholders, verbatim.
    let import_refs: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::ImportRef))
        .map(|n| n.name.as_str())
        .collect();
    for expected in [
        "os",
        "example.invalid/corpus/internal/svc",
        "fmt as .",
        "embed as _",
        "github.com/x/y as y",
        "sync",
        "example.invalid/corpus/pkg/model",
        "testing",
    ] {
        assert!(
            import_refs.contains(expected),
            "missing import {expected}: {import_refs:?}"
        );
    }

    // Syntax-only: zero canonical call edges; call sites are observations
    // named by their callee.
    assert!(
        snapshot.edges.iter().all(|e| e.kind != EdgeKind::Calls),
        "syntax mode must emit zero calls edges"
    );
    let observed: BTreeSet<&str> = snapshot
        .call_sites
        .iter()
        .filter(|c| c.category == CallSiteCategory::Observed)
        .map(|c| c.raw_name.as_str())
        .collect();
    for needle in [
        "svc.New",
        "svc.Drive",
        "os.Exit",
        "run",
        "Println",
        "fmt.Println",
        "s.Name",
        "s.helper",
        "f",
        "len",
        "model.ID",
        "s.Run",
        "s.Unlock",
        "s.Lock",
        "r.Run",
        "append",
        "make",
        "t.Fatal",
    ] {
        assert!(
            observed.contains(needle),
            "missing call observation {needle}: {observed:?}"
        );
    }
}

#[test]
fn malformed_files_are_reported_not_fatal() {
    let repo = common::make_repo_with(&common::go_corpus_files()).expect("create go repo");
    let snapshot = extract_go(&repo);
    assert_eq!(snapshot.report.files_skipped, 0);
    assert!(
        snapshot.report.parse_errors > 0,
        "malformed file must surface parse errors"
    );
    assert!(snapshot.nodes.iter().any(|n| n.id == "go:file:broken.go"));
}

#[test]
fn go_extraction_is_byte_deterministic_across_checkouts() {
    let a = common::make_go_corpus_repo().expect("first checkout");
    let b = common::make_go_corpus_repo().expect("second checkout");

    let first = to_json(&extract_go(&a)).expect("serialize first");
    let second = to_json(&extract_go(&b)).expect("serialize second");

    assert_eq!(
        first, second,
        "Go snapshot bytes diverge across independent checkouts"
    );
}

#[test]
fn semantic_mode_is_refused_until_the_gopls_gate_lands() {
    let repo = common::make_repo_with(&common::go_corpus_files()).expect("create go repo");
    let error = extract(&ExtractOptions {
        path: PathBuf::from(&repo),
        calls_required: true,
        measures: false,
        twins: false,
        language: Language::Go,
    })
    .expect_err("go semantic mode is not available yet");
    assert!(format!("{error:#}").contains("gopls"), "{error:#}");
}
