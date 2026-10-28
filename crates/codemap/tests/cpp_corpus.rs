//! C++ syntax extraction: inventory, overload identity, and no canonical calls.

use std::collections::BTreeSet;
use std::path::PathBuf;

use codemap::schema::{EdgeKind, Language, NodeKind};
use codemap::{ExtractOptions, extract};

mod common;

fn extract_cpp(repo: &std::path::Path) -> codemap::Snapshot {
    extract(&ExtractOptions {
        path: PathBuf::from(repo),
        calls_required: false,
        measures: false,
        twins: false,
        language: Language::Cpp,
    })
    .expect("cpp extraction")
}

#[test]
fn syntax_extraction_names_overloads_and_emits_no_call_edges() {
    let repo = common::make_repo_with(&[
        (
            "include/ops.hpp".into(),
            "namespace ops {\nstruct Base { virtual int id() const; };\nstruct Child : Base { int id() const override { return 1; } };\n}\n".into(),
        ),
        (
            "src/unit.cpp".into(),
            "#include \"ops.hpp\"\nnamespace ops {\nint pick(int value) { return value; }\nint pick(int value, int scale) { return value * scale; }\n}\n".into(),
        ),
        ("src/plain.c".into(), "int ignored(void) { return 0; }\n".into()),
        ("src/unit.C".into(), "int capital() { return 1; }\n".into()),
    ])
    .expect("repo");
    let snapshot = extract_cpp(&repo);
    assert_eq!(snapshot.identity.schema_version, "codemap.snapshot.v8");
    assert_eq!(
        snapshot.identity.structure_policy.as_deref(),
        Some("spelled/cpp/1"),
        "a C++ snapshot says its facts are about the spelled source"
    );
    let mut unstamped = snapshot.clone();
    unstamped.identity.structure_policy = None;
    unstamped.identity.snapshot_id = unstamped.identity.compute_snapshot_id();
    let error = codemap::validate(&unstamped).expect_err("the policy is required");
    assert!(
        format!("{error:#}").contains("structure_policy"),
        "{error:#}"
    );
    assert_eq!(snapshot.identity.tools.tree_sitter_cpp, "0.23.4");
    assert_eq!(snapshot.identity.tools.tree_sitter_go, "");
    let files: BTreeSet<_> = snapshot
        .files
        .iter()
        .map(|file| file.rel_path.as_str())
        .collect();
    assert_eq!(
        files,
        BTreeSet::from(["include/ops.hpp", "src/unit.cpp", "src/unit.C"])
    );
    let keys: BTreeSet<_> = snapshot
        .nodes
        .iter()
        .map(|node| node.symbol_key.as_str())
        .collect();
    assert!(keys.contains("cpp:src/unit.cpp#ops::pick(int)"));
    assert!(keys.contains("cpp:src/unit.cpp#ops::pick(int, int)"));
    assert!(keys.contains("cpp:include/ops.hpp#ops::Child::id() const"));
    assert!(
        snapshot
            .edges
            .iter()
            .any(|edge| edge.kind == EdgeKind::Inherits)
    );
    assert!(
        snapshot
            .edges
            .iter()
            .all(|edge| edge.kind != EdgeKind::Calls)
    );
    assert!(snapshot.nodes.iter().all(|node| match node.kind {
        NodeKind::File => node.id.starts_with("cpp:file:"),
        _ => node.id.starts_with("cpp:"),
    }));
    let again = extract_cpp(&repo);
    assert_eq!(
        codemap::to_json(&snapshot).unwrap(),
        codemap::to_json(&again).unwrap()
    );
}

#[test]
fn a_header_class_types_its_out_of_line_methods_and_declarations_are_not_calls() {
    let repo = common::make_repo_with(&[
        (
            "include/widget.hpp".into(),
            "namespace ui {\nstruct Widget { int draw() const; };\nint declared(int value);\n}\n".into(),
        ),
        (
            "src/widget.cpp".into(),
            "#include \"widget.hpp\"\nnamespace ui {\nint Widget::draw() const { return 1; }\n}\nint Ghost::run() { return 2; }\n".into(),
        ),
    ])
    .expect("repo");
    let snapshot = extract_cpp(&repo);
    let draw = snapshot
        .nodes
        .iter()
        .find(|node| node.symbol_key == "cpp:src/widget.cpp#ui::Widget::draw() const")
        .expect("the out-of-line method, owned by the header's class");
    assert_eq!(draw.kind, NodeKind::Method);
    assert!(
        snapshot.call_sites.is_empty(),
        "a declaration is not a call site: {:?}",
        snapshot.call_sites
    );
    assert_eq!(
        snapshot.report.observations,
        [
            ("declaration".to_string(), 2),
            ("unresolved_qualifier".to_string(), 1),
        ]
        .into()
    );
    codemap::validate(&snapshot).expect("valid");
}
