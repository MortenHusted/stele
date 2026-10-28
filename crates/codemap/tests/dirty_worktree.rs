//! Dirty-worktree contract. Default extraction fails before parsing when
//! the worktree has uncommitted changes; `extract_worktree` reads the tree
//! as it is, marks the snapshot dirty, and identifies it by its own tree.

use std::path::PathBuf;

use codemap::{ExtractOptions, extract, extract_worktree, to_json};

mod common;

fn options(repo: &std::path::Path) -> ExtractOptions {
    ExtractOptions {
        path: PathBuf::from(repo),
        calls_required: false,
        measures: false,
        twins: false,
        language: codemap::schema::Language::Rust,
    }
}

#[test]
fn dirty_worktree_is_rejected() {
    let files = common::corpus_files();
    let repo = common::make_repo_with(&files).expect("create corpus repo");

    // First extraction on a clean worktree succeeds.
    assert!(
        extract(&options(&repo)).is_ok(),
        "clean worktree must extract"
    );

    // Introduce an uncommitted change.
    let new_file = repo.join("uncommitted.rs");
    std::fs::write(&new_file, "// dirty\n").expect("write uncommitted change");

    // Second extraction must fail before parsing because the worktree is dirty.
    let err = extract(&options(&repo)).expect_err("dirty worktree must fail extraction");

    let msg = format!("{err:#}");
    assert!(
        msg.contains("worktree is dirty") || msg.contains("clean-worktree check failed"),
        "error should report dirty worktree, got: {msg}"
    );
}

#[test]
fn worktree_extraction_on_a_clean_tree_is_byte_identical_to_clean_extraction() {
    let files = common::corpus_files();
    let repo = common::make_repo_with(&files).expect("create corpus repo");

    let clean = extract(&options(&repo)).expect("clean extraction");
    let worktree = extract_worktree(&options(&repo)).expect("worktree extraction");

    assert!(!worktree.identity.dirty);
    assert_eq!(
        to_json(&clean).expect("serialize clean"),
        to_json(&worktree).expect("serialize worktree"),
        "a clean tree read from disk is the same snapshot as the commit"
    );
}

#[test]
fn worktree_extraction_reads_uncommitted_changes_honestly() {
    let files = common::corpus_files();
    let repo = common::make_repo_with(&files).expect("create corpus repo");
    let clean = extract(&options(&repo)).expect("clean extraction");

    let edited = "pub fn edited_on_disk() {}\n";
    let added = "pub fn added_on_disk() {}\n";
    let edited_path = files
        .iter()
        .map(|(rel, _)| rel.clone())
        .find(|rel| {
            std::path::Path::new(rel)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
        })
        .expect("a rust corpus file");
    std::fs::write(repo.join(&edited_path), edited).expect("edit tracked file");
    std::fs::write(repo.join("added.rs"), added).expect("add untracked file");

    let dirty = extract_worktree(&options(&repo)).expect("worktree extraction");

    assert!(
        dirty.identity.dirty,
        "the snapshot says it read a dirty tree"
    );
    assert_eq!(
        dirty.identity.commit_sha, clean.identity.commit_sha,
        "the commit the tree grew from is still named"
    );
    assert_ne!(
        dirty.identity.tree_sha, clean.identity.tree_sha,
        "a dirty tree has its own identity"
    );
    assert_ne!(
        dirty.identity.snapshot_id, clean.identity.snapshot_id,
        "a dirty snapshot never collides with the commit's snapshot"
    );

    let file = |rel: &str| {
        dirty
            .files
            .iter()
            .find(|f| f.rel_path == rel)
            .unwrap_or_else(|| panic!("{rel} is inventoried"))
    };
    assert_eq!(
        file(&edited_path).sha256,
        codemap::schema::sha256_hex(edited.as_bytes()),
        "bytes come from disk, not the commit"
    );
    assert_eq!(
        file("added.rs").sha256,
        codemap::schema::sha256_hex(added.as_bytes()),
        "untracked files are inventoried"
    );
    assert!(
        dirty.nodes.iter().any(|n| n.name == "added_on_disk"),
        "facts are extracted from the untracked file"
    );

    codemap::validate::validate(&dirty).expect("a dirty snapshot validates");

    let again = extract_worktree(&options(&repo)).expect("second worktree extraction");
    assert_eq!(
        to_json(&dirty).expect("serialize first"),
        to_json(&again).expect("serialize second"),
        "worktree extraction is byte-deterministic for the same content"
    );
}

#[test]
fn worktree_extraction_drops_deleted_and_ignored_files() {
    let files = vec![
        ("kept.rs".to_string(), "pub fn kept() {}\n".to_string()),
        ("gone.rs".to_string(), "pub fn gone() {}\n".to_string()),
        (".gitignore".to_string(), "hidden.rs\n".to_string()),
    ];
    let repo = common::make_repo_with(&files).expect("create repo");
    std::fs::remove_file(repo.join("gone.rs")).expect("delete tracked file");
    std::fs::write(repo.join("hidden.rs"), "pub fn hidden() {}\n").expect("write ignored file");

    let snapshot = extract_worktree(&options(&repo)).expect("worktree extraction");

    let inventory: Vec<&str> = snapshot.files.iter().map(|f| f.rel_path.as_str()).collect();
    assert_eq!(inventory, vec!["kept.rs"]);
}
