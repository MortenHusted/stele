//! Independently labelled structural oracle; requires no Node or dependencies.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codemap::schema::{EdgeKind, Language, NodeKind};
use codemap::{ExtractOptions, Snapshot, extract, extract_at_commit, extract_worktree};
use serde::Deserialize;

mod common;

fn options(root: &Path) -> ExtractOptions {
    ExtractOptions {
        path: root.to_owned(),
        language: Language::JsTs,
        calls_required: false,
        measures: false,
        twins: false,
    }
}

fn fixture_files() -> Vec<(String, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/js_ts_corpus");
    let mut files: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_str().unwrap().to_owned(),
                fs::read_to_string(entry.path()).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn assert_structure(snapshot: &Snapshot) {
    assert_eq!(snapshot.identity.language, Language::JsTs);
    assert_eq!(
        snapshot.identity.structure_policy.as_deref(),
        Some("spelled/js_ts/1")
    );
    assert!(!snapshot.identity.is_semantic());
    assert!(
        snapshot
            .edges
            .iter()
            .all(|edge| edge.kind != EdgeKind::Calls)
    );
    assert!(
        snapshot
            .nodes
            .iter()
            .all(|node| node.id.starts_with("js_ts:"))
    );
    codemap::validate(snapshot).unwrap();
}

#[derive(Deserialize)]
struct Oracle {
    definitions: BTreeMap<String, BTreeSet<String>>,
}

#[test]
fn mixed_dialects_match_the_independent_structure_oracle() {
    let files = fixture_files();
    let repo = common::make_repo_with(&files).unwrap();
    let snapshot = extract(&options(&repo)).unwrap();
    assert_structure(&snapshot);
    let cpp = extract(&ExtractOptions {
        language: Language::Cpp,
        ..options(&repo)
    })
    .unwrap();
    assert_eq!(snapshot.identity.repository, cpp.identity.repository);
    assert_eq!(snapshot.identity.commit_sha, cpp.identity.commit_sha);
    assert_eq!(snapshot.identity.tree_sha, cpp.identity.tree_sha);
    assert!(cpp.nodes.iter().all(|node| node.id.starts_with("cpp:")));
    assert!(
        snapshot
            .nodes
            .iter()
            .all(|node| cpp.nodes.iter().all(|other| other.id != node.id))
    );
    assert_eq!(snapshot.report.parse_errors, 0);
    let oracle: Oracle =
        serde_json::from_str(&fs::read_to_string(repo.join("expected.json")).unwrap()).unwrap();
    let mut actual: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for node in &snapshot.nodes {
        if matches!(node.kind, NodeKind::File | NodeKind::ImportRef) {
            continue;
        }
        actual
            .entry(node.provenance.rel_path.clone())
            .or_default()
            .insert(format!(
                "{}:{}",
                node.kind.tag(),
                node.symbol_key.split_once('#').unwrap().1
            ));
    }
    assert_eq!(actual, oracle.definitions);
    assert_eq!(snapshot.call_sites.len(), 11);
    let imports: BTreeSet<_> = snapshot
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::ImportRef)
        .map(|node| node.name.as_str())
        .collect();
    assert!(imports.contains("import type { Shape } from \"./types\";"));
    assert!(imports.contains("export type { Shape } from \"./types\";"));
    assert!(!imports.iter().any(|name| name.contains("trap")));
    let again = extract(&options(&repo)).unwrap();
    assert_eq!(
        codemap::to_json(&snapshot).unwrap(),
        codemap::to_json(&again).unwrap()
    );
    for (file, source) in files {
        if !oracle.definitions.contains_key(&file) {
            continue;
        }
        let raw = codemap::js_ts_syntax::extract_file(&file, source.as_bytes()).unwrap();
        for body in &raw.bodies {
            assert!(matches!(
                raw.defs[body.definition].kind,
                NodeKind::Function | NodeKind::Method
            ));
            assert!(
                body.callable.start_byte <= body.body.start_byte
                    && body.body.end_byte <= body.callable.end_byte
            );
            assert_ne!(&source[body.body.start_byte..body.body.end_byte], "");
        }
        if file == "api.ts" {
            let unowned = raw
                .call_anchors
                .iter()
                .filter(|call| call.caller_body.is_none())
                .count();
            assert_eq!(unowned, 2, "field initializer and default parameter");
        }
    }
}

#[test]
fn inventories_share_explicit_exclusions_and_policy_revision() {
    let repo = common::make_repo_with(&[
        (
            "src/entry.TSX".into(),
            "export const Entry = () => <div/>;".into(),
        ),
        (
            "output/generated.js".into(),
            "function generated() {}".into(),
        ),
        ("dist/real.ts".into(), "function real() {}".into()),
        (
            "node_modules/dep/index.ts".into(),
            "function dependency() {}".into(),
        ),
        (
            ".yarn/releases/yarn.cjs".into(),
            "function dependency() {}".into(),
        ),
        (
            ".codemap-js-ts.json".into(),
            "{\"exclude_directories\":[\"output\"]}".into(),
        ),
        ("helper.py".into(), "def foreign(): pass".into()),
    ])
    .unwrap();
    let before = git(&repo, &["rev-parse", "HEAD"]);
    let clean = extract(&options(&repo)).unwrap();
    let names: Vec<_> = clean
        .files
        .iter()
        .map(|file| file.rel_path.as_str())
        .collect();
    assert_eq!(names, ["dist/real.ts", "src/entry.TSX"]);
    assert_eq!(
        codemap::to_json(&clean).unwrap(),
        codemap::to_json(&extract_worktree(&options(&repo)).unwrap()).unwrap()
    );
    assert_eq!(
        codemap::to_json(&clean).unwrap(),
        codemap::to_json(&extract_at_commit(&repo, &before, Language::JsTs).unwrap()).unwrap()
    );
    fs::write(
        repo.join(".codemap-js-ts.json"),
        "{\"exclude_directories\":[]}",
    )
    .unwrap();
    let dirty = extract_worktree(&options(&repo)).unwrap();
    assert!(dirty.identity.dirty);
    assert_eq!(dirty.files.len(), 3);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "change source exclusions"]);
    assert_eq!(extract(&options(&repo)).unwrap().files.len(), 3);
    assert_eq!(
        extract_at_commit(&repo, &before, Language::JsTs)
            .unwrap()
            .files
            .len(),
        2
    );
    fs::write(
        repo.join("src/entry.TSX"),
        "export const Entry = () => <div/>; export const Other = () => <span/>;",
    )
    .unwrap();
    git(&repo, &["add", "src/entry.TSX"]);
    git(&repo, &["commit", "-qm", "TSX-only structure change"]);
    fs::write(repo.join("helper.py"), "def unrelated(): pass").unwrap();
    git(&repo, &["add", "helper.py"]);
    git(&repo, &["commit", "-qm", "foreign-only change"]);
    let out = tempfile::tempdir().unwrap();
    let summary = codemap::history::walk(
        &codemap::history::HistoryOptions {
            #[cfg(unix)]
            python_parser: None,
            repo,
            tip: "HEAD".into(),
            selector: codemap::history::Selector::FirstParentSource,
            limit: None,
            out_dir: out.path().to_owned(),
            language: Language::JsTs,
            semantic: false,
            measures: false,
            twins: false,
        },
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(
        summary.commits, 3,
        "policy-only and TSX-only changes affect structure; foreign-only changes do not"
    );
}

#[test]
fn syntax_reproduces_across_clones_and_refuses_unavailable_modes() {
    let repo = common::make_repo_with(&fixture_files()).unwrap();
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/js-ts.git",
        ],
    );
    let first = extract(&options(&repo)).unwrap();
    let clone = tempfile::tempdir().unwrap();
    git(
        clone.path(),
        &[
            "clone",
            "--quiet",
            "--no-local",
            "--no-hardlinks",
            repo.to_str().unwrap(),
            "source",
        ],
    );
    let root = clone.path().join("source");
    git(
        &root,
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.invalid/js-ts.git",
        ],
    );
    let second = extract(&options(&root)).unwrap();
    assert_eq!(
        codemap::to_json(&first).unwrap(),
        codemap::to_json(&second).unwrap()
    );
    let mut unavailable = options(&root);
    unavailable.calls_required = true;
    assert!(extract(&unavailable).is_err());
    unavailable.calls_required = false;
    unavailable.measures = true;
    assert!(extract(&unavailable).is_err());
    assert!(codemap::extract_at_commit_semantic(&root, "HEAD", Language::JsTs).is_err());
    let raw = codemap::js_ts_syntax::extract_file("bad.ts", b"export function broken( {").unwrap();
    assert!(raw.parse_errors > 0);
    assert!(codemap::js_ts_syntax::extract_file("invalid.js", &[255]).is_err());
}

#[test]
fn ignored_policy_cannot_change_worktree_identity_or_facts() {
    let repo = common::make_repo_with(&[
        ("src/entry.js".into(), "export function entry() {}".into()),
        (".gitignore".into(), ".codemap-js-ts.json\n".into()),
    ])
    .unwrap();
    let before = extract_worktree(&options(&repo)).unwrap();
    fs::write(
        repo.join(".codemap-js-ts.json"),
        "{\"exclude_directories\":[\"src\"]}",
    )
    .unwrap();
    let after = extract_worktree(&options(&repo)).unwrap();
    assert_eq!(
        codemap::to_json(&before).unwrap(),
        codemap::to_json(&after).unwrap()
    );
}

#[test]
fn original_unicode_bytes_and_parser_identity_are_enforced() {
    let source = "const label = '🦄'; export function π() {} export function run() { π(); }";
    let raw = codemap::js_ts_syntax::extract_file("unicode.ts", source.as_bytes()).unwrap();
    assert_eq!(raw.calls.len(), 1);
    let call = &raw.call_anchors[0];
    assert_eq!(
        &source.as_bytes()[call.callee.start_byte..call.callee.end_byte],
        "π".as_bytes()
    );
    assert_eq!(&source[call.span.start_byte..call.span.end_byte], "π()");
    assert!(call.caller_body.is_some());
    let repo = common::make_repo_with(&[("unicode.ts".into(), source.into())]).unwrap();
    let mut snapshot = extract(&options(&repo)).unwrap();
    snapshot.identity.tools.tree_sitter_javascript.clear();
    snapshot.identity.snapshot_id = snapshot.identity.compute_snapshot_id();
    assert!(codemap::validate(&snapshot).is_err());
}

#[test]
fn default_export_names_follow_tokens_and_policy_paths_are_validated() {
    for source in [
        "export default function() {}",
        "export /*comment*/ default function() {}",
        "export default (() => {});",
    ] {
        let raw = codemap::js_ts_syntax::extract_file("default.js", source.as_bytes()).unwrap();
        assert_eq!(raw.parse_errors, 0, "{source}: {:?}", raw.defs);
        assert_eq!(raw.defs.len(), 1, "{source}");
        assert_eq!(raw.defs[0].name, "default");
    }
    let unsupported =
        codemap::js_ts_syntax::extract_file("default.js", b"export\n default function() {}")
            .unwrap();
    assert!(
        unsupported.parse_errors > 0,
        "known pinned grammar recovery remains visible"
    );
    assert!(unsupported.defs.iter().all(|def| def.name != "default"));
    let repo = common::make_repo_with(&[("entry.js".into(), "export function entry() {}".into())])
        .unwrap();
    for policy in [
        "{\"exclude_directories\":[\"../src\"]}",
        "{\"exclude_directories\":[\"/src\"]}",
        "{\"unexpected\":true}",
    ] {
        fs::write(repo.join(".codemap-js-ts.json"), policy).unwrap();
        assert!(extract_worktree(&options(&repo)).is_err());
    }
}

#[test]
fn transparent_typescript_wrappers_preserve_callable_binding_names() {
    for source in [
        "const f = (() => {});",
        "const f = (() => {}) as () => void;",
        "const f = (() => {}) satisfies (() => void);",
        "const f = (() => {})!;",
        "const f = <(() => void)>(() => {});",
    ] {
        let raw = codemap::js_ts_syntax::extract_file("wrapped.ts", source.as_bytes()).unwrap();
        assert_eq!(raw.parse_errors, 0, "{source}");
        assert_eq!(raw.defs.len(), 1, "{source}");
        assert_eq!(raw.defs[0].name, "f", "{source}");
        let body = &raw.bodies[0];
        let name = body.name.unwrap();
        assert_eq!(&source[name.start_byte..name.end_byte], "f");
        assert_eq!(&source[body.body.start_byte..body.body.end_byte], "{}");
        assert_eq!(
            &source[body.callable.start_byte..body.callable.end_byte],
            "() => {}"
        );
    }
}

#[test]
#[cfg(unix)]
fn source_links_are_refused_in_working_and_committed_inventories() {
    let repo = common::make_repo_with(&[("entry.js".into(), "export function entry() {}".into())])
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(
        outside.path().join("outside.js"),
        "export function outside() {}",
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path().join("outside.js"), repo.join("linked.js")).unwrap();
    assert!(extract_worktree(&options(&repo)).is_err());
    git(&repo, &["add", "linked.js"]);
    git(&repo, &["commit", "-qm", "source link"]);
    assert!(extract(&options(&repo)).is_err());
    assert!(extract_at_commit(&repo, "HEAD", Language::JsTs).is_err());
    fs::remove_file(repo.join("linked.js")).unwrap();
    fs::create_dir(outside.path().join("src")).unwrap();
    fs::write(
        outside.path().join("src/inside.ts"),
        "export function alias() {}",
    )
    .unwrap();
    let ancestor_repo =
        common::make_repo_with(&[("src/inside.ts".into(), "export function inside() {}".into())])
            .unwrap();
    fs::remove_file(ancestor_repo.join("src/inside.ts")).unwrap();
    fs::remove_dir(ancestor_repo.join("src")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("src"), ancestor_repo.join("src")).unwrap();
    assert!(extract_worktree(&options(&ancestor_repo)).is_err());
    let policy_repo =
        common::make_repo_with(&[("entry.js".into(), "export function entry() {}".into())])
            .unwrap();
    fs::write(
        outside.path().join("policy.json"),
        "{\"exclude_directories\":[]}",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("policy.json"),
        policy_repo.join(".codemap-js-ts.json"),
    )
    .unwrap();
    assert!(extract_worktree(&options(&policy_repo)).is_err());
    git(&policy_repo, &["add", "-A"]);
    git(&policy_repo, &["commit", "-qm", "policy link"]);
    assert!(extract(&options(&policy_repo)).is_err());
    assert!(extract_at_commit(&policy_repo, "HEAD", Language::JsTs).is_err());
}
