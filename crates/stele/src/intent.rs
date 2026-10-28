//! `stele intent`: check a catalog of intents, record who approved its
//! statements and examples, run the examples, in the ledgers beside the
//! catalog root, report the standing of every statement, and show which
//! statements a code change touches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use codemap::ExtractOptions;
use lemmaspec::SourceContext;
use stele_plan::approvals::{self, Log};
use stele_plan::examples::{self, Example};
use stele_plan::identity::Repo;
use stele_plan::impact::{impact, parse_hunks};
use stele_plan::project::realized_anchors;
use stele_plan::recorder::{self, CargoTest, Selection};
use stele_plan::report::{self, Options};
use stele_plan::{DirReader, INTENT_CHECKER, RUNS_FILE, Statement};

use crate::check::CheckLanguage;
use crate::plan::{Code, languages, locate, work_tree, write};

#[derive(clap::Subcommand, Debug)]
pub enum IntentCommand {
    /// Load the catalog and check every link. Exits 0 when it is valid.
    Check(CatalogArg),
    /// Approve each statement or example as it reads now, with the current
    /// digest of each statement it links to.
    Approve(Approval),
    /// Approve again each statement or example whose own text is unchanged
    /// since its latest approval, with the current digests of the statements
    /// it links to.
    Reconfirm(Approval),
    /// Approve the removal of each approved statement or example. Refused
    /// while another approved item links to one, unless that one is removed
    /// too.
    Remove(Approval),
    /// Run the examples through `cargo test`, each once per channel it is
    /// tagged for with `STELE_CHANNEL` set to that channel, and append one run
    /// each to runs.stele beside the catalog root. Prints `<example> on
    /// <channel>: <outcome>` per run and exits 0 whatever the outcomes.
    Record(Recording),
    /// Report the standing of every intent, definition and exception, what
    /// awaits approval, and the code that realizes each statement. Exits 1
    /// while anything awaits approval or the code lacks a mapped anchor, and 0
    /// otherwise, even when statements are unverified or unmapped.
    Report(Reporting),
    /// Print the shipped intent checker so it can be run without this
    /// binary.
    Checker {
        /// Write here instead of standard output.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Show which statements a code change touches: each statement whose
    /// `realized_by` code changed, with its standing at HEAD's commit and its
    /// examples, then the changed code no statement maps and the examples
    /// whose test changed. Exits 0.
    Impact(Impacting),
}

#[derive(clap::Args, Debug)]
pub struct CatalogArg {
    /// The catalog root. Defaults to intent/catalog.stele at the top of the
    /// git repository holding the current directory.
    #[arg(long)]
    catalog: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct Approval {
    /// The statements and examples, by ID.
    #[arg(required = true)]
    ids: Vec<String>,
    /// Who approves. The ledger records the name as given.
    #[arg(long)]
    by: String,
    #[command(flatten)]
    catalog: CatalogArg,
}

#[derive(clap::Args, Debug)]
pub struct Recording {
    /// Run only this example.
    #[arg(long)]
    example: Option<String>,
    /// Run only on this channel.
    #[arg(long)]
    channel: Option<String>,
    /// Record against this full sha256: source identity instead of HEAD's
    /// commit ID. Must match HEAD's source identity, excluding the ledgers.
    #[arg(long, value_name = "SOURCE")]
    source_baseline: Option<String>,
    #[command(flatten)]
    catalog: CatalogArg,
}

#[derive(clap::Args, Debug)]
pub struct Reporting {
    /// List the approvals and failing runs added since this commit.
    #[arg(long, value_name = "COMMIT")]
    base: Option<String>,
    /// Markdown, or an HTML page of the bound evidence.
    #[arg(long, value_enum, default_value = "md")]
    format: Format,
    /// Write the report here instead of standard output.
    #[arg(short = 'o', long = "out", value_name = "PATH")]
    out: Option<PathBuf>,
    /// Also write the evidence bound to the checker here, which `stele spec
    /// walk` and `render` accept.
    #[arg(long, value_name = "PATH")]
    bound: Option<PathBuf>,
    #[command(flatten)]
    catalog: CatalogArg,
    /// The code `realized_by` anchors are checked against, read only when a
    /// statement names some.
    #[command(flatten)]
    code: Code,
}

#[derive(clap::Args, Debug)]
pub struct Impacting {
    /// The commit the change starts from.
    base: String,
    /// The commit the change ends at. Defaults to the working tree,
    /// uncommitted changes included; untracked files are not part of it.
    head: Option<String>,
    /// A language to extract the code in; repeat it for several. Defaults
    /// to each language a `realized_by` symbol anchor names, or rust when
    /// none does.
    #[arg(long = "language", value_enum)]
    languages: Vec<CheckLanguage>,
    /// Print the impact as JSON.
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    catalog: CatalogArg,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum Format {
    Md,
    Html,
}

/// Run one intent command, reporting an error as `stele intent: …` with exit
/// status 2. Nothing is written when any ID fails, when the source changes
/// while examples run, or when the report cannot be built.
pub fn dispatch(command: IntentCommand) -> ExitCode {
    match run(command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("stele intent: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run(command: IntentCommand) -> Result<ExitCode> {
    match command {
        IntentCommand::Check(catalog) => {
            let (base, root) = catalog.locate()?;
            stele_plan::load_catalog(&DirReader(base), &root).map_err(anyhow::Error::msg)?;
        }
        IntentCommand::Approve(Approval { ids, by, catalog }) => {
            append(&catalog, |_, statements, examples| {
                approvals::approve(statements, examples, &ids, &by)
            })?;
        }
        IntentCommand::Reconfirm(Approval { ids, by, catalog }) => {
            append(&catalog, |log, statements, examples| {
                approvals::reconfirm(log, statements, examples, &ids, &by)
            })?;
        }
        IntentCommand::Remove(Approval { ids, by, catalog }) => {
            append(&catalog, |log, statements, examples| {
                approvals::remove(log, statements, examples, &ids, &by)
            })?;
        }
        IntentCommand::Record(Recording {
            example,
            channel,
            source_baseline,
            catalog,
        }) => record(
            &catalog,
            &Selection { example, channel },
            source_baseline.as_deref(),
        )?,
        IntentCommand::Report(reporting) => return write_report(reporting),
        IntentCommand::Checker { out } => write(out.as_deref(), INTENT_CHECKER)?,
        IntentCommand::Impact(impacting) => show_impact(&impacting)?,
    }
    Ok(ExitCode::SUCCESS)
}

/// Print which statements the change from `base` to `head`, or to the
/// working tree, touches. The catalog and the examples are read from the
/// working tree and the code on each side from a snapshot of it. The
/// standings are taken at HEAD's commit, as `stele plan check` takes them,
/// so an edit in progress leaves committed green runs counting.
fn show_impact(impacting: &Impacting) -> Result<()> {
    let (root, catalog) = impacting.catalog.locate()?;
    let statements =
        stele_plan::load_catalog(&DirReader(root.clone()), &catalog).map_err(anyhow::Error::msg)?;
    let packages = examples::packages(&root).map_err(anyhow::Error::msg)?;
    let examples = examples::scan(&root, &packages, &statements)
        .map_err(anyhow::Error::msg)?
        .examples;
    let languages = languages(&impacting.languages, realized_anchors(&statements))?;
    let mut base = Vec::new();
    let mut head = Vec::new();
    for language in languages {
        base.push(codemap::extract_at_commit(
            &root,
            &impacting.base,
            language,
        )?);
        head.push(match &impacting.head {
            Some(head) => codemap::extract_at_commit(&root, head, language)?,
            None => codemap::extract_worktree(&ExtractOptions {
                path: root.clone(),
                calls_required: false,
                language,
                measures: false,
                twins: false,
            })?,
        });
    }
    let diff = git_diff(&root, &impacting.base, impacting.head.as_deref())?;
    let impact = impact(&statements, &examples, &base, &head, &parse_hunks(&diff));
    // The standings do not depend on which mapped files are present, so the
    // head's snapshots are code enough.
    let head = stele_plan::Code {
        snapshots: head,
        working_tree: None,
    };
    let standings = report::standings(&root, &catalog, Some(&head))
        .map_err(anyhow::Error::msg)?
        .map(|standings| standings.of)
        .unwrap_or_default();
    if impacting.json {
        println!("{:#}", impact.json(&standings));
    } else {
        let compared = impacting.head.as_ref().map_or_else(
            || format!("`{}` to the working tree", impacting.base),
            |head| format!("`{}` to `{head}`", impacting.base),
        );
        print!("{}", impact.markdown(&compared, &standings));
    }
    Ok(())
}

/// The changed lines from `base` to `head`, or to the working tree.
fn git_diff(root: &Path, base: &str, head: Option<&str>) -> Result<String> {
    let mut command = Command::new("git");
    command
        .args(["-c", "core.quotePath=false", "-C"])
        .arg(root)
        .args(["diff", "--unified=0", "--no-color", "--no-ext-diff"])
        .args(["--no-renames", base])
        .args(head)
        .arg("--");
    let output = command.output().context("run git diff")?;
    if !output.status.success() {
        bail!(
            "git diff {base}{} failed: {}",
            head.map_or(String::new(), |head| format!(" {head}")),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Build the whole report, then write it and the bound evidence. Exits 1
/// while anything awaits approval or the code lacks a mapped anchor.
fn write_report(reporting: Reporting) -> Result<ExitCode> {
    let (root, catalog) = reporting.catalog.locate()?;
    let statements =
        stele_plan::load_catalog(&DirReader(root.clone()), &catalog).map_err(anyhow::Error::msg)?;
    let anchors = realized_anchors(&statements);
    let code = if anchors.is_empty() {
        None
    } else {
        let catalog_dir = root.join(&catalog);
        let catalog_dir = catalog_dir.parent().unwrap_or(&root);
        Some(reporting.code.read(&root, catalog_dir, &anchors)?)
    };
    let built = report::report(
        &Options {
            root: root.clone(),
            catalog,
            base: reporting.base,
        },
        code.as_ref(),
    )
    .map_err(anyhow::Error::msg)?;
    let text = match reporting.format {
        Format::Md => built.markdown.clone(),
        Format::Html => {
            // Source links resolve from the directory the report is written to.
            let directory = reporting
                .out
                .as_deref()
                .and_then(Path::parent)
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let context = SourceContext::new(&root, directory).map_err(anyhow::Error::msg)?;
            built.html(Some(&context)).map_err(anyhow::Error::msg)?
        }
    };
    if let Some(path) = &reporting.bound {
        write(Some(path), &lemmaspec::print_artifact(&built.bound))?;
    }
    write(reporting.out.as_deref(), &text)?;
    Ok(if built.pending {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Run the selected examples and print one line per run.
fn record(
    catalog: &CatalogArg,
    selection: &Selection,
    source_baseline: Option<&str>,
) -> Result<()> {
    let (base, root) = catalog.locate()?;
    let statements =
        stele_plan::load_catalog(&DirReader(base.clone()), &root).map_err(anyhow::Error::msg)?;
    let packages = examples::packages(&base).map_err(anyhow::Error::msg)?;
    let examples = examples::scan(&base, &packages, &statements)
        .map_err(anyhow::Error::msg)?
        .examples;
    let root = base.join(root);
    let repo = Repo::of_catalog(root.parent().unwrap_or(&base)).map_err(anyhow::Error::msg)?;
    let baseline = source_baseline
        .map(|identity| recorder::SourceBaseline::new(&repo, identity))
        .transpose()
        .map_err(anyhow::Error::msg)?;
    let source: &dyn recorder::Source = baseline
        .as_ref()
        .map_or(&repo as &dyn recorder::Source, |baseline| baseline);
    let mut runner = CargoTest {
        dir: repo.root.clone(),
    };
    let runs = recorder::record(
        &examples,
        selection,
        &mut runner,
        source,
        &root.with_file_name(RUNS_FILE),
    )
    .map_err(anyhow::Error::msg)?;
    for run in runs {
        println!(
            "{} on {}: {}",
            run.example,
            run.channel,
            run.outcome.as_str()
        );
    }
    Ok(())
}

/// Append the entries `change` returns to the ledger beside the catalog.
fn append(
    catalog: &CatalogArg,
    change: impl FnOnce(&Log, &BTreeMap<String, Statement>, &[Example]) -> Result<String, String>,
) -> Result<()> {
    let (base, root) = catalog.locate()?;
    approvals::append(&base, &root, change).map_err(anyhow::Error::msg)
}

impl CatalogArg {
    /// The directory the catalog's paths resolve from, and the root's path
    /// from there.
    fn locate(&self) -> Result<(PathBuf, String)> {
        if let Some(file) = &self.catalog {
            return locate(file);
        }
        let current = std::env::current_dir().context("read the current directory")?;
        locate(
            &work_tree(&current)
                .unwrap_or(current)
                .join("intent/catalog.stele"),
        )
    }
}
