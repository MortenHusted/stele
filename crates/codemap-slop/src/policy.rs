//! The named thresholds and rules behind every number this crate prints.
//!
//! Each constant carries its source. A reviewer who disagrees with a
//! finding can trace it to the exact rule here and challenge the rule,
//! never the arithmetic.

use anyhow::{Result, bail};
use codemap_graph::schema::{Language, measures_policy};

/// Clone thresholds: a run of equal normalised tokens is a clone at fifty
/// tokens and five lines (`jscpd` defaults; `GitClear`'s duplicated-block
/// definition uses the same line count). Applied by the producer; named
/// here so a report can cite them.
pub use codemap_graph::schema::{CLONE_MIN_LINES, CLONE_MIN_TOKENS};
/// Twin thresholds, applied by the producer (`extract --measures --twins`)
/// and named here so a report can cite them: five-token shingles, both
/// bodies at least eighty tokens, shared shingles at least seventy percent
/// of the smaller body's.
pub use codemap_graph::schema::{
    TWIN_MIN_CONTAINMENT_PERCENT, TWIN_MIN_TOKENS, TWIN_NOISE_CALLABLES, TWIN_SHINGLE_TOKENS,
};

/// A callable whose cyclomatic complexity is above this is a hotspot.
/// Radon's bands put 1–5 low, 6–10 moderate, over 10 high; `SlopCodeBench`
/// (arXiv 2603.24755, eq. 3) cuts erosion at the same value.
pub const HIGH_CC_CUTOFF: u32 = 10;

/// The size term in a callable's complexity mass: `cc * sqrt(sloc)`
/// (`SlopCodeBench` eq. 2). The square root keeps complexity, not size, as
/// the dominant term.
pub const MASS_SIZE_TERM: &str = "sqrt(sloc)";

/// Fan-in at which a callable is a hub. The same value `stele code refactor`
/// defaults to for `--hub-threshold` (`codemap-graph::analysis`).
pub const HUB_FAN_IN: usize = 8;

/// A ratio never reports alone.
///
/// Erosion is printed next to the count of
/// callables over the cutoff, the maximum CC, and the total mass above the
/// cutoff, and a ratio that improved while any of those worsened is
/// flagged on the same line. Earendil, "Measuring the sloppiness of code"
/// (2026-09-10), footnote 4: a project the author considered sloppy scored
/// reasonably well because the aggregate concealed it.
pub const RATIO_NEVER_ALONE: &str = "ratio-never-alone";

/// Verified coupling never reports alone, and never as zero when it is
/// unavailable.
///
/// A `calls` edge exists only where the language resolver verified the
/// target (the R2 verified-edge policy, README). So a verified fan-out is
/// a lower bound, and it is printed beside the call sites the resolver
/// observed but could not verify: in-repository name `candidate`s,
/// `external` targets (other crates and the standard library), and
/// `unresolved` sites. On a syntax snapshot there is no resolver, so the
/// verified figures print `n/a` beside the `observed` sites; `0` would
/// read as "calls nothing", which the snapshot cannot say.
pub const COUPLING_VERIFIED_NEVER_ALONE: &str = "coupling-verified-never-alone";

/// An exclusion never hides a number.
///
/// A report held to a scope (every file outside the excluded directory
/// prefixes) prints the all-files figure on the line above the in-scope
/// figure, with how many callables and files the scope left out and
/// under which prefixes. Run-once code (a migration that drops thirty
/// foreign keys with `rescue nil` on every line) can be kept out of what
/// a change is held to; it can never disappear from the page. Same
/// footnote as [`RATIO_NEVER_ALONE`].
pub const SCOPE_PRINTS_BOTH: &str = "scope-prints-both";

/// Which files a report is held to: every file in the snapshot, or every
/// file outside a list of repository-relative directory prefixes.
///
/// A prefix names a directory (`db/migrate/`, `script/`): it ends with
/// `/`, does not start with one, and holds no `..`. Matching is a plain
/// prefix test on `rel_path`, so `db/` also excludes `db/migrate/`. No
/// glob library: a prefix is what the two real cases needed, and a glob
/// can join later behind this same type.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Scope {
    /// Sorted, de-duplicated prefixes, each ending in `/`.
    pub excluded: Vec<String>,
}

/// Every file: the scope a report has when no prefix is excluded.
pub static ALL_FILES: Scope = Scope {
    excluded: Vec::new(),
};

impl Scope {
    /// A scope excluding the given directory prefixes.
    ///
    /// # Errors
    ///
    /// A prefix that is empty, does not end in `/`, starts with `/`, or
    /// contains a `..` segment.
    pub fn excluding<I, S>(prefixes: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut excluded: Vec<String> = prefixes
            .into_iter()
            .map(|prefix| directory_prefix(prefix.as_ref()))
            .collect::<Result<_>>()?;
        excluded.sort();
        excluded.dedup();
        Ok(Self { excluded })
    }

    /// Whether a repository-relative path is inside the scope.
    #[must_use]
    pub fn includes(&self, rel_path: &str) -> bool {
        !self
            .excluded
            .iter()
            .any(|prefix| rel_path.starts_with(prefix.as_str()))
    }

    /// Whether the scope is every file.
    #[must_use]
    pub fn is_all_files(&self) -> bool {
        self.excluded.is_empty()
    }

    /// The prefixes, comma separated, for a report line.
    #[must_use]
    pub fn describe(&self) -> String {
        self.excluded.join(", ")
    }
}

fn directory_prefix(raw: &str) -> Result<String> {
    let prefix = raw.trim();
    if prefix.is_empty() {
        bail!("scope prefix is empty; name a directory such as `db/migrate/`");
    }
    if prefix.starts_with('/') {
        bail!("scope prefix `{prefix}` starts with `/`; prefixes are repository-relative");
    }
    if !prefix.ends_with('/') {
        bail!(
            "scope prefix `{prefix}` does not end with `/`; a scope excludes directories, not files or name stems"
        );
    }
    if prefix.split('/').any(|segment| segment == "..") {
        bail!("scope prefix `{prefix}` contains `..`");
    }
    Ok(prefix.to_string())
}

/// Twins are never folded into the clone figure.
///
/// A twin pair (two callables whose token shingles overlap past the
/// producer's threshold) is the copy-then-edit shape the run detector
/// cannot see, so it is the information `verbosity_clone` misses. It is
/// printed beside that ratio as its own count, near pairs (under 1.00)
/// apart from exact ones, and a snapshot extracted without `--twins`
/// prints `n/a`, never zero.
pub const TWINS_NEVER_FOLDED: &str = "twins-never-folded";

/// A target breaches on what the change did, never on history.
///
/// A shape target (`[target.<name>]` in `slop.toml`, [`crate::targets`])
/// is a ceiling a repository is meant to be under, seeded from a codebase
/// that is: a controller action under cc 6, a model under twenty-five
/// methods. A repository adopting it is usually over it somewhere
/// already, so the gate holds the change, not the backlog: a statistic is
/// *breached* only when it is over the ceiling **and** worse than the
/// base; an `each` target is breached only by an item over the ceiling
/// that the change added to the prefix or made worse. Everything else
/// over the ceiling is printed as *over target*, with its count, so the
/// distance to the shape is on the page and never a passing gate's
/// secret.
pub const TARGET_RATCHET: &str = "target-ratchet";

/// The verbosity this crate reports until a rule set exists.
///
/// Clone lines over file SLOC, both under the one line rule, so it stays
/// in `[0, 1]`.
/// Named `verbosity_clone` everywhere so it is never mistaken for the
/// paper's full metric (`SlopCodeBench` counts rule-flagged lines too).
pub const VERBOSITY_CLONE: &str = "verbosity_clone";

/// The counting policy this crate reads for a language, if any. A snapshot
/// measured under any other policy is refused: two definitions of CC can
/// never be compared by accident.
pub const fn supported_policy(language: Language) -> Option<&'static str> {
    measures_policy(language)
}

/// Whether a callable sits above [`HIGH_CC_CUTOFF`].
pub const fn is_high_cc(cc: u32) -> bool {
    cc > HIGH_CC_CUTOFF
}

/// A callable's complexity mass, `cc * sqrt(sloc)`.
pub fn mass(cc: u32, sloc: u32) -> f64 {
    f64::from(cc) * f64::from(sloc).sqrt()
}

/// The absolute counts a ratio must be printed with.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Counts {
    /// Callables measured.
    pub callables: usize,
    /// Callables with `cc` above the cutoff.
    pub high_cc: usize,
    /// The largest `cc` measured.
    pub max_cc: u32,
    /// Mass summed over every callable.
    pub mass_total: f64,
    /// Mass summed over the callables above the cutoff.
    pub mass_above_cutoff: f64,
}

/// Apply [`RATIO_NEVER_ALONE`] to a base and a head: the reason the
/// improved ratio is not to be trusted on its own, if there is one.
pub fn ratio_alone_flag(
    base_erosion: f64,
    base: &Counts,
    head_erosion: f64,
    head: &Counts,
) -> Option<String> {
    if head_erosion >= base_erosion {
        return None;
    }
    let mut worsened = Vec::new();
    if head.high_cc > base.high_cc {
        worsened.push(format!(
            "callables over {HIGH_CC_CUTOFF} rose {} -> {}",
            base.high_cc, head.high_cc
        ));
    }
    if head.max_cc > base.max_cc {
        worsened.push(format!("max cc rose {} -> {}", base.max_cc, head.max_cc));
    }
    if head.mass_above_cutoff > base.mass_above_cutoff {
        worsened.push(format!(
            "mass above cutoff rose {:.1} -> {:.1}",
            base.mass_above_cutoff, head.mass_above_cutoff
        ));
    }
    if worsened.is_empty() {
        None
    } else {
        Some(format!(
            "{RATIO_NEVER_ALONE}: erosion fell {base_erosion:.3} -> {head_erosion:.3} while {}",
            worsened.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_is_directory_prefixes_and_nothing_else() {
        let scope = Scope::excluding(["script/", "db/migrate/", "script/"]).expect("valid");
        assert_eq!(scope.excluded, ["db/migrate/", "script/"]);
        assert!(!scope.includes("db/migrate/001_drop.rb"));
        assert!(scope.includes("db/schema.rb"));
        assert!(scope.includes("app/models/card.rb"));
        assert!(!scope.is_all_files());
        assert_eq!(scope.describe(), "db/migrate/, script/");
        assert!(ALL_FILES.is_all_files());
        assert!(ALL_FILES.includes("db/migrate/001_drop.rb"));

        for bad in ["", "db/migrate", "/db/migrate/", "db/../migrate/"] {
            assert!(
                Scope::excluding([bad]).is_err(),
                "`{bad}` is not a directory prefix"
            );
        }
    }

    #[test]
    fn the_cutoff_is_radon_high_and_the_paper_cut() {
        assert_eq!(HIGH_CC_CUTOFF, 10);
        assert!(!is_high_cc(10), "10 is the top of the moderate band");
        assert!(is_high_cc(11));
    }

    #[test]
    fn mass_uses_the_square_root_of_sloc() {
        assert_eq!(MASS_SIZE_TERM, "sqrt(sloc)");
        assert!((mass(10, 100) - 100.0).abs() < f64::EPSILON);
        assert!((mass(3, 16) - 12.0).abs() < f64::EPSILON);
        assert!(mass(1, 0).abs() < f64::EPSILON);
    }

    #[test]
    fn every_tree_sitter_language_has_a_counting_policy_this_crate_reads() {
        assert_eq!(
            supported_policy(Language::Rust),
            Some("cc-sloc-clones-form/rust/1")
        );
        assert_eq!((CLONE_MIN_TOKENS, CLONE_MIN_LINES), (50, 5));
        assert_eq!(
            supported_policy(Language::Php),
            Some("cc-sloc-clones-form/php/1")
        );
        assert_eq!(
            supported_policy(Language::Go),
            Some("cc-sloc-clones-form/go/1")
        );
        assert_eq!(
            supported_policy(Language::Ruby),
            Some("cc-sloc-clones-form/ruby/1")
        );
    }

    const fn counts(high_cc: usize, max_cc: u32, above: f64) -> Counts {
        Counts {
            callables: 10,
            high_cc,
            max_cc,
            mass_total: 500.0,
            mass_above_cutoff: above,
        }
    }

    #[test]
    fn an_improved_ratio_with_a_worse_hotspot_is_flagged() {
        let flag = ratio_alone_flag(0.9, &counts(2, 30, 400.0), 0.8, &counts(2, 31, 400.0));
        let flag = flag.expect("flagged");
        assert!(flag.starts_with("ratio-never-alone:"));
        assert!(flag.contains("max cc rose 30 -> 31"));
    }

    #[test]
    fn an_improved_ratio_with_nothing_worse_is_not_flagged() {
        assert_eq!(
            ratio_alone_flag(0.9, &counts(2, 30, 400.0), 0.8, &counts(2, 30, 380.0)),
            None
        );
    }

    #[test]
    fn a_worse_ratio_is_never_flagged_here() {
        // A rising ratio speaks for itself; the flag guards the case where
        // the ratio looks better than the code.
        assert_eq!(
            ratio_alone_flag(0.8, &counts(2, 30, 380.0), 0.9, &counts(3, 40, 500.0)),
            None
        );
    }
}
