//! A path anchor is present when a snapshot lists it or the working tree
//! holds it, untracked included and ignored left out; a symbol anchor is
//! looked up in the snapshot of its language.

use std::path::Path;
use std::process::Command;

use codemap_graph::Snapshot;
use lemmaspec::EvidenceKind;
use stele_plan::{Code, identity::Repo, languages};

/// The synthetic Rust snapshot of a `src/main.rs` with the functions
/// `entry`, `work`, `demo` and `unused`.
fn snapshot() -> Snapshot {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../codemap-topology/tests/fixtures/rust.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.email=stele@example.com",
            "-c",
            "user.name=stele",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

/// A committed README and ignore file, an ignored build output, and an
/// untracked note written beside the change.
fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (path, text) in [
        ("README.md", "# Greeter\n"),
        (".gitignore", "target/\n"),
        ("target/out.txt", "built\n"),
    ] {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-qm", "start"]);
    std::fs::create_dir(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("docs/next.md"), "# Next\n").unwrap();
    dir
}

#[test]
fn a_path_anchor_is_in_a_snapshot_or_the_working_tree_but_not_ignored_or_missing() {
    let dir = repository();
    let repo = Repo::of_catalog(dir.path()).unwrap();
    let code = Code {
        snapshots: vec![snapshot()],
        working_tree: Some(repo.working_tree().unwrap()),
    };
    let snapshot_id = code.snapshots[0].identity.snapshot_id.clone();
    let tree_identity = repo.working_tree_identity().unwrap();

    let identity = |anchor: &str| {
        code.find(anchor).unwrap().map(|basis| {
            assert_eq!(basis.kind, EvidenceKind::Snapshot);
            assert_eq!(basis.source, anchor);
            basis.identity.unwrap()
        })
    };
    // In the snapshot, though not on disk: the snapshot identifies it.
    assert_eq!(identity("src/main.rs"), Some(snapshot_id));
    // Not source code, committed or not: the working tree identifies it.
    assert_eq!(identity("README.md"), Some(tree_identity.clone()));
    assert_eq!(identity("docs/next.md"), Some(tree_identity));
    assert_eq!(identity("target/out.txt"), None);
    assert_eq!(identity("docs/missing.md"), None);

    let without_tree = Code {
        snapshots: vec![snapshot()],
        working_tree: None,
    };
    assert_eq!(without_tree.find("README.md").unwrap(), None);
}

#[test]
fn a_symbol_anchor_is_looked_up_in_the_snapshot_of_its_language() {
    let code = Code {
        snapshots: vec![snapshot()],
        working_tree: None,
    };
    assert!(code.find("rust:src/main.rs#entry").unwrap().is_some());
    assert_eq!(code.find("rust:src/main.rs#gone").unwrap(), None);
    assert_eq!(
        code.find("go:greet/greet.go#greet::Hello").unwrap_err(),
        "anchor go:greet/greet.go#greet::Hello is go code, but the snapshot is rust; check it against a go snapshot"
    );
    let none = Code::default();
    assert_eq!(
        none.find("go:greet/greet.go#greet::Hello").unwrap_err(),
        "anchor go:greet/greet.go#greet::Hello is go code, but no go snapshot was read"
    );
}

#[test]
fn the_languages_are_the_symbol_anchors_or_rust() {
    let anchors = [
        "rust:src/lib.rs#greet",
        "go:greet/greet.go#greet::Hello",
        "rust:src/lib.rs#farewell",
        "README.md",
    ];
    assert_eq!(
        languages(anchors).into_iter().collect::<Vec<_>>(),
        ["go", "rust"]
    );
    assert_eq!(
        languages(["README.md"]).into_iter().collect::<Vec<_>>(),
        ["rust"]
    );
    assert_eq!(languages([]).into_iter().collect::<Vec<_>>(), ["rust"]);
}
