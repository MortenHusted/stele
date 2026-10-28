//! The budget gate: a policy applied to the change report's evidence.
//!
//! Advisory until the plan's experiment has run (section 7); it still
//! exits non-zero on a breach so CI can show it. A budget names only
//! metrics this phase computes; a key for a metric that does not exist
//! yet is refused by name rather than silently ignored, and an unknown
//! key is an error.

use anyhow::{Result, bail};
use serde::Serialize;

use crate::diff::ChangeReport;
use crate::policy::Scope;
use crate::targets::{Stat, Target, TargetMeasure};

/// The subset of TOML `slop.toml` is written in; the gate takes no TOML
/// dependency.
///
/// Flat `key = number` lines, repeatable `exclude = "<directory>/"` lines,
/// `[target.<name>]` sections holding `prefix`, `measure`, `stat`, and
/// `max`, `#` comments, nothing else.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Budget {
    /// Largest allowed rise in erosion, base to head.
    pub max_erosion_delta: Option<f64>,
    /// Largest allowed rise in repository SLOC, base to head.
    pub max_loc_delta: Option<i64>,
    /// Largest allowed count of callables that are above the cutoff in the
    /// head and were not in the base.
    pub max_new_high_cc_callables: Option<u64>,
    /// Directory prefixes the change is not held to
    /// ([`crate::policy::Scope`]); the all-files figures still print.
    pub exclude: Vec<String>,
    /// Shape targets ([`crate::targets`]), in file order.
    pub targets: Vec<Target>,
}

impl Budget {
    /// Whether the budget sets any limit. A budget that only excludes
    /// directories could never breach.
    #[must_use]
    pub fn has_limit(&self) -> bool {
        self.max_erosion_delta.is_some()
            || self.max_loc_delta.is_some()
            || self.max_new_high_cc_callables.is_some()
            || !self.targets.is_empty()
    }
}

/// Budget keys later phases will accept; refused now by name.
///
/// The change report already prints `verbosity_clone` (P2); a budget on
/// it waits for the P4 series to say what a delta on a real repository
/// looks like, and verbosity proper waits for the rule set (P3).
pub const NOT_YET_KEYS: [(&str, &str); 2] = [
    (
        "max_verbosity_delta",
        "verbosity arrives with the rule set (P3)",
    ),
    (
        "max_verbosity_clone_delta",
        "a verbosity_clone budget waits for the history series (P4); the change report prints the figure",
    ),
];

/// A `[target.<name>]` section while its keys are being read.
#[derive(Debug, Default)]
struct PartialTarget {
    name: String,
    prefix: Option<String>,
    measure: Option<TargetMeasure>,
    stat: Option<Stat>,
    max: Option<f64>,
}

impl PartialTarget {
    fn key(&mut self, key: &str, value: &str) -> Result<()> {
        let name = &self.name;
        match key {
            "prefix" => {
                let prefix = quoted(key, value)?;
                // Validated as a scope prefix so a bad one is refused by name.
                Scope::excluding([prefix.as_str()])?;
                self.prefix = Some(prefix);
            }
            "measure" => {
                let raw = quoted(key, value)?;
                let Some(measure) = TargetMeasure::parse(&raw) else {
                    bail!(
                        "target `{name}`: unknown measure `{raw}`; a target measures one of {}",
                        TargetMeasure::NAMES.join(", ")
                    );
                };
                self.measure = Some(measure);
            }
            "stat" => {
                let raw = quoted(key, value)?;
                let Some(stat) = Stat::parse(&raw) else {
                    bail!(
                        "target `{name}`: unknown stat `{raw}`; a target holds one of {} to its ceiling",
                        Stat::NAMES.join(", ")
                    );
                };
                self.stat = Some(stat);
            }
            "max" => {
                let max = number(key, value)?;
                if max < 0.0 {
                    bail!("target `{name}`: max must not be negative, got {max}");
                }
                self.max = Some(max);
            }
            other => bail!(
                "target `{name}`: unknown key `{other}`; a target has prefix, measure, stat, max"
            ),
        }
        Ok(())
    }

    fn finish(self) -> Result<Target> {
        let mut missing = Vec::new();
        if self.prefix.is_none() {
            missing.push("prefix");
        }
        if self.measure.is_none() {
            missing.push("measure");
        }
        if self.stat.is_none() {
            missing.push("stat");
        }
        if self.max.is_none() {
            missing.push("max");
        }
        if !missing.is_empty() {
            bail!(
                "target `{}` is missing {}; a target has prefix, measure, stat, max",
                self.name,
                missing.join(", ")
            );
        }
        let target = Target {
            name: self.name,
            prefix: self.prefix.unwrap_or_default(),
            measure: self.measure.unwrap_or(TargetMeasure::Cc),
            stat: self.stat.unwrap_or(Stat::Each),
            max: self.max.unwrap_or_default(),
        };
        target.validate()?;
        Ok(target)
    }
}

/// The name in a `[target.<name>]` header, if the line is one.
fn target_header(line: &str) -> Result<Option<String>> {
    let Some(inner) = line
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return Ok(None);
    };
    let Some(name) = inner.trim().strip_prefix("target.") else {
        bail!("unknown section `{line}`; the gate reads `[target.<name>]` sections only");
    };
    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!(
            "target section `{line}`: a name is letters, digits, `_`, and `-`, such as `[target.controller_method_cc]`"
        );
    }
    Ok(Some(name.to_string()))
}

/// Parse a budget file.
///
/// # Errors
///
/// An unknown key, section, measure, or stat, named; a key for a metric a
/// later phase adds; a malformed value; a target missing a key or pairing
/// a measure with a stat it has no meaning for; a budget that sets no
/// limit.
pub fn parse_budget(text: &str) -> Result<Budget> {
    let mut budget = Budget::default();
    let mut current: Option<PartialTarget> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = target_header(line)? {
            if let Some(partial) = current.take() {
                budget.targets.push(partial.finish()?);
            }
            if budget.targets.iter().any(|target| target.name == name) {
                bail!("target `{name}` is defined twice");
            }
            current = Some(PartialTarget {
                name,
                ..PartialTarget::default()
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!(
                "budget line {}: expected `key = value`, got `{raw}`",
                index + 1
            );
        };
        let key = key.trim();
        let value = value.trim();
        if let Some(partial) = current.as_mut() {
            partial.key(key, value)?;
            continue;
        }
        if let Some((_, reason)) = NOT_YET_KEYS.iter().find(|(name, _)| *name == key) {
            bail!("budget key `{key}` is not available yet: {reason}");
        }
        match key {
            "max_erosion_delta" => {
                budget.max_erosion_delta = Some(number(key, value)?);
            }
            "max_loc_delta" => {
                budget.max_loc_delta = Some(integer(key, value)?);
            }
            "max_new_high_cc_callables" => {
                let count = integer(key, value)?;
                if count < 0 {
                    bail!("budget key `{key}` must not be negative, got {count}");
                }
                budget.max_new_high_cc_callables = Some(count.unsigned_abs());
            }
            "exclude" => budget.exclude.push(quoted(key, value)?),
            other => bail!(
                "unknown budget key `{other}`; the gate reads max_erosion_delta, max_loc_delta, max_new_high_cc_callables, exclude, and [target.<name>] sections"
            ),
        }
    }
    if let Some(partial) = current.take() {
        budget.targets.push(partial.finish()?);
    }
    if !budget.has_limit() {
        bail!("empty budget: no limit set, so the gate could never breach");
    }
    // Validated as a scope so a bad prefix is refused here, by name.
    budget.exclude = Scope::excluding(&budget.exclude)?.excluded;
    Ok(budget)
}

/// A TOML basic or literal string: `"db/migrate/"` or `'db/migrate/'`.
fn quoted(key: &str, value: &str) -> Result<String> {
    let inner = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        });
    match inner {
        Some(inner) if !inner.contains(['"', '\'', '\\']) => Ok(inner.to_string()),
        _ => bail!("budget key `{key}`: `{value}` is not a plain quoted string"),
    }
}

fn number(key: &str, value: &str) -> Result<f64> {
    let parsed: f64 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("budget key `{key}`: `{value}` is not a number"))?;
    if !parsed.is_finite() {
        bail!("budget key `{key}`: `{value}` is not a finite number");
    }
    Ok(parsed)
}

fn integer(key: &str, value: &str) -> Result<i64> {
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("budget key `{key}`: `{value}` is not an integer"))
}

/// One limit the change exceeded.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Breach {
    pub key: &'static str,
    pub limit: f64,
    pub actual: f64,
}

/// Hold the change report against the budget's deltas. Targets are held
/// separately by [`crate::targets::evaluate`], which needs the snapshots,
/// not the report.
// Line deltas and callable counts sit far below 2^53; the f64 in a
// breach is for one uniform record shape, not arithmetic.
#[allow(clippy::cast_precision_loss)]
pub fn evaluate(report: &ChangeReport, budget: &Budget) -> Vec<Breach> {
    let mut breaches = Vec::new();
    if let Some(limit) = budget.max_erosion_delta {
        let actual = report.erosion_delta();
        if actual > limit {
            breaches.push(Breach {
                key: "max_erosion_delta",
                limit,
                actual,
            });
        }
    }
    if let Some(limit) = budget.max_loc_delta {
        let actual = report.loc_delta();
        if actual > limit {
            breaches.push(Breach {
                key: "max_loc_delta",
                limit: limit as f64,
                actual: actual as f64,
            });
        }
    }
    if let Some(limit) = budget.max_new_high_cc_callables {
        let actual = report.new_high_cc_callables() as u64;
        if actual > limit {
            breaches.push(Breach {
                key: "max_new_high_cc_callables",
                limit: limit as f64,
                actual: actual as f64,
            });
        }
    }
    breaches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_keys_with_comments() {
        let budget = parse_budget(
            "# advisory\nmax_erosion_delta = 0.01  # per change\nmax_loc_delta = 500\nmax_new_high_cc_callables = 0\n",
        )
        .expect("parses");
        assert_eq!(budget.max_erosion_delta, Some(0.01));
        assert_eq!(budget.max_loc_delta, Some(500));
        assert_eq!(budget.max_new_high_cc_callables, Some(0));
        assert_eq!(budget.targets.len(), 0);
    }

    #[test]
    fn a_verbosity_budget_is_refused_by_name_until_the_rule_set() {
        let error = parse_budget("max_verbosity_delta = 0.1\n").expect_err("refused");
        assert!(error.to_string().contains("max_verbosity_delta"));
        assert!(error.to_string().contains("P3"));
        let error = parse_budget("max_verbosity_clone_delta = 0.1\n").expect_err("refused");
        assert!(error.to_string().contains("P4"));
    }

    #[test]
    fn an_unknown_key_is_an_error() {
        let error = parse_budget("max_cc = 10\n").expect_err("refused");
        assert!(error.to_string().contains("unknown budget key `max_cc`"));
    }

    #[test]
    fn an_empty_budget_is_an_error() {
        assert!(parse_budget("# nothing\n").is_err());
        assert!(
            parse_budget("exclude = \"db/migrate/\"\n").is_err(),
            "an exclusion alone sets no limit"
        );
    }

    #[test]
    fn exclusions_are_quoted_directory_prefixes_and_repeat() {
        let budget = parse_budget(
            "max_loc_delta = 10\nexclude = \"script/\"\nexclude = 'db/migrate/'  # run once\n",
        )
        .expect("parses");
        assert_eq!(budget.exclude, ["db/migrate/", "script/"]);
        assert!(parse_budget("max_loc_delta = 10\nexclude = db/migrate/\n").is_err());
        let error = parse_budget("max_loc_delta = 10\nexclude = \"db/migrate\"\n")
            .expect_err("no trailing slash");
        assert!(error.to_string().contains("db/migrate"), "{error}");
    }

    #[test]
    fn a_non_number_is_an_error() {
        assert!(parse_budget("max_erosion_delta = lots\n").is_err());
        assert!(parse_budget("max_loc_delta = 1.5\n").is_err());
        assert!(parse_budget("max_new_high_cc_callables = -1\n").is_err());
    }

    const TARGETS: &str = "\
# a target alone is a limit
[target.controller_method_cc]   # fizzy: max 6
prefix = \"app/controllers/\"
measure = \"cc\"
stat = \"each\"
max = 6

[target.app_twins]
prefix = 'app/'
measure = 'twins_per_1k'
stat = 'value'
max = 2.5
";

    #[test]
    fn target_sections_parse_in_file_order_and_count_as_a_limit() {
        let budget = parse_budget(TARGETS).expect("parses");
        assert!(budget.has_limit());
        assert_eq!(budget.targets.len(), 2);
        let first = &budget.targets[0];
        assert_eq!(first.name, "controller_method_cc");
        assert_eq!(first.prefix, "app/controllers/");
        assert_eq!(first.measure, TargetMeasure::Cc);
        assert_eq!(first.stat, Stat::Each);
        assert!((first.max - 6.0).abs() < f64::EPSILON);
        let second = &budget.targets[1];
        assert_eq!(second.measure, TargetMeasure::TwinsPer1k);
        assert_eq!(second.stat, Stat::Value);
        assert!((second.max - 2.5).abs() < f64::EPSILON);
        assert_eq!(first.rule(), "cc each <= 6");
    }

    #[test]
    fn top_level_keys_may_precede_the_sections_but_not_follow_them() {
        let budget = parse_budget(&format!(
            "max_loc_delta = 10\nexclude = \"test/\"\n{TARGETS}"
        ))
        .expect("parses");
        assert_eq!(budget.max_loc_delta, Some(10));
        assert_eq!(budget.exclude, ["test/"]);
        assert_eq!(budget.targets.len(), 2);
        let error = parse_budget(&format!("{TARGETS}max_loc_delta = 10\n"))
            .expect_err("a top-level key after a section belongs to the section");
        assert!(
            error.to_string().contains("unknown key `max_loc_delta`"),
            "{error}"
        );
    }

    #[test]
    fn a_target_is_refused_by_name_for_what_is_wrong_with_it() {
        let cases: [(&str, &str); 8] = [
            (
                "[target.x]\nprefix = \"app/\"\nmeasure = \"arity\"\nstat = \"each\"\nmax = 3\n",
                "unknown measure `arity`",
            ),
            (
                "[target.x]\nprefix = \"app/\"\nmeasure = \"cc\"\nstat = \"p99\"\nmax = 3\n",
                "unknown stat `p99`",
            ),
            (
                "[target.x]\nprefix = \"app/\"\nmeasure = \"cc\"\nmax = 3\n",
                "missing stat",
            ),
            (
                "[target.x]\nprefix = \"app\"\nmeasure = \"cc\"\nstat = \"each\"\nmax = 3\n",
                "does not end with `/`",
            ),
            (
                "[target.x]\nprefix = \"app/\"\nmeasure = \"cc\"\nstat = \"value\"\nmax = 3\n",
                "must be each, p50, p90, or max",
            ),
            (
                "[target.x]\nprefix = \"app/\"\nmeasure = \"cc\"\nstat = \"each\"\nmax = 3\nfoo = 1\n",
                "unknown key `foo`",
            ),
            (
                "[target.x]\nprefix = \"app/\"\nmeasure = \"cc\"\nstat = \"each\"\nmax = 3\n[target.x]\nprefix = \"app/\"\nmeasure = \"cc\"\nstat = \"each\"\nmax = 3\n",
                "defined twice",
            ),
            ("[limits]\nmax_loc_delta = 3\n", "unknown section"),
        ];
        for (text, expected) in cases {
            let error = parse_budget(text).expect_err(expected);
            assert!(error.to_string().contains(expected), "{expected}: {error}");
        }
    }
}
