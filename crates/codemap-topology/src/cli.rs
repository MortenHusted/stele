//! `codemap-topology` command line: render a validated snapshot as an offline
//! topology.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::{CheckReport, Selection, project, render_html, render_html_with_source_root};
use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;

/// The inputs and outputs of one topology rendering.
#[derive(clap::Args, Debug)]
pub struct Args {
    #[arg(long)]
    snapshot: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    html: Option<PathBuf>,
    /// Source directory for repository-relative links in the HTML report.
    #[arg(long, requires = "html")]
    source_root: Option<PathBuf>,
    /// JSON from `stele spec check symbols.stele evidence.stele --json`.
    #[arg(long, requires = "selection")]
    liveness: Option<PathBuf>,
    /// Expected fully expanded entry/public API selections; see the README.
    #[arg(long, requires = "liveness")]
    selection: Option<PathBuf>,
}

fn read<T: DeserializeOwned>(path: &PathBuf) -> Result<T> {
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}

fn run(args: Args) -> Result<()> {
    let outputs = std::iter::once(&args.out)
        .chain(args.html.iter())
        .map(|path| destination(path))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        outputs.len() < 2 || outputs[0] != outputs[1],
        "output paths must differ"
    );
    for input in std::iter::once(&args.snapshot)
        .chain(args.liveness.iter())
        .chain(args.selection.iter())
    {
        ensure!(
            !outputs.contains(&input.canonicalize()?),
            "output must not overwrite an input"
        );
    }
    let snapshot = read(&args.snapshot)?;
    let report: Option<CheckReport> = args.liveness.as_ref().map(read).transpose()?;
    let selection: Option<Selection> = args.selection.as_ref().map(read).transpose()?;
    let topology = project(&snapshot, report.as_ref().zip(selection.as_ref()))?;
    let json = serde_json::to_string_pretty(&topology)? + "\n";
    let html = args
        .html
        .as_ref()
        .map(|path| {
            args.source_root.as_ref().map_or_else(
                || render_html(&topology),
                |root| render_html_with_source_root(&topology, root, path),
            )
        })
        .transpose()?;
    std::fs::write(&args.out, json)?;
    if let (Some(path), Some(html)) = (args.html, html) {
        std::fs::write(path, html)?;
    }
    Ok(())
}

fn destination(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(parent
        .canonicalize()?
        .join(path.file_name().context("output requires a filename")?))
}

/// Render one parsed topology command, reporting an error as
/// `stele topology: …`.
pub fn dispatch(args: Args) -> ExitCode {
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("stele topology: {error:#}");
            ExitCode::FAILURE
        }
    }
}
