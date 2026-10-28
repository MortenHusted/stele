//! Erosion, LOC, and mass over a measured snapshot, with the absolute
//! counts every ratio is printed beside.

use std::collections::BTreeMap;

use codemap_graph::analysis::clusters;
use codemap_graph::nav::Graph;
use codemap_graph::schema::{
    CallSiteCategory, EdgeKind, Measure, Node, NodeKind, Snapshot, Span, TwinPair,
};
use serde::Serialize;

use crate::Measured;
use crate::policy::{self, Counts, Scope};

/// Label-propagation rounds for per-cluster erosion; `stele code clusters`
/// defaults to the same.
pub const CLUSTER_ROUNDS: u32 = 10;

/// A callable node joined with its measure.
#[derive(Debug, Clone, Copy)]
pub struct Callable<'a> {
    pub node: &'a Node,
    pub measure: &'a Measure,
}

impl Callable<'_> {
    pub fn mass(&self) -> f64 {
        policy::mass(self.measure.cc, self.measure.sloc)
    }
}

/// Every measured callable, in node-id order.
pub fn callables(snapshot: &Snapshot) -> Vec<Callable<'_>> {
    let nodes: BTreeMap<&str, &Node> = snapshot
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Macro
            )
        })
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut out: Vec<Callable<'_>> = snapshot
        .measures
        .iter()
        .filter_map(|measure| {
            nodes
                .get(measure.node_id.as_str())
                .map(|node| Callable { node, measure })
        })
        .collect();
    out.sort_by(|a, b| a.node.id.cmp(&b.node.id));
    out
}

/// The measured callables inside the admitted scope, in node-id order.
#[must_use]
pub fn callables_in<'a>(measured: &Measured<'a>) -> Vec<Callable<'a>> {
    callables_under(measured.snapshot, measured.scope)
}

fn callables_under<'a>(snapshot: &'a Snapshot, scope: &Scope) -> Vec<Callable<'a>> {
    let mut all = callables(snapshot);
    all.retain(|callable| scope.includes(&callable.node.provenance.rel_path));
    all
}

/// The path of every `File` node, by node id, so a file measure can be
/// held to a scope.
fn file_paths(snapshot: &Snapshot) -> BTreeMap<&str, &str> {
    snapshot
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::File)
        .map(|node| (node.id.as_str(), node.provenance.rel_path.as_str()))
        .collect()
}

/// The counts over a set of callables. Summed in the order given, which
/// callers keep sorted so the floating sums are reproducible.
pub fn counts<'a>(callables: impl IntoIterator<Item = &'a Callable<'a>>) -> Counts {
    let mut out = Counts {
        callables: 0,
        high_cc: 0,
        max_cc: 0,
        mass_total: 0.0,
        mass_above_cutoff: 0.0,
    };
    for callable in callables {
        let mass = callable.mass();
        out.callables += 1;
        out.max_cc = out.max_cc.max(callable.measure.cc);
        out.mass_total += mass;
        if policy::is_high_cc(callable.measure.cc) {
            out.high_cc += 1;
            out.mass_above_cutoff += mass;
        }
    }
    out
}

/// Erosion: the share of complexity mass sitting in callables above the
/// cutoff (`SlopCodeBench` eq. 3). Zero when nothing is measured; the
/// counts beside it say so.
pub fn erosion(counts: &Counts) -> f64 {
    if counts.mass_total == 0.0 {
        0.0
    } else {
        counts.mass_above_cutoff / counts.mass_total
    }
}

/// Repository-wide figures for one snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    /// Share of mass above the cutoff.
    pub erosion: f64,
    /// Source lines over every file in the inventory (`FileMeasure.sloc`
    /// summed): the denominator verbosity will use, and the honest LOC.
    pub loc: u64,
    /// Every line over every file, blank and comment lines included.
    pub lines: u64,
    /// Files measured.
    pub files: usize,
    /// The counts the ratio is never printed without.
    pub counts: Counts,
    /// [`policy::VERBOSITY_CLONE`]: `clone_sloc / loc`; absent without clone measures.
    pub verbosity_clone: Option<f64>,
    /// The counts `verbosity_clone` is never printed without; absent under CC-only policies.
    pub clones: Option<CloneCounts>,
    /// What the scope left out of these figures; zero, and not serialized,
    /// over every file, so an unscoped document keeps its bytes.
    #[serde(skip_serializing_if = "Excluded::is_nothing")]
    pub excluded: Excluded,
    /// [`policy::TWINS_NEVER_FOLDED`]: twin pairs with both callables in
    /// scope; absent when the snapshot was extracted without `--twins`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub twins: Option<TwinCounts>,
}

/// Twin figures beside the clone figure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TwinCounts {
    /// Pairs with both callables in scope.
    pub pairs: usize,
    /// Pairs under containment 1.00: edited copies.
    pub near: usize,
    /// Token-identical pairs; these are also clone groups.
    pub exact: usize,
}

/// One side of a twin pair, located.
#[derive(Debug, Clone, Serialize)]
pub struct TwinSide {
    pub node_id: String,
    pub name: String,
    pub rel_path: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// One twin pair as the report lists it.
#[derive(Debug, Clone, Serialize)]
pub struct Twin {
    pub a: TwinSide,
    pub b: TwinSide,
    pub shared: u32,
    pub shingles_a: u32,
    pub shingles_b: u32,
    /// `shared` over the smaller body's shingles.
    pub containment: f64,
    pub exact: bool,
    /// Whether the two callables already sit in one clone group, so the
    /// pair is not new information to a reader of the clone list.
    pub clone_shared: bool,
}

/// Whether both callables of a pair are in scope.
fn pair_in_scope(pair: &TwinPair, by_id: &BTreeMap<&str, Callable<'_>>, scope: &Scope) -> bool {
    [&pair.a, &pair.b].iter().all(|id| {
        by_id
            .get(id.as_str())
            .is_some_and(|callable| scope.includes(&callable.node.provenance.rel_path))
    })
}

fn twin_counts_under(snapshot: &Snapshot, scope: &Scope) -> Option<TwinCounts> {
    snapshot.identity.twins_policy.as_ref()?;
    let by_id: BTreeMap<&str, Callable<'_>> = callables(snapshot)
        .into_iter()
        .map(|callable| (callable.node.id.as_str(), callable))
        .collect();
    let mut counts = TwinCounts::default();
    for pair in &snapshot.twin_pairs {
        if !pair_in_scope(pair, &by_id, scope) {
            continue;
        }
        counts.pairs += 1;
        if pair.is_exact() {
            counts.exact += 1;
        } else {
            counts.near += 1;
        }
    }
    Some(counts)
}

/// The `top` twin pairs in scope: near pairs first, then by containment,
/// shared shingles, and ids. Empty when the snapshot carries no twins.
pub fn twins(measured: &Measured<'_>, top: usize) -> Vec<Twin> {
    let snapshot = measured.snapshot;
    let by_id: BTreeMap<&str, Callable<'_>> = callables(snapshot)
        .into_iter()
        .map(|callable| (callable.node.id.as_str(), callable))
        .collect();
    let side = |id: &str| -> Option<TwinSide> {
        let callable = by_id.get(id)?;
        let span = &callable.node.provenance.span;
        Some(TwinSide {
            node_id: callable.node.id.clone(),
            name: callable.node.name.clone(),
            rel_path: callable.node.provenance.rel_path.clone(),
            start_line: span.start_line,
            end_line: span.end_line,
        })
    };
    let mut out: Vec<Twin> = snapshot
        .twin_pairs
        .iter()
        .filter(|pair| pair_in_scope(pair, &by_id, measured.scope))
        .filter_map(|pair| {
            let (a, b) = (by_id.get(pair.a.as_str())?, by_id.get(pair.b.as_str())?);
            let clone_shared = a
                .measure
                .clone_group_ids
                .iter()
                .any(|group| b.measure.clone_group_ids.contains(group));
            Some(Twin {
                a: side(&pair.a)?,
                b: side(&pair.b)?,
                shared: pair.shared,
                shingles_a: pair.shingles_a,
                shingles_b: pair.shingles_b,
                containment: pair.containment(),
                exact: pair.is_exact(),
                clone_shared,
            })
        })
        .collect();
    out.sort_by(|x, y| {
        x.exact
            .cmp(&y.exact)
            .then(y.containment.total_cmp(&x.containment))
            .then(y.shared.cmp(&x.shared))
            .then(x.a.node_id.cmp(&y.a.node_id))
            .then(x.b.node_id.cmp(&y.b.node_id))
    });
    out.truncate(top);
    out
}

/// What a scope left out ([`policy::SCOPE_PRINTS_BOTH`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Excluded {
    pub callables: usize,
    pub files: usize,
}

impl Excluded {
    #[must_use]
    pub const fn is_nothing(&self) -> bool {
        self.callables == 0 && self.files == 0
    }
}

/// Absolute clone figures beside `verbosity_clone`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CloneCounts {
    /// Clone groups in the snapshot.
    pub groups: usize,
    /// Source lines under at least one clone occurrence, summed over
    /// files, each line once.
    pub clone_sloc: u64,
    /// Occurrences in the group with the most.
    pub largest_group: usize,
}

/// The clone counts of one admitted snapshot over every file.
#[must_use]
pub fn clone_counts(snapshot: &Snapshot) -> CloneCounts {
    clone_counts_under(snapshot, &policy::ALL_FILES)
}

/// Clone groups are counted whole (a group is a fact about the
/// repository); clone lines are summed over the files in scope, since
/// they are the numerator of a ratio whose denominator is in scope.
fn clone_counts_under(snapshot: &Snapshot, scope: &Scope) -> CloneCounts {
    let paths = file_paths(snapshot);
    CloneCounts {
        groups: snapshot.clone_groups.len(),
        clone_sloc: snapshot
            .file_measures
            .iter()
            .filter(|file| in_scope(&paths, &file.file_id, scope))
            .map(|file| u64::from(file.clone_sloc))
            .sum(),
        largest_group: snapshot
            .clone_groups
            .iter()
            .map(|group| group.occurrences.len())
            .max()
            .unwrap_or(0),
    }
}

/// Whether a file measure's file is inside the scope. A file node the
/// snapshot does not name (which the validator refuses) counts as in.
fn in_scope(paths: &BTreeMap<&str, &str>, file_id: &str, scope: &Scope) -> bool {
    paths.get(file_id).is_none_or(|path| scope.includes(path))
}

/// `clone_sloc / loc`; zero over an empty denominator.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn verbosity_clone(clones: &CloneCounts, loc: u64) -> f64 {
    if loc == 0 {
        0.0
    } else {
        clones.clone_sloc as f64 / loc as f64
    }
}

/// Summarise one admitted snapshot inside its scope.
pub fn summary(measured: &Measured<'_>) -> Summary {
    summary_under(measured.snapshot, measured.scope)
}

/// Summarise one admitted snapshot over every file, whatever its scope:
/// the line [`policy::SCOPE_PRINTS_BOTH`] puts above the in-scope one.
#[must_use]
pub fn summary_all_files(measured: &Measured<'_>) -> Summary {
    summary_under(measured.snapshot, &policy::ALL_FILES)
}

fn summary_under(snapshot: &Snapshot, scope: &Scope) -> Summary {
    let in_scope_callables = callables_under(snapshot, scope);
    let counts = counts(&in_scope_callables);
    let paths = file_paths(snapshot);
    let files: Vec<&codemap_graph::schema::FileMeasure> = snapshot
        .file_measures
        .iter()
        .filter(|file| in_scope(&paths, &file.file_id, scope))
        .collect();
    let loc: u64 = files.iter().map(|file| u64::from(file.sloc)).sum();
    let clones = codemap_graph::schema::clone_measures_available(snapshot.identity.language)
        .then(|| clone_counts_under(snapshot, scope));
    Summary {
        erosion: erosion(&counts),
        verbosity_clone: clones.as_ref().map(|clones| verbosity_clone(clones, loc)),
        clones,
        loc,
        lines: files.iter().map(|file| u64::from(file.loc)).sum(),
        files: files.len(),
        excluded: Excluded {
            callables: callables(snapshot).len() - in_scope_callables.len(),
            files: snapshot.file_measures.len() - files.len(),
        },
        twins: twin_counts_under(snapshot, scope),
        counts,
    }
}

/// Erosion over one group of callables (a file, a cluster).
#[derive(Debug, Clone, Serialize)]
pub struct GroupErosion {
    pub key: String,
    pub erosion: f64,
    pub counts: Counts,
}

fn group(key: String, members: &[Callable<'_>]) -> GroupErosion {
    let counts = counts(members);
    GroupErosion {
        key,
        erosion: erosion(&counts),
        counts,
    }
}

/// Order groups so the heaviest sit first: by mass above the cutoff, then
/// erosion, then key.
fn order_groups(groups: &mut [GroupErosion]) {
    groups.sort_by(|a, b| {
        b.counts
            .mass_above_cutoff
            .total_cmp(&a.counts.mass_above_cutoff)
            .then(b.erosion.total_cmp(&a.erosion))
            .then(a.key.cmp(&b.key))
    });
}

/// Erosion per file in scope, heaviest first.
pub fn per_file(measured: &Measured<'_>) -> Vec<GroupErosion> {
    let mut by_file: BTreeMap<&str, Vec<Callable<'_>>> = BTreeMap::new();
    for callable in callables_in(measured) {
        by_file
            .entry(callable.node.provenance.rel_path.as_str())
            .or_default()
            .push(callable);
    }
    let mut out: Vec<GroupErosion> = by_file
        .into_iter()
        .map(|(path, members)| group(path.to_string(), &members))
        .collect();
    order_groups(&mut out);
    out
}

/// Erosion per functional cluster (`codemap-graph::analysis::clusters`,
/// deterministic label propagation), heaviest first. Clusters are
/// derived over the whole graph; a cluster holding no measured callable
/// in scope is omitted, and an excluded callable adds nothing to its
/// cluster's figures.
pub fn per_cluster(measured: &Measured<'_>) -> Vec<GroupErosion> {
    let snapshot = measured.snapshot;
    let by_id: BTreeMap<&str, Callable<'_>> = callables_in(measured)
        .into_iter()
        .map(|callable| (callable.node.id.as_str(), callable))
        .collect();
    let graph = Graph::from_snapshot(snapshot);
    let mut out: Vec<GroupErosion> = clusters(&graph, CLUSTER_ROUNDS)
        .into_iter()
        .filter_map(|cluster| {
            let mut members: Vec<Callable<'_>> = cluster
                .members
                .iter()
                .filter_map(|id| by_id.get(id.as_str()).copied())
                .collect();
            if members.is_empty() {
                return None;
            }
            members.sort_by(|a, b| a.node.id.cmp(&b.node.id));
            Some(group(
                format!("{} ({} members)", cluster.label, cluster.members.len()),
                &members,
            ))
        })
        .collect();
    order_groups(&mut out);
    out
}

/// Call sites inside one callable that did not become a verified edge,
/// by what the resolver said about them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SiteCounts {
    /// `Candidate`: an in-repository definition matches by name, but the
    /// receiver could not be verified.
    pub candidate: usize,
    /// `ExternalCrate` and `StandardLibrary`: the target is outside the
    /// repository.
    pub external: usize,
    /// `Unmapped`, `Ambiguous`, `Failed`: nothing usable was found.
    pub unresolved: usize,
    /// `Observed`: a syntax snapshot's sites, which no resolver classified.
    pub observed: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub unsupported: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub inactive: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unprofiled: usize,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip predicate takes a reference.
const fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Verified `calls` edges and unverified call sites of one callable
/// ([`policy::COUPLING_VERIFIED_NEVER_ALONE`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CallSites {
    /// Verified `calls` edges into the callable; absent on a syntax
    /// snapshot, where no call is verified.
    pub verified_in: Option<usize>,
    /// Verified `calls` edges out of the callable; absent likewise.
    pub verified_out: Option<usize>,
    /// The sites that are not verified edges.
    pub sites: SiteCounts,
}

impl CallSites {
    const fn unavailable() -> Self {
        Self {
            verified_in: None,
            verified_out: None,
            sites: SiteCounts {
                candidate: 0,
                external: 0,
                unresolved: 0,
                observed: 0,
                unsupported: 0,
                inactive: 0,
                unprofiled: 0,
            },
        }
    }

    const fn none_verified() -> Self {
        Self {
            verified_in: Some(0),
            verified_out: Some(0),
            ..Self::unavailable()
        }
    }
}

const fn span_within(inner: &Span, outer: &Span) -> bool {
    inner.start_byte >= outer.start_byte && inner.end_byte <= outer.end_byte
}

/// The verified edges and unverified sites of every measured callable.
///
/// Keyed by node id. A site belongs to the innermost callable whose span
/// holds it, the same attribution the measures use for nested
/// definitions; a site outside every callable (file-level code) belongs
/// to none.
#[must_use]
pub fn call_sites(snapshot: &Snapshot) -> BTreeMap<String, CallSites> {
    let semantic = snapshot.identity.is_semantic();
    let all = callables(snapshot);
    let mut out: BTreeMap<String, CallSites> = all
        .iter()
        .map(|callable| {
            let sites = if semantic {
                CallSites::none_verified()
            } else {
                CallSites::unavailable()
            };
            (callable.node.id.clone(), sites)
        })
        .collect();
    if semantic {
        for edge in snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Calls)
        {
            if let Some(count) = out
                .get_mut(edge.from.as_str())
                .and_then(|sites| sites.verified_out.as_mut())
            {
                *count += 1;
            }
            if let Some(count) = out
                .get_mut(edge.to.as_str())
                .and_then(|sites| sites.verified_in.as_mut())
            {
                *count += 1;
            }
        }
    }
    let mut by_file: BTreeMap<&str, Vec<&Callable<'_>>> = BTreeMap::new();
    for callable in &all {
        by_file
            .entry(callable.node.provenance.rel_path.as_str())
            .or_default()
            .push(callable);
    }
    for site in &snapshot.call_sites {
        let Some(candidates) = by_file.get(site.rel_path.as_str()) else {
            continue;
        };
        let owner = candidates
            .iter()
            .filter(|callable| span_within(&site.span, &callable.node.provenance.span))
            .min_by_key(|callable| {
                let span = &callable.node.provenance.span;
                (span.end_byte - span.start_byte, span.start_byte)
            });
        let Some(owner) = owner else {
            continue;
        };
        let Some(entry) = out.get_mut(owner.node.id.as_str()) else {
            continue;
        };
        let bucket = match site.category {
            CallSiteCategory::Resolved => continue,
            CallSiteCategory::Candidate => &mut entry.sites.candidate,
            CallSiteCategory::ExternalCrate | CallSiteCategory::StandardLibrary => {
                &mut entry.sites.external
            }
            CallSiteCategory::Unmapped | CallSiteCategory::Ambiguous | CallSiteCategory::Failed => {
                &mut entry.sites.unresolved
            }
            CallSiteCategory::Observed => &mut entry.sites.observed,
            CallSiteCategory::Unsupported => &mut entry.sites.unsupported,
            CallSiteCategory::Inactive => &mut entry.sites.inactive,
            CallSiteCategory::Unprofiled => &mut entry.sites.unprofiled,
        };
        *bucket += 1;
    }
    out
}

/// One callable ranked by mass.
#[derive(Debug, Clone, Serialize)]
pub struct Hotspot {
    pub node_id: String,
    pub name: String,
    pub rel_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub cc: u32,
    pub sloc: u32,
    pub mass: f64,
    /// Verified coupling beside the sites it leaves out.
    pub calls: CallSites,
}

impl Hotspot {
    fn of(callable: &Callable<'_>, calls: CallSites) -> Self {
        let span = &callable.node.provenance.span;
        Self {
            node_id: callable.node.id.clone(),
            name: callable.node.name.clone(),
            rel_path: callable.node.provenance.rel_path.clone(),
            start_line: span.start_line,
            end_line: span.end_line,
            cc: callable.measure.cc,
            sloc: callable.measure.sloc,
            mass: callable.mass(),
            calls,
        }
    }
}

/// The `top` heaviest callables in scope: by mass, then cc, then node id.
/// Call sites are attributed over every callable, so a site inside an
/// excluded callable never leaks into an included one.
pub fn hotspots(measured: &Measured<'_>, top: usize) -> Vec<Hotspot> {
    let snapshot = measured.snapshot;
    let mut all = callables_in(measured);
    all.sort_by(|a, b| {
        b.mass()
            .total_cmp(&a.mass())
            .then(b.measure.cc.cmp(&a.measure.cc))
            .then(a.node.id.cmp(&b.node.id))
    });
    let sites = call_sites(snapshot);
    all.iter()
        .take(top)
        .map(|callable| {
            let calls = sites
                .get(callable.node.id.as_str())
                .copied()
                .unwrap_or(CallSites::unavailable());
            Hotspot::of(callable, calls)
        })
        .collect()
}
