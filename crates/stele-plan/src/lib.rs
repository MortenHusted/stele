//! Catalogs of what a system is for, and the plans that change it.
//!
//! A catalog is a directory of `.stele` files under one root file. It holds
//! intents, the definitions that refine them, the exceptions that narrow a
//! definition on named channels, and the channels, joined by typed links. A
//! definition or exception may also name the code that realizes it. A plan
//! imports a catalog and holds the decisions and the ordered changes
//! that serve its statements; it may also propose new statements, or new
//! versions of existing ones. [`load`] reads either and checks every link;
//! [`evidence`] states it as facts, with the plan's code anchors resolved
//! against the [`Code`], for the shipped [`CHECKER`]. [`approvals`] records
//! who approved each catalog statement as it reads, by its [`digest`], in a
//! ledger beside the catalog root. [`examples`] are the tests tagged as
//! exemplifying a statement on its channels; [`identity`] names the source
//! they run on, and the [`recorder`] appends each run to the [`runs`] ledger.
//! The [`report`] binds all of it, as [`project`]ed evidence, to the shipped
//! [`INTENT_CHECKER`] to give every statement its standing.

use std::collections::BTreeMap;

pub mod approvals;
pub mod digest;
mod evidence;
pub mod examples;
pub mod identity;
pub mod impact;
mod load;
pub mod minitest;
pub mod project;
pub mod recorder;
pub mod report;
pub mod runs;
pub mod syntax;

pub use evidence::{CHECKER, Code, WorkingTree, anchors, evidence, languages};
pub use load::{APPROVALS_FILE, DirReader, RUNS_FILE, Reader, load, load_catalog};
pub use project::INTENT_CHECKER;

/// A catalog root or a plan, loaded with every link checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Every catalog statement by its ID, which is unique across the catalog
    /// and the plan. Empty for a plan that imports no catalog.
    pub catalog: BTreeMap<String, Statement>,
    /// The plan, when the document is one.
    pub plan: Option<Plan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub id: String,
    pub text: String,
    /// The declaring file, by its `/`-separated path from the reader's base.
    pub file: String,
    pub line: usize,
    pub kind: StatementKind,
    /// Code anchors that realize a catalog definition or exception, sorted;
    /// empty for every other statement. Not part of what is approved.
    pub realized_by: Vec<String>,
}

/// Links are resolved to the IDs of the statements they point at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementKind {
    Intent,
    Definition {
        refines: String,
    },
    Exception {
        narrows: String,
        kind: ExceptionKind,
        channels: Vec<String>,
    },
    Channel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionKind {
    DoesNotApply,
    HoldsInstead,
}

impl ExceptionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DoesNotApply => "does_not_apply",
            Self::HoldsInstead => "holds_instead",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "does_not_apply" => Some(Self::DoesNotApply),
            "holds_instead" => Some(Self::HoldsInstead),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub name: String,
    pub file: String,
    /// The root of the catalog the plan imports, by its path from the
    /// reader's base.
    pub catalog: Option<String>,
    /// Statements the plan proposes for the catalog.
    pub proposals: BTreeMap<String, Proposal>,
    pub decisions: BTreeMap<String, Decision>,
    pub changes: BTreeMap<String, Change>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub statement: Statement,
    /// The catalog statement of the same kind this one would replace.
    pub replaces: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub id: String,
    pub text: String,
    pub line: usize,
    /// The statements the decision is about.
    pub governs: Vec<String>,
    /// The alternatives considered and why they lost.
    pub rejected: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub id: String,
    pub text: String,
    pub line: usize,
    /// The statements the change makes true.
    pub serves: Vec<String>,
    /// The decisions the change carries out.
    pub follows: Vec<String>,
    /// The changes that come before this one.
    pub after: Vec<String>,
    /// Code the change edits, which must exist.
    pub touches: Vec<String>,
    /// Code the change adds, which must exist once the change is done.
    pub creates: Vec<String>,
    pub state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Planned,
    Done,
}

impl State {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Done => "done",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "planned" => Some(Self::Planned),
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}
