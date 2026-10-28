//! `codemap-slop` — slop metrics over a measured codemap snapshot.
//!
//! codemap emits counts (`extract --measures`: cyclomatic complexity and
//! source lines per callable, line classes per file) under a named
//! counting policy. This crate turns those counts into the things a
//! reviewer acts on, in this order:
//!
//! - [`diff`]: the explainable change report, "what became harder to
//!   maintain in this change": per-callable deltas, callables that
//!   crossed the high-CC cutoff, the hotspots by mass, coupling deltas;
//! - [`metrics`]: erosion, `verbosity_clone`, LOC, mass, per-file and
//!   per-cluster erosion, always next to the absolute counts
//!   [`policy::RATIO_NEVER_ALONE`] requires;
//! - [`gate`]: a budget applied to the change report's evidence. Advisory
//!   until the experiment in the plan has run; it still exits non-zero on
//!   a breach so CI can show it;
//! - [`series`]: one line per commit over a measured history walk
//!   (`stele code history --measures`) and the slope of each figure per
//!   author label, which is the data the plan's experiment needs;
//! - [`targets`]: shape targets from `[target.<name>]` sections of
//!   `slop.toml`, ceilings on `cc`, `sloc`, methods per class, class
//!   lines, inheritance depth, and twin pairs under a directory prefix,
//!   held with the ratchet rule ([`policy::TARGET_RATCHET`]) so a
//!   repository that starts over a target can adopt it.
//!
//! It reads the published snapshot schema through `codemap-graph`, never
//! the extractor, exactly as `codemap-lemmaspec` does. Every threshold is
//! a named constant in [`policy`] with a citation.
//!
//! **Every entry point fails closed.** A snapshot without a counting
//! policy, two snapshots measured under different policies, a language
//! this crate has no policy for, or an empty inventory is an error with a
//! named reason, never a report of zero and never a passing gate.

pub mod cli;
pub mod diff;
pub mod gate;
pub mod metrics;
pub mod policy;
pub mod render;
pub mod series;
pub mod targets;

use anyhow::{Context, Result, bail};
use codemap_graph::schema::Snapshot;

/// A snapshot admitted for metrics: validator-clean, measured under the
/// counting policy this crate reads for its language, with a non-empty
/// inventory, held to a [`policy::Scope`].
#[derive(Debug, Clone, Copy)]
pub struct Measured<'a> {
    pub snapshot: &'a Snapshot,
    /// The counting policy the identity names (`cc-sloc-clones-form/rust/1`).
    pub policy: &'a str,
    /// The files the figures are held to. Every file unless the caller
    /// excluded directory prefixes; the all-files figure is still printed
    /// beside them ([`policy::SCOPE_PRINTS_BOTH`]).
    pub scope: &'a policy::Scope,
}

/// Admit one snapshot over every file, or say exactly why not.
pub fn measured(snapshot: &Snapshot) -> Result<Measured<'_>> {
    measured_in(snapshot, &policy::ALL_FILES)
}

/// Admit one snapshot held to a scope, or say exactly why not.
///
/// # Errors
///
/// Everything [`measured`] refuses; a prefix that matches no file in the
/// snapshot (a typo would otherwise widen the scope silently); a scope
/// that leaves no file.
pub fn measured_in<'a>(snapshot: &'a Snapshot, scope: &'a policy::Scope) -> Result<Measured<'a>> {
    let language = snapshot.identity.language;
    let Some(policy) = snapshot.identity.measures_policy.as_deref() else {
        bail!(
            "unmeasured snapshot: {} carries no measures_policy; extract it with `stele code extract --measures`",
            snapshot.identity.snapshot_id
        );
    };
    match policy::supported_policy(language) {
        Some(expected) if expected == policy => {}
        Some(expected) => bail!(
            "unsupported policy: snapshot {} was measured under `{policy}`, this crate reads `{expected}` for {}",
            snapshot.identity.snapshot_id,
            language.tag()
        ),
        None => bail!(
            "unsupported language: no counting policy exists for {} snapshots yet (snapshot {} is measured under `{policy}`)",
            language.tag(),
            snapshot.identity.snapshot_id
        ),
    }
    if snapshot.files.is_empty() || snapshot.file_measures.is_empty() {
        bail!(
            "empty inventory: snapshot {} holds no measured files, so no ratio over it means anything",
            snapshot.identity.snapshot_id
        );
    }
    codemap_graph::validate(snapshot).context("snapshot failed validation")?;
    for prefix in &scope.excluded {
        if !snapshot
            .files
            .iter()
            .any(|file| file.rel_path.starts_with(prefix.as_str()))
        {
            bail!(
                "scope prefix `{prefix}` matches no file in snapshot {}; a prefix that excludes nothing is refused so a typo cannot widen the scope silently",
                snapshot.identity.snapshot_id
            );
        }
    }
    if !snapshot
        .files
        .iter()
        .any(|file| scope.includes(&file.rel_path))
    {
        bail!(
            "scope excludes every file in snapshot {} (under {}), so no ratio over it means anything",
            snapshot.identity.snapshot_id,
            scope.describe()
        );
    }
    Ok(Measured {
        snapshot,
        policy,
        scope,
    })
}

/// Admit a base and a head for comparison over every file: both
/// admitted, same policy.
pub fn measured_pair<'a>(
    base: &'a Snapshot,
    head: &'a Snapshot,
) -> Result<(Measured<'a>, Measured<'a>)> {
    measured_pair_in(base, head, &policy::ALL_FILES)
}

/// Admit a base and a head held to one scope.
///
/// # Errors
///
/// Whatever [`measured_in`] refuses for either side, named; two policies
/// or two languages.
pub fn measured_pair_in<'a>(
    base: &'a Snapshot,
    head: &'a Snapshot,
    scope: &'a policy::Scope,
) -> Result<(Measured<'a>, Measured<'a>)> {
    let base = measured_in(base, scope).context("base snapshot")?;
    let head = measured_in(head, scope).context("head snapshot")?;
    if base.policy != head.policy {
        bail!(
            "policy mismatch: base was measured under `{}` and head under `{}`; counts under different rules are never compared",
            base.policy,
            head.policy
        );
    }
    if base.snapshot.identity.language != head.snapshot.identity.language {
        bail!(
            "language mismatch: base is {} and head is {}; erosion is never compared across languages",
            base.snapshot.identity.language.tag(),
            head.snapshot.identity.language.tag()
        );
    }
    Ok((base, head))
}
