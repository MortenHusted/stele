//! `series`: the per-commit line over a measured history walk, and the
//! slope of each figure per author label over a window.
//!
//! `stele code history --measures` keeps one measured snapshot per walked
//! commit and lists them in `measured.ndjson` with the commit's author
//! label. This module folds those snapshots into one line per commit:
//! erosion and `verbosity_clone`, each beside the absolute counts
//! [`crate::policy::RATIO_NEVER_ALONE`] requires, LOC, and the count of
//! callables over the cutoff. `verbosity` proper does not exist before P3
//! and is reported as absent, never as zero.
//!
//! The slope is an ordinary least-squares fit of a figure against the
//! walk position (`seq`, the commit index in the walk), per author label,
//! over the last `window` commits. The label is what the commit says;
//! what it means is the operator's call, and nothing here claims that a
//! label identifies a person or an agent.
//!
//! The producer computed no metric: every number here is derived in this
//! crate from the snapshots, and a walk whose snapshots were measured
//! under different policies, or not at all, is refused.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use codemap_graph::schema::Snapshot;
use serde::{Deserialize, Serialize};

use crate::measured_in;
use crate::metrics::{CloneCounts, Excluded, TwinCounts, summary, summary_all_files};
use crate::policy::{Counts, HIGH_CC_CUTOFF, Scope};

/// The listing `stele code history --measures` writes, one line per commit.
pub const MEASURED_LISTING: &str = "measured.ndjson";

/// One line of `measured.ndjson`, as the producer wrote it.
#[derive(Debug, Clone, Deserialize)]
pub struct MeasuredEntry {
    pub seq: usize,
    pub commit: String,
    pub ts: i64,
    pub author: String,
    #[serde(default)]
    pub author_email: String,
    pub measures_policy: Option<String>,
    #[serde(default)]
    pub snapshot_id: String,
    /// Path of the measured snapshot, relative to the history directory.
    pub snapshot: String,
}

/// One commit's figures.
#[derive(Debug, Clone, Serialize)]
pub struct SeriesPoint {
    pub seq: usize,
    pub commit: String,
    pub ts: i64,
    pub author: String,
    pub author_email: String,
    pub snapshot_id: String,
    /// Erosion inside the scope.
    pub erosion: f64,
    pub counts: Counts,
    pub verbosity_clone: Option<f64>,
    pub clones: Option<CloneCounts>,
    /// `verbosity` proper: rule hits and clone lines over LOC. Absent until
    /// P3 ships the rules; never zero in its place.
    pub verbosity: Option<f64>,
    pub loc: u64,
    pub lines: u64,
    pub files: usize,
    /// Erosion over every file, printed beside the in-scope figure when a
    /// scope is set ([`crate::policy::SCOPE_PRINTS_BOTH`]); absent when
    /// the scope is every file and the two would be one number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub erosion_all_files: Option<f64>,
    /// What the scope left out at this commit.
    #[serde(skip_serializing_if = "Excluded::is_nothing")]
    pub excluded: Excluded,
    /// Twin pairs in scope at this commit; absent without `--twins`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub twins: Option<TwinCounts>,
}

/// The least-squares slope of each figure for one author label over the
/// window. `None` when the label has fewer than two commits in the
/// window, or when every commit sits at one walk position.
#[derive(Debug, Clone, Serialize)]
pub struct Slope {
    pub author: String,
    pub commits: usize,
    pub first_seq: usize,
    pub last_seq: usize,
    pub erosion: Option<f64>,
    pub verbosity_clone: Option<f64>,
    pub loc: Option<f64>,
    pub high_cc: Option<f64>,
}

/// The whole series document.
#[derive(Debug, Clone, Serialize)]
pub struct Series {
    pub history_dir: String,
    pub language: String,
    pub policy: String,
    /// The files every point is held to; not serialized when it is every
    /// file.
    #[serde(skip_serializing_if = "Scope::is_all_files")]
    pub scope: Scope,
    pub commits: usize,
    pub points: Vec<SeriesPoint>,
    /// Commits the slopes were fitted over (the last `window`, or all).
    pub window: usize,
    pub slopes: Vec<Slope>,
}

/// Read `measured.ndjson` from a history directory.
///
/// # Errors
///
/// A missing listing (no measured walk was made there), a line that is
/// not a measured entry, or an empty walk.
pub fn read_listing(dir: &Path) -> Result<Vec<MeasuredEntry>> {
    let path = dir.join(MEASURED_LISTING);
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "no measured walk: {} not found; run `stele code history --measures --out {}` first",
            path.display(),
            dir.display()
        )
    })?;
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: MeasuredEntry = serde_json::from_str(line)
            .with_context(|| format!("{}:{} is not a measured entry", path.display(), index + 1))?;
        entries.push(entry);
    }
    if entries.is_empty() {
        bail!(
            "empty walk: {} lists no commits, so there is no series to fold",
            path.display()
        );
    }
    entries.sort_by_key(|entry| entry.seq);
    Ok(entries)
}

fn read_snapshot(dir: &Path, entry: &MeasuredEntry) -> Result<Snapshot> {
    let path: PathBuf = dir.join(&entry.snapshot);
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "missing snapshot: commit {} (seq {}) names {} and it is not there",
            entry.commit,
            entry.seq,
            path.display()
        )
    })?;
    serde_json::from_str(&text).with_context(|| format!("parse snapshot {}", path.display()))
}

/// Fold a measured walk into its series, held to a scope.
///
/// Every snapshot is admitted through [`crate::measured_in`], so the
/// series holds only commits measured under one policy this crate reads,
/// and a scope prefix must match a file at every commit.
///
/// # Errors
///
/// Whatever [`read_listing`] refuses; a listed snapshot that is missing or
/// does not parse; an unmeasured commit, a language without a policy, an
/// empty inventory, or a scope prefix matching nothing (each with its
/// reason and the commit named); two commits under different policies or
/// languages.
pub fn series(dir: &Path, window: Option<usize>, scope: &Scope) -> Result<Series> {
    let entries = read_listing(dir)?;
    let mut points = Vec::with_capacity(entries.len());
    let mut policy: Option<String> = None;
    let mut language: Option<String> = None;
    for entry in &entries {
        let snapshot = read_snapshot(dir, entry)?;
        let admitted = measured_in(&snapshot, scope)
            .with_context(|| format!("commit {} (seq {})", entry.commit, entry.seq))?;
        match policy.as_deref() {
            None => policy = Some(admitted.policy.to_string()),
            Some(expected) if expected == admitted.policy => {}
            Some(expected) => bail!(
                "policy mismatch: commit {} (seq {}) was measured under `{}` while the walk started under `{expected}`; counts under different rules are never put on one series",
                entry.commit,
                entry.seq,
                admitted.policy
            ),
        }
        let tag = snapshot.identity.language.tag().to_string();
        match language.as_deref() {
            None => language = Some(tag),
            Some(expected) if expected == tag => {}
            Some(expected) => bail!(
                "language mismatch: commit {} (seq {}) is {tag} while the walk started as {expected}",
                entry.commit,
                entry.seq
            ),
        }
        let s = summary(&admitted);
        points.push(SeriesPoint {
            seq: entry.seq,
            commit: entry.commit.clone(),
            ts: entry.ts,
            author: entry.author.clone(),
            author_email: entry.author_email.clone(),
            snapshot_id: snapshot.identity.snapshot_id.clone(),
            erosion: s.erosion,
            counts: s.counts,
            verbosity_clone: s.verbosity_clone,
            clones: s.clones,
            verbosity: None,
            loc: s.loc,
            lines: s.lines,
            files: s.files,
            erosion_all_files: (!scope.is_all_files())
                .then(|| summary_all_files(&admitted).erosion),
            excluded: s.excluded,
            twins: s.twins,
        });
    }
    let window = window.map_or(points.len(), |w| w.clamp(1, points.len()));
    let slopes = slopes(&points[points.len() - window..]);
    Ok(Series {
        history_dir: dir.display().to_string(),
        language: language.unwrap_or_default(),
        policy: policy.unwrap_or_default(),
        scope: scope.clone(),
        commits: points.len(),
        points,
        window,
        slopes,
    })
}

/// Ordinary least squares: the slope of `y` against `x`. `None` under two
/// points or without spread in `x`.
#[must_use]
pub fn least_squares_slope(points: &[(f64, f64)]) -> Option<f64> {
    if points.len() < 2 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / n;
    let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|(x, _)| (x - mean_x).powi(2)).sum();
    if sxx == 0.0 {
        return None;
    }
    let sxy: f64 = points
        .iter()
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum();
    Some(sxy / sxx)
}

/// Slopes per author label over the given points, labels in name order.
#[must_use]
pub fn slopes(points: &[SeriesPoint]) -> Vec<Slope> {
    let mut by_author: BTreeMap<&str, Vec<&SeriesPoint>> = BTreeMap::new();
    for point in points {
        by_author
            .entry(point.author.as_str())
            .or_default()
            .push(point);
    }
    by_author
        .into_iter()
        .map(|(author, commits)| {
            #[allow(clippy::cast_precision_loss)]
            let fit = |y: &dyn Fn(&SeriesPoint) -> f64| {
                let pairs: Vec<(f64, f64)> = commits.iter().map(|p| (p.seq as f64, y(p))).collect();
                least_squares_slope(&pairs)
            };
            #[allow(clippy::cast_precision_loss)]
            Slope {
                author: author.to_string(),
                commits: commits.len(),
                first_seq: commits.first().map_or(0, |p| p.seq),
                last_seq: commits.last().map_or(0, |p| p.seq),
                erosion: fit(&|p| p.erosion),
                verbosity_clone: commits
                    .iter()
                    .map(|p| p.verbosity_clone.map(|value| (p.seq as f64, value)))
                    .collect::<Option<Vec<_>>>()
                    .and_then(|pairs| least_squares_slope(&pairs)),
                loc: fit(&|p| p.loc as f64),
                high_cc: fit(&|p| p.counts.high_cc as f64),
            }
        })
        .collect()
}

fn opt(value: Option<f64>, digits: usize) -> String {
    value.map_or_else(|| "n/a".to_string(), |v| format!("{v:+.digits$}"))
}

/// Render the series as text a reviewer reads top down: one line per
/// commit, then the slope table.
#[must_use]
pub fn series_text(series: &Series) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "history: {}", series.history_dir);
    let _ = writeln!(
        out,
        "language: {}  policy: {}  commits: {}",
        series.language, series.policy, series.commits
    );
    if !series.scope.is_all_files() {
        let _ = writeln!(
            out,
            "scope: excluding {}; every figure below is in scope, with the all-files erosion in parentheses",
            series.scope.describe()
        );
    }
    let _ = writeln!(
        out,
        "per commit (erosion and verbosity_clone print with their counts; verbosity proper is absent until rule hits exist):"
    );
    for p in &series.points {
        let clones = match (p.verbosity_clone, p.clones) {
            (Some(ratio), Some(counts)) => format!(
                "verbosity_clone {ratio:.3} (groups {}, clone lines {} of {})",
                counts.groups, counts.clone_sloc, p.loc,
            ),
            _ => "verbosity_clone n/a (not measured)".into(),
        };
        let twins = p.twins.map_or_else(
            || "twins n/a".to_string(),
            |t| format!("twins {} ({} near)", t.pairs, t.near),
        );
        let all_files = p.erosion_all_files.map_or_else(String::new, |erosion| {
            format!(
                " (all files {erosion:.3}, excluded {} callables in {} files)",
                p.excluded.callables, p.excluded.files
            )
        });
        let _ = writeln!(
            out,
            "  {:>4}  {}  {:<24}  erosion {:.3} (over cc {HIGH_CC_CUTOFF}: {}, max cc {}, mass above {:.1} of {:.1}){all_files}  {clones}  {twins}  loc {}  verbosity n/a",
            p.seq,
            &p.commit[..12.min(p.commit.len())],
            truncate(&p.author, 24),
            p.erosion,
            p.counts.high_cc,
            p.counts.max_cc,
            p.counts.mass_above_cutoff,
            p.counts.mass_total,
            p.loc,
        );
    }
    let _ = writeln!(
        out,
        "slope per author label over the last {} commit(s), least squares against walk position (per commit):",
        series.window
    );
    for s in &series.slopes {
        let _ = writeln!(
            out,
            "  {:<24}  commits {:>3} (seq {}..{})  erosion {}  verbosity_clone {}  loc {}  over-cutoff {}",
            truncate(&s.author, 24),
            s.commits,
            s.first_seq,
            s.last_seq,
            opt(s.erosion, 5),
            opt(s.verbosity_clone, 5),
            opt(s.loc, 1),
            opt(s.high_cc, 3),
        );
    }
    let _ = writeln!(
        out,
        "  (the author label is what the commit says; the operator decides what it means. A slope over fewer than two commits is n/a.)"
    );
    out
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        let head: String = s.chars().take(width - 1).collect();
        format!("{head}~")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_straight_line_has_its_slope_and_one_point_has_none() {
        let line = [(0.0, 1.0), (1.0, 3.0), (2.0, 5.0)];
        assert!((least_squares_slope(&line).expect("slope") - 2.0).abs() < 1e-12);
        assert_eq!(least_squares_slope(&[(4.0, 9.0)]), None);
        assert_eq!(
            least_squares_slope(&[(4.0, 9.0), (4.0, 11.0)]),
            None,
            "no spread in x fits nothing"
        );
    }

    #[test]
    fn a_flat_series_has_zero_slope() {
        let flat = [(0.0, 0.5), (1.0, 0.5), (5.0, 0.5)];
        assert_eq!(least_squares_slope(&flat), Some(0.0));
    }
}
