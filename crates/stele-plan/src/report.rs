//! The intent report: every intent, definition and exception with its
//! standing and the reasons for it, what awaits approval, the code that
//! realizes each statement, and what changed since a base commit, decided by
//! binding the projected evidence to the
//! [`INTENT_CHECKER`](crate::INTENT_CHECKER).

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::{Path, PathBuf},
};

use lemmaspec::{
    Artifact, SourceContext, WalkReport, bind_parsed_artifact, parse_artifact, print_artifact,
    project_parsed_artifact, render_projection_html_with_context, walk_parsed_artifact,
};

use crate::{
    APPROVALS_FILE, DirReader, RUNS_FILE, StatementKind,
    approvals::{Action, ItemKind, Log},
    digest::DIGEST_VERSION,
    evidence::Code,
    examples::Example,
    identity::Repo,
    load::{ledger_path, parent, read_or_empty},
    project::{INTENT_CHECKER, Inputs, project},
    runs::{self, Outcome},
};

pub struct Options {
    /// The directory the catalog's paths resolve from: the repository root.
    pub root: PathBuf,
    /// The catalog root, by its path from `root`.
    pub catalog: String,
    /// The commit whose approvals and failing runs the report compares with.
    pub base: Option<String>,
}

pub struct Report {
    /// The evidence bound to the checker: the proof record behind every
    /// standing, which `stele spec walk` and `render` accept unchanged.
    pub bound: Artifact,
    pub markdown: String,
    /// Whether an expectation of the bound evidence is open, as one is while
    /// anything awaits approval or the code lacks a mapped anchor.
    pub pending: bool,
    pub standings: BTreeMap<String, String>,
    /// The evaluated source identity: the working tree's.
    pub target: String,
}

/// Build the whole report, reading the repository but writing nothing. A
/// catalog that maps statements to code needs that `code`.
pub fn report(options: &Options, code: Option<&Code>) -> Result<Report, String> {
    let inputs = Inputs::read(&options.root, &options.catalog)?;
    let repo = Repo::of_catalog(&options.root.join(parent(&options.catalog)))?;
    let target = repo.working_tree_identity()?;
    let head = repo.head()?;
    let since = options
        .base
        .as_deref()
        .map(|base| since(&repo, base, &inputs, options))
        .transpose()?;
    let header = if repo.commit_identity(&head)? == target {
        format!("Source identity `{target}` matches the tree of HEAD commit `{head}`.")
    } else {
        format!(
            "Source identity `{target}` is the working tree, which does not match the tree of HEAD commit `{head}`, so these standings do not hold for that commit."
        )
    };
    let (checker, bound, walk) = evaluate(&inputs, &target, code)?;
    let found = Found::read(&walk, checker);

    let mut sections = vec![
        format!("# Intent report\n\n{header}\n"),
        awaiting(&found),
        statements(&inputs, &found),
    ];
    sections.extend(map(&inputs, &found));
    sections.extend(ignored(&inputs));
    sections.extend(not_evaluated(&inputs));
    sections.extend(since);
    Ok(Report {
        markdown: sections.join("\n"),
        pending: walk.status != "clean",
        standings: found.standings,
        bound,
        target,
    })
}

/// The intent checker, the evidence for `inputs` at the source identity
/// `target` bound to it, and the walk of what they derive.
fn evaluate(
    inputs: &Inputs,
    target: &str,
    code: Option<&Code>,
) -> Result<(Artifact, Artifact, WalkReport), String> {
    let checker = parse_artifact(INTENT_CHECKER).map_err(|e| e.to_string())?;
    let evidence = project(inputs, target, code)?;
    let bound = bind_parsed_artifact(&checker, &evidence).map_err(|e| e.to_string())?;
    let walk = walk_parsed_artifact(&bound).map_err(|e| e.to_string())?;
    Ok((checker, bound, walk))
}

/// The standing the intent checker found for each catalog statement, and the
/// source identity it evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standings {
    pub target: String,
    /// Each intent, definition and exception by ID: unapproved, stale,
    /// unverified or verified.
    pub of: BTreeMap<String, String>,
}

impl Standings {
    /// The statements whose standing is verified, in ID order.
    pub fn verified(&self) -> impl Iterator<Item = &str> {
        self.of
            .iter()
            .filter(|(_, standing)| *standing == "verified")
            .map(|(id, _)| id.as_str())
    }
}

/// The standings of the catalog rooted at `catalog`, a path from `root`, as
/// a plan that imports it sees them; `None` when no approvals ledger sits
/// beside the root, since nothing can be verified without approvals. A
/// catalog that maps statements to code needs that `code`.
///
/// They are taken at HEAD's commit identity, not the working tree's. A plan
/// is edited in a dirty tree, while a done change is committed work, so the
/// green runs committed with it must keep counting while the next change is
/// in progress. Without a HEAD, the working tree's identity is used.
pub fn standings(
    root: &Path,
    catalog: &str,
    code: Option<&Code>,
) -> Result<Option<Standings>, String> {
    if !root.join(ledger_path(catalog, APPROVALS_FILE)).is_file() {
        return Ok(None);
    }
    let inputs = Inputs::read(root, catalog)?;
    let repo = Repo::of_catalog(&root.join(parent(catalog)))?;
    let target = match repo.head() {
        Ok(head) => repo.commit_identity(&head)?,
        Err(_) => repo.working_tree_identity()?,
    };
    let (_, _, walk) = evaluate(&inputs, &target, code)?;
    let of = walk
        .facts
        .iter()
        .filter_map(
            |fact| match (fact.relation.as_str(), fact.args.as_slice()) {
                ("standing", [item, standing]) => Some((item.clone(), standing.clone())),
                _ => None,
            },
        )
        .collect();
    Ok(Some(Standings { target, of }))
}

impl Report {
    /// The bound evidence rendered as HTML, with source links made relative
    /// by `context`.
    pub fn html(&self, context: Option<&SourceContext>) -> Result<String, String> {
        let projection = project_parsed_artifact(&self.bound).map_err(|e| e.to_string())?;
        Ok(render_projection_html_with_context(
            &print_artifact(&self.bound),
            &projection,
            Some(&self.target),
            context,
        ))
    }
}

struct Reason {
    reason: String,
    subject: String,
}

#[derive(Default)]
struct Found {
    standings: BTreeMap<String, String>,
    kinds: BTreeMap<String, String>,
    reasons: BTreeMap<String, Vec<Reason>>,
    awaiting: BTreeMap<String, Vec<Reason>>,
    /// Each statement's mapped anchors that the code lacks.
    missing: BTreeMap<String, BTreeSet<String>>,
    labels: BTreeMap<String, String>,
}

impl Found {
    fn read(walk: &WalkReport, checker: Artifact) -> Self {
        let mut found = Self {
            labels: checker
                .symbols
                .into_iter()
                .map(|symbol| (symbol.value, symbol.label))
                .collect(),
            ..Self::default()
        };
        let mut awaiting = BTreeSet::new();
        let mut why: BTreeMap<String, Vec<Reason>> = BTreeMap::new();
        let reason = |reason: &str, subject: &str| Reason {
            reason: reason.into(),
            subject: subject.into(),
        };
        for fact in &walk.facts {
            match (fact.relation.as_str(), fact.args.as_slice()) {
                ("standing", [item, standing]) => {
                    found.standings.insert(item.clone(), standing.clone());
                }
                ("present", [item, kind]) => {
                    found.kinds.insert(item.clone(), kind.clone());
                }
                ("reason", [item, cause, subject]) => found
                    .reasons
                    .entry(item.clone())
                    .or_default()
                    .push(reason(cause, subject)),
                ("awaiting_approval", [item]) => {
                    awaiting.insert(item.clone());
                }
                ("approval_reason", [item, cause, subject]) => why
                    .entry(item.clone())
                    .or_default()
                    .push(reason(cause, subject)),
                ("stale_link", [item, target]) => why
                    .entry(item.clone())
                    .or_default()
                    .push(reason("linked_statement_changed", target)),
                ("broken_map", [statement, anchor]) => {
                    found
                        .missing
                        .entry(statement.clone())
                        .or_default()
                        .insert(anchor.clone());
                }
                _ => {}
            }
        }
        found.awaiting = awaiting
            .into_iter()
            .map(|item| {
                let reasons = why.remove(&item).unwrap_or_default();
                (item, reasons)
            })
            .collect();
        found
    }

    fn kind(&self, item: &str) -> &str {
        self.kinds.get(item).map_or("item", String::as_str)
    }

    fn describe(&self, item: &str, reason: &Reason) -> String {
        let label = self
            .labels
            .get(&reason.reason)
            .map_or(reason.reason.as_str(), String::as_str);
        if reason.subject == item {
            label.to_string()
        } else {
            format!("{label} (`{}`)", reason.subject)
        }
    }
}

/// The kinds Awaiting approval groups items by, in order, with each
/// group's heading.
const KIND_GROUPS: [(&str, &str); 5] = [
    ("channel", "Channels"),
    ("intent", "Intents"),
    ("definition", "Definitions"),
    ("exception", "Exceptions"),
    ("example", "Examples"),
];

/// Everything that awaits approval, with why, grouped by kind in the order
/// of [`KIND_GROUPS`].
fn awaiting(found: &Found) -> String {
    let mut out = String::from("## Awaiting approval\n");
    if found.awaiting.is_empty() {
        out.push_str("\nNothing awaits approval.\n");
        return out;
    }
    let group = |item: &str| {
        let kind = found.kind(item);
        KIND_GROUPS
            .iter()
            .position(|(group, _)| *group == kind)
            .map_or((KIND_GROUPS.len(), "Other items"), |index| {
                (index, KIND_GROUPS[index].1)
            })
    };
    let mut groups: BTreeMap<(usize, &str), String> = BTreeMap::new();
    for (item, reasons) in &found.awaiting {
        let reasons: Vec<String> = reasons
            .iter()
            .map(|reason| found.describe(item, reason))
            .collect();
        let lines = groups.entry(group(item)).or_default();
        let _ = writeln!(lines, "- `{item}`: {}", reasons.join("; "));
    }
    for ((_, heading), lines) in groups {
        let _ = write!(out, "\n### {heading}\n\n{lines}");
    }
    out
}

fn statements(inputs: &Inputs, found: &Found) -> String {
    let mut blocks = vec![String::from("## Statements\n")];
    if found.standings.is_empty() {
        blocks.push("No intents, definitions or exceptions.\n".into());
    }
    for (id, standing) in &found.standings {
        let kind = found.kind(id);
        let mut block = format!("### `{id}` ({kind}): {standing}\n\n");
        match inputs.catalog.get(id) {
            Some(statement) => {
                block.push_str(&blockquote(&statement.text));
                let _ = writeln!(block, "- File: `{}:{}`", statement.file, statement.line);
                match &statement.kind {
                    StatementKind::Definition { refines } => {
                        let _ = writeln!(block, "- Refines: `{refines}`");
                    }
                    StatementKind::Exception {
                        narrows,
                        kind,
                        channels,
                    } => {
                        let _ = writeln!(
                            block,
                            "- Narrows: `{narrows}`\n- Kind: {} on {}",
                            kind.as_str(),
                            ticked(channels)
                        );
                    }
                    StatementKind::Intent | StatementKind::Channel => {}
                }
            }
            None => removed(inputs, id, &mut block),
        }
        let examples: Vec<&Example> = inputs
            .examples
            .examples
            .iter()
            .filter(|example| example.statement == *id)
            .collect();
        if !examples.is_empty() {
            block.push_str("- Examples:\n");
        }
        for example in examples {
            for channel in &example.channels {
                let _ = writeln!(
                    block,
                    "  - `{}` on `{channel}`: {}",
                    example.id,
                    latest_runs(inputs, example, channel)
                );
            }
        }
        for reason in found.reasons.get(id).into_iter().flatten() {
            let _ = writeln!(block, "- Reason: {}", found.describe(id, reason));
        }
        blocks.push(block);
    }
    blocks.join("\n")
}

/// The latest failing and passing runs of `example` on `channel` with its
/// current text, and the source identity the passing one ran at.
fn latest_runs(inputs: &Inputs, example: &Example, channel: &str) -> String {
    let runs: Vec<_> = inputs
        .evaluated_runs()
        .filter(|run| {
            run.example == example.id && run.channel == channel && run.digest == example.digest
        })
        .collect();
    let red = if runs.iter().any(|run| run.outcome == Outcome::Fail) {
        "red fail"
    } else {
        "no red run"
    };
    let green = runs
        .iter()
        .rev()
        .find(|run| run.outcome == Outcome::Pass)
        .map_or_else(
            || "no green run".to_string(),
            |run| format!("green pass at `{}`", run.source),
        );
    format!("{red}, {green}")
}

/// A statement deleted from the catalog whose approval still stands, as its
/// latest entry recorded it.
fn removed(inputs: &Inputs, id: &str, block: &mut String) {
    let entry = inputs
        .evaluated_entries()
        .map(|(_, entry)| entry)
        .filter(|entry| entry.item == id)
        .last();
    if let Some(text) = entry.and_then(|entry| entry.text.as_ref()) {
        block.push_str(&blockquote(text));
    }
    block.push_str(
        "- File: removed from the catalog; its approval stands until a removal is approved\n",
    );
    if let Some(entry) = entry {
        let link = if entry.kind == ItemKind::Definition {
            "Refines"
        } else {
            "Narrows"
        };
        for target in entry.links.keys() {
            let _ = writeln!(block, "- {link}: `{target}`");
        }
    }
}

/// Each definition and exception with the anchors that realize it and those
/// the code lacks, then those that name none. `None` for a catalog with no
/// definition or exception.
fn map(inputs: &Inputs, found: &Found) -> Option<String> {
    let mappable: Vec<_> = inputs
        .catalog
        .values()
        .filter(|statement| {
            matches!(
                statement.kind,
                StatementKind::Definition { .. } | StatementKind::Exception { .. }
            )
        })
        .collect();
    if mappable.is_empty() {
        return None;
    }
    let (mapped, unmapped): (Vec<_>, Vec<_>) = mappable
        .into_iter()
        .partition(|statement| !statement.realized_by.is_empty());
    let mut out = String::from("## Map\n\n");
    if mapped.is_empty() {
        out.push_str("No statement names the code that realizes it.\n");
    }
    for statement in mapped {
        let id = &statement.id;
        let _ = write!(
            out,
            "- `{id}` ({}) is realized by {}",
            found.kind(id),
            ticked(&statement.realized_by)
        );
        if let Some(missing) = found.missing.get(id) {
            let missing: Vec<String> = missing.iter().cloned().collect();
            let _ = write!(out, "; missing from the code: {}", ticked(&missing));
        }
        out.push('\n');
    }
    if !unmapped.is_empty() {
        out.push_str("\n### Unmapped\n\n");
        for statement in unmapped {
            let _ = writeln!(out, "- `{}` ({})", statement.id, found.kind(&statement.id));
        }
    }
    Some(out)
}

fn ignored(inputs: &Inputs) -> Option<String> {
    if inputs.examples.issues.is_empty() {
        return None;
    }
    let mut out = String::from("## Ignored examples\n\n");
    for issue in &inputs.examples.issues {
        let at = format!("`{}:{}`", issue.file, issue.line);
        let example = match &issue.example {
            Some(id) => format!("`{id}` at {at}"),
            None => at,
        };
        let _ = writeln!(out, "- {example} (`{}`): {}", issue.test, issue.reason);
    }
    Some(out)
}

fn not_evaluated(inputs: &Inputs) -> Option<String> {
    let mut lines = String::new();
    for (index, entry) in inputs.log.entries.iter().enumerate() {
        if entry.digest_version != DIGEST_VERSION {
            let _ = writeln!(
                lines,
                "- Approval entry {} for `{}` by {}: recorded under digest version {}",
                index + 1,
                entry.item,
                code_span(&entry.by),
                entry.digest_version
            );
        }
    }
    let ledger = &inputs.ledger;
    for run in &ledger.runs {
        if run.digest_version != DIGEST_VERSION {
            let _ = writeln!(
                lines,
                "- Run of `{}` on `{}` in `{}`: recorded under digest version {}",
                run.example, run.channel, ledger.path, run.digest_version
            );
        }
    }
    (!lines.is_empty()).then(|| format!("## Not evaluated\n\n{lines}"))
}

/// The approval entries and failing runs added to the two ledgers since
/// `base`, when the ledgers at `base` are prefixes of the ledgers now.
fn since(repo: &Repo, base: &str, inputs: &Inputs, options: &Options) -> Result<String, String> {
    let reader = DirReader(options.root.clone());
    let at_base = |name: &str| -> Result<String, String> {
        Ok(repo.catalog_file_at(base, name)?.unwrap_or_default())
    };
    let altered = |file: &str| {
        format!(
            "The {file} was altered: its content at the base is not a prefix of its content now, so what was added since cannot be told apart. Review the whole {file}.\n"
        )
    };
    let mut out = format!("## Since base `{base}`\n\n### Approvals\n\n");
    let then = at_base(APPROVALS_FILE)?;
    if read_or_empty(&reader, &inputs.log_path)?.starts_with(&then) {
        let before = Log::parse(&then, &format!("{base}:{}", inputs.log_path))?
            .entries
            .len();
        let mut added = String::new();
        for (index, entry) in inputs.log.entries.iter().enumerate().skip(before) {
            let verb = match entry.action {
                Action::Approve => "approved",
                Action::Remove => "removed",
            };
            let _ = writeln!(
                added,
                "- Entry {}: {} {verb} {} `{}`",
                index + 1,
                code_span(&entry.by),
                entry.kind.as_str(),
                entry.item
            );
        }
        out.push_str(&none_or(added));
    } else {
        out.push_str(&altered("approval log"));
    }
    out.push_str("\n### Failing runs\n\n");
    let then = at_base(RUNS_FILE)?;
    if read_or_empty(&reader, &inputs.ledger.path)?.starts_with(&then) {
        let before = runs::parse(&then, &format!("{base}:{}", inputs.ledger.path))?.len();
        let failures: Vec<String> = inputs
            .ledger
            .runs
            .iter()
            .skip(before)
            .filter(|run| run.outcome == Outcome::Fail)
            .map(|run| {
                let diff = run.diff.as_deref().unwrap_or_default();
                let fence = fence(diff);
                let newline = if diff.is_empty() || diff.ends_with('\n') {
                    ""
                } else {
                    "\n"
                };
                let baseline = run.base.strip_prefix("source:").map_or_else(
                    || code_span(&run.base),
                    |identity| format!("source {}", code_span(identity)),
                );
                format!(
                    "`{}` failed on `{}` at source {}, based on {}:\n\n{fence}diff\n{diff}{newline}{fence}\n",
                    run.example,
                    run.channel,
                    code_span(&run.source),
                    baseline
                )
            })
            .collect();
        out.push_str(&none_or(failures.join("\n")));
    } else {
        out.push_str(&altered("run ledger"));
    }
    Ok(out)
}

fn none_or(text: String) -> String {
    if text.is_empty() {
        "None.\n".into()
    } else {
        text
    }
}

fn fence(text: &str) -> String {
    "`".repeat(longest_backtick_run(text).max(2) + 1)
}

/// Free text from the approval log or the run ledger as one inline code
/// span, so no name or source a branch writes can start a block of its own.
fn code_span(text: &str) -> String {
    let text = text.replace(['\r', '\n'], " ");
    let ticks = "`".repeat(longest_backtick_run(&text) + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{text}{pad}{ticks}")
}

fn longest_backtick_run(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

fn blockquote(text: &str) -> String {
    format!("> {}\n\n", text.replace('\n', "\n> "))
}

fn ticked(ids: &[String]) -> String {
    ids.iter()
        .map(|id| format!("`{id}`"))
        .collect::<Vec<_>>()
        .join(", ")
}
