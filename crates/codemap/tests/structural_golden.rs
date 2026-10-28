//! P1 structural correctness gate: assert specific expected facts in the
//! extracted corpus snapshot.

use std::path::PathBuf;

use codemap::schema::{EdgeKind, NodeKind};
use codemap::{ExtractOptions, extract};

mod common;

#[test]
fn corpus_has_expected_structural_facts() {
    let files = common::corpus_files();
    let repo = common::make_repo_with(&files).expect("create corpus repo");
    let snapshot = extract(&ExtractOptions {
        path: PathBuf::from(&repo),
        calls_required: false,
        measures: false,
        twins: false,
        language: codemap::schema::Language::Rust,
    })
    .expect("extract corpus");

    // 1. The Repository struct exists with the correct symbol key.
    let repository = snapshot
        .nodes
        .iter()
        .find(|n| {
            n.kind == NodeKind::Type(codemap::schema::TypeKind::Struct)
                && n.name == "Repository"
                && n.symbol_key == "rust:types.rs#Repository"
        })
        .expect("Repository struct node");
    assert_eq!(repository.id, "rust:type:types.rs#Repository");

    // 2. A `contains` edge links the modules.rs file to the `outer` module.
    let file_id = "rust:file:modules.rs";
    let outer_id = "rust:module:modules.rs#outer";
    let contains_to_outer = snapshot
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Contains && e.from == file_id && e.to == outer_id);
    assert!(contains_to_outer, "file must contain outer module");

    // 3. An `imports` edge links imports.rs to at least one ImportRef node.
    let imports_file = "rust:file:imports.rs";
    let imports_edge = snapshot
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Imports && e.from == imports_file);
    assert!(imports_edge, "imports.rs must have imports edges");

    // 4. In-repo trait impl produces an `implements` edge.
    let person_id = "rust:type:traits.rs#Person";
    let greeter_id = "rust:trait:traits.rs#Greeter";
    let implements_greeter = snapshot
        .edges
        .iter()
        .any(|e| e.kind == EdgeKind::Implements && e.from == person_id && e.to == greeter_id);
    assert!(
        implements_greeter,
        "impl Greeter for Person must produce an implements edge"
    );

    // 5. External-trait impl does NOT produce an implements edge.
    let implements_display = snapshot.edges.iter().any(|e| {
        e.kind == EdgeKind::Implements
            && e.from == person_id
            && e.to.starts_with("rust:trait:")
            && e.to.ends_with("Display")
    });
    assert!(
        !implements_display,
        "impl Display for Person must NOT produce an in-repo implements edge"
    );

    // 6. Syntax mode emits zero canonical `calls` edges.
    let calls_count = snapshot
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .count();
    assert_eq!(calls_count, 0, "syntax mode must emit zero calls edges");

    // 7. Call-site observations are non-empty.
    assert!(
        !snapshot.call_sites.is_empty(),
        "call_sites observations must be non-empty"
    );
}
