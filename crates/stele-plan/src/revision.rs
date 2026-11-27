//! Product wording from a plan, and a decision reference bound to that wording.

use std::{collections::BTreeSet, fmt::Write};

use serde::{Deserialize, Serialize};

use crate::{DecisionScope, Document, StatementKind, digest::sha256};

pub struct ProductRevision {
    pub digest: String,
    pub wording: String,
    has_statements: bool,
}

/// A reference supplied after checking the person's original message.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    format: String,
    revision: String,
    actor: String,
    source: String,
}

impl Confirmation {
    fn validate(&self) -> Result<(), String> {
        if self.format != "stele-product-confirmation/1" {
            return Err("unknown product confirmation format".into());
        }
        for (field, value) in [("actor", &self.actor), ("source", &self.source)] {
            if value.trim().is_empty() || value.chars().any(char::is_control) {
                return Err(format!(
                    "confirmation {field} must be a nonempty single line"
                ));
            }
        }
        Ok(())
    }
}

impl ProductRevision {
    /// Require the digest presented to the person even if the plan has since changed.
    pub fn confirm(
        &self,
        expected_digest: &str,
        actor: &str,
        source: &str,
    ) -> Result<String, String> {
        self.require_revision(expected_digest)?;
        let confirmation = Confirmation {
            format: "stele-product-confirmation/1".into(),
            revision: expected_digest.into(),
            actor: actor.into(),
            source: source.into(),
        };
        confirmation.validate()?;
        serde_json::to_string_pretty(&confirmation).map_err(|error| error.to_string())
    }

    /// Verify the reference's format and product revision. The caller verifies the external decision.
    pub fn verify(&self, receipt: &str) -> Result<(), String> {
        let confirmation: Confirmation = serde_json::from_str(receipt)
            .map_err(|error| format!("invalid product confirmation: {error}"))?;
        confirmation.validate()?;
        self.require_revision(&confirmation.revision)
    }

    fn require_revision(&self, expected: &str) -> Result<(), String> {
        if !self.has_statements {
            return Err("the plan must name a product statement before confirmation".into());
        }
        if expected != self.digest {
            return Err(format!(
                "product revision differs: expected {expected}, current {}",
                self.digest
            ));
        }
        Ok(())
    }
}

/// Render the proposed and referenced statements with their full meaning. This
/// same rendering is presented to the requester and hashed for their decision.
pub fn product_revision(document: &Document) -> Result<ProductRevision, String> {
    let plan = document.plan.as_ref().ok_or("shaping requires a plan")?;
    let mut pending: Vec<_> = plan.proposals.keys().collect();
    pending.extend(plan.changes.values().flat_map(|change| &change.serves));
    pending.extend(
        plan.decisions
            .values()
            .flat_map(|decision| &decision.governs),
    );
    let mut ids = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !ids.insert(id) {
            continue;
        }
        let statement = if let Some(proposal) = plan.proposals.get(id) {
            pending.extend(&proposal.replaces);
            &proposal.statement
        } else {
            document
                .catalog
                .get(id)
                .ok_or_else(|| format!("missing product statement {id}"))?
        };
        match &statement.kind {
            StatementKind::Definition { refines } => pending.push(refines),
            StatementKind::Exception {
                narrows, channels, ..
            } => {
                pending.push(narrows);
                pending.extend(channels);
            }
            StatementKind::Intent | StatementKind::Channel => {}
        }
    }
    let has_statements = !ids.is_empty();
    let mut wording = format!("Plan {}\n", plan.name);
    for id in ids {
        let proposal = plan.proposals.get(id);
        let (origin, statement) = proposal.map_or_else(
            || ("Existing", &document.catalog[id]),
            |p| ("Proposed", &p.statement),
        );
        let kind = match &statement.kind {
            StatementKind::Intent => "intent".into(),
            StatementKind::Channel => "channel".into(),
            StatementKind::Definition { refines } => format!("definition of {refines}"),
            StatementKind::Exception {
                narrows,
                channels,
                kind,
            } => {
                format!(
                    "exception to {narrows} ({} on {})",
                    kind.as_str(),
                    channels.join(", ")
                )
            }
        };
        writeln!(wording, "\n{origin} {kind}: {id}").unwrap();
        line(&mut wording, "Wording", &statement.text);
        if let Some(proposal) = proposal {
            if let Some(previous) = &proposal.replaces {
                line(&mut wording, "Replaces", previous);
            }
            for example in &proposal.examples {
                line(&mut wording, "Example", example);
            }
        }
    }
    for decision in plan
        .decisions
        .values()
        .filter(|d| d.scope == DecisionScope::Product)
    {
        writeln!(wording, "\nProduct decision: {}", decision.id).unwrap();
        line(&mut wording, "Governs", &decision.governs.join(", "));
        line(&mut wording, "Wording", &decision.text);
        if let Some(rejected) = &decision.rejected {
            line(&mut wording, "Rejected", rejected);
        }
        for example in &decision.examples {
            line(&mut wording, "Example", example);
        }
    }
    let digest = sha256(&format!("stele-product-revision/1\n{wording}"));
    Ok(ProductRevision {
        digest,
        wording,
        has_statements,
    })
}

fn line(wording: &mut String, label: &str, prose: &str) {
    let text = prose.split_whitespace().collect::<Vec<_>>().join(" ");
    writeln!(wording, "  {label}: {text}").unwrap();
}
