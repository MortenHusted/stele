//! Text renderings a reviewer reads top down, in the style of
//! `stele code report`: `key: value` lines and indented tables.

use std::fmt::Write as _;

use serde::Serialize;

use codemap_graph::schema::EdgeKind;

use crate::Measured;
use crate::diff::{ChangeReport, FindingKind};
use crate::gate::{Breach, Budget};
use crate::metrics::{
    CallSites, CloneCounts, GroupErosion, Hotspot, SiteCounts, Summary, Twin, TwinCounts, hotspots,
    per_cluster, per_file, summary, summary_all_files, twins,
};
use crate::policy::{
    CLONE_MIN_LINES, CLONE_MIN_TOKENS, Counts, HIGH_CC_CUTOFF, Scope, TARGET_RATCHET,
    TWIN_MIN_CONTAINMENT_PERCENT, TWIN_MIN_TOKENS, TWIN_SHINGLE_TOKENS,
};
use crate::targets::{Item, Stat, Status, TargetResult, figure};

/// `report`: one snapshot's figures.
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotReport {
    pub snapshot_id: String,
    pub repository: String,
    pub commit: String,
    pub language: String,
    pub policy: String,
    /// Verified `calls` edges; absent on a syntax snapshot, whose
    /// coupling columns read `n/a`.
    pub verified_calls: Option<usize>,
    /// The files the figures are held to; not serialized when it is every
    /// file, so an unscoped document keeps its bytes.
    #[serde(skip_serializing_if = "Scope::is_all_files")]
    pub scope: Scope,
    /// The in-scope figures.
    pub summary: Summary,
    /// The all-files figures beside the in-scope ones
    /// ([`crate::policy::SCOPE_PRINTS_BOTH`]); absent when the scope is
    /// every file and they would repeat `summary`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_files: Option<Summary>,
    pub hotspots: Vec<Hotspot>,
    /// Twin pairs in scope, near pairs first; empty without `--twins`.
    pub twins: Vec<Twin>,
    pub files: Vec<GroupErosion>,
    pub clusters: Vec<GroupErosion>,
    /// Shape targets held against this one snapshot (`report --budget`):
    /// no base, so nothing is breached and the excess is the backlog.
    /// Absent without `--budget`, so the document keeps its bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<TargetResult>>,
}

/// Build the `report` document. `top` bounds every list.
pub fn snapshot_report(measured: &Measured<'_>, top: usize) -> SnapshotReport {
    let snapshot = measured.snapshot;
    let identity = &snapshot.identity;
    let mut files = per_file(measured);
    files.truncate(top);
    let mut clusters = per_cluster(measured);
    clusters.truncate(top);
    SnapshotReport {
        snapshot_id: identity.snapshot_id.clone(),
        repository: identity.repository.id.clone(),
        commit: identity.commit_sha.clone(),
        language: identity.language.tag().to_string(),
        policy: measured.policy.to_string(),
        verified_calls: identity.is_semantic().then(|| {
            snapshot
                .edges
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Calls)
                .count()
        }),
        scope: measured.scope.clone(),
        summary: summary(measured),
        all_files: (!measured.scope.is_all_files()).then(|| summary_all_files(measured)),
        hotspots: hotspots(measured, top),
        twins: twins(measured, top),
        files,
        clusters,
        targets: None,
    }
}

/// What a clone is, once, from the producer's thresholds.
fn clone_definition() -> String {
    format!(
        "a clone is a run of {CLONE_MIN_TOKENS} or more equal normalised tokens over {CLONE_MIN_LINES} or more lines, exact or renamed"
    )
}

/// The twin count beside the clone figure ([`crate::policy::TWINS_NEVER_FOLDED`]).
fn twins_line(label: &str, twins: Option<&TwinCounts>) -> String {
    twins.map_or_else(
        || format!("{label}: twin pairs n/a (not computed; extract with --measures --twins)"),
        |t| {
            format!(
                "{label}: twin pairs {}  ({} near, {} exact; {}-token shingles, containment >= {}%, both bodies >= {} tokens)",
                t.pairs, t.near, t.exact, TWIN_SHINGLE_TOKENS, TWIN_MIN_CONTAINMENT_PERCENT, TWIN_MIN_TOKENS
            )
        },
    )
}

fn twin_line(t: &Twin) -> String {
    let note = match (t.exact, t.clone_shared) {
        (true, _) => "  exact, also a clone group",
        (false, true) => "  also a clone group",
        (false, false) => "",
    };
    format!(
        "  {:.2}  shared {:>4} of {:>4}  {}  {}:{}-{}  <->  {}  {}:{}-{}{note}",
        t.containment,
        t.shared,
        t.shingles_a.min(t.shingles_b),
        t.a.name,
        t.a.rel_path,
        t.a.start_line,
        t.a.end_line,
        t.b.name,
        t.b.rel_path,
        t.b.start_line,
        t.b.end_line
    )
}

/// The two lines [`crate::policy::SCOPE_PRINTS_BOTH`] requires when a
/// scope is set: every file first, then what the report is held to.
fn scope_lines(out: &mut String, label: &str, scope: &Scope, all: &Summary, in_scope: &Summary) {
    let _ = writeln!(
        out,
        "{}  loc {}  files {}",
        ratio_line(&format!("{label}all files"), all.erosion, &all.counts),
        all.loc,
        all.files
    );
    let _ = writeln!(
        out,
        "{}  excluded {} callables in {} files under {}",
        ratio_line(
            &format!("{label}in scope"),
            in_scope.erosion,
            &in_scope.counts
        ),
        in_scope.excluded.callables,
        in_scope.excluded.files,
        scope.describe()
    );
}

/// The header line that says what the coupling columns can and cannot
/// claim for one snapshot.
fn coupling_line(verified_calls: Option<usize>) -> String {
    verified_calls.map_or_else(
        || "coupling: syntax snapshot, verified calls unavailable (extract with --calls=required); call sites are observed, not resolved".to_string(),
        |edges| format!(
            "coupling: {edges} verified calls edges; fan-in and fan-out count those only, beside the call sites the resolver could not verify"
        ),
    )
}

fn opt(n: Option<usize>) -> String {
    n.map_or_else(|| "n/a".to_string(), |n| n.to_string())
}

/// The unverified sites beside a verified figure.
fn sites_text(sites: &SiteCounts, semantic: bool) -> String {
    if semantic {
        format!(
            "{} candidate, {} external, {} unresolved",
            sites.candidate, sites.external, sites.unresolved
        )
    } else {
        format!("{} observed", sites.observed)
    }
}

fn calls_text(calls: &CallSites) -> String {
    match (calls.verified_in, calls.verified_out) {
        (Some(fan_in), Some(fan_out)) => format!(
            "calls in {fan_in} out {fan_out} verified, {}",
            sites_text(&calls.sites, true)
        ),
        _ => format!("calls n/a verified, {}", sites_text(&calls.sites, false)),
    }
}

fn ratio_line(label: &str, erosion: f64, counts: &Counts) -> String {
    format!(
        "{label}: erosion {erosion:.3}  (callables {}, over cc {HIGH_CC_CUTOFF}: {}, max cc {}, mass above cutoff {:.1} of {:.1})",
        counts.callables,
        counts.high_cc,
        counts.max_cc,
        counts.mass_above_cutoff,
        counts.mass_total
    )
}

fn verbosity_line(
    label: &str,
    verbosity: Option<f64>,
    clones: Option<&CloneCounts>,
    loc: u64,
) -> String {
    let (Some(verbosity), Some(clones)) = (verbosity, clones) else {
        return format!("{label}: verbosity_clone n/a (not measured)");
    };
    format!(
        "{label}: verbosity_clone {verbosity:.3}  (clone groups {}, clone lines {} of {loc}, largest group {} occurrences; {})",
        clones.groups,
        clones.clone_sloc,
        clones.largest_group,
        clone_definition()
    )
}

fn hotspot_line(h: &Hotspot) -> String {
    format!(
        "  {:>8.1}  cc {:>4}  sloc {:>5}  {}  {}:{}-{}  {}",
        h.mass,
        h.cc,
        h.sloc,
        h.name,
        h.rel_path,
        h.start_line,
        h.end_line,
        calls_text(&h.calls)
    )
}

fn group_lines(out: &mut String, groups: &[GroupErosion]) {
    for g in groups {
        let _ = writeln!(
            out,
            "  {:.3}  over cutoff {:>3} of {:>4}  mass above {:>8.1}  {}",
            g.erosion, g.counts.high_cc, g.counts.callables, g.counts.mass_above_cutoff, g.key
        );
    }
}

/// Render `report` as text.
pub fn snapshot_text(report: &SnapshotReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "snapshot: {}", report.snapshot_id);
    let _ = writeln!(out, "repository: {}", report.repository);
    let _ = writeln!(out, "commit: {}", report.commit);
    let _ = writeln!(
        out,
        "language: {}  policy: {}",
        report.language, report.policy
    );
    let _ = writeln!(out, "{}", coupling_line(report.verified_calls));
    let s = &report.summary;
    let _ = writeln!(
        out,
        "lines: files={} sloc={} all={}",
        s.files, s.loc, s.lines
    );
    match &report.all_files {
        Some(all) => scope_lines(&mut out, "erosion, ", &report.scope, all, s),
        None => {
            let _ = writeln!(out, "{}", ratio_line("erosion", s.erosion, &s.counts));
        }
    }
    let _ = writeln!(
        out,
        "{}",
        verbosity_line("clones", s.verbosity_clone, s.clones.as_ref(), s.loc)
    );
    match &report.all_files {
        Some(all) => {
            let _ = writeln!(
                out,
                "{}",
                twins_line("twins, all files", all.twins.as_ref())
            );
            let _ = writeln!(out, "{}", twins_line("twins, in scope", s.twins.as_ref()));
        }
        None => {
            let _ = writeln!(out, "{}", twins_line("twins", s.twins.as_ref()));
        }
    }
    let _ = writeln!(out, "hotspots by mass (cc * sqrt(sloc)):");
    for h in &report.hotspots {
        let _ = writeln!(out, "{}", hotspot_line(h));
    }
    if !report.twins.is_empty() {
        let _ = writeln!(out, "twin pairs by containment (near pairs first):");
        for t in &report.twins {
            let _ = writeln!(out, "{}", twin_line(t));
        }
    }
    let _ = writeln!(out, "files by mass above cutoff:");
    group_lines(&mut out, &report.files);
    let _ = writeln!(out, "clusters by mass above cutoff:");
    group_lines(&mut out, &report.clusters);
    if clusters_are_singletons(&report.clusters) {
        let _ = writeln!(
            out,
            "  (every cluster is one callable: the snapshot has no calls edges, so cluster erosion repeats the hotspot list; extract with --calls=required for real clusters)"
        );
    }
    if let Some(targets) = &report.targets {
        out.push_str(&targets_text(targets, report.hotspots.len().max(1)));
    }
    out
}

fn opt_figure(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_string(), figure)
}

const fn status_text(status: Status) -> &'static str {
    match status {
        Status::Met => "met",
        Status::OverTarget => "over target",
        Status::Breached => "BREACHED",
        Status::Unavailable => "n/a",
    }
}

fn item_line(measure: &str, item: &Item) -> String {
    let was = item
        .base
        .map(|base| format!("  (was {})", figure(base)))
        .unwrap_or_default();
    format!(
        "    {measure} {:>5}  {}  {}:{}-{}{was}",
        figure(item.value),
        item.name,
        item.rel_path,
        item.start_line,
        item.end_line
    )
}

/// One target's line, and its items for an `each` target: every breached
/// item, then up to `top` of the backlog.
fn target_lines(out: &mut String, result: &TargetResult, top: usize) {
    let target = &result.target;
    let head = match (result.status, target.stat) {
        (Status::Unavailable, _) => format!(
            "n/a ({})",
            result.reason.as_deref().unwrap_or("not available")
        ),
        (_, Stat::Each) => format!(
            "over the ceiling {} of {} {}, base {}",
            opt_figure(result.head),
            result.sample,
            target.measure.item_noun(),
            opt_figure(result.base)
        ),
        _ => format!(
            "head {}  base {}  over {} {}",
            opt_figure(result.head),
            opt_figure(result.base),
            result.sample,
            target.measure.item_noun()
        ),
    };
    let _ = writeln!(
        out,
        "  {:<22}  {:<18}  {:<28}  {head}  {}",
        target.name,
        target.prefix,
        target.rule(),
        status_text(result.status)
    );
    if result.status == Status::Unavailable {
        return;
    }
    let measure = target.measure.name();
    for item in &result.breached {
        let _ = writeln!(
            out,
            "{}  <- added or made worse by this change",
            item_line(measure, item)
        );
    }
    for item in result.backlog.iter().take(top) {
        let _ = writeln!(out, "{}", item_line(measure, item));
    }
    if result.backlog.len() > top {
        let _ = writeln!(
            out,
            "    ... and {} more {} over the ceiling",
            result.backlog.len() - top,
            target.measure.item_noun()
        );
    }
}

/// The `targets:` block: one line per target and the items behind an
/// `each` target, up to `top` of the backlog.
#[must_use]
pub fn targets_text(results: &[TargetResult], top: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "targets ({TARGET_RATCHET}: breached only when over the ceiling and worse than the base; the rest is over target):"
    );
    for result in results {
        target_lines(&mut out, result, top);
    }
    out
}

/// True when every listed cluster holds exactly one callable, which is
/// what label propagation yields over a snapshot without `calls` edges.
fn clusters_are_singletons(clusters: &[GroupErosion]) -> bool {
    !clusters.is_empty() && clusters.iter().all(|c| c.counts.callables == 1)
}

const fn kind_heading(kind: FindingKind) -> &'static str {
    match kind {
        FindingKind::CrossedAboveCutoff => "crossed above the cutoff",
        FindingKind::AddedAboveCutoff => "new callables above the cutoff",
        FindingKind::Harder => "got harder (mass grew)",
        FindingKind::CrossedBelowCutoff => "crossed below the cutoff",
        FindingKind::RemovedAboveCutoff => "removed callables that were above the cutoff",
        FindingKind::Easier => "got easier (mass fell)",
    }
}

/// Render `diff` as text.
pub fn change_text(report: &ChangeReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "change: {} -> {}  policy: {}",
        report.base_commit, report.head_commit, report.policy
    );
    let _ = writeln!(
        out,
        "callables: added={} removed={} moved={}",
        report.added_callables, report.removed_callables, report.moved_callables
    );
    let _ = writeln!(
        out,
        "coupling: verified calls edges {} -> {}; fan-in and fan-out count those only (n/a on a syntax snapshot)",
        opt(report.verified_calls.base),
        opt(report.verified_calls.head)
    );
    let mut current: Option<FindingKind> = None;
    for f in &report.findings {
        if current != Some(f.kind) {
            current = Some(f.kind);
            let _ = writeln!(out, "{}:", kind_heading(f.kind));
        }
        let cc = match (f.base, f.head) {
            (Some(b), Some(h)) => format!("cc {} -> {}", b.cc, h.cc),
            (None, Some(h)) => format!("cc {}", h.cc),
            (Some(b), None) => format!("cc {} -> gone", b.cc),
            (None, None) => String::new(),
        };
        let sloc = match (f.base, f.head) {
            (Some(b), Some(h)) => format!("sloc {} -> {}", b.sloc, h.sloc),
            (None, Some(h)) => format!("sloc {}", h.sloc),
            (Some(b), None) => format!("sloc {} -> gone", b.sloc),
            (None, None) => String::new(),
        };
        let moved = f
            .moved_from
            .as_deref()
            .map(|from| format!("  moved from {from}"))
            .unwrap_or_default();
        // The sites describe the side the finding is about.
        let semantic = if f.head.is_some() {
            report.verified_calls.head.is_some()
        } else {
            report.verified_calls.base.is_some()
        };
        let _ = writeln!(
            out,
            "  {:<14}  {:<16}  fan-in {} -> {}  fan-out {} -> {}  {}  {}:{}-{}  {}{moved}",
            cc,
            sloc,
            opt(f.fan_in.base),
            opt(f.fan_in.head),
            opt(f.fan_out.base),
            opt(f.fan_out.head),
            f.name,
            f.rel_path,
            f.start_line,
            f.end_line,
            sites_text(&f.sites, semantic)
        );
    }
    if report.findings.is_empty() {
        let _ = writeln!(
            out,
            "no callable changed its cc, sloc, or side of the cutoff"
        );
    }
    let _ = writeln!(out, "hotspots by mass in head:");
    for h in &report.hotspots {
        let base = match (h.base_cc, h.cc_delta) {
            (Some(cc), Some(delta)) if delta != 0 => format!("  (was cc {cc}, {delta:+})"),
            (Some(_), _) => "  (unchanged)".to_string(),
            _ => "  (new)".to_string(),
        };
        let _ = writeln!(out, "{}{base}", hotspot_line(&h.hotspot));
    }
    ratio_lines(&mut out, report);
    let _ = writeln!(
        out,
        "loc: {} -> {} ({:+})",
        report.base.loc,
        report.head.loc,
        report.loc_delta()
    );
    clone_lines(&mut out, report);
    twin_lines(&mut out, report);
    if let Some(flag) = &report.ratio_flag {
        let _ = writeln!(out, "flag: {flag}");
    }
    out
}

/// The twin figures of a change report, after the clone lines.
fn twin_lines(out: &mut String, report: &ChangeReport) {
    let (Some(base), Some(head), Some(introduced), Some(removed)) = (
        report.base.twins,
        report.head.twins,
        &report.twin_pairs_introduced,
        report.twin_pairs_removed,
    ) else {
        let _ = writeln!(
            out,
            "twins: twin pairs n/a (not computed on both sides; extract with --measures --twins)"
        );
        return;
    };
    let _ = writeln!(
        out,
        "twins: pairs {} -> {}  introduced {}  removed {}  (near {} -> {}, exact {} -> {})",
        base.pairs,
        head.pairs,
        introduced.len(),
        removed,
        base.near,
        head.near,
        base.exact,
        head.exact
    );
    for t in introduced {
        let _ = writeln!(out, "{}", twin_line(t));
    }
}

/// The base and head ratio lines, both scopes when a scope is set.
fn ratio_lines(out: &mut String, report: &ChangeReport) {
    if let Some(all) = &report.all_files {
        scope_lines(out, "base, ", &report.scope, &all.base, &report.base);
        scope_lines(out, "head, ", &report.scope, &all.head, &report.head);
    } else {
        let _ = writeln!(
            out,
            "{}",
            ratio_line("base", report.base.erosion, &report.base.counts)
        );
        let _ = writeln!(
            out,
            "{}",
            ratio_line("head", report.head.erosion, &report.head.counts)
        );
    }
}

/// The clone figures of a change report, after the loc line.
fn clone_lines(out: &mut String, report: &ChangeReport) {
    let (
        Some(base),
        Some(head),
        Some(introduced),
        Some(removed),
        Some(base_ratio),
        Some(head_ratio),
    ) = (
        report.base.clones,
        report.head.clones,
        &report.clone_groups_introduced,
        report.clone_groups_removed,
        report.base.verbosity_clone,
        report.head.verbosity_clone,
    )
    else {
        let _ = writeln!(out, "clones: verbosity_clone n/a (not measured)");
        return;
    };
    let _ = writeln!(
        out,
        "clones: groups {} -> {}  introduced {}  removed {}  verbosity_clone {:.3} -> {:.3}  (clone lines {} -> {}; the {} moved callable(s) above are node moves, not clones; {})",
        base.groups,
        head.groups,
        introduced.len(),
        removed,
        base_ratio,
        head_ratio,
        base.clone_sloc,
        head.clone_sloc,
        report.moved_callables,
        clone_definition()
    );
    for group in introduced {
        let _ = writeln!(
            out,
            "  {}  {} occurrences  {} tokens  {} lines  {}",
            group.id, group.occurrences, group.tokens, group.lines, group.first
        );
    }
}

/// Render `gate` as text, after the change report: the budget's deltas,
/// the targets block when the budget has targets, and one verdict over
/// both. `top` bounds the backlog listed under an `each` target.
pub fn gate_text(
    budget: &Budget,
    breaches: &[Breach],
    targets: &[TargetResult],
    top: usize,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "budget (advisory until the gate experiment has run):");
    if let Some(v) = budget.max_erosion_delta {
        let _ = writeln!(out, "  max_erosion_delta = {v}");
    }
    if let Some(v) = budget.max_loc_delta {
        let _ = writeln!(out, "  max_loc_delta = {v}");
    }
    if let Some(v) = budget.max_new_high_cc_callables {
        let _ = writeln!(out, "  max_new_high_cc_callables = {v}");
    }
    for prefix in &budget.exclude {
        let _ = writeln!(out, "  exclude = \"{prefix}\"");
    }
    if !targets.is_empty() {
        out.push_str(&targets_text(targets, top));
    }
    let breached_targets: Vec<&TargetResult> = targets.iter().filter(|t| t.is_breached()).collect();
    if breaches.is_empty() && breached_targets.is_empty() {
        let _ = writeln!(out, "gate: within budget");
    } else {
        let _ = writeln!(out, "gate: BREACHED");
        for b in breaches {
            let _ = writeln!(out, "  {}: {} exceeds {}", b.key, b.actual, b.limit);
        }
        for t in breached_targets {
            let what = if t.target.stat == Stat::Each {
                format!(
                    "{} {} over {} added or made worse by this change",
                    t.breached.len(),
                    t.target.measure.item_noun(),
                    t.target.rule()
                )
            } else {
                format!(
                    "{} is {} against {} and was {} in the base",
                    t.target.rule(),
                    opt_figure(t.head),
                    figure(t.target.max),
                    opt_figure(t.base)
                )
            };
            let _ = writeln!(out, "  target {}: {what}", t.target.name);
        }
    }
    out
}
