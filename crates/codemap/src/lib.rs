//! `codemap` — the deterministic, source-addressed Rust extractor.
//!
//! This crate PRODUCES a `codemap_graph::Snapshot` (tree-sitter structure +
//! rust-analyzer semantic calls) and projects it for consumers. The snapshot
//! schema, validation, and graph navigation/analysis live in the
//! dependency-light, LLM-free [`codemap_graph`] crate, re-exported here so the
//! extractor and its consumers share one truth-boundaried graph layer.
//!
//! See `AGENTS.md` for the project contract and truth boundary.

// Re-export the shared graph layer's modules so internal `crate::schema` /
// `crate::validate` references in this crate resolve to the shared crate, and
// so `codemap::schema::...` / `codemap::nav::...` work for the CLI.
pub use codemap_graph::{analysis, diff, nav, schema, validate};

pub mod cli;
pub(crate) mod clones;
pub(crate) mod cpp_admission;
#[cfg(test)]
mod cpp_benchmark_anchors;
pub(crate) mod cpp_evidence;
pub(crate) mod cpp_inputs;
#[cfg(test)]
mod cpp_isolation_tests;
pub(crate) mod cpp_lsp;
pub(crate) mod cpp_ownership;
pub(crate) mod cpp_profile;
pub mod cpp_runtime;
pub(crate) mod cpp_snapshot;
pub(crate) mod cpp_syntax;
pub(crate) mod extract;
pub(crate) mod git;
pub(crate) mod go_lsp;
pub(crate) mod go_syntax;
pub mod history;
pub(crate) mod js_ts_admission;
pub mod js_ts_inputs;
pub(crate) mod js_ts_inventory;
#[cfg(target_os = "linux")]
mod js_ts_linux;
pub mod js_ts_runtime;
pub mod js_ts_semantic;
pub(crate) mod js_ts_snapshot;
pub mod js_ts_syntax;
#[cfg(target_os = "linux")]
pub mod linux_guard;
#[cfg(target_os = "linux")]
pub mod linux_io;
#[cfg(target_os = "linux")]
pub mod linux_kernel_images;
#[cfg(target_os = "linux")]
mod linux_maps;
#[cfg(target_os = "linux")]
pub mod linux_process;
#[cfg(unix)]
pub mod linux_runtime;
pub(crate) mod lsp;
#[cfg(target_os = "linux")]
pub use lsp::{LinuxLspConfig, LinuxLspLimits, Transport as LinuxLspTransport};
pub(crate) mod measures;
pub(crate) mod php_lsp;
pub(crate) mod php_syntax;
#[cfg(unix)]
pub(crate) mod python_evidence;
#[cfg(unix)]
pub mod python_inputs;
#[cfg(unix)]
pub mod python_native;
#[cfg(unix)]
pub mod python_parser;
#[cfg(unix)]
pub(crate) mod python_process;
#[cfg(unix)]
pub mod python_profile;
#[cfg(unix)]
pub(crate) mod python_resolve;
#[cfg(unix)]
pub mod python_semantic;
#[cfg(unix)]
pub mod python_syntax;
pub(crate) mod resolve;
pub(crate) mod ruby_lex;
pub(crate) mod ruby_lsp;
pub(crate) mod ruby_syntax;
pub(crate) mod runtime;
pub(crate) mod runtime_images;
pub(crate) mod rust_analyzer;
pub(crate) mod rust_syntax;
pub(crate) mod twins;

// Re-export the shared graph items + the extractor surface at the crate root.
pub use codemap_graph::{
    Direction, EdgeKind, Graph, ImpactNode, NodeKind, ResolveResult, Snapshot, SnapshotDiff,
    clusters, diff_edge_display, diff_node_display, hotspots, processes, refactor_signals,
};
pub use codemap_graph::{NodeMove, infer_moves, stable_edge_keys, stable_node_keys};
pub use cpp_admission::{AdmittedCall as CppAdmittedCall, CompiledBody as CppCompiledBody};
pub use cpp_inputs::{CapturedInputs, InputManifest};
pub use cpp_lsp::{CppCallObservation, CppSemanticResult, CppSession};
pub use cpp_profile::{CompilationCommand, CompilationProfile, ProfileRoots, load_profile};
pub use cpp_snapshot::{CppExtractConfig, extract_cpp, extract_cpp_worktree};
pub use extract::{
    ExtractOptions, extract, extract_at_commit, extract_at_commit_measured,
    extract_at_commit_semantic, extract_at_commit_semantic_measured,
    extract_at_commit_semantic_with, extract_at_commit_with, extract_worktree, to_json,
};
pub use history::{HistoryOptions, HistorySummary, Selector, walk as walk_history};
pub use js_ts_snapshot::{JsTsExtractConfig, extract_js_ts, extract_js_ts_worktree};
pub use runtime::{
    CacheFamily, CapturedRuntime, RuntimeFile, RuntimeManifest, RuntimeRole, guard_policy,
};
pub use runtime_images::{DarwinImageCatalog, LoadedImages};

/// Load a snapshot from a JSON file (deserialize + validate).
pub fn load_snapshot(path: &std::path::Path) -> anyhow::Result<schema::Snapshot> {
    validate_file(path)
}

/// Validate a snapshot loaded from a JSON file. Convenience wrapper for
/// CLI-style consumers that round-trip a written snapshot.
pub fn validate_file(path: &std::path::Path) -> anyhow::Result<schema::Snapshot> {
    let bytes = std::fs::read_to_string(path)?;
    let snapshot: schema::Snapshot = serde_json::from_str(&bytes)?;
    validate(&snapshot)?;
    Ok(snapshot)
}
