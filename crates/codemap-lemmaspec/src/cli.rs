//! `codemap-lemmaspec` command line: project a snapshot into evidence, or
//! print one of the shipped checkers.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::{Checker, Options, Projection, citations, project_output};
use anyhow::{Context, Result};
use clap::{Subcommand, ValueEnum};
use codemap_graph::schema::Snapshot;

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Write the evidence file a checker evaluates for this snapshot.
    Evidence {
        /// Path to a versioned codemap snapshot JSON file.
        #[arg(long)]
        file: PathBuf,
        /// Which shipped checker the evidence targets.
        #[arg(long, value_enum, default_value = "architecture")]
        checker: CheckerArg,
        #[command(flatten)]
        projection: ProjectionArgs,
        /// Write actual expanded entry/API selections as JSON (symbols only).
        #[arg(long)]
        selection_out: Option<PathBuf>,
        /// Write here instead of standard output. Must end in `.stele`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Prove a positive observed call path between exact semantic C++, JS/TS or Python node IDs.
    PositivePath {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        entry_node: String,
        #[arg(long)]
        target_node: String,
        #[arg(long)]
        allow_dirty: bool,
        /// Write a self-contained artifact instead of standard output.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Print a shipped checker so it can be run without this repository.
    Checker {
        /// Which checker to print.
        #[arg(value_enum)]
        name: CheckerArg,
        /// Write here instead of standard output.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum CheckerArg {
    Architecture,
    Symbols,
    Hotspots,
    Citations,
}

impl From<CheckerArg> for Checker {
    fn from(arg: CheckerArg) -> Self {
        match arg {
            CheckerArg::Architecture => Self::Architecture,
            CheckerArg::Symbols => Self::Symbols,
            CheckerArg::Hotspots => Self::Hotspots,
            CheckerArg::Citations => Self::Citations,
        }
    }
}

/// How a snapshot becomes evidence for a shipped checker.
#[derive(clap::Args, Debug)]
pub struct ProjectionArgs {
    /// Emit `LemmaSpec` 0.4 evidence bases, labels, and maintainer notes.
    #[arg(long)]
    readable: bool,
    /// Rule-card Markdown input; required only by the citations checker.
    #[arg(long)]
    rule_cards: Option<PathBuf>,
    /// Spec name; derived from the repository identity when omitted.
    #[arg(long)]
    spec: Option<String>,
    /// Accept a snapshot extracted from a dirty worktree.
    #[arg(long)]
    allow_dirty: bool,
    /// Minimum exact in-repository resolution per 1000 recognized sites.
    /// Admission policy only; does not prove complete extraction.
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u16).range(0..=1000))]
    min_call_coverage_per_mille: u16,
    /// A file that is the evaluation core (repository-relative; repeatable).
    #[arg(long = "core")]
    core_files: Vec<String>,
    /// A file that is presentation, not evaluation (repeatable).
    #[arg(long = "presentation")]
    presentation_files: Vec<String>,
    /// A symbol key where execution starts (repeatable).
    #[arg(long = "entry")]
    entry_points: Vec<String>,
    /// A symbol key exported as public API (repeatable).
    #[arg(long = "public-api")]
    public_api: Vec<String>,
}

/// Project `snapshot` into the evidence `checker` evaluates, with the
/// symbols checker's expanded entry-point and public-API selection.
pub fn project_evidence(
    snapshot: &Snapshot,
    checker: Checker,
    args: ProjectionArgs,
) -> Result<Projection> {
    let options = Options {
        checker,
        spec_name: args.spec,
        allow_dirty: args.allow_dirty,
        min_call_coverage_per_mille: args.min_call_coverage_per_mille,
        core_files: args.core_files,
        presentation_files: args.presentation_files,
        entry_points: args.entry_points,
        public_api: args.public_api,
    };
    if checker == Checker::Citations {
        let path = args.rule_cards.context("citations requires --rule-cards")?;
        let markdown = std::fs::read_to_string(&path)
            .with_context(|| format!("read rule cards {}", path.display()))?;
        return Ok(Projection {
            evidence: citations::project(snapshot, &options, &markdown)?,
            selection: None,
        });
    }
    anyhow::ensure!(
        args.rule_cards.is_none(),
        "--rule-cards is supported only by the citations checker"
    );
    project_output(snapshot, &options, args.readable)
}

/// Read and parse a versioned snapshot JSON file.
pub fn read_snapshot(file: &std::path::Path) -> Result<Snapshot> {
    let bytes = std::fs::read_to_string(file)
        .with_context(|| format!("read snapshot {}", file.display()))?;
    serde_json::from_str(&bytes).with_context(|| format!("parse snapshot {}", file.display()))
}

#[allow(clippy::too_many_lines)] // Keep each CLI command's validation and output together.
fn run(command: Command) -> Result<()> {
    match command {
        Command::Evidence {
            file,
            checker,
            projection,
            selection_out,
            out,
        } => {
            let snapshot = read_snapshot(&file)?;
            let checker: Checker = checker.into();
            if selection_out.is_some() && checker != Checker::Symbols {
                anyhow::bail!("--selection-out is supported only by the symbols checker");
            }
            if let (Some(evidence_path), Some(selection_path)) = (&out, &selection_out) {
                anyhow::ensure!(
                    output_destination(evidence_path)? != output_destination(selection_path)?,
                    "--out and --selection-out must name different files"
                );
            }
            let Projection {
                evidence,
                selection,
            } = project_evidence(&snapshot, checker, projection)?;
            if let Some(path) = &out {
                anyhow::ensure!(
                    is_spec(path),
                    "--out {} must end in .stele; the engine refuses other extensions",
                    path.display()
                );
            }
            if let Some(path) = selection_out {
                let selection =
                    selection.context("symbols projection has no selection manifest")?;
                std::fs::write(
                    &path,
                    format!("{}\n", serde_json::to_string_pretty(&selection)?),
                )
                .with_context(|| format!("write {}", path.display()))?;
            }
            match out {
                Some(path) => {
                    std::fs::write(&path, evidence)
                        .with_context(|| format!("write {}", path.display()))?;
                    let name = checker.name();
                    eprintln!(
                        "wrote {0}\ncheck it with:\n  stele code checker {name} --out {name}.stele\n  stele spec check {name}.stele {0}",
                        path.display()
                    );
                }
                None => print!("{evidence}"),
            }
        }
        Command::PositivePath {
            file,
            entry_node,
            target_node,
            allow_dirty,
            out,
        } => {
            if let Some(path) = &out {
                anyhow::ensure!(is_spec(path), "--out must end in .stele");
                anyhow::ensure!(
                    output_destination(path)? != output_destination(&file)?,
                    "positive path output must differ from its input snapshot"
                );
            }
            let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(&file)?)?;
            let evidence = crate::positive_path::project(
                &snapshot,
                &crate::positive_path::Options {
                    entry_node,
                    target_node,
                    allow_dirty,
                },
            )?;
            match out {
                Some(path) => {
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .with_context(|| format!("create new artifact {}", path.display()))?;
                    std::io::Write::write_all(&mut file, evidence.as_bytes())?;
                    eprintln!("wrote {}; verify with stele spec walk", path.display());
                }
                None => print!("{evidence}"),
            }
        }
        Command::Checker { name, out } => {
            let source = Checker::from(name).source();
            match out {
                Some(path) => std::fs::write(&path, source)
                    .with_context(|| format!("write {}", path.display()))?,
                None => print!("{source}"),
            }
        }
    }
    Ok(())
}

fn output_destination(path: &std::path::Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    Ok(parent
        .canonicalize()?
        .join(path.file_name().context("output requires a filename")?))
}

/// Run one parsed command, reporting an error as `stele code: …`, the
/// prefix of the group it shares with codemap's commands.
pub fn dispatch(command: Command) -> ExitCode {
    match run(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("stele code: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Whether `path` names a specification the engine reads: `.stele`, or
/// `.lemmaspec` from before the engine joined Stele.
fn is_spec(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("stele" | "lemmaspec")
    )
}
