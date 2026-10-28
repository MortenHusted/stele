//! `stele check`: extract a repository's code facts, project them as evidence
//! for a shipped checker, and check them, in one process.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use codemap::ExtractOptions;
use codemap::schema::Language;
use codemap_lemmaspec::Checker;
use codemap_lemmaspec::cli::{CheckerArg, ProjectionArgs, project_evidence, read_snapshot};

#[derive(clap::Args, Debug)]
pub struct CheckArgs {
    /// Which shipped checker to run.
    #[arg(value_enum)]
    checker: CheckerArg,
    /// Repository to extract. Its worktree must be clean; check a dirty tree
    /// through `stele code extract --dirty`, `--snapshot` and `--allow-dirty`.
    #[arg(
        long,
        required_unless_present = "snapshot",
        conflicts_with = "snapshot"
    )]
    path: Option<PathBuf>,
    /// Source language of `--path`.
    #[arg(long, value_enum, default_value = "rust", requires = "path")]
    language: CheckLanguage,
    /// A snapshot written by `stele code extract`, instead of extracting.
    #[arg(long)]
    snapshot: Option<PathBuf>,
    #[command(flatten)]
    projection: ProjectionArgs,
    /// Emit the check report as JSON.
    #[arg(long)]
    json: bool,
}

/// The languages `--path` extracts without a prepared profile.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum CheckLanguage {
    Rust,
    Ruby,
    Php,
    Go,
}

impl From<CheckLanguage> for Language {
    fn from(language: CheckLanguage) -> Self {
        match language {
            CheckLanguage::Rust => Self::Rust,
            CheckLanguage::Ruby => Self::Ruby,
            CheckLanguage::Php => Self::Php,
            CheckLanguage::Go => Self::Go,
        }
    }
}

/// Run one check, reporting an error as `stele check: …` with exit status 2.
pub fn dispatch(args: CheckArgs) -> ExitCode {
    match check(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("stele check: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn check(args: CheckArgs) -> Result<ExitCode> {
    let checker: Checker = args.checker.into();
    let snapshot = if let Some(file) = &args.snapshot {
        read_snapshot(file)?
    } else {
        let path = args.path.context("--path or --snapshot is required")?;
        // Absence checks need verified calls; hotspots needs measures.
        codemap::extract(&ExtractOptions {
            calls_required: matches!(checker, Checker::Architecture | Checker::Symbols),
            language: args.language.into(),
            measures: checker == Checker::Hotspots,
            twins: false,
            path,
        })?
    };
    let evidence = project_evidence(&snapshot, checker, args.projection)?.evidence;
    let report = lemmaspec::check_artifact(checker.source(), &evidence)
        .map_err(|error| anyhow::anyhow!("check {}: {error}", checker.name()))?;
    lemmaspec::cli::report_walk(&report, args.json).map_err(anyhow::Error::msg)
}
