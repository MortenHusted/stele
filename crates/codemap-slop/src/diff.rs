//! The explainable change report: what became harder to maintain between
//! a base and a head snapshot.
//!
//! Callables are matched by node id. A callable `infer_moves` pairs across
//! files (same kind and name, unique on both sides, different path) is
//! treated as one callable that moved and is labelled so; it is never
//! reported as removed and added. A rename inside one file is, at Layer
//! 0, a death and a birth, and the report says exactly that.

use std::collections::BTreeMap;

use codemap_graph::analysis::{NodeMetrics, metrics};
use codemap_graph::diff::{SnapshotDiff, infer_moves};
use codemap_graph::nav::Graph;
use codemap_graph::schema::{CloneGroup, EdgeKind, NodeKind, Snapshot};
use serde::Serialize;

use crate::Measured;
use crate::metrics::{
    CallSites, Callable, Hotspot, SiteCounts, Summary, Twin, call_sites, callables_in, hotspots,
    summary, summary_all_files, twins,
};
use crate::policy::{self, Scope, ratio_alone_flag};

/// What a finding says about one callable. Ordered by how much a reviewer
/// needs to see it first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// `cc` went from at most the cutoff to above it.
    CrossedAboveCutoff,
    /// A new callable that starts above the cutoff.
    AddedAboveCutoff,
    /// Mass grew without crossing the cutoff.
    Harder,
    /// `cc` went from above the cutoff to at most it.
    CrossedBelowCutoff,
    /// A callable above the cutoff that no longer exists.
    RemovedAboveCutoff,
    /// Mass fell without crossing the cutoff.
    Easier,
}

/// `cc`, `sloc`, and mass on one side.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Point {
    pub cc: u32,
    pub sloc: u32,
    pub mass: f64,
}

impl Point {
    fn of(callable: &Callable<'_>) -> Self {
        Self {
            cc: callable.measure.cc,
            sloc: callable.measure.sloc,
            mass: callable.mass(),
        }
    }
}

/// A verified-calls figure on both sides (`calls` edges in or out, from
/// `codemap-graph::analysis::metrics`). Absent on a side whose snapshot
/// is syntax-only, or where the callable does not exist
/// ([`policy::COUPLING_VERIFIED_NEVER_ALONE`]).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Coupling {
    pub base: Option<usize>,
    pub head: Option<usize>,
}

/// One finding about one callable.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub kind: FindingKind,
    /// The head node id, or the base id for a removed callable.
    pub node_id: String,
    pub name: String,
    pub rel_path: String,
    pub start_line: u32,
    pub end_line: u32,
    /// The base node id when `infer_moves` paired this callable across
    /// files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<Point>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<Point>,
    pub cc_delta: i64,
    pub sloc_delta: i64,
    /// Verified callers, base and head.
    pub fan_in: Coupling,
    /// Verified callees, base and head.
    pub fan_out: Coupling,
    /// The unverified call sites inside the callable on the side the
    /// finding describes: the head, or the base for a removed callable.
    pub sites: SiteCounts,
}

/// One clone group the change introduced.
#[derive(Debug, Clone, Serialize)]
pub struct CloneDelta {
    pub id: String,
    pub occurrences: usize,
    pub tokens: u32,
    pub lines: u32,
    /// `path:start-end` of the first occurrence.
    pub first: String,
}

impl CloneDelta {
    fn of(group: &CloneGroup) -> Self {
        let first = &group.occurrences[0];
        Self {
            id: group.id.clone(),
            occurrences: group.occurrences.len(),
            tokens: group.tokens,
            lines: group.lines,
            first: format!("{}:{}-{}", first.rel_path, first.start_line, first.end_line),
        }
    }
}

/// Moves: head id -> base id, callable kinds only, as `infer_moves` pairs
/// them across files.
fn moved_callables<'a>(
    base: &'a Snapshot,
    head: &'a Snapshot,
    base_callables: &BTreeMap<&'a str, Callable<'a>>,
    head_callables: &BTreeMap<&'a str, Callable<'a>>,
) -> BTreeMap<&'a str, &'a str> {
    let structural = SnapshotDiff::between(base, head);
    infer_moves(&structural)
        .iter()
        .filter(|pair| {
            matches!(
                pair.to.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Macro
            )
        })
        .filter_map(|pair| {
            let to = head_callables.get_key_value(pair.to.id.as_str())?.0;
            let from = base_callables.get_key_value(pair.from.id.as_str())?.0;
            Some((*to, *from))
        })
        .collect()
}

/// Groups the head carries and the base does not, heaviest first (tokens
/// times occurrences), and how many base groups are gone. Ids are content
/// digests, so a group that merely moved keeps its id.
fn clone_deltas(base: &[CloneGroup], head: &[CloneGroup]) -> (Vec<CloneDelta>, usize) {
    let base_ids: BTreeMap<&str, &CloneGroup> = base
        .iter()
        .map(|group| (group.id.as_str(), group))
        .collect();
    let head_ids: BTreeMap<&str, &CloneGroup> = head
        .iter()
        .map(|group| (group.id.as_str(), group))
        .collect();
    let weight = |delta: &CloneDelta| u64::from(delta.tokens) * delta.occurrences as u64;
    let mut introduced: Vec<CloneDelta> = head_ids
        .iter()
        .filter(|(id, _)| !base_ids.contains_key(*id))
        .map(|(_, group)| CloneDelta::of(group))
        .collect();
    introduced.sort_by(|a, b| weight(b).cmp(&weight(a)).then(a.id.cmp(&b.id)));
    let removed = base_ids
        .keys()
        .filter(|id| !head_ids.contains_key(*id))
        .count();
    (introduced, removed)
}

/// A head hotspot with what it was in the base.
#[derive(Debug, Clone, Serialize)]
pub struct HotspotDelta {
    #[serde(flatten)]
    pub hotspot: Hotspot,
    /// `cc` in the base, when the callable existed there (by id or by
    /// move); absent for a new callable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_cc: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cc_delta: Option<i64>,
}

/// The change report.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeReport {
    pub base_commit: String,
    pub head_commit: String,
    pub policy: String,
    pub base: Summary,
    pub head: Summary,
    /// Findings in the order a reviewer reads them: kind, then the size of
    /// the `cc` move, then node id.
    pub findings: Vec<Finding>,
    /// The head's heaviest callables with their base `cc`.
    pub hotspots: Vec<HotspotDelta>,
    pub added_callables: usize,
    pub removed_callables: usize,
    pub moved_callables: usize,
    /// Clone groups in the head that the base did not carry (by content
    /// id), heaviest first. Listed next to `moved_callables`: a callable
    /// `infer_moves` paired is a move, never a clone. Absent without clone measures.
    pub clone_groups_introduced: Option<Vec<CloneDelta>>,
    /// Clone groups the base carried and the head does not; absent without clone measures.
    pub clone_groups_removed: Option<usize>,
    /// [`policy::RATIO_NEVER_ALONE`]: the reason an improved erosion is not
    /// to be read alone, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio_flag: Option<String>,
    /// Verified `calls` edges in each snapshot; absent for a syntax
    /// snapshot, whose coupling columns read `n/a`.
    pub verified_calls: Coupling,
    /// The files both sides are held to; not serialized when it is every
    /// file, so an unscoped document keeps its bytes.
    #[serde(skip_serializing_if = "Scope::is_all_files")]
    pub scope: Scope,
    /// The all-files summaries beside the in-scope ones
    /// ([`policy::SCOPE_PRINTS_BOTH`]); absent when the scope is every
    /// file and they would repeat `base` and `head`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_files: Option<AllFiles>,
    /// Twin pairs in the head (both callables in scope) that the base did
    /// not carry, by the pair's node ids, near pairs first
    /// ([`policy::TWINS_NEVER_FOLDED`]). Absent unless both snapshots were
    /// extracted with `--twins`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub twin_pairs_introduced: Option<Vec<Twin>>,
    /// Twin pairs the base carried (in scope) and the head does not; absent likewise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub twin_pairs_removed: Option<usize>,
}

/// Base and head over every file.
#[derive(Debug, Clone, Serialize)]
pub struct AllFiles {
    pub base: Summary,
    pub head: Summary,
}

impl ChangeReport {
    /// Callables that are above the cutoff in the head and were not in the
    /// base: crossed up or added above.
    pub fn new_high_cc_callables(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| {
                matches!(
                    finding.kind,
                    FindingKind::CrossedAboveCutoff | FindingKind::AddedAboveCutoff
                )
            })
            .count()
    }

    pub fn erosion_delta(&self) -> f64 {
        self.head.erosion - self.base.erosion
    }

    pub fn loc_delta(&self) -> i64 {
        i64::try_from(self.head.loc).unwrap_or(i64::MAX)
            - i64::try_from(self.base.loc).unwrap_or(i64::MAX)
    }
}

/// One side's graph metrics and whether its snapshot verified any call.
struct Side<'a> {
    metrics: &'a BTreeMap<String, NodeMetrics>,
    semantic: bool,
}

impl Side<'_> {
    fn of(&self, id: Option<&str>) -> Option<&NodeMetrics> {
        if !self.semantic {
            return None;
        }
        id.and_then(|id| self.metrics.get(id))
    }
}

/// Verified `calls` in and out on both sides. Only `calls` edges count:
/// `contains`, `inherits`, and the rest are structure, not coupling.
fn coupling(
    base: &Side<'_>,
    base_id: Option<&str>,
    head: &Side<'_>,
    head_id: Option<&str>,
) -> (Coupling, Coupling) {
    let base_m = base.of(base_id);
    let head_m = head.of(head_id);
    (
        Coupling {
            base: base_m.map(|m| m.calls_in),
            head: head_m.map(|m| m.calls_in),
        },
        Coupling {
            base: base_m.map(|m| m.calls_out),
            head: head_m.map(|m| m.calls_out),
        },
    )
}

fn sites_of(sites: &BTreeMap<String, CallSites>, id: &str) -> SiteCounts {
    sites.get(id).map_or_else(SiteCounts::default, |c| c.sites)
}

/// What the head pass learned: which base callables were matched (by id
/// or by move), the base `cc` per head id, and how many head callables
/// were new.
#[derive(Default)]
struct Matched<'a> {
    base_ids: Vec<&'a str>,
    base_cc: BTreeMap<&'a str, u32>,
    added: usize,
}

/// The finding for one head callable, if it changed or arrived above the
/// cutoff. A matched pair that did not change its `cc` or `sloc` is no
/// finding.
fn head_finding<'a>(
    head_callable: &Callable<'a>,
    base: Option<(&&'a str, &Callable<'a>)>,
    fan_in: Coupling,
    fan_out: Coupling,
    sites: SiteCounts,
    matched: &mut Matched<'a>,
) -> Option<Finding> {
    let head_id = head_callable.node.id.as_str();
    let head_point = Point::of(head_callable);
    let span = &head_callable.node.provenance.span;
    let mut finding = Finding {
        kind: FindingKind::AddedAboveCutoff,
        node_id: head_id.to_string(),
        name: head_callable.node.name.clone(),
        rel_path: head_callable.node.provenance.rel_path.clone(),
        start_line: span.start_line,
        end_line: span.end_line,
        moved_from: None,
        base: None,
        head: Some(head_point),
        cc_delta: i64::from(head_point.cc),
        sloc_delta: i64::from(head_point.sloc),
        fan_in,
        fan_out,
        sites,
    };
    let Some((base_id, base_callable)) = base else {
        matched.added += 1;
        return policy::is_high_cc(head_point.cc).then_some(finding);
    };
    let base_id: &'a str = base_id;
    matched.base_ids.push(base_id);
    matched
        .base_cc
        .insert(head_callable.node.id.as_str(), base_callable.measure.cc);
    let base_point = Point::of(base_callable);
    finding.base = Some(base_point);
    finding.moved_from = (base_id != head_id).then(|| base_id.to_string());
    finding.cc_delta = i64::from(head_point.cc) - i64::from(base_point.cc);
    finding.sloc_delta = i64::from(head_point.sloc) - i64::from(base_point.sloc);
    let was_high = policy::is_high_cc(base_point.cc);
    let is_high = policy::is_high_cc(head_point.cc);
    finding.kind = match (was_high, is_high) {
        (false, true) => FindingKind::CrossedAboveCutoff,
        (true, false) => FindingKind::CrossedBelowCutoff,
        _ if head_point.mass > base_point.mass => FindingKind::Harder,
        _ if head_point.mass < base_point.mass => FindingKind::Easier,
        _ => return None,
    };
    Some(finding)
}

/// The finding for a base callable above the cutoff that the head no
/// longer has.
fn removed_finding(
    base_callable: &Callable<'_>,
    fan_in: Coupling,
    fan_out: Coupling,
    sites: SiteCounts,
) -> Finding {
    let base_point = Point::of(base_callable);
    let span = &base_callable.node.provenance.span;
    Finding {
        kind: FindingKind::RemovedAboveCutoff,
        node_id: base_callable.node.id.clone(),
        name: base_callable.node.name.clone(),
        rel_path: base_callable.node.provenance.rel_path.clone(),
        start_line: span.start_line,
        end_line: span.end_line,
        moved_from: None,
        base: Some(base_point),
        head: None,
        cc_delta: -i64::from(base_point.cc),
        sloc_delta: -i64::from(base_point.sloc),
        fan_in,
        fan_out,
        sites,
    }
}

/// Build the change report for an admitted pair. `top` bounds the hotspot
/// list.
#[allow(clippy::too_many_lines)] // one report assembled field by field
pub fn change_report(base: &Measured<'_>, head: &Measured<'_>, top: usize) -> ChangeReport {
    let base_snapshot = base.snapshot;
    let head_snapshot = head.snapshot;
    let base_callables: BTreeMap<&str, Callable<'_>> = callables_in(base)
        .into_iter()
        .map(|callable| (callable.node.id.as_str(), callable))
        .collect();
    let head_callables: BTreeMap<&str, Callable<'_>> = callables_in(head)
        .into_iter()
        .map(|callable| (callable.node.id.as_str(), callable))
        .collect();

    let moved = moved_callables(
        base_snapshot,
        head_snapshot,
        &base_callables,
        &head_callables,
    );

    let base_graph = Graph::from_snapshot(base_snapshot);
    let head_graph = Graph::from_snapshot(head_snapshot);
    let base_metrics = metrics(&base_graph);
    let head_metrics = metrics(&head_graph);
    let base_side = Side {
        metrics: &base_metrics,
        semantic: base_snapshot.identity.is_semantic(),
    };
    let head_side = Side {
        metrics: &head_metrics,
        semantic: head_snapshot.identity.is_semantic(),
    };
    let base_sites = call_sites(base_snapshot);
    let head_sites = call_sites(head_snapshot);

    let mut matched = Matched::default();
    let mut findings: Vec<Finding> = head_callables
        .iter()
        .filter_map(|(head_id, head_callable)| {
            let base_id = if base_callables.contains_key(head_id) {
                Some(*head_id)
            } else {
                moved.get(head_id).copied()
            };
            let (fan_in, fan_out) = coupling(&base_side, base_id, &head_side, Some(head_id));
            head_finding(
                head_callable,
                base_id.and_then(|id| base_callables.get_key_value(id)),
                fan_in,
                fan_out,
                sites_of(&head_sites, head_id),
                &mut matched,
            )
        })
        .collect();

    let mut removed = 0;
    for (base_id, base_callable) in &base_callables {
        if matched.base_ids.contains(base_id) {
            continue;
        }
        removed += 1;
        if !policy::is_high_cc(base_callable.measure.cc) {
            continue;
        }
        let (fan_in, fan_out) = coupling(&base_side, Some(base_id), &head_side, None);
        findings.push(removed_finding(
            base_callable,
            fan_in,
            fan_out,
            sites_of(&base_sites, base_id),
        ));
    }

    findings.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then(b.cc_delta.abs().cmp(&a.cc_delta.abs()))
            .then(a.node_id.cmp(&b.node_id))
    });

    let hotspots = hotspots(head, top)
        .into_iter()
        .map(|hotspot| {
            let base_cc = matched.base_cc.get(hotspot.node_id.as_str()).copied();
            HotspotDelta {
                cc_delta: base_cc.map(|cc| i64::from(hotspot.cc) - i64::from(cc)),
                base_cc,
                hotspot,
            }
        })
        .collect();

    let (clone_groups_introduced, clone_groups_removed) =
        if codemap_graph::schema::clone_measures_available(base_snapshot.identity.language) {
            let (introduced, removed) =
                clone_deltas(&base_snapshot.clone_groups, &head_snapshot.clone_groups);
            (Some(introduced), Some(removed))
        } else {
            (None, None)
        };

    let (twin_pairs_introduced, twin_pairs_removed) = twin_deltas(base, head);

    let base_summary = summary(base);
    let head_summary = summary(head);
    let ratio_flag = ratio_alone_flag(
        base_summary.erosion,
        &base_summary.counts,
        head_summary.erosion,
        &head_summary.counts,
    );
    ChangeReport {
        base_commit: base_snapshot.identity.commit_sha.clone(),
        head_commit: head_snapshot.identity.commit_sha.clone(),
        policy: base.policy.to_string(),
        base: base_summary,
        head: head_summary,
        findings,
        hotspots,
        added_callables: matched.added,
        removed_callables: removed,
        moved_callables: moved.len(),
        clone_groups_introduced,
        clone_groups_removed,
        ratio_flag,
        verified_calls: Coupling {
            base: verified_calls(base_snapshot),
            head: verified_calls(head_snapshot),
        },
        scope: head.scope.clone(),
        all_files: (!head.scope.is_all_files()).then(|| AllFiles {
            base: summary_all_files(base),
            head: summary_all_files(head),
        }),
        twin_pairs_introduced,
        twin_pairs_removed,
    }
}

/// Head twin pairs the base did not carry and base pairs the head lost,
/// both in scope, keyed by the pair's node ids. `None` unless both
/// snapshots carry twins: a pair count against an unmeasured side would
/// read as every pair being new.
fn twin_deltas(base: &Measured<'_>, head: &Measured<'_>) -> (Option<Vec<Twin>>, Option<usize>) {
    if base.snapshot.identity.twins_policy.is_none()
        || head.snapshot.identity.twins_policy.is_none()
    {
        return (None, None);
    }
    let key = |twin: &Twin| (twin.a.node_id.clone(), twin.b.node_id.clone());
    let base_pairs: std::collections::BTreeSet<(String, String)> =
        twins(base, usize::MAX).iter().map(key).collect();
    let head_twins = twins(head, usize::MAX);
    let head_keys: std::collections::BTreeSet<(String, String)> =
        head_twins.iter().map(key).collect();
    let introduced: Vec<Twin> = head_twins
        .into_iter()
        .filter(|twin| !base_pairs.contains(&key(twin)))
        .collect();
    let removed = base_pairs.difference(&head_keys).count();
    (Some(introduced), Some(removed))
}

/// The `calls` edges a snapshot verified; absent on a syntax snapshot.
fn verified_calls(snapshot: &Snapshot) -> Option<usize> {
    snapshot.identity.is_semantic().then(|| {
        snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Calls)
            .count()
    })
}
