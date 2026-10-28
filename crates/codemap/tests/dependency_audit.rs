//! P3 hardening gate: dependency audit. Asserts the crate has no
//! authentication, network, LLM, Graphiti, or remote-storage dependency.
//!
//! Per the AGENTS.md truth boundary and quality gates:
//! "no hidden network, LLM, or remote-storage dependency." This test parses
//! `Cargo.toml` so a future dependency that violates the boundary fails the
//! build, not just a review.

use std::collections::HashSet;

#[test]
fn no_forbidden_runtime_dependencies() {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("read Cargo.toml");

    // Forbinned substrings — a dependency name or source containing any of
    // these signals a network/auth/LLM/remote-storage/graph concern.
    let forbidden = [
        "reqwest",
        "hyper",
        "tonic",
        "tower",
        "axios",
        "ureq",
        "attohttpc",
        "isahc",
        "surf",
        "oauth",
        "auth",
        "openai",
        "llm",
        "tokenizers",
        "tch",
        "candle",
        "graphiti",
        "neo4j",
        "redis",
        "diesel",
        "sqlx",
        "postgres",
        "mongodb",
        "s3",
        "aws-sdk",
        "cloud",
    ];

    // Extract the [dependencies] and [dev-dependencies] sections and check
    // each dependency line. The explicit runtime set includes filesystem
    // capture (`tempfile`), Unix open flags (`libc`), and the offline byte
    // codec (`base64`) for owned Python parser requests.
    let in_deps = section_lines(&manifest, "dependencies");
    let in_dev = section_lines(&manifest, "dev-dependencies");

    let allowed_runtime: HashSet<&str> = [
        "anyhow",
        "clap",
        "codemap-graph",
        "serde",
        "serde_json",
        "sha2",
        "tree-sitter",
        "tree-sitter-rust",
        "tree-sitter-php",
        "tree-sitter-go",
        "tree-sitter-cpp",
        "tree-sitter-javascript",
        "tree-sitter-typescript",
        "ruby-prism",
        "tempfile",
        "libc",
        "base64",
    ]
    .into_iter()
    .collect();
    let allowed_dev: HashSet<&str> = HashSet::from(["tempfile"]);

    for line in &in_deps {
        let name = dep_name(line);
        if name.is_empty() {
            continue;
        }
        assert!(
            allowed_runtime.contains(name.as_str()),
            "unexpected runtime dependency '{name}' — not in the allowed set; \
             codemap must have no network/LLM/auth/remote-storage dependency"
        );
        for f in &forbidden {
            assert!(
                !name.contains(f),
                "forbidden runtime dependency '{name}' (matches '{f}')"
            );
        }
    }
    for line in &in_dev {
        let name = dep_name(line);
        if name.is_empty() {
            continue;
        }
        assert!(
            allowed_dev.contains(name.as_str()),
            "unexpected dev-dependency '{name}' — not in the allowed test-only set"
        );
    }
}

/// Return the non-comment, non-empty lines within a `[section]` of the manifest.
fn section_lines(manifest: &str, section: &str) -> Vec<String> {
    let header = format!("[{section}]");
    let mut lines = Vec::new();
    let mut in_section = false;
    for raw in manifest.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == header;
            continue;
        }
        if in_section && !trimmed.is_empty() && !trimmed.starts_with('#') {
            lines.push(trimmed.to_string());
        }
    }
    lines
}

/// Extract the dependency name (key before `=` or whitespace) from a line.
fn dep_name(line: &str) -> String {
    line.split(['=', ' '])
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

#[test]
fn codemap_lemmaspec_has_no_forbidden_deps() {
    // The LemmaSpec evidence projection is a consumer hosted in this
    // workspace: schema access through codemap-graph, a CLI, and nothing
    // that could reach a network, a model, or a store.
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../codemap-lemmaspec/Cargo.toml"),
    )
    .expect("read codemap-lemmaspec Cargo.toml");
    let allowed_runtime: HashSet<&str> = ["anyhow", "clap", "codemap-graph", "serde_json"]
        .into_iter()
        .collect();
    // Its tests check projected evidence with the workspace's LemmaSpec engine.
    let allowed_dev: HashSet<&str> = ["lemmaspec", "tempfile"].into_iter().collect();
    for (section, allowed) in [
        ("dependencies", &allowed_runtime),
        ("dev-dependencies", &allowed_dev),
    ] {
        for line in section_lines(&manifest, section) {
            let name = dep_name(&line);
            if name.is_empty() {
                continue;
            }
            assert!(
                allowed.contains(name.as_str()),
                "codemap-lemmaspec has unexpected {section} entry '{name}'"
            );
        }
    }
    assert!(
        !section_lines(&manifest, "dependencies")
            .iter()
            .any(|line| dep_name(line) == "codemap"),
        "codemap-lemmaspec must read the schema through codemap-graph, never the extractor"
    );
}

#[test]
fn codemap_graph_has_no_forbidden_deps() {
    // The shared graph crate must also stay LLM/network-free (it's the
    // truth-boundaried layer shared with consumers).
    let graph_manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../codemap-graph/Cargo.toml"),
    )
    .expect("read codemap-graph Cargo.toml");
    let allowed: HashSet<&str> = ["anyhow", "serde", "serde_json", "sha2"]
        .into_iter()
        .collect();
    let forbidden = [
        "reqwest",
        "hyper",
        "tonic",
        "tower",
        "oauth",
        "openai",
        "llm",
        "tokenizers",
        "tch",
        "graphiti",
        "neo4j",
        "redis",
        "sqlx",
        "postgres",
    ];
    for line in section_lines(&graph_manifest, "dependencies") {
        let name = dep_name(&line);
        if name.is_empty() {
            continue;
        }
        assert!(
            allowed.contains(name.as_str()),
            "codemap-graph has unexpected dependency '{name}'"
        );
        for f in &forbidden {
            assert!(
                !name.contains(f),
                "codemap-graph must not depend on '{name}' (matches '{f}')"
            );
        }
    }
}

#[test]
fn codemap_topology_is_an_offline_consumer_of_the_canonical_graph_only() {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../codemap-topology/Cargo.toml"),
    )
    .expect("read codemap-topology Cargo.toml");
    let allowed_runtime: HashSet<&str> = ["anyhow", "clap", "codemap-graph", "serde", "serde_json"]
        .into_iter()
        .collect();
    let allowed_dev: HashSet<&str> = std::iter::once("tempfile").collect();
    for (section, allowed) in [
        ("dependencies", &allowed_runtime),
        ("dev-dependencies", &allowed_dev),
    ] {
        for line in section_lines(&manifest, section) {
            let name = dep_name(&line);
            assert!(
                allowed.contains(name.as_str()),
                "codemap-topology has unexpected {section} entry '{name}'"
            );
        }
    }
    for crate_name in ["codemap", "codemap-graph"] {
        let canonical = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../{crate_name}/Cargo.toml")),
        )
        .expect("read canonical manifest");
        assert!(
            !canonical.contains("codemap-topology"),
            "canonical crates must not depend on topology presentation"
        );
    }
}
