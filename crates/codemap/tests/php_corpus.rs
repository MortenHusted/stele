//! P1 PHP adapter gates: language purity (bidirectional), structural
//! correctness of namespaced facts, syntax-only call discipline,
//! heritage/trait resolution, mixed HTML/PHP handling, malformed-file
//! reporting, and byte determinism across independent checkouts.

use std::collections::BTreeSet;
use std::path::PathBuf;

use codemap::schema::{EdgeKind, Language, NodeKind, TypeKind};
use codemap::{ExtractOptions, extract, to_json};

mod common;

fn extract_php(repo: &std::path::Path) -> codemap::Snapshot {
    extract(&ExtractOptions {
        path: PathBuf::from(repo),
        calls_required: false,
        measures: false,
        twins: false,
        language: Language::Php,
    })
    .expect("php extraction")
}

#[test]
fn only_php_files_produce_facts() {
    let repo = common::make_repo_with(&common::php_corpus_files()).expect("create php repo");
    let snapshot = extract_php(&repo);

    for node in &snapshot.nodes {
        assert!(node.id.starts_with("php:"), "non-php node id: {}", node.id);
    }
    for file in &snapshot.files {
        assert!(
            std::path::Path::new(&file.rel_path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("php")),
            "non-PHP file in php snapshot: {}",
            file.rel_path
        );
    }
    for node in &snapshot.nodes {
        assert!(
            !node.provenance.rel_path.ends_with("helper.rs")
                && !node.provenance.rel_path.ends_with("helper.rb")
                && !node.provenance.rel_path.ends_with("helper.go"),
            "cross-language provenance leak: {}",
            node.provenance.rel_path
        );
    }
    assert_eq!(snapshot.identity.language, Language::Php);
}

#[test]
#[allow(clippy::too_many_lines)] // a labelled golden fixture asserts as one table
fn php_structure_matches_the_labelled_corpus() {
    let repo = common::make_repo_with(&common::php_corpus_files()).expect("create php repo");
    let snapshot = extract_php(&repo);

    let ids: BTreeSet<&str> = snapshot.nodes.iter().map(|n| n.id.as_str()).collect();
    for expected in [
        "php:file:page.php",
        "php:file:src/Http/Router.php",
        "php:file:src/Model/Auditable.php",
        "php:file:src/Model/Record.php",
        "php:file:src/Model/User.php",
        "php:file:src/Util.php",
        "php:file:broken.php",
        // Unbraced namespace: namespace segments scope following
        // definitions without emitting a Module node (mirrors the Ruby
        // module-less nesting decision — the `namespace` statement is a
        // qualifier, not a lexical container).
        "php:type:src/Model/Auditable.php#App\\Model::Auditable",
        // Interface methods.
        "php:method:src/Model/Auditable.php#App\\Model::Auditable::auditLog",
        "php:trait:src/Model/Record.php#App\\Model::Timestamped",
        "php:method:src/Model/Record.php#App\\Model::Timestamped::touch",
        "php:type:src/Model/Record.php#App\\Model::Record",
        "php:type:src/Model/User.php#App\\Model::User",
        "php:method:src/Model/User.php#App\\Model::User::auditLog",
        "php:method:src/Model/User.php#App\\Model::User::find",
        // Braced namespaces produce Module nodes; their contents nest
        // lexically.
        "php:module:src/Http/Router.php#App",
        "php:module:src/Http/Router.php#App\\Http",
        "php:type:src/Http/Router.php#App::Method",
        "php:type:src/Http/Router.php#App\\Http::Router",
        "php:method:src/Http/Router.php#App\\Http::Router::dispatch",
        // Unbraced namespace: Module node for identity; following defs are
        // file-level siblings with qualified names.
        "php:module:src/Model/User.php#App\\Model",
        "php:function:src/Util.php#App\\Util::slugify",
    ] {
        assert!(
            ids.contains(expected),
            "missing node {expected}\nhave: {ids:?}"
        );
    }

    // Kinds: interface vs class vs enum vs trait.
    let kind_of = |id: &str| {
        snapshot
            .nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("node {id}"))
            .kind
    };
    assert_eq!(
        kind_of("php:type:src/Model/Auditable.php#App\\Model::Auditable"),
        NodeKind::Type(TypeKind::Interface)
    );
    assert_eq!(
        kind_of("php:type:src/Http/Router.php#App::Method"),
        NodeKind::Type(TypeKind::Enum)
    );
    assert_eq!(
        kind_of("php:type:src/Model/User.php#App\\Model::User"),
        NodeKind::Type(TypeKind::Class)
    );
    assert_eq!(
        kind_of("php:trait:src/Model/Record.php#App\\Model::Timestamped"),
        NodeKind::Trait
    );

    // Heritage edges resolve cross-file when both sides are in-repo and
    // uniquely named.
    let edges: Vec<(EdgeKind, &str, &str)> = snapshot
        .edges
        .iter()
        .map(|e| (e.kind, e.from.as_str(), e.to.as_str()))
        .collect();
    assert!(
        edges.contains(&(
            EdgeKind::Inherits,
            "php:type:src/Model/User.php#App\\Model::User",
            "php:type:src/Model/Record.php#App\\Model::Record",
        )),
        "missing inherits User -> Record"
    );
    assert!(
        edges.contains(&(
            EdgeKind::Implements,
            "php:type:src/Model/User.php#App\\Model::User",
            "php:type:src/Model/Auditable.php#App\\Model::Auditable",
        )),
        "missing implements User -> Auditable"
    );
    assert!(
        edges.contains(&(
            EdgeKind::Implements,
            "php:type:src/Model/User.php#App\\Model::User",
            "php:trait:src/Model/Record.php#App\\Model::Timestamped",
        )),
        "missing trait-use User -> Timestamped"
    );

    // Namespace-use imports become ImportRef placeholder nodes, verbatim.
    let import_refs: BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::ImportRef))
        .map(|n| n.id.as_str())
        .collect();
    assert!(
        import_refs
            .iter()
            .any(|id| id.contains("Vendor\\Pkg\\Alpha")),
        "group use member missing: {import_refs:?}"
    );
    assert!(
        import_refs
            .iter()
            .any(|id| id.contains("Vendor\\Pkg\\Beta as B")),
        "aliased group use member missing"
    );
    assert!(
        import_refs.iter().any(|id| id.contains("sprintf as fmt")),
        "aliased function use missing"
    );

    // Syntax-only: zero canonical call edges; call sites are observations.
    assert!(
        snapshot.edges.iter().all(|e| e.kind != EdgeKind::Calls),
        "syntax mode must emit zero calls edges"
    );
    let observed: BTreeSet<&str> = snapshot
        .call_sites
        .iter()
        .map(|c| c.raw_name.as_str())
        .collect();
    // Call observations record the called name for functions and the whole
    // receiver expression for member/object calls.
    for needle in ["markDirty", "Logger", "fmt", "log", "strtolower"] {
        assert!(
            observed.iter().any(|name| name.contains(needle)),
            "missing call observation containing {needle}: {observed:?}"
        );
    }
    // Heritage references never become call observations.
    assert!(!observed.iter().any(|name| name.contains("auditLog")));
}

#[test]
fn unverifiable_call_shapes_stay_observations_and_invent_nothing() {
    let repo = common::make_repo_with(&common::php_corpus_files()).expect("create php repo");
    let snapshot = extract_php(&repo);

    // The magic-dispatch file's own definitions are ordinary nodes.
    let ids: BTreeSet<&str> = snapshot.nodes.iter().map(|n| n.id.as_str()).collect();
    for expected in [
        "php:type:src/Magic.php#App::Magic",
        "php:method:src/Magic.php#App::Magic::__call",
        "php:method:src/Magic.php#App::Magic::dispatch",
    ] {
        assert!(ids.contains(expected), "missing node {expected}");
    }
    // Nothing is invented for a name the source only holds in a variable.
    for forbidden in ["$method", "$className", "undefinedOnPurpose", "APP_MAGIC"] {
        assert!(
            !ids.iter().any(|id| id.contains(forbidden)),
            "a node was invented for {forbidden}: {ids:?}"
        );
    }

    // Each shape is recorded as written, as an observation only.
    let observed: BTreeSet<&str> = snapshot
        .call_sites
        .iter()
        .map(|c| c.raw_name.as_str())
        .collect();
    for needle in [
        "$this->$method()",
        "$target->undefinedOnPurpose()",
        "call_user_func",
        "call_user_func_array",
        "define",
    ] {
        assert!(
            observed.iter().any(|name| name.contains(needle)),
            "missing observation containing {needle}: {observed:?}"
        );
    }
    // `new $className()` names no class, so it is not even an observation.
    assert!(!observed.iter().any(|name| name.contains("$className")));
    assert!(snapshot.edges.iter().all(|e| e.kind != EdgeKind::Calls));

    // `use const` is an import placeholder, never a class import.
    assert!(
        snapshot
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::ImportRef && n.id.contains("App\\Util\\LIMIT")),
        "use const must be recorded as an import placeholder"
    );
}

#[test]
fn mixed_html_php_and_malformed_files_are_reported_not_fatal() {
    let repo = common::make_repo_with(&common::php_corpus_files()).expect("create php repo");
    let snapshot = extract_php(&repo);

    // The mixed HTML/PHP file parses without errors via the mixed-mode
    // grammar entrypoint; the malformed file reports parse errors without
    // aborting extraction.
    assert_eq!(snapshot.report.files_skipped, 0);
    assert!(
        snapshot.report.parse_errors > 0,
        "malformed file must surface parse errors"
    );
    assert!(
        snapshot.nodes.iter().any(|n| n.id == "php:file:page.php"),
        "mixed file must produce a file node"
    );
}

#[test]
fn php_extraction_is_byte_deterministic_across_checkouts() {
    let a = common::make_php_corpus_repo().expect("first checkout");
    let b = common::make_php_corpus_repo().expect("second checkout");

    let first = to_json(&extract_php(&a)).expect("serialize first");
    let second = to_json(&extract_php(&b)).expect("serialize second");

    assert_eq!(
        first, second,
        "PHP snapshot bytes diverge across independent checkouts"
    );
}
