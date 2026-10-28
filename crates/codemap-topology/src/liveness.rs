//! Admission at the file boundary; this crate never runs a checker or infers
//! new liveness findings. Reachability independently rejects false markers.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use codemap_graph::{EdgeKind, Snapshot};
use serde::{Deserialize, Serialize};

use crate::{Absence, callable};

const COVERAGE_POLICY: &str = "observed-call-coverage/v1";
const CATEGORIES: [&str; 8] = [
    "Observed",
    "Resolved",
    "ExternalCrate",
    "StandardLibrary",
    "Ambiguous",
    "Unmapped",
    "Failed",
    "Candidate",
];

/// Exact independently supplied selections after entry-point policy expansion.
/// Values are canonical node IDs (the values in `entry_point/public_api` facts),
/// not names or symbol keys. Lists must be sorted and duplicate-free.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub entry_policy: String,
    pub coverage_policy: String,
    pub min_call_coverage_per_mille: u16,
    pub entry_points: Vec<String>,
    pub public_api: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CheckReport {
    pub status: String,
    pub facts: Vec<Fact>,
    pub expectations: Vec<Expectation>,
}

#[derive(Debug, Deserialize)]
pub struct Fact {
    pub relation: String,
    pub args: Vec<String>,
    pub origin: String,
    pub confidence: f64,
    pub provenance: Vec<String>,
    pub why: String,
}

#[derive(Debug, Deserialize)]
pub struct Expectation {
    pub id: String,
    pub query: String,
    pub expected_count: usize,
    pub actual_count: usize,
    pub satisfied: bool,
    pub found: Vec<String>,
}

impl Fact {
    fn rendered(&self) -> String {
        format!("{}({})", self.relation, self.args.join(", "))
    }
}

fn facts<'a>(report: &'a CheckReport, relation: &str) -> Vec<&'a Fact> {
    report
        .facts
        .iter()
        .filter(|f| f.relation == relation)
        .collect()
}

fn one<'a>(report: &'a CheckReport, relation: &str) -> Result<&'a Fact> {
    let rows = facts(report, relation);
    ensure!(rows.len() == 1, "expected exactly one {relation} fact");
    Ok(rows[0])
}

fn observation_provenance(fact: &Fact, source: &str) -> bool {
    fact.provenance.iter().any(|p| p == source)
        && fact.provenance.iter().all(|p| {
            p == source
                || p.strip_prefix(&format!("{}_", fact.relation))
                    .is_some_and(|index| {
                        !index.is_empty() && index.bytes().all(|c| c.is_ascii_digit())
                    })
        })
}

fn asserted(fact: &Fact) -> Result<()> {
    ensure!(
        fact.origin == "asserted" && fact.confidence.to_bits() == 1.0_f64.to_bits(),
        "{} must be a canonical asserted observation",
        fact.relation
    );
    Ok(())
}

fn member(witness: &str, fact: &Fact) -> bool {
    let prefix = format!("{}  (conf ", fact.rendered());
    witness
        .lines()
        .any(|line| line.trim_start().starts_with(&prefix))
}

fn derivation(fact: &Fact, rule: &str, premises: &[&Fact]) -> Result<()> {
    ensure!(
        fact.origin == "derived" && fact.confidence.to_bits() == 1.0_f64.to_bits(),
        "{} must be derived",
        fact.relation
    );
    ensure!(
        fact.why
            .starts_with(&format!("{}  (conf ", fact.rendered())),
        "witness root differs from derived fact"
    );
    ensure!(
        fact.why
            .lines()
            .any(|line| line.trim() == format!("↳ via {rule}")),
        "missing {rule} derivation"
    );
    for premise in premises {
        ensure!(
            member(&fact.why, premise),
            "witness missing {} member",
            premise.relation
        );
    }
    Ok(())
}

pub fn validate(
    snapshot: &Snapshot,
    report: &CheckReport,
    selection: &Selection,
) -> Result<BTreeMap<String, Absence>> {
    ensure!(
        matches!(report.status.as_str(), "clean" | "incomplete"),
        "unsupported liveness report status"
    );
    validate_selection(snapshot, report, selection)?;
    validate_report_members(report)?;
    validate_identity(snapshot, report, selection)?;
    let coverage = validate_coverage(snapshot, report, selection)?;
    let admitted = one(report, "coverage_admitted")?;
    ensure!(
        admitted.args == [selection.entry_policy.clone()],
        "admission policy differs"
    );
    derivation(admitted, "observed_coverage_is_admitted", &[coverage])?;
    let expectation = report
        .expectations
        .iter()
        .find(|e| e.id == "coverage_is_admitted")
        .context("missing required coverage_is_admitted expectation")?;
    ensure!(
        expectation.query == "coverage_admitted(P)"
            && expectation.satisfied
            && expectation.expected_count == 1
            && expectation.actual_count == 1
            && expectation.found == [format!("P={}", selection.entry_policy)],
        "coverage admission expectation did not pass"
    );
    validate_structural_facts(snapshot, report)?;
    let reachable = reachable(snapshot, selection);
    let mut absence = BTreeMap::new();
    let finding_expectation = report
        .expectations
        .iter()
        .find(|e| e.id == "no_observed_unreached_symbols")
        .context("liveness requires the symbols checker expectation")?;
    ensure!(
        finding_expectation.query == "unused_production_symbol(S, _)"
            && finding_expectation.expected_count == 0,
        "unexpected symbols finding query"
    );
    let findings = facts(report, "unused_production_symbol");
    let rendered: BTreeSet<_> = findings
        .iter()
        .filter_map(|f| f.args.first().map(|id| format!("S={id}")))
        .collect();
    ensure!(
        finding_expectation
            .found
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            == rendered,
        "finding expectation differs from derived members"
    );
    for finding in findings {
        ensure!(
            finding.args.len() == 2 && finding.args[1] == selection.entry_policy,
            "finding entry policy differs"
        );
        let id = &finding.args[0];
        let node = snapshot
            .nodes
            .iter()
            .find(|n| &n.id == id && callable(n))
            .context("finding does not name a callable snapshot node")?;
        ensure!(
            !reachable.contains(id.as_str()),
            "finding has a path in the exact snapshot graph"
        );
        let definition = facts(report, "defines")
            .into_iter()
            .find(|f| f.args == [id.clone(), node.provenance.rel_path.clone()])
            .context("finding lacks matching definition fact")?;
        let production = facts(report, "production_file")
            .into_iter()
            .find(|f| f.args == [node.provenance.rel_path.clone()])
            .context("finding lacks production file fact")?;
        derivation(
            finding,
            "shipped_without_observed_reach",
            &[admitted, definition, production, coverage],
        )?;
        absence.insert(
            id.clone(),
            Absence {
                qualification: format!(
                    "no path in the observed graph under entry-point policy {}",
                    selection.entry_policy
                ),
                witness: finding.why.clone(),
                admission_witness: admitted.why.clone(),
            },
        );
    }
    Ok(absence)
}

fn validate_identity(
    snapshot: &Snapshot,
    report: &CheckReport,
    selection: &Selection,
) -> Result<()> {
    let identity = one(report, "snapshot_identity")?;
    asserted(identity)?;
    ensure!(
        identity.args
            == [
                snapshot.identity.snapshot_id.clone(),
                snapshot.identity.repository.id.clone(),
                snapshot.identity.commit_sha.clone(),
                snapshot.identity.tree_sha.clone(),
                selection.entry_policy.clone()
            ],
        "liveness snapshot identity or entry policy differs"
    );
    ensure!(
        observation_provenance(
            identity,
            &format!("snapshot:{}", snapshot.identity.snapshot_id)
        ),
        "snapshot identity provenance differs"
    );
    Ok(())
}

fn validate_selection(
    snapshot: &Snapshot,
    report: &CheckReport,
    selection: &Selection,
) -> Result<()> {
    ensure!(
        selection.entry_policy
            == format!("policy:entry-point/{}", snapshot.identity.language.tag()),
        "unsupported expected entry policy"
    );
    ensure!(
        selection.coverage_policy == COVERAGE_POLICY
            && selection.min_call_coverage_per_mille <= 1000,
        "unsupported coverage policy or minimum"
    );
    for (relation, expected) in [
        ("entry_point", &selection.entry_points),
        ("public_api", &selection.public_api),
    ] {
        ensure!(
            expected.windows(2).all(|pair| pair[0] < pair[1]),
            "{relation} selection must be sorted and duplicate-free"
        );
        for id in expected {
            ensure!(
                snapshot.nodes.iter().any(|n| &n.id == id && callable(n)),
                "unknown or non-callable {relation} selection"
            );
        }
        let mut actual = Vec::new();
        for fact in facts(report, relation) {
            asserted(fact)?;
            ensure!(fact.args.len() == 1, "invalid {relation} arity");
            actual.push(fact.args[0].clone());
        }
        actual.sort();
        ensure!(
            &actual == expected,
            "{relation} selection differs from liveness report"
        );
    }
    Ok(())
}

fn validate_report_members(report: &CheckReport) -> Result<()> {
    let mut members = BTreeSet::new();
    let mut tuples = BTreeSet::new();
    for fact in &report.facts {
        ensure!(
            tuples.insert((&fact.relation, &fact.args)),
            "duplicate report fact"
        );
        ensure!(
            members.insert(fact.rendered()),
            "ambiguous rendered report fact"
        );
    }
    let mut ids = BTreeSet::new();
    for expectation in &report.expectations {
        ensure!(ids.insert(&expectation.id), "duplicate expectation id");
        ensure!(
            expectation.actual_count == expectation.found.len()
                && expectation.satisfied
                    == (expectation.expected_count == expectation.actual_count),
            "inconsistent expectation counts"
        );
        let mut found = BTreeSet::new();
        for row in &expectation.found {
            ensure!(
                found.insert(row),
                "expectation references missing or duplicate fact"
            );
        }
    }
    Ok(())
}

fn validate_coverage<'a>(
    snapshot: &Snapshot,
    report: &'a CheckReport,
    selection: &Selection,
) -> Result<&'a Fact> {
    let total = snapshot.call_sites.len();
    let resolved = snapshot
        .report
        .call_sites_by_category
        .get("Resolved")
        .copied()
        .unwrap_or(0);
    let ratio = if total == 0 {
        0
    } else {
        (resolved as u128 * 1000) / total as u128
    };
    ensure!(
        total > 0
            && snapshot.identity.is_semantic()
            && snapshot.report.parse_errors == 0
            && snapshot.report.files_skipped == 0
            && ratio >= u128::from(selection.min_call_coverage_per_mille),
        "snapshot coverage is not admitted"
    );
    let coverage = one(report, "coverage_input")?;
    asserted(coverage)?;
    ensure!(
        coverage.args
            == [
                selection.entry_policy.clone(),
                total.to_string(),
                ratio.to_string(),
                selection.min_call_coverage_per_mille.to_string(),
                "1".into(),
                "0".into(),
                "0".into()
            ],
        "coverage input differs from snapshot or expected threshold"
    );
    ensure!(
        observation_provenance(coverage, &format!("policy:{COVERAGE_POLICY}")),
        "coverage provenance differs"
    );
    let census = facts(report, "call_census");
    ensure!(
        census.len() == CATEGORIES.len(),
        "missing census categories"
    );
    for category in CATEGORIES {
        let count = snapshot
            .report
            .call_sites_by_category
            .get(category)
            .copied()
            .unwrap_or(0);
        let fact = census
            .iter()
            .find(|f| f.args == [category.to_string(), count.to_string()])
            .context("census differs from snapshot")?;
        asserted(fact)?;
        ensure!(
            observation_provenance(fact, &format!("snapshot:{}", snapshot.identity.snapshot_id)),
            "census provenance differs"
        );
    }
    Ok(coverage)
}

fn validate_structural_facts(snapshot: &Snapshot, report: &CheckReport) -> Result<()> {
    for fact in facts(report, "defines") {
        asserted(fact)?;
        ensure!(
            fact.args.len() == 2
                && snapshot.nodes.iter().any(|n| n.id == fact.args[0]
                    && n.provenance.rel_path == fact.args[1]
                    && callable(n)
                    && observation_provenance(
                        fact,
                        &format!("{}:{}", n.provenance.rel_path, n.provenance.span.start_line)
                    )),
            "definition not in snapshot"
        );
    }
    for fact in facts(report, "calls") {
        asserted(fact)?;
        ensure!(
            fact.args.len() == 2
                && snapshot.edges.iter().any(|e| e.kind == EdgeKind::Calls
                    && e.from == fact.args[0]
                    && e.to == fact.args[1]
                    && observation_provenance(
                        fact,
                        &format!("{}:{}", e.provenance.rel_path, e.provenance.span.start_line)
                    )),
            "reported call not in snapshot"
        );
    }
    for fact in facts(report, "production_file") {
        asserted(fact)?;
        ensure!(
            fact.args.len() == 1 && snapshot.files.iter().any(|f| f.rel_path == fact.args[0]),
            "production file not in snapshot"
        );
        ensure!(
            observation_provenance(
                fact,
                &format!("policy:file-layout/{}", snapshot.identity.language.tag())
            ),
            "production policy differs"
        );
    }
    Ok(())
}

fn reachable<'a>(snapshot: &'a Snapshot, selection: &'a Selection) -> BTreeSet<&'a str> {
    let mut reachable: BTreeSet<_> = selection
        .entry_points
        .iter()
        .chain(&selection.public_api)
        .map(String::as_str)
        .collect();
    loop {
        let old_len = reachable.len();
        for edge in snapshot.edges.iter().filter(|e| e.kind == EdgeKind::Calls) {
            if reachable.contains(edge.from.as_str()) {
                reachable.insert(&edge.to);
            }
        }
        if reachable.len() == old_len {
            return reachable;
        }
    }
}
