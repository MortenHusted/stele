//! Inspect the existing code named during shaping and the product wording being decided.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use stele_plan::identity::Repo;
use stele_plan::revision::product_revision;
use stele_plan::{DirReader, Document, Plan, Statement, anchor_path, valid_anchor};

use super::{Source, locate};

pub(super) fn parse_anchor(value: &str) -> Result<String, String> {
    if valid_anchor(value) {
        Ok(value.into())
    } else {
        Err(format!(
            "{value:?} must be a repository-relative file or language:path#symbol"
        ))
    }
}

fn load(file: &Path) -> Result<(PathBuf, Document)> {
    let (base, path) = locate(file)?;
    let document = stele_plan::load(&DirReader(base.clone()), &path).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(document.plan.is_some(), "{path} is a catalog; shape a plan");
    Ok((base, document))
}

pub(super) fn confirm(file: &Path, revision: &str, actor: &str, source: &str) -> Result<()> {
    let (_, document) = load(file)?;
    let product = product_revision(&document).map_err(anyhow::Error::msg)?;
    println!(
        "{}",
        product
            .confirm(revision, actor, source)
            .map_err(anyhow::Error::msg)?
    );
    Ok(())
}

#[derive(Serialize)]
struct Mapping<'a> {
    statement: &'a str,
    text: &'a str,
    source: String,
    anchor: &'a str,
}

#[derive(Serialize)]
struct Observation<'a> {
    anchor: &'a str,
    /// Snapshot or tracked-content identity, or an explicit untracked observation.
    observed_at: Option<String>,
    mapped: Vec<Mapping<'a>>,
    same_file: Vec<Mapping<'a>>,
    proposed: Vec<Mapping<'a>>,
}

/// A nearby symbol can point to relevant product history without establishing
/// which business rule governs the anchor being inspected.
fn mappings<'a>(
    anchor: &str,
    catalog: impl Iterator<Item = &'a Statement>,
) -> (Vec<Mapping<'a>>, Vec<Mapping<'a>>) {
    let mut exact = Vec::new();
    let mut nearby = Vec::new();
    for statement in catalog {
        for mapped in &statement.realized_by {
            let list = if mapped == anchor {
                &mut exact
            } else if anchor_path(mapped) == anchor_path(anchor) {
                &mut nearby
            } else {
                continue;
            };
            list.push(Mapping {
                statement: &statement.id,
                text: &statement.text,
                source: format!("{}:{}", statement.file, statement.line),
                anchor: mapped,
            });
        }
    }
    (exact, nearby)
}

#[derive(Serialize)]
struct Shape<'a> {
    plan: &'a str,
    revision: String,
    wording: String,
    decision_matches_revision: bool,
    #[serde(flatten)]
    discovery: Discovery<'a>,
}

#[derive(Default, Serialize)]
struct Discovery<'a> {
    sources: Vec<String>,
    inspected: Vec<Observation<'a>>,
}

impl Shape<'_> {
    fn print(&self) {
        println!("{}\nRevision: {}", self.wording, self.revision);
        if self.decision_matches_revision {
            println!("Decision receipt matches this product revision.");
        }
        for source in &self.discovery.sources {
            println!("Code source: {source}");
        }
        for item in &self.discovery.inspected {
            item.print();
        }
    }
}

impl Observation<'_> {
    fn print(&self) {
        match &self.observed_at {
            Some(identity) => println!("{}: present ({identity})", self.anchor),
            None => println!("{}: not found in the inspected code", self.anchor),
        }
        for mapping in &self.mapped {
            println!(
                "  mapped: {} ({}) {}",
                mapping.statement, mapping.source, mapping.text
            );
        }
        for mapping in &self.same_file {
            println!(
                "  same file: {} at {} ({})",
                mapping.statement, mapping.anchor, mapping.source
            );
        }
        for mapping in &self.proposed {
            println!(
                "  proposed mapping: {} ({}) {}",
                mapping.statement, mapping.source, mapping.text
            );
        }
        if self.observed_at.is_some() && self.mapped.is_empty() && self.proposed.is_empty() {
            println!(
                "  No exact catalog mapping. Check the current behavior and its product reason. Propose the missing statement when confirmed."
            );
        }
    }
}

fn read_code(
    source: &Source,
    base: &Path,
    plan: &Plan,
    anchors: &BTreeSet<&str>,
) -> Result<stele_plan::Code> {
    let catalog_root = plan.catalog.as_ref().map(|file| base.join(file));
    let catalog_dir = catalog_root
        .as_deref()
        .and_then(Path::parent)
        .unwrap_or(base);
    if let Some(snapshot) = &source.code.snapshot {
        // An exported snapshot and a local worktree may describe different commits.
        return Ok(stele_plan::Code {
            snapshots: vec![codemap_lemmaspec::cli::read_snapshot(snapshot)?],
            working_tree: None,
        });
    }
    if source.code.languages.is_empty() && anchors.iter().all(|anchor| !anchor.contains(':')) {
        let dir = source.code.path.as_deref().unwrap_or(catalog_dir);
        let tree = Repo::of_catalog(dir)
            .and_then(|repo| repo.working_tree())
            .map_err(anyhow::Error::msg)?;
        return Ok(stele_plan::Code {
            snapshots: Vec::new(),
            working_tree: Some(tree),
        });
    }
    source.code.read(base, catalog_dir, anchors)
}

pub(super) fn show(
    source: &Source,
    inspect: &[String],
    decision: Option<&Path>,
    json: bool,
) -> Result<()> {
    let (base, document) = load(&source.file)?;
    let plan = document.plan.as_ref().expect("loaded a plan");
    let product = product_revision(&document).map_err(anyhow::Error::msg)?;
    if let Some(file) = decision {
        let receipt = std::fs::read_to_string(file)
            .with_context(|| format!("read decision {}", file.display()))?;
        product.verify(&receipt).map_err(anyhow::Error::msg)?;
    }
    let shape = Shape {
        plan: &plan.name,
        revision: product.digest,
        wording: product.wording,
        decision_matches_revision: decision.is_some(),
        discovery: discover(source, &base, &document, inspect)?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&shape)?);
    } else {
        shape.print();
    }
    Ok(())
}

fn discover<'a>(
    source: &Source,
    base: &Path,
    document: &'a Document,
    inspect: &'a [String],
) -> Result<Discovery<'a>> {
    let plan = document.plan.as_ref().expect("loaded a plan");
    let anchors: BTreeSet<&str> = plan
        .changes
        .values()
        .flat_map(|change| &change.touches)
        .chain(
            plan.proposals
                .values()
                .flat_map(|p| &p.statement.realized_by),
        )
        .chain(inspect)
        .map(String::as_str)
        .collect();
    if anchors.is_empty() {
        return Ok(Discovery::default());
    }
    let code = read_code(source, base, plan, &anchors)?;
    let (sources, untracked) = code_sources(&code, source.code.path.as_deref().unwrap_or(base))?;
    let mut inspected = Vec::new();
    for anchor in anchors {
        let (mapped, same_file) = mappings(anchor, document.catalog.values());
        let (proposed, _) = mappings(anchor, plan.proposals.values().map(|p| &p.statement));
        inspected.push(Observation {
            anchor,
            observed_at: observed_at(anchor, &code, &untracked)?,
            mapped,
            same_file,
            proposed,
        });
    }
    Ok(Discovery { sources, inspected })
}

fn code_sources(code: &stele_plan::Code, path: &Path) -> Result<(Vec<String>, BTreeSet<String>)> {
    let mut sources = Vec::new();
    for snapshot in &code.snapshots {
        let id = &snapshot.identity;
        sources.push(format!(
            "{} {} tree {} ({}{})",
            id.language.tag(),
            id.commit_sha,
            id.tree_sha,
            id.snapshot_id,
            if id.dirty { ", dirty" } else { "" }
        ));
    }
    let Some(tree) = &code.working_tree else {
        return Ok((sources, BTreeSet::new()));
    };
    let repo = Repo::of_catalog(path).map_err(anyhow::Error::msg)?;
    let head = repo.head().unwrap_or_else(|_| "unavailable".into());
    sources.push(format!(
        "working tree (HEAD {head}, tracked content identity {})",
        tree.identity
    ));
    Ok((sources, repo.untracked_files().map_err(anyhow::Error::msg)?))
}

fn observed_at(
    anchor: &str,
    code: &stele_plan::Code,
    untracked: &BTreeSet<String>,
) -> Result<Option<String>> {
    let identity = code
        .find(anchor)
        .map_err(anyhow::Error::msg)?
        .and_then(|basis| basis.identity);
    if untracked.contains(anchor)
        && code
            .working_tree
            .as_ref()
            .is_some_and(|tree| identity.as_ref() == Some(&tree.identity))
    {
        return Ok(Some("untracked working tree file".into()));
    }
    Ok(identity)
}
