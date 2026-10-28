//! `stele plan`: check a plan against its catalog and the code, or write the
//! evidence and the checker to run the steps one at a time.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use codemap::ExtractOptions;
use codemap::cli::LanguageArg;
use codemap::schema::Language;
use codemap_lemmaspec::cli::read_snapshot;
use lemmaspec::WalkReport;
use stele_plan::identity::Repo;
use stele_plan::project::realized_anchors;
use stele_plan::{APPROVALS_FILE, DirReader, report};

use crate::check::CheckLanguage;

#[derive(clap::Subcommand, Debug)]
pub enum PlanCommand {
    /// Check a plan, or a catalog root, against its catalog and the code.
    /// The report reads and exits as `stele spec check`; the changes that
    /// are ready and blocked follow it.
    Check {
        #[command(flatten)]
        source: Source,
        /// Emit the check report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Write the evidence the shipped plan checker reads.
    Evidence {
        #[command(flatten)]
        source: Source,
        /// Write here instead of standard output.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Print the shipped plan checker so it can be run without this binary.
    Checker {
        /// Write here instead of standard output.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(clap::Args, Debug)]
pub struct Source {
    /// A plan file, or the root file of a catalog.
    file: PathBuf,
    #[command(flatten)]
    code: Code,
}

/// Where the code that anchors name is read from.
#[derive(clap::Args, Debug)]
pub struct Code {
    /// Repository whose working tree the anchors are checked against.
    /// Defaults to the repository holding the plan or catalog.
    #[arg(long, conflicts_with = "snapshot")]
    path: Option<PathBuf>,
    /// A language to extract the working tree in; repeat it for several.
    /// Defaults to each language a symbol anchor names, or rust when none
    /// does.
    #[arg(long = "language", value_enum)]
    languages: Vec<CheckLanguage>,
    /// A snapshot written by `stele code extract`, instead of extracting.
    /// It holds one language.
    #[arg(long)]
    snapshot: Option<PathBuf>,
}

impl Code {
    /// The code `anchors` are checked against: the snapshot given, or else
    /// the working tree of `--path`, by default `base`, extracted
    /// structurally in each of their languages; and that working tree's
    /// files, identified as from the catalog directory `catalog`.
    pub fn read(
        &self,
        base: &Path,
        catalog: &Path,
        anchors: &BTreeSet<&str>,
    ) -> Result<stele_plan::Code> {
        let snapshots = match &self.snapshot {
            Some(snapshot) => vec![read_snapshot(snapshot)?],
            // Plans and catalogs are written alongside the code they
            // describe, so the working tree is read as it is, uncommitted
            // changes included.
            None => languages(&self.languages, anchors.iter().copied())?
                .into_iter()
                .map(|language| {
                    codemap::extract_worktree(&ExtractOptions {
                        path: self.path.clone().unwrap_or_else(|| base.to_path_buf()),
                        calls_required: false,
                        language,
                        measures: false,
                        twins: false,
                    })
                })
                .collect::<Result<_>>()?,
        };
        // Outside a git repository, only the snapshot's files are known.
        let working_tree = match Repo::of_catalog(self.path.as_deref().unwrap_or(catalog)) {
            Ok(repo) => Some(repo.working_tree().map_err(anyhow::Error::msg)?),
            Err(_) => None,
        };
        Ok(stele_plan::Code {
            snapshots,
            working_tree,
        })
    }
}

/// The languages to extract: those `chosen`, or else each one a symbol
/// anchor names, or rust when none does. A language that needs a prepared
/// profile to extract is refused.
pub fn languages<'a>(
    chosen: &[CheckLanguage],
    anchors: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<Language>> {
    if !chosen.is_empty() {
        let mut languages: Vec<Language> = chosen.iter().map(|&language| language.into()).collect();
        languages.sort_by_key(|language| language.tag());
        languages.dedup();
        return Ok(languages);
    }
    stele_plan::languages(anchors)
        .into_iter()
        .map(|tag| {
            if let Some(&language) = CheckLanguage::value_variants()
                .iter()
                .find(|&&language| Language::from(language).tag() == tag)
            {
                return Ok(language.into());
            }
            if LanguageArg::value_variants()
                .iter()
                .any(|&language| Language::from(language).tag() == tag)
            {
                bail!(
                    "the anchors name {tag} code, which is extracted only with a prepared profile; extract it as `stele code extract --help` describes and pass it with --snapshot"
                );
            }
            bail!("the anchors name {tag} code, a language `stele code extract` does not read")
        })
        .collect()
}

/// Run one plan command, reporting an error as `stele plan: …` with exit
/// status 2.
pub fn dispatch(command: PlanCommand) -> ExitCode {
    match run(command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("stele plan: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run(command: PlanCommand) -> Result<ExitCode> {
    match command {
        PlanCommand::Check { source, json } => {
            let report = lemmaspec::check_artifact(stele_plan::CHECKER, &evidence(&source)?)
                .map_err(|error| anyhow!("check the plan: {error}"))?;
            let code = lemmaspec::cli::report_walk(&report, json).map_err(anyhow::Error::msg)?;
            if !json {
                print_order(&report);
            }
            Ok(code)
        }
        PlanCommand::Evidence { source, out } => {
            write(out.as_deref(), &evidence(&source)?)?;
            Ok(ExitCode::SUCCESS)
        }
        PlanCommand::Checker { out } => {
            write(out.as_deref(), stele_plan::CHECKER)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// The evidence for the plan or catalog `source` names, read from the
/// repository that holds it, with anchors checked against its code and, when
/// the catalog a plan imports has approvals, the catalog's standings.
fn evidence(source: &Source) -> Result<String> {
    let (base, relative) = locate(&source.file)?;
    let reader = DirReader(base.clone());
    let document = stele_plan::load(&reader, &relative).map_err(anyhow::Error::msg)?;
    let catalog = document
        .plan
        .as_ref()
        .and_then(|plan| plan.catalog.as_deref());
    let approved_catalog =
        catalog.filter(|catalog| base.join(catalog).with_file_name(APPROVALS_FILE).is_file());
    let mut anchors = stele_plan::anchors(&document);
    if approved_catalog.is_some() {
        anchors.extend(realized_anchors(&document.catalog));
    }
    let code = if anchors.is_empty() {
        None
    } else {
        let catalog_dir = catalog
            .and_then(|catalog| base.join(catalog).parent().map(Path::to_path_buf))
            .unwrap_or_else(|| base.clone());
        Some(source.code.read(&base, &catalog_dir, &anchors)?)
    };
    let standings = match approved_catalog {
        Some(catalog) => {
            report::standings(&base, catalog, code.as_ref()).map_err(anyhow::Error::msg)?
        }
        None => None,
    };
    let evidence = stele_plan::evidence(&reader, &document, code.as_ref(), standings.as_ref())
        .map_err(anyhow::Error::msg)?;
    Ok(lemmaspec::print_artifact(&evidence))
}

/// The directory `file`'s paths resolve from (see [`base_directory`]), and
/// `file` as a `/`-separated path from there.
pub fn locate(file: &Path) -> Result<(PathBuf, String)> {
    let file = file
        .canonicalize()
        .with_context(|| format!("read {}", file.display()))?;
    let base = base_directory(&file)?;
    let relative = file
        .strip_prefix(&base)
        .map_err(|_| anyhow!("{} is outside {}", file.display(), base.display()))?
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Ok((base, relative))
}

/// The top of the git work tree holding `dir`, if there is one.
pub fn work_tree(dir: &Path) -> Option<PathBuf> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
}

/// The top of the git work tree holding `file`, or the current directory
/// when `file` is in none, so the plan's imports and anchors resolve from
/// the repository root.
fn base_directory(file: &Path) -> Result<PathBuf> {
    let dir = file.parent().unwrap_or(file);
    let base = match work_tree(dir) {
        Some(top) => top,
        None => std::env::current_dir().context("read the current directory")?,
    };
    let base = base
        .canonicalize()
        .with_context(|| format!("read {}", base.display()))?;
    if !file.starts_with(&base) {
        bail!(
            "{} is in no git repository and outside the current directory",
            file.display()
        );
    }
    Ok(base)
}

/// The changes ready to start, and each change with what it waits on.
fn print_order(report: &WalkReport) {
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    for fact in &report.facts {
        match (fact.relation.as_str(), fact.args.as_slice()) {
            ("ready", [change]) => ready.push(change.clone()),
            ("blocked", [change, earlier]) => blocked.push(format!("{change} (after {earlier})")),
            _ => {}
        }
    }
    let list = |items: Vec<String>| {
        if items.is_empty() {
            "none".to_string()
        } else {
            items.join(", ")
        }
    };
    println!("ready: {}", list(ready));
    println!("blocked: {}", list(blocked));
}

/// Write `text` to `out`, or print it when there is none.
pub fn write(out: Option<&Path>, text: &str) -> Result<()> {
    if let Some(path) = out {
        return std::fs::write(path, text).with_context(|| format!("write {}", path.display()));
    }
    print!("{text}");
    Ok(())
}
