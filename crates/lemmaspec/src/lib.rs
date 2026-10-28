//! Typed, deterministic specifications compiled to deductive logic.
//!
//! The initial engine is a focused port of Lemmalog's deterministic core.
//! Agent memory, LLM extraction, MCP, episodes, retrieval, and persistence
//! are deliberately outside this crate.

pub mod artifact;
pub mod ast;
pub mod check;
pub mod cli;
pub mod eval;
mod guide;
pub mod html;
pub mod intern;
pub mod magic;
pub mod markdown;
pub mod mutation;
mod narrative;
pub mod printer;
mod projection;
mod source;

/// The engine version, as `stele spec --version` prints it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use artifact::{
    parse_artifact, walk_artifact, walk_parsed_artifact, Artifact, ArtifactError, EvidenceBasis,
    EvidenceKind, ExpectationDecl, FactDecl, FactValue, MutationDecl, MutationOperator,
    RelationDecl, RuleDecl, SymbolDecl, ValueType, WalkExpectation, WalkFact, WalkReport,
};
pub use ast::{parse_program, ParseError};
pub use check::{bind_artifact, bind_parsed_artifact, check_artifact, check_parsed_artifact};
pub(crate) use eval::check_expression;
pub use eval::{Ann, Change, Engine, StoredFact, StratError};
pub use guide::observation_identity_mismatch;
pub use html::{
    render_projection_html, render_projection_html_with_context, render_projection_html_with_target,
};
pub use intern::{Interner, Term, Value};
pub use markdown::{
    render_projection_markdown, render_projection_markdown_with_context,
    render_projection_markdown_with_target,
};
pub use mutation::{
    mutate_artifact, mutate_parsed_artifact, MutationPolicyReport, MutationReport, MutationResult,
    MutationStatus, MutationSummary, MutationTarget,
};
pub use narrative::read_fact;
pub use printer::print_artifact;
pub use projection::{
    project_artifact, project_parsed_artifact, GraphEdge, GraphNode, GraphNodeData,
    GraphProjection, ProjectionError,
};
pub use source::{is_safe_source, SourceContext};
