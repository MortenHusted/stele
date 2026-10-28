//! The evidence the intent checker reads: the catalog's statements, the
//! examples, the approval log and the run ledger as facts, with the
//! invariant expectations every report holds them to.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
};

use lemmaspec::{Artifact, EvidenceBasis, EvidenceKind, ExpectationDecl, FactValue, SymbolDecl};

use crate::{
    APPROVALS_FILE, DirReader, RUNS_FILE, Reader, Statement, StatementKind,
    approvals::{Entry, ItemKind, Log},
    digest::{DIGEST_VERSION, sha256, statement_digest},
    evidence::{Code, Facts, anchor_path, label},
    examples::{Scan, packages, scan},
    load::ledger_path,
    load_catalog,
    runs::{self, Run},
};

/// The checker intent evidence is written for.
pub const INTENT_CHECKER: &str = include_str!("../intent_checker.stele");

/// A run ledger and the runs it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledger {
    /// By its path from the repository root.
    pub path: String,
    pub runs: Vec<Run>,
}

impl Ledger {
    /// The runs recorded under the current digest version.
    pub fn evaluated_runs(&self) -> impl Iterator<Item = &Run> {
        self.runs
            .iter()
            .filter(|run| run.digest_version == DIGEST_VERSION)
    }
}

/// Everything a report reads from the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    pub catalog: BTreeMap<String, Statement>,
    pub examples: Scan,
    pub log: Log,
    /// The approval log, by its path from the repository root.
    pub log_path: String,
    pub ledger: Ledger,
    /// The digest of each file the facts cite, by its path.
    digests: BTreeMap<String, String>,
}

impl Inputs {
    /// Read the catalog rooted at `catalog`, a path from `root`, the
    /// examples of the packages at `root`, and the two ledgers beside the
    /// catalog root.
    pub fn read(root: &Path, catalog: &str) -> Result<Self, String> {
        let reader = DirReader(root.into());
        let statements = load_catalog(&reader, catalog)?;
        let examples = scan(root, &packages(root)?, &statements)?;
        let log_path = ledger_path(catalog, APPROVALS_FILE);
        let runs_path = ledger_path(catalog, RUNS_FILE);
        let log = Log::read(&reader, &log_path)?;
        let runs = runs::read(&reader, &runs_path)?;
        let files: BTreeSet<&str> = statements
            .values()
            .map(|statement| statement.file.as_str())
            .chain(
                examples
                    .examples
                    .iter()
                    .map(|example| example.file.as_str()),
            )
            .chain([log_path.as_str(), runs_path.as_str()])
            .collect();
        let mut digests = BTreeMap::new();
        for file in files {
            match reader.read(file) {
                Ok(text) => {
                    digests.insert(file.to_string(), sha256(&text));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{file}: {e}")),
            }
        }
        Ok(Self {
            catalog: statements,
            examples,
            log,
            log_path,
            ledger: Ledger {
                path: runs_path,
                runs,
            },
            digests,
        })
    }

    /// Log entries with their positions, counted from 1 in file order, that
    /// were recorded under the current digest version.
    pub fn evaluated_entries(&self) -> impl Iterator<Item = (usize, &Entry)> {
        self.log
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (index + 1, entry))
            .filter(|(_, entry)| entry.digest_version == DIGEST_VERSION)
    }

    /// A basis citing `source` in `file`, identified by the file's digest.
    fn cited(&self, file: &str, source: String) -> EvidenceBasis {
        snapshot_basis(source, self.digests[file].clone())
    }
}

const fn snapshot_basis(source: String, identity: String) -> EvidenceBasis {
    EvidenceBasis {
        kind: EvidenceKind::Snapshot,
        source,
        identity: Some(identity),
    }
}

const WORKING_TREE: &str = "working tree";

/// Every anchor the catalog's statements name as realizing them.
pub fn realized_anchors(catalog: &BTreeMap<String, Statement>) -> BTreeSet<&str> {
    catalog
        .values()
        .flat_map(|statement| &statement.realized_by)
        .map(String::as_str)
        .collect()
}

/// The evidence for `inputs` evaluated at the source identity `target`, with
/// the invariant expectations: nothing awaits approval, every intent,
/// definition and exception that counts has exactly one standing, and the
/// code contains every anchor a statement names as realizing it. A catalog
/// that maps statements to code needs that `code`.
pub fn project(inputs: &Inputs, target: &str, code: Option<&Code>) -> Result<Artifact, String> {
    let mut facts = Facts::default();
    let mut symbols = Vec::new();
    let mut judged = statement_facts(inputs, &mut facts, &mut symbols);
    example_facts(inputs, &mut facts, &mut symbols);
    map_facts(inputs, code, &mut facts, &mut symbols)?;
    let evaluated = log_facts(inputs, &mut facts);
    for entry in evaluated.reinstated(&inputs.catalog, &inputs.examples.examples) {
        if matches!(entry.kind, ItemKind::Channel | ItemKind::Example) {
            continue;
        }
        if let Some(text) = &entry.text {
            symbols.push(SymbolDecl {
                value: entry.item.clone(),
                label: text.clone(),
                source: None,
                doc: None,
            });
        }
        judged.push(entry.item.clone());
    }
    let runs: Vec<&Run> = inputs.ledger.evaluated_runs().collect();
    if !runs.is_empty() {
        let basis = inputs.cited(&inputs.ledger.path, inputs.ledger.path.clone());
        for run in runs {
            facts.add(
                &basis,
                "run",
                &[
                    &run.example,
                    &run.digest,
                    &run.channel,
                    &run.source,
                    run.outcome.as_str(),
                ],
            );
        }
    }
    let working_tree = snapshot_basis(WORKING_TREE.into(), target.into());
    facts.add(&working_tree, "target_identity", &[target]);

    let mut expectations = vec![
        ExpectationDecl {
            id: "nothing_awaits_approval".into(),
            query: "awaiting_approval(I)".into(),
            count: 0,
            doc: None,
        },
        ExpectationDecl {
            id: "nothing_mapped_is_missing".into(),
            query: "broken_map(S, A)".into(),
            count: 0,
            doc: None,
        },
    ];
    judged.sort();
    for id in judged {
        expectations.push(ExpectationDecl {
            id: format!("one_standing_{id}"),
            query: format!("standing(\"{id}\", Kind)"),
            count: 1,
            doc: None,
        });
    }
    symbols.sort_by(|a, b| a.value.cmp(&b.value));
    Ok(Artifact {
        name: "intent_evidence".into(),
        doc: Some("Which intents, domain definitions and exceptions are verified at the target source identity, which changes await approval, and does the code contain what realizes them?".into()),
        notes: None,
        symbols,
        relations: Vec::new(),
        facts: facts.declarations(),
        rules: Vec::new(),
        expectations,
        mutations: Vec::new(),
    })
}

/// Each anchor a statement names as realizing it, cited at the statement,
/// and each such anchor the code contains, identified by what holds it.
fn map_facts(
    inputs: &Inputs,
    code: Option<&Code>,
    facts: &mut Facts,
    symbols: &mut Vec<SymbolDecl>,
) -> Result<(), String> {
    let anchors = realized_anchors(&inputs.catalog);
    if anchors.is_empty() {
        return Ok(());
    }
    let code = code.ok_or("the catalog maps statements to code, so it needs a code snapshot")?;
    for statement in inputs.catalog.values() {
        let cited = inputs.cited(
            &statement.file,
            format!("{}#L{}", statement.file, statement.line),
        );
        for anchor in &statement.realized_by {
            facts.add(&cited, "realized", &[&statement.id, anchor]);
        }
    }
    for anchor in anchors {
        symbols.push(label(anchor, anchor, anchor_path(anchor)));
        if let Some(found) = code.find(anchor)? {
            facts.add(&found, "anchor_present", &[anchor]);
        }
    }
    Ok(())
}

/// Each statement's kind, digest and links, cited at its line. Returns the
/// statements that get a standing: all but channels.
fn statement_facts(
    inputs: &Inputs,
    facts: &mut Facts,
    symbols: &mut Vec<SymbolDecl>,
) -> Vec<String> {
    let mut judged = Vec::new();
    for statement in inputs.catalog.values() {
        let basis = inputs.cited(
            &statement.file,
            format!("{}#L{}", statement.file, statement.line),
        );
        let id = statement.id.as_str();
        symbols.push(label(id, &statement.text, &basis.source));
        let kind = ItemKind::of(&statement.kind);
        facts.add(&basis, "item", &[id, kind.as_str()]);
        facts.add(
            &basis,
            "current_digest",
            &[id, &statement_digest(statement)],
        );
        match &statement.kind {
            StatementKind::Definition { refines } => facts.add(&basis, "refines", &[id, refines]),
            StatementKind::Exception {
                narrows,
                kind,
                channels,
            } => {
                facts.add(&basis, "narrows", &[id, narrows]);
                facts.add(&basis, "exception_kind", &[id, kind.as_str()]);
                for channel in channels {
                    facts.add(&basis, "scoped_to", &[id, channel]);
                }
            }
            StatementKind::Intent | StatementKind::Channel => {}
        }
        if kind != ItemKind::Channel {
            judged.push(id.to_string());
        }
    }
    judged
}

/// Each example's digest, statement and channel tags, cited at its tag.
fn example_facts(inputs: &Inputs, facts: &mut Facts, symbols: &mut Vec<SymbolDecl>) {
    for example in &inputs.examples.examples {
        let basis = inputs.cited(&example.file, format!("{}#L{}", example.file, example.line));
        let id = example.id.as_str();
        let text = format!("Test {} in {}", example.test, example.file);
        symbols.push(label(id, &text, &basis.source));
        facts.add(&basis, "item", &[id, ItemKind::Example.as_str()]);
        facts.add(&basis, "current_digest", &[id, &example.digest]);
        facts.add(&basis, "exemplifies", &[id, &example.statement]);
        for channel in &example.channels {
            facts.add(&basis, "tagged", &[id, channel]);
        }
    }
}

/// The evaluated approval log entries as facts, and as a log of their own.
fn log_facts(inputs: &Inputs, facts: &mut Facts) -> Log {
    let evaluated: Vec<(usize, &Entry)> = inputs.evaluated_entries().collect();
    if !evaluated.is_empty() {
        let basis = inputs.cited(&inputs.log_path, inputs.log_path.clone());
        for &(position, entry) in &evaluated {
            let position = FactValue::Integer(
                i64::try_from(position).expect("a log has fewer entries than i64 counts"),
            );
            facts.push(
                &basis,
                "log_entry",
                vec![
                    position.clone(),
                    symbol(&entry.item),
                    symbol(entry.kind.as_str()),
                    symbol(entry.action.as_str()),
                    symbol(&entry.digest),
                ],
            );
            for (target, digest) in &entry.links {
                facts.push(
                    &basis,
                    "log_link",
                    vec![position.clone(), symbol(target), symbol(digest)],
                );
            }
            for channel in &entry.channels {
                facts.push(&basis, "log_tag", vec![position.clone(), symbol(channel)]);
            }
        }
    }
    Log {
        entries: evaluated
            .into_iter()
            .map(|(_, entry)| entry.clone())
            .collect(),
    }
}

fn symbol(value: &str) -> FactValue {
    FactValue::Symbol(value.into())
}
