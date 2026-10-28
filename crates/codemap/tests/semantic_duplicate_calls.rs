//! Regression: nested calls can share a call-expression start byte. Calls
//! must be identified by the callee token so chained invocations of the same
//! method remain distinct canonical facts.

use std::collections::HashSet;

use codemap::schema::EdgeKind;
use codemap::{ExtractOptions, extract};

mod common;

#[test]
#[ignore = "requires rust-analyzer on PATH"]
fn chained_calls_to_the_same_method_have_unique_ids() {
    let files = vec![
        (
            ".gitignore".to_string(),
            "/Cargo.lock\n/target/\n".to_string(),
        ),
        (
            "Cargo.toml".to_string(),
            "[package]\nname = \"duplicate-calls\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
                .to_string(),
        ),
        (
            "src/main.rs".to_string(),
            "struct Step;\nimpl Step { fn step(self) -> Self { self } }\nfn run() { let _ = Step.step().step(); }\nfn main() { run(); }\n"
                .to_string(),
        ),
    ];
    let repo = common::make_repo_with(&files).expect("create fixture repository");
    let snapshot = extract(&ExtractOptions {
        path: repo,
        calls_required: true,
        measures: false,
        twins: false,
        language: codemap::schema::Language::Rust,
    })
    .expect("semantic extraction");

    let step_id = &snapshot
        .nodes
        .iter()
        .find(|node| node.name == "step")
        .expect("step method node")
        .id;
    let step_calls: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls && edge.to == *step_id)
        .collect();
    assert_eq!(step_calls.len(), 2, "both chained calls must be retained");
    assert_eq!(
        step_calls
            .iter()
            .map(|edge| edge.id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        2,
        "chained calls must have distinct deterministic IDs"
    );
}
