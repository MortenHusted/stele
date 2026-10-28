//! Live checks of the fixed rust-analyzer policy, with executable positive controls.

use std::process::Command;

use codemap::schema::{EdgeKind, Language};
use codemap::{ExtractOptions, extract};

mod common;

#[test]
#[ignore = "requires rust-analyzer and rust-src on PATH"]
fn semantic_extraction_does_not_execute_build_scripts_or_proc_macros() {
    let files: Vec<(String, String)> = [
        ("Cargo.toml", "[workspace]\nmembers = [\"app\", \"policy_macro\"]\nresolver = \"2\"\n"),
        ("Cargo.lock", "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"policy_macro\"]\n\n[[package]]\nname = \"policy_macro\"\nversion = \"0.1.0\"\n"),
        (".gitignore", "/target\n"),
        ("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\npolicy_macro = { path = \"../policy_macro\" }\n"),
        ("app/build.rs", "fn main() { std::fs::write(\"build-script-ran\", b\"ran\").unwrap(); }\n"),
        ("app/src/lib.rs", "policy_macro::mark!();\npub fn value() -> u32 { 7 }\npub fn caller() -> u32 { value() }\n"),
        ("policy_macro/Cargo.toml", "[package]\nname = \"policy_macro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\nproc-macro = true\n"),
        ("policy_macro/src/lib.rs", "extern crate proc_macro;\n#[proc_macro]\npub fn mark(_: proc_macro::TokenStream) -> proc_macro::TokenStream {\nstd::fs::write(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/proc-macro-ran\"), b\"ran\").unwrap();\nproc_macro::TokenStream::new()\n}\n"),
    ]
    .into_iter()
    .map(|(path, content)| (path.to_owned(), content.to_owned()))
    .collect();

    let control = common::make_repo_with(&files).expect("create executable control");
    let output = Command::new("cargo")
        .args(["check", "--locked", "--offline", "-p", "app"])
        .current_dir(&control)
        .output()
        .expect("run synthetic positive control");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(control.join("app/build-script-ran").is_file());
    assert!(control.join("policy_macro/proc-macro-ran").is_file());

    let repo = common::make_repo_with(&files).expect("create fresh semantic fixture");
    let snapshot = extract(&ExtractOptions {
        path: repo.clone(),
        calls_required: true,
        measures: false,
        twins: false,
        language: Language::Rust,
    })
    .expect("semantic extraction with the fixed policy");
    assert!(!repo.join("app/build-script-ran").exists());
    assert!(!repo.join("policy_macro/proc-macro-ran").exists());
    let id = |key: &str| {
        &snapshot
            .nodes
            .iter()
            .find(|node| node.symbol_key == key)
            .expect(key)
            .id
    };
    let caller = id("rust:app/src/lib.rs#caller");
    let value = id("rust:app/src/lib.rs#value");
    assert!(
        snapshot.edges.iter().any(|edge| {
            edge.kind == EdgeKind::Calls && &edge.from == caller && &edge.to == value
        }),
        "the refusal check must still produce a resolved ordinary call"
    );
}
