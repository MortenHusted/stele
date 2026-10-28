//! Narrow rule-card Markdown input and declaration-containment evidence.
//! Claims are always reviewer declarations; no source bytes or business logic
//! are inspected. See `CITATIONS.md` for the accepted grammar and limits.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail, ensure};
use codemap_graph::{Graph, Snapshot};

use crate::lemmaspec::{Basis, Fact, Readable, SymbolLabel};
use crate::{Checker, Options, lemmaspec};

struct Card {
    id: String,
    title: String,
    source: Option<String>,
    claim: Option<String>,
}

fn parse(markdown: &str) -> Result<Vec<Card>> {
    let mut cards = Vec::new();
    let mut current: Option<Card> = None;
    let mut ids = BTreeSet::new();
    let mut fence: Option<(char, usize)> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(marker @ ('`' | '~')) = trimmed.chars().next() {
            let length = trimmed.chars().take_while(|c| *c == marker).count();
            if length >= 3 {
                if let Some((open, width)) = fence {
                    if open == marker && length >= width && trimmed[length..].trim().is_empty() {
                        fence = None;
                    }
                } else {
                    fence = Some((marker, length));
                }
                continue;
            }
        }
        if fence.is_some() {
            continue;
        }
        if line.starts_with('#') {
            if let Some(card) = current.take() {
                cards.push(card);
            }
            if let Some(heading) = line.strip_prefix("### RULE-") {
                let (digits, title) = heading
                    .split_once(": ")
                    .context("expected ### RULE-NNN: title")?;
                ensure!(
                    digits.len() == 3
                        && digits.bytes().all(|b| b.is_ascii_digit())
                        && !title.trim().is_empty(),
                    "expected ### RULE-NNN: title"
                );
                let id = format!("RULE-{digits}");
                ensure!(ids.insert(id.clone()), "duplicate rule ID {id}");
                current = Some(Card {
                    id,
                    title: title.into(),
                    source: None,
                    claim: None,
                });
            }
            continue;
        }
        let Some(card) = &mut current else { continue };
        if let Some(source) = line.strip_prefix("**Source:**") {
            ensure!(
                card.source.is_none(),
                "{} has duplicate Source fields",
                card.id
            );
            card.source = Some(source.trim().into());
        }
        if let Some(claim) = line.strip_prefix("**Plain English:**") {
            ensure!(
                card.claim.is_none(),
                "{} has duplicate Plain English fields",
                card.id
            );
            ensure!(
                !claim.trim().is_empty(),
                "{} has empty Plain English",
                card.id
            );
            card.claim = Some(claim.trim().into());
        }
    }
    if let Some(card) = current {
        cards.push(card);
    }
    ensure!(fence.is_none(), "unclosed Markdown fence");
    ensure!(!cards.is_empty(), "no rule cards found");
    for card in &cards {
        ensure!(
            card.claim.is_some(),
            "{} has no Plain English field",
            card.id
        );
    }
    Ok(cards)
}

/// Produce typed `LemmaSpec` 0.4 evidence. Every parsed card retains separate
/// location validity and semantic-unverified results, even for bad locations.
pub fn project(snapshot: &Snapshot, options: &Options, markdown: &str) -> Result<String> {
    codemap_graph::validate(snapshot).context("snapshot failed validation")?;
    ensure!(
        options.checker == Checker::Citations,
        "rule cards require the citations checker"
    );
    ensure!(
        !snapshot.identity.dirty || options.allow_dirty,
        "dirty snapshot requires --allow-dirty"
    );
    if !options.core_files.is_empty()
        || !options.presentation_files.is_empty()
        || !options.entry_points.is_empty()
        || !options.public_api.is_empty()
    {
        bail!("citations does not accept core, presentation, entry, or public API selections");
    }
    let cards = parse(markdown)?;
    let graph = Graph::from_snapshot(snapshot);
    let mut facts = vec![Fact {
        relation: "snapshot_identity",
        args: vec![
            snapshot.identity.snapshot_id.clone(),
            snapshot.identity.repository.id.clone(),
            snapshot.identity.commit_sha.clone(),
            snapshot.identity.tree_sha.clone(),
            "policy:declaration-containment/v1".into(),
        ],
        provenance: Some(format!("snapshot:{}", snapshot.identity.snapshot_id)),
        basis: Basis::Snapshot,
    }];
    let mut labels = Vec::new();
    let card_count = cards.len();
    for card in cards {
        let (card_facts, label) = project_card(&graph, card);
        facts.extend(card_facts);
        labels.push(label);
    }
    facts.sort();
    labels.extend(crate::labels(snapshot, &facts));
    labels.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    let notes = coverage_notes(snapshot);
    let spec = options
        .spec_name
        .clone()
        .unwrap_or_else(|| lemmaspec::spec_name(&snapshot.identity.repository.id));
    let mut expectations = Checker::Citations.expectations();
    expectations.insert(
        0,
        lemmaspec::Expectation {
            id: "location_results_for_every_card",
            query: "location_valid(R, V)",
            count: card_count,
        },
    );
    Ok(lemmaspec::render_with_metadata(
        &[crate::question(Checker::Citations).into()],
        &spec,
        &facts,
        &expectations,
        Some(&Readable {
            snapshot_id: &snapshot.identity.snapshot_id,
            notes: &notes,
            labels: &labels,
        }),
    ))
}

fn coverage_notes(snapshot: &Snapshot) -> String {
    let mut notes = vec![format!(
        "Snapshot: {}; repository: {}; commit: {}; tree: {}; language: {}; dirty: {}.\nPolicy: declaration-containment/v1.\nExecutable classification: unsupported (declaration spans cannot distinguish comments, blanks, and executable lines).\nEvery claim is reviewer-declared. Semantic support and business-rule correctness remain unverified, even for a valid location.",
        snapshot.identity.snapshot_id,
        snapshot.identity.repository.id,
        snapshot.identity.commit_sha,
        snapshot.identity.tree_sha,
        snapshot.identity.language.tag(),
        snapshot.identity.dirty,
    )];
    let coverage = crate::coverage::Coverage::from_snapshot(snapshot);
    notes.push(format!(
        "Extraction scope: {} recorded {} source files; skipped files: {}; parse errors: {}; semantic: {}.\nScope includes only snapshot file records; excluded-source inventory and reasons: unknown.\nUnsupported observations: unknown (snapshot schema has no unsupported-observation accounting).\nEntry-point policy: {}; not applied to declaration containment.\nResolved sites / all recognized sites: {}/{}. An empty census has no coverage ratio and cannot admit absence. Call coverage is descriptive here; location validity does not require absence admission.",
        snapshot.files.len(), snapshot.identity.language.tag(), coverage.files_skipped,
        coverage.parse_errors, coverage.semantic,
        crate::policy::provenance(crate::policy::ENTRY_POINT, snapshot.identity.language),
        coverage.resolved, coverage.recognized,
    ));
    for category in crate::coverage::categories(snapshot.identity.language) {
        let key = category.report_key();
        notes.push(format!(
            "Call-site category {key}: {}.",
            snapshot
                .report
                .call_sites_by_category
                .get(key)
                .copied()
                .unwrap_or(0)
        ));
    }
    let mut paths: Vec<_> = snapshot
        .files
        .iter()
        .map(|file| file.rel_path.as_str())
        .collect();
    paths.sort_unstable();
    for path in paths {
        notes.push(format!("Scope file: {}", lemmaspec::term(path)));
    }
    notes.join("\n")
}

fn project_card(graph: &Graph<'_>, card: Card) -> (Vec<Fact>, SymbolLabel) {
    let mut facts = Vec::new();
    let raw = card.source.as_deref().unwrap_or("");
    let citation = raw
        .strip_prefix('`')
        .and_then(|value| value.strip_suffix('`'))
        .filter(|value| !value.contains('`'));
    let found = citation.and_then(|query| graph.locate(query).ok());
    let valid = found
        .as_ref()
        .is_some_and(|result| !result.definitions.is_empty());
    let reason = match &found {
        None => "malformed_or_missing_location",
        Some(result) if result.definitions.is_empty() => "no_containing_definition",
        Some(_) => "declaration_containment_only",
    };
    facts.push(Fact {
        relation: "rule_card",
        args: vec![
            card.id.clone(),
            card.claim.expect("parser checks claim"),
            citation.unwrap_or(raw).into(),
        ],
        provenance: Some(format!("rule-card:{}", card.id)),
        basis: Basis::ReviewerDeclared,
    });
    facts.push(Fact {
        relation: "location_status",
        args: vec![card.id.clone(), u8::from(valid).to_string(), reason.into()],
        provenance: Some("policy:declaration-containment/v1".into()),
        basis: Basis::Policy,
    });
    let label = SymbolLabel {
        symbol: card.id.clone(),
        label: format!("{}: {}", card.id, card.title),
        source: found.as_ref().filter(|_| valid).and_then(|result| {
            crate::source_link(&result.location.rel_path, result.location.start_line)
        }),
    };
    if let Some(result) = found {
        for node in result.definitions {
            facts.push(Fact {
                relation: "located",
                args: vec![card.id.clone(), node.id.clone()],
                provenance: Some(result.query.clone()),
                basis: Basis::Snapshot,
            });
        }
    }
    (facts, label)
}
