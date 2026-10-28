//! Typed `.lemmaspec` artifacts and deterministic evaluation.

mod parser;
mod validate;

use std::fmt;

use serde::Serialize;

pub use parser::parse_artifact;
pub(crate) use validate::{
    evaluate_parsed_for_projection, evaluate_prepared_expectations, evaluate_prepared_mutant,
    prepare_artifact, PreparedArtifact,
};
pub use validate::{walk_artifact, walk_parsed_artifact};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Symbol,
    Integer,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(untagged)]
pub enum FactValue {
    Symbol(String),
    Integer(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationDecl {
    pub name: String,
    pub args: Vec<ValueType>,
    /// Human names for each argument position, e.g. `[mutation, policy]`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    /// Sentence template over the roles, e.g. `"{mutation} is governed by {policy}"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reads: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// The producer's declared evidence boundary, not an authentication claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Snapshot,
    Policy,
    Observed,
    ReviewerDeclared,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Policy => "policy",
            Self::Observed => "observed",
            Self::ReviewerDeclared => "reviewer_declared",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct EvidenceBasis {
    pub kind: EvidenceKind,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FactDecl {
    pub id: String,
    pub relation: String,
    pub args: Vec<FactValue>,
    pub confidence: f64,
    pub provenance: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<EvidenceBasis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleDecl {
    pub id: String,
    pub derive: String,
    pub when: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub condition_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

impl RuleDecl {
    pub(crate) fn condition_id(&self, index: usize) -> Option<&str> {
        self.condition_ids.get(index).map(String::as_str)
    }

    pub(crate) fn remove_condition(&mut self, index: usize) {
        self.when.remove(index);
        if !self.condition_ids.is_empty() {
            self.condition_ids.remove(index);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpectationDecl {
    pub id: String,
    pub query: String,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationOperator {
    DropRule,
    DropCondition,
    DropFact,
}

impl MutationOperator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DropRule => "drop_rule",
            Self::DropCondition => "drop_condition",
            Self::DropFact => "drop_fact",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MutationDecl {
    pub id: String,
    pub operator: MutationOperator,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub except: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub must_fail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

impl MutationDecl {
    pub(crate) fn excludes_condition(&self, rule: &str, condition: Option<&str>) -> bool {
        self.except.iter().any(|except| {
            except == rule
                || condition.is_some_and(|condition| {
                    except
                        .strip_prefix(rule)
                        .and_then(|suffix| suffix.strip_prefix('.'))
                        == Some(condition)
                })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SymbolDecl {
    pub value: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Artifact {
    pub name: String,
    /// Every comment written before the `spec` keyword: the question the
    /// artifact answers, in the author's words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Maintenance guidance, separate from the reader's question.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Display metadata for constants already present in the artifact.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<SymbolDecl>,
    pub relations: Vec<RelationDecl>,
    pub facts: Vec<FactDecl>,
    pub rules: Vec<RuleDecl>,
    pub expectations: Vec<ExpectationDecl>,
    pub mutations: Vec<MutationDecl>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WalkFact {
    pub relation: String,
    pub args: Vec<String>,
    pub origin: String,
    pub confidence: f64,
    pub provenance: Vec<String>,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WalkExpectation {
    pub id: String,
    pub query: String,
    pub expected_count: usize,
    pub actual_count: usize,
    pub satisfied: bool,
    /// The facts the query matched, in deterministic order: the proof of a
    /// satisfied claim, or what was found instead of the expected count.
    pub found: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WalkReport {
    pub spec: String,
    pub status: String,
    pub asserted: usize,
    pub derived: usize,
    pub facts: Vec<WalkFact>,
    pub expectations: Vec<WalkExpectation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactError {
    kind: ArtifactErrorKind,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactErrorKind {
    InvalidArtifact,
    #[cfg_attr(not(test), allow(dead_code))]
    Evaluation,
}

impl ArtifactError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            kind: ArtifactErrorKind::InvalidArtifact,
            message: message.into(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn evaluation(message: impl Into<String>) -> Self {
        Self {
            kind: ArtifactErrorKind::Evaluation,
            message: message.into(),
        }
    }

    pub(crate) fn is_invalid_artifact(&self) -> bool {
        self.kind == ArtifactErrorKind::InvalidArtifact
    }
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ArtifactError {}
