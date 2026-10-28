//! The evidence the shipped checker reads: a plan and its catalog as facts,
//! and which of the plan's anchors the code contains.

use std::collections::{BTreeMap, BTreeSet};

use codemap_graph::Snapshot;
use lemmaspec::{
    Artifact, EvidenceBasis, EvidenceKind, ExpectationDecl, FactDecl, FactValue, SymbolDecl,
};
use sha2::{Digest, Sha256};

use crate::{Document, Reader, State, StatementKind, report::Standings};

/// The checker plan evidence is written for.
pub const CHECKER: &str = include_str!("../checker.stele");

/// The findings a sound plan has none of, each with the query that finds it,
/// in ID order, the order the artifact keeps them in.
const SOUND: [(&str, &str); 7] = [
    ("done_changes_built_what_they_add", "unbuilt(C, A)"),
    (
        "done_changes_serve_verified_statements",
        "unverified_done(C, S)",
    ),
    (
        "every_change_serves_a_statement",
        "change_serves_nothing(C)",
    ),
    ("every_decision_is_carried_out", "uncited_decision(D)"),
    ("every_proposal_is_served", "unserved_proposal(S)"),
    ("the_order_has_no_cycle", "cyclic_order(C)"),
    ("touched_code_exists", "missing_anchor(C, A)"),
];

/// Every anchor the plan names, touched or created.
#[must_use]
pub fn anchors(document: &Document) -> BTreeSet<&str> {
    document
        .plan
        .iter()
        .flat_map(|plan| plan.changes.values())
        .flat_map(|change| change.touches.iter().chain(&change.creates))
        .map(String::as_str)
        .collect()
}

/// The evidence for `document`: its statements, decisions and changes,
/// cited at their lines in the files `reader` reads, the plan's anchors that
/// `code` contains, and, for a plan, the catalog statements `standings`
/// found verified. A plan that names anchors needs code, with a snapshot in
/// the language of each symbol anchor.
pub fn evidence(
    reader: &dyn Reader,
    document: &Document,
    code: Option<&Code>,
    standings: Option<&Standings>,
) -> Result<Artifact, String> {
    let mut facts = Facts::default();
    let mut symbols = Vec::new();
    let mut files = Files {
        reader,
        digests: BTreeMap::new(),
    };

    let proposals = document
        .plan
        .iter()
        .flat_map(|plan| plan.proposals.values());
    let statements = document
        .catalog
        .values()
        .chain(proposals.clone().map(|proposal| &proposal.statement));
    for statement in statements {
        let basis = files.cited(&statement.file, statement.line)?;
        let id = statement.id.as_str();
        symbols.push(label(id, &statement.text, &basis.source));
        let kind = match &statement.kind {
            StatementKind::Intent => "intent",
            StatementKind::Definition { refines } => {
                facts.add(&basis, "refines", &[id, refines]);
                "definition"
            }
            StatementKind::Exception { narrows, .. } => {
                facts.add(&basis, "narrows", &[id, narrows]);
                "exception"
            }
            StatementKind::Channel => "channel",
        };
        facts.add(&basis, "statement", &[id, kind]);
    }

    let Some(plan) = &document.plan else {
        return Ok(artifact(
            "catalog",
            "Does the catalog hold together?",
            symbols,
            facts,
        ));
    };
    for proposal in proposals {
        let statement = &proposal.statement;
        let basis = files.cited(&statement.file, statement.line)?;
        facts.add(&basis, "proposed", &[&statement.id]);
        if let Some(replaced) = &proposal.replaces {
            facts.add(&basis, "replaces", &[&statement.id, replaced]);
        }
    }
    for decision in plan.decisions.values() {
        let basis = files.cited(&plan.file, decision.line)?;
        let id = decision.id.as_str();
        symbols.push(label(id, &decision.text, &basis.source));
        facts.add(&basis, "decision", &[id]);
        for statement in &decision.governs {
            facts.add(&basis, "governs", &[id, statement]);
        }
    }
    for change in plan.changes.values() {
        let basis = files.cited(&plan.file, change.line)?;
        let id = change.id.as_str();
        symbols.push(label(id, &change.text, &basis.source));
        facts.add(&basis, "change", &[id]);
        for (relation, targets) in [
            ("serves", &change.serves),
            ("follows", &change.follows),
            ("after", &change.after),
            ("touches", &change.touches),
            ("creates", &change.creates),
        ] {
            for target in targets {
                facts.add(&basis, relation, &[id, target]);
            }
        }
        if change.state == State::Done {
            let declared = EvidenceBasis {
                kind: EvidenceKind::ReviewerDeclared,
                source: basis.source.clone(),
                identity: None,
            };
            facts.add(&declared, "done", &[id]);
        }
    }

    anchor_facts(document, code, &mut symbols, &mut facts)?;
    if let Some(standings) = standings {
        standings_facts(standings, &mut facts);
    }

    let question = format!(
        "Does plan {} hold together against its catalog and the code, and which of its changes are ready?",
        plan.name
    );
    Ok(artifact(&plan.name, &question, symbols, facts))
}

/// One `anchor_present` fact for each of the plan's anchors the code
/// contains, and a symbol for every anchor.
fn anchor_facts(
    document: &Document,
    code: Option<&Code>,
    symbols: &mut Vec<SymbolDecl>,
    facts: &mut Facts,
) -> Result<(), String> {
    let anchors = anchors(document);
    if anchors.is_empty() {
        return Ok(());
    }
    let code = code.ok_or("the plan names code anchors, so it needs a code snapshot")?;
    for anchor in anchors {
        symbols.push(SymbolDecl {
            value: anchor.into(),
            label: anchor.into(),
            source: Some(anchor_path(anchor).into()),
            doc: None,
        });
        if let Some(found) = code.find(anchor)? {
            facts.add(&found, "anchor_present", &[anchor]);
        }
    }
    Ok(())
}

/// That the catalog's standings were evaluated, and each statement found
/// verified, on the intent checker's word at the identity it evaluated.
fn standings_facts(standings: &Standings, facts: &mut Facts) {
    let basis = EvidenceBasis {
        kind: EvidenceKind::Policy,
        source: format!("stele intent checker at {}", standings.target),
        identity: None,
    };
    facts.add(&basis, "standings_evaluated", &[&standings.target]);
    for statement in standings.verified() {
        facts.add(&basis, "verified", &[statement]);
    }
}

fn artifact(name: &str, question: &str, mut symbols: Vec<SymbolDecl>, facts: Facts) -> Artifact {
    symbols.sort_by(|a, b| a.value.cmp(&b.value));
    Artifact {
        name: name.into(),
        doc: Some(question.into()),
        notes: None,
        symbols,
        relations: Vec::new(),
        facts: facts.declarations(),
        rules: Vec::new(),
        expectations: SOUND
            .iter()
            .map(|(id, query)| ExpectationDecl {
                id: (*id).into(),
                query: (*query).into(),
                count: 0,
                doc: None,
            })
            .collect(),
        mutations: Vec::new(),
    }
}

pub fn label(value: &str, text: &str, source: &str) -> SymbolDecl {
    SymbolDecl {
        value: value.into(),
        label: text.into(),
        source: Some(source.into()),
        doc: None,
    }
}

/// The repository path an anchor points into.
pub fn anchor_path(anchor: &str) -> &str {
    anchor
        .split_once(':')
        .and_then(|(_, symbol)| symbol.split_once('#'))
        .map_or(anchor, |(path, _)| path)
}

/// The files facts cite, each identified by the digest of its bytes.
struct Files<'a> {
    reader: &'a dyn Reader,
    digests: BTreeMap<String, String>,
}

impl Files<'_> {
    fn cited(&mut self, file: &str, line: usize) -> Result<EvidenceBasis, String> {
        if !self.digests.contains_key(file) {
            let bytes = self.reader.read(file).map_err(|e| format!("{file}: {e}"))?;
            let digest = format!("sha256:{:x}", Sha256::digest(bytes.as_bytes()));
            self.digests.insert(file.into(), digest);
        }
        Ok(EvidenceBasis {
            kind: EvidenceKind::Snapshot,
            source: format!("{file}#L{line}"),
            identity: Some(self.digests[file].clone()),
        })
    }
}

/// The code anchors are checked against: a snapshot of each language the
/// symbol anchors name, and the files of the working tree they came from.
#[derive(Debug, Default)]
pub struct Code {
    pub snapshots: Vec<Snapshot>,
    /// `None` when the code is in no git repository.
    pub working_tree: Option<WorkingTree>,
}

/// The files of a git working tree that a path anchor may name, whatever
/// language they are in: tracked or untracked, present, and not ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingTree {
    /// By their `/`-separated paths from the repository root.
    pub files: BTreeSet<String>,
    /// The working tree's source identity (see [`crate::identity`]).
    pub identity: String,
}

impl Code {
    /// Where the code contains `anchor`, as the basis of an `anchor_present`
    /// fact, or `None` when it does not. A symbol anchor is looked up in the
    /// snapshot of its language, and finding none is an error. A path anchor
    /// is in a snapshot's files, identified by that snapshot, or else in the
    /// working tree, identified by its source identity.
    pub fn find(&self, anchor: &str) -> Result<Option<EvidenceBasis>, String> {
        let found = |identity: &str| EvidenceBasis {
            kind: EvidenceKind::Snapshot,
            source: anchor_path(anchor).into(),
            identity: Some(identity.into()),
        };
        if let Some((language, _)) = anchor.split_once(':') {
            let snapshot = self
                .snapshot(language)
                .ok_or_else(|| self.missing(anchor, language))?;
            let present = snapshot.nodes.iter().any(|node| node.symbol_key == anchor);
            return Ok(present.then(|| found(&snapshot.identity.snapshot_id)));
        }
        let listed = self
            .snapshots
            .iter()
            .find(|snapshot| snapshot.files.iter().any(|file| file.rel_path == anchor));
        Ok(match (listed, &self.working_tree) {
            (Some(snapshot), _) => Some(found(&snapshot.identity.snapshot_id)),
            (None, Some(tree)) if tree.files.contains(anchor) => Some(found(&tree.identity)),
            _ => None,
        })
    }

    fn snapshot(&self, language: &str) -> Option<&Snapshot> {
        self.snapshots
            .iter()
            .find(|snapshot| snapshot.identity.language.tag() == language)
    }

    fn missing(&self, anchor: &str, language: &str) -> String {
        match self.snapshots.as_slice() {
            [snapshot] => format!(
                "anchor {anchor} is {language} code, but the snapshot is {}; check it against a {language} snapshot",
                snapshot.identity.language.tag()
            ),
            _ => format!("anchor {anchor} is {language} code, but no {language} snapshot was read"),
        }
    }
}

/// The languages whose snapshots `anchors` are checked against: each one a
/// symbol anchor names, or rust when none does.
pub fn languages<'a>(anchors: impl IntoIterator<Item = &'a str>) -> BTreeSet<&'a str> {
    let mut languages: BTreeSet<&str> = anchors
        .into_iter()
        .filter_map(|anchor| anchor.split_once(':').map(|(language, _)| language))
        .collect();
    if languages.is_empty() {
        languages.insert("rust");
    }
    languages
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct Fact {
    relation: String,
    args: Vec<FactValue>,
    basis: EvidenceBasis,
}

/// Facts as they are found, declared once each.
#[derive(Default)]
pub struct Facts(Vec<Fact>);

impl Facts {
    pub fn add(&mut self, basis: &EvidenceBasis, relation: &str, args: &[&str]) {
        let args = args
            .iter()
            .map(|arg| FactValue::Symbol((*arg).into()))
            .collect();
        self.push(basis, relation, args);
    }

    pub fn push(&mut self, basis: &EvidenceBasis, relation: &str, args: Vec<FactValue>) {
        self.0.push(Fact {
            relation: relation.into(),
            args,
            basis: basis.clone(),
        });
    }

    /// Sorted, deduplicated declarations with IDs `relation_N`.
    pub fn declarations(mut self) -> Vec<FactDecl> {
        self.0.sort();
        self.0.dedup();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        self.0
            .into_iter()
            .map(
                |Fact {
                     relation,
                     args,
                     basis,
                 }| {
                    let count = counts.entry(relation.clone()).or_default();
                    let id = format!("{relation}_{count}");
                    *count += 1;
                    FactDecl {
                        id,
                        relation,
                        args,
                        confidence: 1.0,
                        provenance: vec![basis.source.clone()],
                        basis: Some(basis),
                        doc: None,
                    }
                },
            )
            .collect()
    }
}
