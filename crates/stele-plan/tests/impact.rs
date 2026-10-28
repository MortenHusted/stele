//! Which statements a change touches: diff hunks, anchors overlapping the
//! changed lines before or after it, and the changed code nothing maps.

use std::collections::BTreeMap;

use codemap_graph::{Node, NodeKind, Snapshot, schema::TypeKind};
use stele_plan::{
    Statement, StatementKind,
    examples::Example,
    impact::{AnchorChange, FileDiff, Impact, TouchedAnchor, impact, parse_hunks},
};

/// The synthetic `src/main.rs`: module `demo` spanning the file (lines
/// 1-30), and functions `entry` (2-9), `work` (11-19) and `unused` (21-29).
fn snapshot() -> Snapshot {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../codemap-topology/tests/fixtures/rust.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// `snapshot()` with a type `Greeter` in place of `unused`, spanning a
/// method `Greeter::hello` on lines 23-25.
fn with_greeter() -> Snapshot {
    let mut snapshot = snapshot();
    let unused = snapshot
        .nodes
        .iter()
        .position(|node| node.name == "unused")
        .unwrap();
    let mut greeter = snapshot.nodes.remove(unused);
    let mut hello = greeter.clone();
    rename(&mut greeter, NodeKind::Type(TypeKind::Struct), "Greeter");
    rename(&mut hello, NodeKind::Method, "Greeter::hello");
    hello.name = "hello".into();
    let span = &mut hello.provenance.span;
    (
        span.start_byte,
        span.end_byte,
        span.start_line,
        span.end_line,
    ) = (220, 260, 23, 26);
    snapshot.nodes.extend([greeter, hello]);
    snapshot
}

fn rename(node: &mut Node, kind: NodeKind, name: &str) {
    node.kind = kind;
    node.name = name.into();
    node.id = format!("rust:src/main.rs#{name}");
    node.symbol_key = format!("rust:src/main.rs#{name}");
}

fn without(name: &str) -> Snapshot {
    let mut snapshot = snapshot();
    snapshot.nodes.retain(|node| node.name != name);
    snapshot
}

fn definition(id: &str, realized_by: &[&str]) -> (String, Statement) {
    (
        id.into(),
        Statement {
            id: id.into(),
            text: format!("{id} holds."),
            file: "intent/catalog.stele".into(),
            line: 1,
            kind: StatementKind::Definition {
                refines: "goal".into(),
            },
            realized_by: realized_by
                .iter()
                .map(|anchor| (*anchor).to_string())
                .collect(),
        },
    )
}

fn catalog(statements: &[(&str, &[&str])]) -> BTreeMap<String, Statement> {
    statements
        .iter()
        .map(|(id, anchors)| definition(id, anchors))
        .collect()
}

fn example(id: &str, statement: &str, file: &str) -> Example {
    Example {
        id: id.into(),
        statement: statement.into(),
        channels: vec!["cli".into()],
        package: "demo".into(),
        file: file.into(),
        test: id.into(),
        line: 1,
        digest: format!("sha256:{id}"),
    }
}

/// A diff of `src/main.rs` changing `hunks`, given as `-a,b +c,d` headers.
fn main_rs(hunks: &[&str]) -> Vec<FileDiff> {
    let mut diff = String::from(
        "diff --git a/src/main.rs b/src/main.rs\nindex 1..2 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n",
    );
    for hunk in hunks {
        diff += &["@@ ", hunk, " @@\n-old\n+new\n"].concat();
    }
    parse_hunks(&diff)
}

/// An anchor as the tests compare it: key, change and location.
type Anchor<'a> = (&'a str, AnchorChange, &'a str);

fn touched(impact: &Impact) -> Vec<(&str, Vec<Anchor<'_>>)> {
    impact
        .statements
        .iter()
        .map(|statement| {
            (
                statement.id.as_str(),
                statement
                    .anchors
                    .iter()
                    .map(
                        |TouchedAnchor {
                             anchor,
                             change,
                             location,
                         }| {
                            (anchor.as_str(), *change, location.as_str())
                        },
                    )
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn a_diff_gives_each_file_its_paths_and_changed_ranges() {
    let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
index 1..2 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -3 +3,2 @@ fn greet
-a
+b
+c
@@ -10,2 +11,0 @@
-d
-e
diff --git a/src/new.rs b/src/new.rs
new file mode 100644
--- /dev/null
+++ b/src/new.rs
@@ -0,0 +1,4 @@
+x
diff --git a/old.rs b/old.rs
deleted file mode 100644
--- a/old.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-y
diff --git a/logo.png b/logo.png
Binary files a/logo.png and b/logo.png differ
diff --git "a/say \"hi\".rs" "b/say \"hi\".rs"
--- "a/say \"hi\".rs"
+++ "b/say \"hi\".rs"
@@ -1 +1 @@
"#;
    let file = |old: Option<&str>,
                new: Option<&str>,
                before: &[(u32, u32)],
                after: &[(u32, u32)]| FileDiff {
        old_path: old.map(Into::into),
        new_path: new.map(Into::into),
        old: before.to_vec(),
        new: after.to_vec(),
    };
    assert_eq!(
        parse_hunks(diff),
        [
            file(
                Some("src/lib.rs"),
                Some("src/lib.rs"),
                &[(3, 3), (10, 11)],
                &[(3, 4)]
            ),
            file(None, Some("src/new.rs"), &[], &[(1, 4)]),
            file(Some("old.rs"), None, &[(1, 2)], &[]),
            file(Some("logo.png"), Some("logo.png"), &[], &[]),
            file(
                Some("say \"hi\".rs"),
                Some("say \"hi\".rs"),
                &[(1, 1)],
                &[(1, 1)]
            ),
        ]
    );
}

#[test]
fn a_body_change_touches_the_anchors_that_overlap_it_and_the_files_path_anchors() {
    let catalog = catalog(&[
        ("entry_def", &["rust:src/main.rs#entry"]),
        ("work_def", &["rust:src/main.rs#work"]),
        ("file_def", &["src/main.rs"]),
        ("module_def", &["rust:src/main.rs#demo"]),
        ("elsewhere_def", &["src/other.rs"]),
    ]);
    let impact = impact(
        &catalog,
        &[],
        &[snapshot()],
        &[snapshot()],
        &main_rs(&["-4 +4"]),
    );
    assert_eq!(
        touched(&impact),
        [
            (
                "entry_def",
                vec![(
                    "rust:src/main.rs#entry",
                    AnchorChange::Changed,
                    "src/main.rs:2-9"
                )]
            ),
            (
                "file_def",
                vec![("src/main.rs", AnchorChange::Changed, "src/main.rs")]
            ),
            (
                "module_def",
                vec![(
                    "rust:src/main.rs#demo",
                    AnchorChange::Changed,
                    "src/main.rs:1-30"
                )]
            ),
        ]
    );
    assert_eq!(impact.unmapped, Vec::<String>::new());
    let text = impact.markdown("`main` to the working tree", &BTreeMap::new());
    assert!(
        text.contains("### `file_def` (definition)\n\n- Changed: `src/main.rs`\n\n"),
        "{text}"
    );
}

#[test]
fn a_symbol_only_the_base_has_is_removed_and_one_only_the_head_has_is_added() {
    let catalog = catalog(&[("unused_def", &["rust:src/main.rs#unused"])]);
    let removed = impact(
        &catalog,
        &[],
        &[snapshot()],
        &[without("unused")],
        &main_rs(&["-21,9 +20,0"]),
    );
    assert_eq!(
        touched(&removed),
        [(
            "unused_def",
            vec![(
                "rust:src/main.rs#unused",
                AnchorChange::Removed,
                "src/main.rs:21-29"
            )]
        )]
    );
    let added = impact(
        &catalog,
        &[],
        &[without("unused")],
        &[snapshot()],
        &main_rs(&["-20,0 +21,9"]),
    );
    assert_eq!(added.statements[0].anchors[0].change, AnchorChange::Added);
}

#[test]
fn a_type_anchor_counts_a_change_to_a_method_it_spans() {
    let catalog = catalog(&[("greeter_def", &["rust:src/main.rs#Greeter"])]);
    let impact = impact(
        &catalog,
        &[],
        &[with_greeter()],
        &[with_greeter()],
        &main_rs(&["-24 +24"]),
    );
    assert_eq!(
        touched(&impact),
        [(
            "greeter_def",
            vec![(
                "rust:src/main.rs#Greeter",
                AnchorChange::Changed,
                "src/main.rs:21-29"
            )]
        )]
    );
    assert_eq!(impact.unmapped, Vec::<String>::new(), "the type covers it");
}

#[test]
fn changed_declarations_no_anchor_covers_are_listed_innermost_first() {
    let catalog = catalog(&[("entry_def", &["rust:src/main.rs#entry"])]);
    let impact = impact(
        &catalog,
        &[],
        &[with_greeter()],
        &[with_greeter()],
        // In entry, in work, between functions, and in a method.
        &main_rs(&["-5 +5", "-15 +15", "-10 +10", "-24 +24"]),
    );
    assert_eq!(
        impact.unmapped,
        [
            "rust:src/main.rs#Greeter::hello",
            "rust:src/main.rs#demo",
            "rust:src/main.rs#work"
        ]
    );
    let deleted_only = impact_of_deletion();
    assert_eq!(deleted_only.unmapped, ["rust:src/main.rs#work"]);
}

#[test]
fn an_examples_test_function_is_not_unmapped_code() {
    let mut tagged = example("work_runs", "entry_def", "src/main.rs");
    tagged.test = "work".into();
    let impact = impact(
        &BTreeMap::new(),
        &[tagged],
        &[snapshot()],
        &[snapshot()],
        &main_rs(&["-15 +15", "-25 +25"]),
    );
    assert_eq!(impact.unmapped, ["rust:src/main.rs#unused"]);
}

/// Lines deleted from `work` touch it only at the base.
fn impact_of_deletion() -> Impact {
    impact(
        &BTreeMap::new(),
        &[],
        &[snapshot()],
        &[snapshot()],
        &main_rs(&["-14,2 +13,0"]),
    )
}

#[test]
fn examples_are_touched_when_their_test_file_changes() {
    let catalog = catalog(&[("entry_def", &["rust:src/main.rs#entry"])]);
    let examples = [
        example("entry_runs", "entry_def", "tests/entry.rs"),
        example("entry_prints", "entry_def", "src/main.rs"),
    ];
    let impact = impact(
        &catalog,
        &examples,
        &[snapshot()],
        &[snapshot()],
        &main_rs(&["-4 +4"]),
    );
    assert_eq!(
        impact.statements[0].examples,
        [
            ("entry_runs".to_string(), false),
            ("entry_prints".to_string(), true)
        ]
    );
    assert_eq!(
        impact
            .examples
            .iter()
            .map(|example| example.id.as_str())
            .collect::<Vec<_>>(),
        ["entry_prints"]
    );
}

#[test]
fn the_impact_reads_as_markdown_and_as_json() {
    let catalog = catalog(&[("entry_def", &["rust:src/main.rs#entry"])]);
    let examples = [example("entry_prints", "entry_def", "src/main.rs")];
    let impact = impact(
        &catalog,
        &examples,
        &[snapshot()],
        &[snapshot()],
        &main_rs(&["-4 +4", "-15 +15"]),
    );
    let standings = BTreeMap::from([("entry_def".to_string(), "verified".to_string())]);
    assert_eq!(
        impact.markdown("`main` to the working tree", &standings),
        "# Intent impact\n\nChanges from `main` to the working tree.\n\n## Touched statements\n\n### `entry_def` (definition): verified\n\n- Changed: `rust:src/main.rs#entry` at `src/main.rs:2-9`\n- Examples: `entry_prints` (test changed)\n\n## Changed code no statement maps\n\n- `rust:src/main.rs#work`\n\n## Examples whose test changed\n\n- `entry_prints` of `entry_def` in `src/main.rs`\n"
    );
    assert_eq!(
        impact.json(&standings),
        serde_json::json!({
            "statements": [{
                "id": "entry_def",
                "kind": "definition",
                "standing": "verified",
                "anchors": [{
                    "anchor": "rust:src/main.rs#entry",
                    "change": "changed",
                    "location": "src/main.rs:2-9",
                }],
                "examples": [{"id": "entry_prints", "test_changed": true}],
            }],
            "unmapped": ["rust:src/main.rs#work"],
            "examples": [{"id": "entry_prints", "statement": "entry_def", "file": "src/main.rs"}],
        })
    );
    let nothing = stele_plan::impact::impact(&catalog, &[], &[snapshot()], &[snapshot()], &[]);
    assert_eq!(
        nothing.markdown("`HEAD` to the working tree", &BTreeMap::new()),
        "# Intent impact\n\nChanges from `HEAD` to the working tree.\n\n## Touched statements\n\nNone.\n\n## Changed code no statement maps\n\nNone.\n\n## Examples whose test changed\n\nNone.\n"
    );
}
