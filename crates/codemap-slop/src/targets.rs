//! Shape targets: ceilings a repository is held to under a directory
//! prefix, read from `[target.<name>]` sections of `slop.toml`.
//!
//! A budget's deltas say whether one change made things worse. A target
//! says where the code is meant to be: a controller action under cc 6,
//! no model over twenty-five methods, under two twin pairs per thousand
//! callables in `app/`. Every measure here is a fact the snapshot already
//! carries; nothing is estimated. The ratchet rule
//! ([`crate::policy::TARGET_RATCHET`]) is what lets a repository that
//! starts over a target adopt it anyway: the gate breaches on what a
//! change added or made worse, and prints the rest as the backlog.

// Callable and class counts sit far below 2^53; every value is an f64 so
// one record shape carries a count, a line count, and a per-thousand rate.
#![allow(clippy::cast_precision_loss)]

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use codemap_graph::schema::{EdgeKind, Node, NodeKind, Snapshot};
use serde::Serialize;

use crate::Measured;
use crate::metrics::{Callable, callables_in};
use crate::policy::Scope;

/// The longest inheritance chain followed before giving up: deeper than
/// any real hierarchy, so a cycle the snapshot should not carry cannot
/// spin.
pub const INHERITS_DEPTH_CAP: usize = 32;

/// What a target measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetMeasure {
    /// Cyclomatic complexity per callable (`Measure.cc`).
    Cc,
    /// Source lines per callable (`Measure.sloc`).
    Sloc,
    /// Measured callables a class-like definition contains (`contains`
    /// edges from a type, trait, or module to a callable).
    MethodsPerClass,
    /// Lines a class-like definition spans.
    ClassLoc,
    /// In-repository superclasses above a class-like definition
    /// (`inherits` edges, followed child to parent).
    InheritsDepth,
    /// Twin pairs with both callables under the prefix, per thousand
    /// callables under it.
    #[serde(rename = "twins_per_1k")]
    TwinsPer1k,
    /// Parameters per callable (`Measure.params`); n/a under a policy
    /// without form measures.
    Params,
    /// Deepest control-structure nesting per callable (`Measure.nesting`).
    Nesting,
    /// Longest dotted call chain per callable (`Measure.chain`).
    Chain,
    /// Distinct instance variables a callable writes (`Measure.ivars`);
    /// Ruby only.
    Ivars,
}

impl TargetMeasure {
    /// Every measure name a target may use, in the order the error lists them.
    pub const NAMES: [&'static str; 10] = [
        "cc",
        "sloc",
        "methods_per_class",
        "class_loc",
        "inherits_depth",
        "twins_per_1k",
        "params",
        "nesting",
        "chain",
        "ivars",
    ];

    /// The measure a `slop.toml` value names, if any.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "cc" => Some(Self::Cc),
            "sloc" => Some(Self::Sloc),
            "methods_per_class" => Some(Self::MethodsPerClass),
            "class_loc" => Some(Self::ClassLoc),
            "inherits_depth" => Some(Self::InheritsDepth),
            "twins_per_1k" => Some(Self::TwinsPer1k),
            "params" => Some(Self::Params),
            "nesting" => Some(Self::Nesting),
            "chain" => Some(Self::Chain),
            "ivars" => Some(Self::Ivars),
            _ => None,
        }
    }

    /// The name as `slop.toml` spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cc => "cc",
            Self::Sloc => "sloc",
            Self::MethodsPerClass => "methods_per_class",
            Self::ClassLoc => "class_loc",
            Self::InheritsDepth => "inherits_depth",
            Self::TwinsPer1k => "twins_per_1k",
            Self::Params => "params",
            Self::Nesting => "nesting",
            Self::Chain => "chain",
            Self::Ivars => "ivars",
        }
    }

    /// Whether the measure is one of the form measures, which only a
    /// policy with them carries (`schema::form_measures_available`).
    #[must_use]
    pub const fn is_form(self) -> bool {
        matches!(
            self,
            Self::Params | Self::Nesting | Self::Chain | Self::Ivars
        )
    }

    /// Whether the measure is one number for the whole prefix rather than
    /// one per callable or class.
    #[must_use]
    pub const fn is_single_value(self) -> bool {
        matches!(self, Self::TwinsPer1k)
    }

    /// What one item of the sample is, for a report line.
    #[must_use]
    pub const fn item_noun(self) -> &'static str {
        match self {
            Self::Cc
            | Self::Sloc
            | Self::TwinsPer1k
            | Self::Params
            | Self::Nesting
            | Self::Chain
            | Self::Ivars => "callables",
            Self::MethodsPerClass | Self::ClassLoc | Self::InheritsDepth => "classes",
        }
    }

    /// The singular of [`Self::item_noun`].
    #[must_use]
    pub const fn item_noun_singular(self) -> &'static str {
        match self {
            Self::Cc
            | Self::Sloc
            | Self::TwinsPer1k
            | Self::Params
            | Self::Nesting
            | Self::Chain
            | Self::Ivars => "callable",
            Self::MethodsPerClass | Self::ClassLoc | Self::InheritsDepth => "class",
        }
    }
}

/// How the sample under the prefix is held to the ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stat {
    /// Every item under the prefix is held to the ceiling on its own.
    Each,
    /// The median (nearest rank).
    P50,
    /// The ninetieth percentile (nearest rank).
    P90,
    /// The largest item.
    Max,
    /// The single figure a whole-prefix measure yields.
    Value,
}

impl Stat {
    /// Every statistic name a target may use.
    pub const NAMES: [&'static str; 5] = ["each", "p50", "p90", "max", "value"];

    /// The statistic a `slop.toml` value names, if any.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "each" => Some(Self::Each),
            "p50" => Some(Self::P50),
            "p90" => Some(Self::P90),
            "max" => Some(Self::Max),
            "value" => Some(Self::Value),
            _ => None,
        }
    }

    /// The name as `slop.toml` spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Each => "each",
            Self::P50 => "p50",
            Self::P90 => "p90",
            Self::Max => "max",
            Self::Value => "value",
        }
    }
}

/// One `[target.<name>]` section.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Target {
    pub name: String,
    /// A repository-relative directory prefix, validated like a scope
    /// prefix.
    pub prefix: String,
    pub measure: TargetMeasure,
    pub stat: Stat,
    /// The ceiling; an item or statistic strictly above it is over target.
    pub max: f64,
}

impl Target {
    /// Refuse a measure and statistic that do not go together.
    ///
    /// # Errors
    ///
    /// `value` with a per-item measure, or a per-item statistic with a
    /// whole-prefix measure.
    pub fn validate(&self) -> Result<()> {
        match (self.measure.is_single_value(), self.stat) {
            (true, Stat::Value) | (false, Stat::Each | Stat::P50 | Stat::P90 | Stat::Max) => Ok(()),
            (true, stat) => bail!(
                "target `{}`: `{}` is one figure for the prefix, so its stat must be `value`, not `{}`",
                self.name,
                self.measure.name(),
                stat.name()
            ),
            (false, Stat::Value) => bail!(
                "target `{}`: `{}` is measured per {}, so its stat must be each, p50, p90, or max, not `value`",
                self.name,
                self.measure.name(),
                self.measure.item_noun_singular()
            ),
        }
    }

    /// `measure stat <= max`, for a report line.
    #[must_use]
    pub fn rule(&self) -> String {
        format!(
            "{} {} <= {}",
            self.measure.name(),
            self.stat.name(),
            figure(self.max)
        )
    }
}

/// One callable or class under the prefix, with its value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub node_id: String,
    pub name: String,
    pub rel_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub value: f64,
    /// The same item's value in the base, when it existed there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<f64>,
}

/// Where a target stands after the ratchet rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Under the ceiling.
    Met,
    /// Over the ceiling, and no worse than the base (or there is no base):
    /// the backlog, printed, never a breach.
    OverTarget,
    /// Over the ceiling and worse than the base; for `each`, at least one
    /// item over the ceiling that the change added or made worse.
    Breached,
    /// The measure is not available on this snapshot, or the prefix holds
    /// nothing to measure; never read as met.
    Unavailable,
}

/// One target held against a head, and a base when there is one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TargetResult {
    #[serde(flatten)]
    pub target: Target,
    /// The statistic in the head; for `each`, the count of items over the
    /// ceiling. Absent when unavailable.
    pub head: Option<f64>,
    /// The same in the base; absent without a base or when unavailable
    /// there.
    pub base: Option<f64>,
    /// Items under the prefix in the head (callables under the prefix for
    /// `twins_per_1k`).
    pub sample: usize,
    /// `each`: items over the ceiling that are no worse than in the base
    /// (the backlog), largest first.
    pub backlog: Vec<Item>,
    /// `each`: items over the ceiling that the change added to the prefix
    /// or made worse, largest first.
    pub breached: Vec<Item>,
    pub status: Status,
    /// Why the target is unavailable, when it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl TargetResult {
    #[must_use]
    pub fn is_breached(&self) -> bool {
        self.status == Status::Breached
    }
}

/// A number as a report prints it: whole when it is whole.
#[must_use]
pub fn figure(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

/// The nearest-rank percentile of a sorted sample: the value at rank
/// `ceil(p/100 * n)`, one-based. Deterministic, never interpolated.
fn nearest_rank(sorted: &[f64], percent: u32) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (f64::from(percent) / 100.0 * sorted.len() as f64).ceil();
    // `rank` is in `1..=n`, so the cast is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let index = (rank as usize).clamp(1, sorted.len()) - 1;
    Some(sorted[index])
}

/// Whether a node is a class-like definition: a type, a trait, or a module.
const fn is_class_like(node: &Node) -> bool {
    matches!(
        node.kind,
        NodeKind::Type(_) | NodeKind::Trait | NodeKind::Module
    )
}

fn item_of(node: &Node, value: f64) -> Item {
    let span = &node.provenance.span;
    Item {
        node_id: node.id.clone(),
        name: node.name.clone(),
        rel_path: node.provenance.rel_path.clone(),
        start_line: span.start_line,
        end_line: span.end_line,
        value,
        base: None,
    }
}

/// The class-like nodes under a prefix and in scope, in node-id order.
fn classes_under<'a>(snapshot: &'a Snapshot, scope: &Scope, prefix: &str) -> Vec<&'a Node> {
    let mut out: Vec<&Node> = snapshot
        .nodes
        .iter()
        .filter(|node| is_class_like(node))
        .filter(|node| {
            let path = node.provenance.rel_path.as_str();
            path.starts_with(prefix) && scope.includes(path)
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The callables under a prefix and in scope, in node-id order.
fn callables_under<'a>(measured: &Measured<'a>, prefix: &str) -> Vec<Callable<'a>> {
    let mut all = callables_in(measured);
    all.retain(|callable| callable.node.provenance.rel_path.starts_with(prefix));
    all
}

/// Measured callables per class-like node, over the whole snapshot.
fn methods_per_class(snapshot: &Snapshot) -> BTreeMap<&str, usize> {
    let callable_ids: BTreeSet<&str> = snapshot
        .measures
        .iter()
        .map(|measure| measure.node_id.as_str())
        .collect();
    let mut out = BTreeMap::new();
    for edge in &snapshot.edges {
        if edge.kind == EdgeKind::Contains && callable_ids.contains(edge.to.as_str()) {
            *out.entry(edge.from.as_str()).or_insert(0) += 1;
        }
    }
    out
}

/// In-repository superclasses above each class-like node: the longest
/// `inherits` chain, followed child to parent, cycle-safe, capped at
/// [`INHERITS_DEPTH_CAP`].
fn inherits_depth(snapshot: &Snapshot) -> BTreeMap<&str, usize> {
    let mut parents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &snapshot.edges {
        if edge.kind == EdgeKind::Inherits {
            parents
                .entry(edge.from.as_str())
                .or_default()
                .push(edge.to.as_str());
        }
    }
    parents
        .keys()
        .map(|node| (*node, depth(node, &parents, &mut Vec::new())))
        .collect()
}

/// Superclasses above one node, following `parents`; `seen` is the chain
/// being walked, so a cycle ends it.
fn depth<'a>(
    node: &'a str,
    parents: &BTreeMap<&'a str, Vec<&'a str>>,
    seen: &mut Vec<&'a str>,
) -> usize {
    if seen.len() >= INHERITS_DEPTH_CAP || seen.contains(&node) {
        return 0;
    }
    let Some(ups) = parents.get(node) else {
        return 0;
    };
    seen.push(node);
    let deepest = ups
        .iter()
        .map(|parent| 1 + depth(parent, parents, seen))
        .max()
        .unwrap_or(0);
    seen.pop();
    deepest
}

/// The items of a per-item measure under the prefix, in node-id order.
fn items(measured: &Measured<'_>, target: &Target) -> Vec<Item> {
    let snapshot = measured.snapshot;
    match target.measure {
        TargetMeasure::Cc => callables_under(measured, &target.prefix)
            .iter()
            .map(|c| item_of(c.node, f64::from(c.measure.cc)))
            .collect(),
        TargetMeasure::Sloc => callables_under(measured, &target.prefix)
            .iter()
            .map(|c| item_of(c.node, f64::from(c.measure.sloc)))
            .collect(),
        TargetMeasure::MethodsPerClass => {
            let counts = methods_per_class(snapshot);
            classes_under(snapshot, measured.scope, &target.prefix)
                .into_iter()
                .map(|node| {
                    let n = counts.get(node.id.as_str()).copied().unwrap_or(0);
                    item_of(node, n as f64)
                })
                .collect()
        }
        TargetMeasure::ClassLoc => classes_under(snapshot, measured.scope, &target.prefix)
            .into_iter()
            .map(|node| {
                let span = &node.provenance.span;
                item_of(node, f64::from(span.end_line - span.start_line + 1))
            })
            .collect(),
        TargetMeasure::InheritsDepth => {
            let depths = inherits_depth(snapshot);
            classes_under(snapshot, measured.scope, &target.prefix)
                .into_iter()
                .map(|node| {
                    let d = depths.get(node.id.as_str()).copied().unwrap_or(0);
                    item_of(node, d as f64)
                })
                .collect()
        }
        TargetMeasure::Params => form_items(measured, &target.prefix, |m| m.params),
        TargetMeasure::Nesting => form_items(measured, &target.prefix, |m| m.nesting),
        TargetMeasure::Chain => form_items(measured, &target.prefix, |m| m.chain),
        TargetMeasure::Ivars => form_items(measured, &target.prefix, |m| m.ivars),
        TargetMeasure::TwinsPer1k => Vec::new(),
    }
}

/// The items of one form measure under the prefix; a callable without
/// the value is left out, and [`form_available`] has already said
/// whether any callable has it.
fn form_items(
    measured: &Measured<'_>,
    prefix: &str,
    value: impl Fn(&codemap_graph::schema::Measure) -> Option<u32>,
) -> Vec<Item> {
    callables_under(measured, prefix)
        .iter()
        .filter_map(|c| value(c.measure).map(|v| item_of(c.node, f64::from(v))))
        .collect()
}

/// Why a form target cannot be held on this snapshot, if it cannot: the
/// policy has no form measures, or (`ivars`) the language has none.
fn form_unavailable(measured: &Measured<'_>, measure: TargetMeasure) -> Option<String> {
    let language = measured.snapshot.identity.language;
    if !codemap_graph::schema::form_measures_available(language) {
        return Some(format!(
            "not measured under {}; {} needs a counting policy with form measures",
            measured.policy,
            measure.name()
        ));
    }
    if measure == TargetMeasure::Ivars && language != codemap_graph::schema::Language::Ruby {
        return Some(format!(
            "{} has no instance variables to count",
            language.tag()
        ));
    }
    None
}

/// Twin pairs with both callables under the prefix and in scope, per
/// thousand callables under the prefix, with the callable count. `None`
/// when the snapshot was extracted without `--twins`.
fn twins_per_1k(measured: &Measured<'_>, prefix: &str) -> Option<(f64, usize)> {
    measured.snapshot.identity.twins_policy.as_ref()?;
    let under: BTreeSet<&str> = callables_under(measured, prefix)
        .iter()
        .map(|c| c.node.id.as_str())
        .collect();
    if under.is_empty() {
        return Some((0.0, 0));
    }
    let pairs = measured
        .snapshot
        .twin_pairs
        .iter()
        .filter(|pair| under.contains(pair.a.as_str()) && under.contains(pair.b.as_str()))
        .count();
    Some((1000.0 * pairs as f64 / under.len() as f64, under.len()))
}

/// The statistic over a sample.
fn statistic(stat: Stat, values: &[f64]) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    match stat {
        Stat::P50 => nearest_rank(&sorted, 50),
        Stat::P90 => nearest_rank(&sorted, 90),
        Stat::Max => sorted.last().copied(),
        Stat::Each | Stat::Value => None,
    }
}

fn unavailable(target: &Target, sample: usize, reason: String) -> TargetResult {
    TargetResult {
        target: target.clone(),
        head: None,
        base: None,
        sample,
        backlog: Vec::new(),
        breached: Vec::new(),
        status: Status::Unavailable,
        reason: Some(reason),
    }
}

/// Over the ceiling and worse than the base ([`crate::policy::TARGET_RATCHET`]).
fn ratchet(max: f64, head: f64, base: Option<f64>) -> Status {
    if head <= max {
        Status::Met
    } else if base.is_some_and(|base| head > base) {
        Status::Breached
    } else {
        Status::OverTarget
    }
}

fn largest_first(items: &mut [Item]) {
    items.sort_by(|a, b| {
        b.value
            .total_cmp(&a.value)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
}

/// `each`: every item over the ceiling, split by the ratchet into the
/// backlog and the breaches.
fn evaluate_each(
    target: &Target,
    head: &Measured<'_>,
    base: Option<&Measured<'_>>,
) -> TargetResult {
    let head_items = items(head, target);
    let base_items: Option<BTreeMap<String, f64>> = base.map(|base| {
        items(base, target)
            .into_iter()
            .map(|item| (item.node_id, item.value))
            .collect()
    });
    let sample = head_items.len();
    let mut backlog = Vec::new();
    let mut breached = Vec::new();
    for mut item in head_items {
        if item.value <= target.max {
            continue;
        }
        let base_value = base_items
            .as_ref()
            .and_then(|base| base.get(&item.node_id).copied());
        item.base = base_value;
        match base_items {
            // Without a base nothing is a breach: the whole excess is backlog.
            None => backlog.push(item),
            Some(_) if base_value.is_some_and(|base| item.value <= base) => backlog.push(item),
            Some(_) => breached.push(item),
        }
    }
    largest_first(&mut backlog);
    largest_first(&mut breached);
    let base_over = base_items
        .as_ref()
        .map(|base| base.values().filter(|value| **value > target.max).count() as f64);
    let status = if !breached.is_empty() {
        Status::Breached
    } else if !backlog.is_empty() {
        Status::OverTarget
    } else {
        Status::Met
    };
    TargetResult {
        target: target.clone(),
        head: Some((backlog.len() + breached.len()) as f64),
        base: base_over,
        sample,
        backlog,
        breached,
        status,
        reason: None,
    }
}

fn evaluate_statistic(
    target: &Target,
    head: &Measured<'_>,
    base: Option<&Measured<'_>>,
) -> TargetResult {
    let head_items = items(head, target);
    let sample = head_items.len();
    let values: Vec<f64> = head_items.iter().map(|item| item.value).collect();
    let Some(head_value) = statistic(target.stat, &values) else {
        return unavailable(
            target,
            0,
            format!(
                "no {} under {} in scope",
                target.measure.item_noun(),
                target.prefix
            ),
        );
    };
    let base_value = base.and_then(|base| {
        let values: Vec<f64> = items(base, target).iter().map(|item| item.value).collect();
        statistic(target.stat, &values)
    });
    TargetResult {
        target: target.clone(),
        head: Some(head_value),
        base: base_value,
        sample,
        backlog: Vec::new(),
        breached: Vec::new(),
        status: ratchet(target.max, head_value, base_value),
        reason: None,
    }
}

fn evaluate_twins(
    target: &Target,
    head: &Measured<'_>,
    base: Option<&Measured<'_>>,
) -> TargetResult {
    let Some((head_value, sample)) = twins_per_1k(head, &target.prefix) else {
        return unavailable(
            target,
            0,
            "not computed; extract with --measures --twins".to_string(),
        );
    };
    if sample == 0 {
        return unavailable(
            target,
            0,
            format!("no callables under {} in scope", target.prefix),
        );
    }
    let base_value = base
        .and_then(|base| twins_per_1k(base, &target.prefix))
        .map(|(value, _)| value);
    TargetResult {
        target: target.clone(),
        head: Some(head_value),
        base: base_value,
        sample,
        backlog: Vec::new(),
        breached: Vec::new(),
        status: ratchet(target.max, head_value, base_value),
        reason: None,
    }
}

/// Hold every target against the head, and the base when there is one.
///
/// # Errors
///
/// A target prefix that matches no file in the head snapshot: a typo
/// would otherwise hold nothing to the ceiling and read as met.
pub fn evaluate(
    targets: &[Target],
    head: &Measured<'_>,
    base: Option<&Measured<'_>>,
) -> Result<Vec<TargetResult>> {
    for target in targets {
        if !head
            .snapshot
            .files
            .iter()
            .any(|file| file.rel_path.starts_with(target.prefix.as_str()))
        {
            bail!(
                "target `{}`: prefix `{}` matches no file in snapshot {}; a prefix that holds nothing to the ceiling is refused so a typo cannot read as met",
                target.name,
                target.prefix,
                head.snapshot.identity.snapshot_id
            );
        }
    }
    Ok(targets
        .iter()
        .map(|target| {
            if target.measure.is_form() {
                if let Some(reason) = form_unavailable(head, target.measure) {
                    return unavailable(target, 0, reason);
                }
            }
            match target.stat {
                Stat::Each => evaluate_each(target, head, base),
                Stat::P50 | Stat::P90 | Stat::Max => evaluate_statistic(target, head, base),
                Stat::Value => evaluate_twins(target, head, base),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_is_the_ceiling_rank_never_interpolated() {
        let sample = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(nearest_rank(&sample, 50), Some(2.0));
        assert_eq!(nearest_rank(&sample, 90), Some(4.0));
        assert_eq!(nearest_rank(&[7.0], 50), Some(7.0));
        assert_eq!(nearest_rank(&[], 50), None);
        let ten: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(nearest_rank(&ten, 90), Some(9.0));
        assert_eq!(nearest_rank(&ten, 50), Some(5.0));
    }

    #[test]
    fn the_ratchet_breaches_only_when_over_and_worse() {
        assert_eq!(ratchet(6.0, 6.0, Some(8.0)), Status::Met);
        assert_eq!(ratchet(6.0, 8.0, Some(8.0)), Status::OverTarget);
        assert_eq!(ratchet(6.0, 8.0, Some(9.0)), Status::OverTarget, "improved");
        assert_eq!(ratchet(6.0, 8.0, Some(7.0)), Status::Breached);
        assert_eq!(ratchet(6.0, 8.0, None), Status::OverTarget, "no base");
    }

    #[test]
    fn a_measure_and_stat_must_go_together() {
        let target = |measure, stat| Target {
            name: "t".into(),
            prefix: "app/".into(),
            measure,
            stat,
            max: 1.0,
        };
        assert!(target(TargetMeasure::Cc, Stat::Each).validate().is_ok());
        assert!(
            target(TargetMeasure::TwinsPer1k, Stat::Value)
                .validate()
                .is_ok()
        );
        let error = target(TargetMeasure::Cc, Stat::Value)
            .validate()
            .expect_err("cc has no single value");
        assert!(error.to_string().contains("per callable"), "{error}");
        let error = target(TargetMeasure::TwinsPer1k, Stat::P90)
            .validate()
            .expect_err("twins_per_1k is one figure");
        assert!(error.to_string().contains("must be `value`"), "{error}");
        let error = target(TargetMeasure::ClassLoc, Stat::Value)
            .validate()
            .expect_err("class_loc is per class");
        assert!(error.to_string().contains("per class"), "{error}");
    }

    #[test]
    fn figures_print_whole_when_whole() {
        assert_eq!(figure(6.0), "6");
        assert_eq!(figure(0.8), "0.8");
        assert_eq!(figure(333.333), "333.3");
    }

    #[test]
    fn names_round_trip() {
        for name in TargetMeasure::NAMES {
            assert_eq!(TargetMeasure::parse(name).expect("known").name(), name);
        }
        for name in Stat::NAMES {
            assert_eq!(Stat::parse(name).expect("known").name(), name);
        }
        assert!(TargetMeasure::parse("arity").is_none());
        assert!(Stat::parse("p99").is_none());
    }
}
