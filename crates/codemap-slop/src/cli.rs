//! `codemap-slop` command line: the change report, one snapshot's figures,
//! the advisory budget gate, and the per-commit series over a measured
//! history walk.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::diff::change_report;
use crate::gate::{Budget, evaluate, parse_budget};
use crate::policy::Scope;
use crate::render::{change_text, gate_text, snapshot_report, snapshot_text};
use crate::series::{series, series_text};
use crate::targets::{self, TargetResult};
use crate::{measured_in, measured_pair_in};
use anyhow::{Context, Result};
use clap::{Subcommand, ValueEnum};
use codemap_graph::schema::Snapshot;

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum Format {
    Text,
    Json,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Erosion with its counts, hotspots, per-file and per-cluster erosion
    /// for one measured snapshot.
    Report {
        /// A snapshot written by `stele code extract --measures`.
        #[arg(long)]
        file: PathBuf,
        /// How many hotspots, files, and clusters to list.
        #[arg(long, default_value_t = 10)]
        top: usize,
        #[arg(long, value_enum, default_value = "text")]
        format: Format,
        /// A repository-relative directory the figures are not held to
        /// (`db/migrate/`); repeatable. The all-files figures still print.
        #[arg(long = "exclude", value_name = "DIR/")]
        exclude: Vec<String>,
        /// A `slop.toml` whose `[target.<name>]` sections are held against
        /// this one snapshot: no base, so the excess prints as the backlog
        /// and nothing is breached.
        #[arg(long)]
        budget: Option<PathBuf>,
    },
    /// What became harder to maintain between two measured snapshots.
    Diff {
        #[arg(long)]
        base: PathBuf,
        #[arg(long)]
        head: PathBuf,
        /// How many head hotspots to list.
        #[arg(long, default_value_t = 10)]
        top: usize,
        #[arg(long, value_enum, default_value = "text")]
        format: Format,
        /// A repository-relative directory the figures are not held to
        /// (`db/migrate/`); repeatable. The all-files figures still print.
        #[arg(long = "exclude", value_name = "DIR/")]
        exclude: Vec<String>,
    },
    /// The change report held against a budget; exits 1 on a breach.
    Gate {
        #[arg(long)]
        base: PathBuf,
        #[arg(long)]
        head: PathBuf,
        /// A `slop.toml`: flat `key = number` lines, `exclude` lines, and
        /// `[target.<name>]` sections.
        #[arg(long)]
        budget: PathBuf,
        #[arg(long, default_value_t = 10)]
        top: usize,
        #[arg(long, value_enum, default_value = "text")]
        format: Format,
        /// A repository-relative directory the figures are not held to
        /// (`db/migrate/`); repeatable. The all-files figures still print.
        #[arg(long = "exclude", value_name = "DIR/")]
        exclude: Vec<String>,
    },
    /// One line per commit over a measured history walk, and the slope of
    /// each figure per author label over a window.
    Series {
        /// A directory written by `stele code history --measures`.
        #[arg(long)]
        history: PathBuf,
        /// Fit the slopes over the last N commits (default: the whole walk).
        #[arg(long)]
        window: Option<usize>,
        #[arg(long, value_enum, default_value = "text")]
        format: Format,
        /// A repository-relative directory the figures are not held to
        /// (`db/migrate/`); repeatable. The all-files figures still print.
        #[arg(long = "exclude", value_name = "DIR/")]
        exclude: Vec<String>,
    },
}

fn read_snapshot(path: &Path) -> Result<Snapshot> {
    let bytes = std::fs::read_to_string(path)
        .with_context(|| format!("read snapshot {}", path.display()))?;
    serde_json::from_str(&bytes).with_context(|| format!("parse snapshot {}", path.display()))
}

fn read_budget(path: &Path) -> Result<Budget> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read budget {}", path.display()))?;
    parse_budget(&text)
}

/// The scope a budgeted command is held to: the budget's `exclude` lines
/// or `--exclude`, never both, so a gate reads its scope from one place.
fn budget_scope(budget: &Budget, exclude: &[String]) -> Result<Scope> {
    if !budget.exclude.is_empty() && !exclude.is_empty() {
        anyhow::bail!(
            "the budget excludes {} and --exclude was also given; a gate reads its scope from one place",
            budget.exclude.join(", ")
        );
    }
    Scope::excluding(if budget.exclude.is_empty() {
        exclude
    } else {
        &budget.exclude
    })
}

fn run(command: Command) -> Result<bool> {
    match command {
        Command::Report {
            file,
            top,
            format,
            exclude,
            budget,
        } => {
            let budget = budget.as_deref().map(read_budget).transpose()?;
            let scope = match &budget {
                Some(budget) => budget_scope(budget, &exclude)?,
                None => Scope::excluding(&exclude)?,
            };
            let snapshot = read_snapshot(&file)?;
            let admitted = measured_in(&snapshot, &scope)?;
            let mut report = snapshot_report(&admitted, top);
            if let Some(budget) = &budget {
                report.targets = Some(targets::evaluate(&budget.targets, &admitted, None)?);
            }
            match format {
                Format::Text => print!("{}", snapshot_text(&report)),
                Format::Json => println!("{}", serde_json::to_string_pretty(&report)?),
            }
            Ok(true)
        }
        Command::Diff {
            base,
            head,
            top,
            format,
            exclude,
        } => {
            let scope = Scope::excluding(&exclude)?;
            let base = read_snapshot(&base)?;
            let head = read_snapshot(&head)?;
            let (base, head) = measured_pair_in(&base, &head, &scope)?;
            let report = change_report(&base, &head, top);
            match format {
                Format::Text => print!("{}", change_text(&report)),
                Format::Json => println!("{}", serde_json::to_string_pretty(&report)?),
            }
            Ok(true)
        }
        Command::Gate {
            base,
            head,
            budget,
            top,
            format,
            exclude,
        } => {
            let budget = read_budget(&budget)?;
            let scope = budget_scope(&budget, &exclude)?;
            let base = read_snapshot(&base)?;
            let head = read_snapshot(&head)?;
            let (base, head) = measured_pair_in(&base, &head, &scope)?;
            let report = change_report(&base, &head, top);
            let breaches = evaluate(&report, &budget);
            let targets = targets::evaluate(&budget.targets, &head, Some(&base))?;
            let within = breaches.is_empty() && !targets.iter().any(TargetResult::is_breached);
            match format {
                Format::Text => {
                    print!("{}", change_text(&report));
                    print!("{}", gate_text(&budget, &breaches, &targets, top));
                }
                Format::Json => {
                    let doc = serde_json::json!({
                        "report": report,
                        "budget": budget,
                        "breaches": breaches,
                        "targets": targets,
                        "within_budget": within,
                    });
                    println!("{}", serde_json::to_string_pretty(&doc)?);
                }
            }
            Ok(within)
        }
        Command::Series {
            history,
            window,
            format,
            exclude,
        } => {
            let scope = Scope::excluding(&exclude)?;
            let doc = series(&history, window, &scope)?;
            match format {
                Format::Text => print!("{}", series_text(&doc)),
                Format::Json => println!("{}", serde_json::to_string_pretty(&doc)?),
            }
            Ok(true)
        }
    }
}

/// Run one parsed command, reporting an error as `stele slop: …`.
pub fn dispatch(command: Command) -> ExitCode {
    match run(command) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("stele slop: {error:#}");
            ExitCode::FAILURE
        }
    }
}
