//! `codemap-graph` — the versioned snapshot schema and deterministic graph
//! navigation/analysis, shared between the codemap extractor and its
//! consumers.
//!
//! Light and LLM-free by construction (deps: `serde`, `serde_json`, `sha2`).
//! It never parses source, runs Git, or speaks LSP — it owns the snapshot
//! contract plus read-only graph queries over it. See the extractor crate
//! (`codemap`) for fact production, and `AGENTS.md` for the truth boundary.

pub mod analysis;
pub mod diff;
pub mod nav;
pub mod schema;
pub mod validate;

pub use analysis::{
    Cluster, Hotspot, Process, RefactorSignal, SignalKind, clusters, hotspots, processes,
    refactor_signals,
};
pub use diff::{
    NodeMove, SnapshotDiff, edge_display as diff_edge_display, infer_moves,
    node_display as diff_node_display, stable_edge_keys, stable_node_keys,
};
pub use nav::{
    Direction, ExecutableClassification, Graph, ImpactNode, LocateResult, ResolveResult,
    SourceLocation,
};
pub use schema::{
    CallSite, CallSiteCategory, Edge, EdgeKind, FactSource, FileRecord, Node, NodeKind, Provenance,
    RepoIdentity, Report, RustAnalyzerPolicy, Snapshot, SnapshotIdentity, Span, ToolVersions,
};
pub use validate::validate;
