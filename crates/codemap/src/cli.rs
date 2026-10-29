//! `codemap` CLI — local `extract`, `validate`, and `report` commands.
//!
//! Owns presentation and flags only. Never derives facts, runs a server,
//! or performs transport. All commands are local and write only requested
//! local output.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{ExtractOptions, extract, extract_worktree, to_json};

macro_rules! outputln {
    ($($arg:tt)*) => {
        write_stdout(format_args!($($arg)*))?
    };
}

#[derive(Debug)]
struct StdoutError(std::io::Error);

impl std::fmt::Display for StdoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "write stdout: {}", self.0)
    }
}

impl std::error::Error for StdoutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

fn write_stdout(args: std::fmt::Arguments<'_>) -> Result<()> {
    writeln!(std::io::stdout().lock(), "{args}").map_err(StdoutError)?;
    Ok(())
}

/// Standalone, deterministic, source-addressed Rust code-architecture extractor.
#[derive(Parser, Debug)]
#[command(name = "codemap", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Extract a snapshot from a clean worktree and print canonical JSON.
    Extract {
        /// Path inside the repository to extract.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Extract the working tree as it is, uncommitted changes included.
        /// The snapshot is marked dirty and identified by its own tree hash.
        #[arg(long)]
        dirty: bool,
        /// Require the selected language's semantic call resolver.
        #[arg(long, default_value = "off")]
        calls: CallsMode,
        /// Source language of the snapshot (one language per extraction).
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        /// Operator-owned C++ profile JSON (database, roots, input and runtime manifests).
        #[arg(long)]
        cpp_profile: Option<PathBuf>,
        /// Repository-relative TypeScript project config for required JS/TS calls.
        #[arg(long, requires = "js_ts_environment", conflicts_with = "cpp_profile")]
        js_ts_project: Option<String>,
        /// Prepared JS/TS input, runtime and helper bundle JSON.
        #[arg(long, requires = "js_ts_project", conflicts_with = "cpp_profile")]
        js_ts_environment: Option<PathBuf>,
        /// Explicit captured source-only Python parser configuration JSON.
        #[arg(long, conflicts_with_all = ["cpp_profile", "js_ts_project", "js_ts_environment"])]
        python_parser: Option<PathBuf>,
        /// Captured Python parser/native configuration and explicit compilation selection.
        #[arg(long, conflicts_with_all = ["python_parser", "cpp_profile", "js_ts_project", "js_ts_environment"])]
        python_native: Option<PathBuf>,
        /// Add per-callable cyclomatic complexity and line counts and
        /// per-file line classes; clone groups for Rust, PHP, and Go.
        /// Changes the snapshot id; the default output is untouched.
        #[arg(long)]
        measures: bool,
        /// Also emit twin pairs: callables whose normalised token shingles
        /// overlap past the twin threshold (edited copies the clone
        /// detector cannot see). Needs --measures.
        #[arg(long)]
        twins: bool,
        /// Show elapsed status on terminal/file stderr; suppressed for pipes.
        #[arg(long)]
        progress: bool,
        /// Write JSON to this path instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Validate a previously written snapshot JSON file.
    Validate {
        /// Path to a versioned Codemap snapshot JSON file.
        #[arg(long)]
        file: PathBuf,
    },
    /// Print a human-readable report from a snapshot JSON file.
    Report {
        /// Path to a versioned Codemap snapshot JSON file.
        #[arg(long)]
        file: PathBuf,
    },
    // ── Layer 1 navigation (deterministic graph queries) ──
    /// Find declarations containing path:line[-line] in a pinned snapshot.
    /// Reports structural containment; executable classification is unsupported.
    Locate {
        query: String,
        /// Path to a versioned Codemap snapshot JSON file.
        #[arg(long)]
        file: PathBuf,
        /// Print the structured result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// 360-degree context for a symbol: container, incoming/outgoing edges,
    /// fan-in/fan-out. Loads --file or extracts --path.
    Context {
        query: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Source language when extracting --path; --file carries its own language.
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        #[arg(long, default_value = "off")]
        calls: CallsMode,
    },
    /// Transitive impact (blast radius) from a symbol. --direction
    /// upstream (callers) or downstream (callees). --kinds comma-list.
    Impact {
        query: String,
        #[arg(long, default_value = "downstream")]
        direction: String,
        #[arg(long, default_value_t = 10)]
        depth: u32,
        #[arg(long, default_value = "")]
        kinds: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Source language when extracting --path; --file carries its own language.
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        #[arg(long, default_value = "off")]
        calls: CallsMode,
    },
    /// Shortest path between two symbols along the given edge kinds.
    Trace {
        from: String,
        to: String,
        #[arg(long, default_value = "")]
        kinds: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Source language when extracting --path; --file carries its own language.
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        #[arg(long, default_value = "off")]
        calls: CallsMode,
    },
    /// Search symbols by name/symbol-key substring. Each hit prints its kind,
    /// name, file and symbol key, the key plan anchors and `realized_by` take.
    Search {
        needle: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Source language when extracting --path; --file carries its own language.
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        #[arg(long, default_value = "off")]
        calls: CallsMode,
    },
    /// Containment tree under a file/module path or symbol.
    Structure {
        root: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Source language when extracting --path; --file carries its own language.
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        #[arg(long, default_value = "off")]
        calls: CallsMode,
    },
    /// Strongly-connected components (cycles) over edge kinds (default: calls).
    Cycles {
        #[arg(long, default_value = "calls")]
        kinds: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Source language when extracting --path; --file carries its own language.
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        #[arg(long, default_value = "off")]
        calls: CallsMode,
    },
    /// Cross-commit structural diff: extract two commits and diff the
    /// snapshots. Deterministic — the unique capability from Layer 0.
    Diff {
        /// Base commit-ish (e.g. SHA, HEAD~3, a tag).
        base: String,
        /// Head commit-ish.
        head: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Source language for both commits.
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        /// Also print non-authoritative churn hotspots.
        #[arg(long, default_value_t = 10)]
        top: usize,
        /// Resolve semantic `calls` edges on both sides, so the delta covers
        /// the call graph and not only structure. Materializes each commit in
        /// a throwaway worktree for rust-analyzer.
        #[arg(long, default_value = "off")]
        calls: CallsMode,
    },
    /// Walk commits oldest-first and emit the structural change stream as
    /// NDJSON: what appeared, what disappeared, and what that moved.
    History {
        /// Tip to walk back from (default: HEAD).
        #[arg(long, default_value = "HEAD")]
        tip: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Which commits to extract. `first-parent-source` skips commits that
        /// cannot change structure.
        #[arg(long, default_value = "first-parent-source")]
        selector: SelectorArg,
        /// Walk only the most recent N selected commits.
        #[arg(long)]
        limit: Option<usize>,
        /// Directory for commits/transitions/metrics NDJSON.
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "rust")]
        language: LanguageArg,
        /// Resolve semantic `calls` edges at every commit. Materializes each
        /// commit in a throwaway worktree for rust-analyzer — roughly three
        /// orders of magnitude slower, so pair it with --limit.
        #[arg(long, default_value = "off")]
        calls: CallsMode,
        /// Explicit captured source-only Python parser configuration JSON.
        #[arg(long)]
        python_parser: Option<PathBuf>,
        /// Extract every commit with --measures and keep the measured
        /// snapshot under <out>/measures/, listed in measured.ndjson with
        /// the commit author label. Data for `stele slop series`; the
        /// walk itself computes no metric.
        #[arg(long)]
        measures: bool,
        /// Also keep twin pairs in every measured snapshot; needs --measures.
        #[arg(long)]
        twins: bool,
    },
    // ── Layer 1.5 heuristic analysis (derived, non-authoritative) ──
    /// Rank the "heaviest bits" by coupling × churn. Non-authoritative.
    Hotspots {
        #[arg(long, default_value_t = 15)]
        top: usize,
        /// Optional diff base commit to weight by churn.
        #[arg(long)]
        diff_base: Option<String>,
        /// Optional diff head commit (requires --diff-base).
        #[arg(long)]
        diff_head: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Source language when extracting --path; --file carries its own language.
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        #[arg(long, default_value = "off")]
        calls: CallsMode,
    },
    /// Refactor-opportunity signals: cycles, hubs, god-symbols, orphans, and
    /// (with --diff-base/--diff-head) new cycles + growing hubs. Non-authoritative.
    Refactor {
        #[arg(long, default_value_t = 8)]
        hub_threshold: usize,
        /// Span in bytes above which an unmeasured symbol is a god symbol.
        #[arg(long, default_value_t = 2000)]
        god_size: usize,
        /// Cyclomatic complexity above which a measured callable is a god
        /// symbol (`extract --measures`); the same cutoff `stele slop`
        /// erodes at.
        #[arg(long, default_value_t = 10)]
        god_cc: u32,
        #[arg(long)]
        diff_base: Option<String>,
        #[arg(long)]
        diff_head: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Source language when extracting --path; --file carries its own language.
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        #[arg(long, default_value = "off")]
        calls: CallsMode,
    },
    /// Functional clusters via seeded label propagation. Non-authoritative.
    Clusters {
        #[arg(long, default_value_t = 10)]
        rounds: u32,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Source language when extracting --path; --file carries its own language.
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        #[arg(long, default_value = "off")]
        calls: CallsMode,
    },
    /// Execution-flow processes traced from entry points. Non-authoritative.
    Processes {
        #[arg(long, default_value_t = 50)]
        max_steps: usize,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Source language when extracting --path; --file carries its own language.
        #[arg(long, value_enum, default_value = "rust")]
        language: LanguageArg,
        #[arg(long, default_value = "off")]
        calls: CallsMode,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum CallsMode {
    Off,
    Required,
}

/// Source language of an extraction. One language per snapshot; the
/// validator enforces that every fact lives in the declared namespace.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum LanguageArg {
    Rust,
    Ruby,
    Php,
    Go,
    Cpp,
    JsTs,
    Python,
}

/// Which commits a history walk extracts.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum SelectorArg {
    /// Every commit reachable from the tip.
    All,
    /// Mainline only — a merged branch reads as one change.
    FirstParent,
    /// Mainline commits that touched a source file of this language.
    FirstParentSource,
}

impl From<SelectorArg> for crate::Selector {
    fn from(value: SelectorArg) -> Self {
        match value {
            SelectorArg::All => Self::All,
            SelectorArg::FirstParent => Self::FirstParent,
            SelectorArg::FirstParentSource => Self::FirstParentSource,
        }
    }
}

impl From<LanguageArg> for crate::schema::Language {
    fn from(arg: LanguageArg) -> Self {
        match arg {
            LanguageArg::Rust => Self::Rust,
            LanguageArg::Ruby => Self::Ruby,
            LanguageArg::Php => Self::Php,
            LanguageArg::Go => Self::Go,
            LanguageArg::Cpp => Self::Cpp,
            LanguageArg::JsTs => Self::JsTs,
            LanguageArg::Python => Self::Python,
        }
    }
}

/// Run one parsed command, reporting an error as `stele code: …`.
pub fn dispatch(command: Command) -> ExitCode {
    match run(Cli { command }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e)
            if e.downcast_ref::<StdoutError>()
                .is_some_and(|error| error.0.kind() == std::io::ErrorKind::BrokenPipe) =>
        {
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("stele code: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn progress_output() -> Option<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // fstat initializes this buffer on success.
        if unsafe { libc::fstat(libc::STDERR_FILENO, status.as_mut_ptr()) } != 0 {
            return None;
        }
        // The successful fstat initialized the buffer.
        let status = unsafe { status.assume_init() };
        if status.st_mode & libc::S_IFMT == libc::S_IFREG {
            // Duplication preserves the log file offset and does not change stderr flags.
            let fd = unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_DUPFD_CLOEXEC, 0) };
            if fd >= 0 {
                // fcntl returned a new owned descriptor.
                return Some(unsafe { std::fs::File::from_raw_fd(fd) });
            }
        } else {
            let mut name: [libc::c_char; 1024] = [0; 1024];
            // ttyname_r writes a NUL-terminated path on success.
            if unsafe { libc::ttyname_r(libc::STDERR_FILENO, name.as_mut_ptr(), name.len()) } != 0 {
                return None;
            }
            // ttyname_r succeeded and terminated the buffer.
            let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) };
            let output = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
                .open(std::ffi::OsStr::from_bytes(name.to_bytes()))
                .ok()?;
            let mut actual = std::mem::MaybeUninit::<libc::stat>::uninit();
            // Both calls inspect this owned descriptor without retaining pointers.
            if unsafe { libc::isatty(output.as_raw_fd()) } == 1
                && unsafe { libc::fstat(output.as_raw_fd(), actual.as_mut_ptr()) } == 0
                // The successful fstat initialized actual; reject a changed terminal path.
                && unsafe { actual.assume_init() }.st_rdev == status.st_rdev
            {
                return Some(output);
            }
        }
    }
    None
}

struct ExtractionProgress {
    stop: std::sync::mpsc::Sender<bool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ExtractionProgress {
    fn start() -> Result<Option<Self>> {
        use std::io::Write;
        let Some(mut output) = progress_output() else {
            return Ok(None);
        };
        let started = std::time::Instant::now();
        let (stop, receiver) = std::sync::mpsc::channel();
        let _ = writeln!(output, "stele code: extraction started");
        let worker = std::thread::Builder::new()
            .name("extraction-progress".into())
            .spawn(move || {
                loop {
                    match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            let _ = writeln!(
                                output,
                                "stele code: extraction running ({}s elapsed)",
                                started.elapsed().as_secs()
                            );
                        }
                        Ok(true) => {
                            let _ = writeln!(
                                output,
                                "stele code: extraction completed ({}s elapsed)",
                                started.elapsed().as_secs()
                            );
                            break;
                        }
                        Ok(false) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .context("start extraction progress")?;
        Ok(Some(Self {
            stop,
            worker: Some(worker),
        }))
    }

    fn finish(self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for ExtractionProgress {
    fn drop(&mut self) {
        let _ = self.stop.send(false);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[allow(clippy::too_many_lines)] // Thin CLI dispatcher; command branches delegate domain work.
fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Extract {
            path,
            dirty,
            calls,
            language,
            cpp_profile,
            js_ts_project,
            js_ts_environment,
            python_parser,
            python_native,
            measures,
            twins,
            progress,
            out,
        } => {
            let progress = if progress {
                ExtractionProgress::start()?
            } else {
                None
            };
            let opts = ExtractOptions {
                path,
                calls_required: matches!(calls, CallsMode::Required),
                language: language.into(),
                measures,
                twins,
            };
            let snapshot = if let Some(profile) = cpp_profile {
                anyhow::ensure!(
                    opts.language == crate::schema::Language::Cpp && opts.calls_required,
                    "--cpp-profile requires --language cpp --calls required"
                );
                let config: crate::CppExtractConfig = serde_json::from_slice(
                    &std::fs::read(&profile)
                        .with_context(|| format!("read C++ profile {}", profile.display()))?,
                )
                .context("parse C++ profile")?;
                if dirty {
                    crate::extract_cpp_worktree(&opts, &config)?
                } else {
                    crate::extract_cpp(&opts, &config)?
                }
            } else if let (Some(project), Some(environment)) = (js_ts_project, js_ts_environment) {
                anyhow::ensure!(
                    opts.language == crate::schema::Language::JsTs && opts.calls_required,
                    "--js-ts-project requires --language js-ts --calls required"
                );
                let config: crate::JsTsExtractConfig =
                    serde_json::from_slice(&std::fs::read(&environment).with_context(|| {
                        format!("read JS/TS environment {}", environment.display())
                    })?)
                    .context("parse JS/TS environment")?;
                if dirty {
                    crate::extract_js_ts_worktree(&opts, &project, &config)?
                } else {
                    crate::extract_js_ts(&opts, &project, &config)?
                }
            } else if let Some(config) = python_native {
                extract_python_native_cli(&opts, dirty, &config)?
            } else if opts.language == crate::schema::Language::Python || python_parser.is_some() {
                extract_python_cli(&opts, dirty, python_parser.as_deref())?
            } else if dirty {
                extract_worktree(&opts)?
            } else {
                extract(&opts)?
            };
            let json = to_json(&snapshot)?;
            match out {
                Some(p) => std::fs::write(&p, &json)?,
                None => outputln!("{json}"),
            }
            if let Some(progress) = progress {
                progress.finish();
            }
            Ok(())
        }
        Command::Validate { file } => {
            let bytes = std::fs::read_to_string(&file)?;
            let snapshot: crate::Snapshot = serde_json::from_str(&bytes)?;
            crate::validate(&snapshot)?;
            outputln!(
                "ok: {} nodes, {} edges",
                snapshot.nodes.len(),
                snapshot.edges.len()
            );
            Ok(())
        }
        Command::Report { file } => {
            let bytes = std::fs::read_to_string(&file)?;
            let snapshot: crate::Snapshot = serde_json::from_str(&bytes)?;
            print_report(&snapshot)?;
            Ok(())
        }
        Command::Locate { query, file, json } => run_locate(&query, &file, json),
        Command::Context {
            query,
            file,
            path,
            calls,
            language,
        } => run_context(&query, file.as_deref(), &path, calls, language),
        Command::Impact {
            query,
            direction,
            depth,
            kinds,
            file,
            path,
            calls,
            language,
        } => run_impact(
            &query,
            &direction,
            depth,
            &kinds,
            file.as_deref(),
            &path,
            calls,
            language,
        ),
        Command::Trace {
            from,
            to,
            kinds,
            file,
            path,
            calls,
            language,
        } => run_trace(&from, &to, &kinds, file.as_deref(), &path, calls, language),
        Command::Search {
            needle,
            file,
            path,
            calls,
            language,
        } => run_search(&needle, file.as_deref(), &path, calls, language),
        Command::Structure {
            root,
            file,
            path,
            calls,
            language,
        } => run_structure(root.as_deref(), file.as_deref(), &path, calls, language),
        Command::Cycles {
            kinds,
            file,
            path,
            calls,
            language,
        } => run_cycles(&kinds, file.as_deref(), &path, calls, language),
        Command::Diff {
            base,
            head,
            path,
            top,
            calls,
            language,
        } => run_diff(&base, &head, &path, top, calls, language),
        Command::History {
            tip,
            path,
            selector,
            limit,
            out,
            language,
            calls,
            python_parser,
            measures,
            twins,
        } => run_history(
            &tip,
            &path,
            selector,
            limit,
            &out,
            language,
            calls,
            measures,
            twins,
            python_parser.as_deref(),
        ),
        Command::Hotspots {
            top,
            diff_base,
            diff_head,
            file,
            path,
            calls,
            language,
        } => run_hotspots(
            top,
            diff_base.as_deref(),
            diff_head.as_deref(),
            file.as_deref(),
            &path,
            calls,
            language,
        ),
        Command::Refactor {
            hub_threshold,
            god_size,
            god_cc,
            diff_base,
            diff_head,
            file,
            path,
            calls,
            language,
        } => run_refactor(
            hub_threshold,
            god_size,
            god_cc,
            diff_base.as_deref(),
            diff_head.as_deref(),
            file.as_deref(),
            &path,
            calls,
            language,
        ),
        Command::Clusters {
            rounds,
            file,
            path,
            calls,
            language,
        } => run_clusters(rounds, file.as_deref(), &path, calls, language),
        Command::Processes {
            max_steps,
            file,
            path,
            calls,
            language,
        } => run_processes(max_steps, file.as_deref(), &path, calls, language),
    }
}

// ── Layer 1 navigation dispatch ──

fn run_locate(query: &str, file: &std::path::Path, json: bool) -> Result<()> {
    let snapshot = crate::load_snapshot(file)?;
    let result = codemap_graph::Graph::from_snapshot(&snapshot).locate(query)?;
    if json {
        outputln!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        outputln!("Snapshot: {}", result.snapshot_id);
        outputln!("Location: {}", result.query);
        outputln!("Executable classification: unsupported (declaration spans only)");
        if result.definitions.is_empty() {
            outputln!("Containing definitions: none");
        } else {
            outputln!("Containing definitions:");
            for node in result.definitions {
                let span = node.provenance.span;
                outputln!(
                    "  {} {} [{}] {}:{}:{}-{}:{} (end exclusive; columns from 0)",
                    node.kind,
                    node.name,
                    node.id,
                    node.provenance.rel_path,
                    span.start_line,
                    span.start_col,
                    span.end_line,
                    span.end_col,
                );
            }
        }
    }
    Ok(())
}

/// Load a snapshot from `--file` or extract one from `--path` (with optional
/// semantic calls). The single source of snapshots for every nav command.
fn nav_snapshot(
    file: Option<&std::path::Path>,
    path: &std::path::Path,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<crate::Snapshot> {
    if let Some(f) = file {
        return crate::load_snapshot(f);
    }
    crate::extract(&ExtractOptions {
        path: path.to_path_buf(),
        calls_required: matches!(calls, CallsMode::Required),
        measures: false,
        twins: false,
        language: language.into(),
    })
}

/// Parse a comma-list of edge kinds (e.g. "calls,imports") into a Vec. Empty
/// string means "all kinds".
fn parse_kinds(s: &str) -> Result<Vec<crate::EdgeKind>> {
    use crate::EdgeKind;
    if s.trim().is_empty() {
        return Ok(Vec::new());
    }
    s.split(',')
        .map(|k| match k.trim() {
            "calls" => Ok(EdgeKind::Calls),
            "contains" => Ok(EdgeKind::Contains),
            "imports" => Ok(EdgeKind::Imports),
            "implements" => Ok(EdgeKind::Implements),
            "inherits" => Ok(EdgeKind::Inherits),
            "extends" => Ok(EdgeKind::Extends),
            other => anyhow::bail!("unknown edge kind '{other}'"),
        })
        .collect()
}

/// Resolve a query string to a single node id, printing ambiguity candidates
/// and exiting on failure.
fn resolve_to_id(g: &crate::Graph, query: &str) -> Result<String> {
    use crate::ResolveResult;
    match g.resolve(query) {
        ResolveResult::Unique(n) => Ok(n.id.clone()),
        ResolveResult::Ambiguous(cands) => {
            eprintln!("ambiguous symbol '{query}'; candidates:");
            for n in cands {
                eprintln!("  {}  ({})", n.name, n.symbol_key);
            }
            anyhow::bail!("ambiguous symbol '{query}'");
        }
        ResolveResult::Unknown => anyhow::bail!("no symbol matching '{query}'"),
    }
}

fn run_context(
    query: &str,
    file: Option<&std::path::Path>,
    path: &std::path::Path,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<()> {
    let snap = nav_snapshot(file, path, calls, language)?;
    let g = crate::Graph::from_snapshot(&snap);
    let id = resolve_to_id(&g, query)?;
    let ctx = g.context(&id).expect("resolved node has context");
    outputln!("{}  {}  ({})", ctx.kind, ctx.name, ctx.symbol_key);
    outputln!("file: {}", ctx.rel_path);
    if let Some(c) = &ctx.container {
        outputln!("contained by: {c}");
    }
    outputln!("fan-in: {}  fan-out: {}", ctx.fan_in, ctx.fan_out);
    if !ctx.incoming.is_empty() {
        outputln!("incoming:");
        for (k, from, rel) in &ctx.incoming {
            outputln!("  {k:<10} <- {from}  [{rel}]");
        }
    }
    if !ctx.outgoing.is_empty() {
        outputln!("outgoing:");
        for (k, to, rel) in &ctx.outgoing {
            outputln!("  {k:<10} -> {to}  [{rel}]");
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_impact(
    query: &str,
    direction: &str,
    depth: u32,
    kinds: &str,
    file: Option<&std::path::Path>,
    path: &std::path::Path,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<()> {
    let snap = nav_snapshot(file, path, calls, language)?;
    let g = crate::Graph::from_snapshot(&snap);
    let id = resolve_to_id(&g, query)?;
    let dir = match direction {
        "upstream" => crate::Direction::Upstream,
        _ => crate::Direction::Downstream,
    };
    let ks = parse_kinds(kinds)?;
    let label = if ks.is_empty() {
        "all kinds".to_string()
    } else {
        ks.iter().map(|k| k.tag()).collect::<Vec<_>>().join(",")
    };
    outputln!("impact of {id}  direction={direction}  depth={depth}  kinds={label}");
    let mut by_depth: std::collections::BTreeMap<u32, Vec<&crate::ImpactNode>> =
        std::collections::BTreeMap::new();
    let imp = g.impact(&id, dir, &ks, depth);
    for n in &imp {
        by_depth.entry(n.depth).or_default().push(n);
    }
    for (d, nodes) in &by_depth {
        outputln!("  depth {d}:");
        for n in nodes {
            outputln!("    [{:<9}] {}", n.via.tag(), n.node);
        }
    }
    if imp.is_empty() {
        outputln!("  (no transitive impact)");
    }
    Ok(())
}

fn run_trace(
    from: &str,
    to: &str,
    kinds: &str,
    file: Option<&std::path::Path>,
    path: &std::path::Path,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<()> {
    let snap = nav_snapshot(file, path, calls, language)?;
    let g = crate::Graph::from_snapshot(&snap);
    let from_id = resolve_to_id(&g, from)?;
    let to_id = resolve_to_id(&g, to)?;
    let ks = parse_kinds(kinds)?;
    match g.trace(&from_id, &to_id, &ks) {
        Some(path_nodes) => {
            outputln!("trace ({} hops):", path_nodes.len().saturating_sub(1));
            for (i, n) in path_nodes.iter().enumerate() {
                let arrow = if i == 0 { "" } else { " -> " };
                outputln!("{arrow}{n}");
            }
        }
        None => {
            anyhow::bail!("no path from {from_id} to {to_id}");
        }
    }
    Ok(())
}

fn run_search(
    needle: &str,
    file: Option<&std::path::Path>,
    path: &std::path::Path,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<()> {
    let snap = nav_snapshot(file, path, calls, language)?;
    let g = crate::Graph::from_snapshot(&snap);
    let hits = g.search(needle);
    if hits.is_empty() {
        outputln!("no symbols matching '{needle}'");
        return Ok(());
    }
    for n in hits {
        outputln!(
            "{}  {}  [{}]  {}",
            n.kind,
            n.name,
            n.provenance.rel_path,
            n.symbol_key
        );
    }
    Ok(())
}

fn run_structure(
    root: Option<&str>,
    file: Option<&std::path::Path>,
    path: &std::path::Path,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<()> {
    let snap = nav_snapshot(file, path, calls, language)?;
    let g = crate::Graph::from_snapshot(&snap);
    let root_id = if let Some(r) = &root {
        resolve_to_id(&g, r)?
    } else {
        // Default: print top-level files.
        let files: Vec<_> = g
            .nodes()
            .filter(|n| matches!(n.kind, crate::NodeKind::File))
            .collect();
        for f in files {
            print_subtree(&g, &f.id, 0)?;
        }
        return Ok(());
    };
    print_subtree(&g, &root_id, 0)?;
    Ok(())
}

fn print_subtree(g: &crate::Graph, id: &str, indent: usize) -> Result<()> {
    let node = g.snapshot().nodes.iter().find(|n| n.id == id);
    if let Some(n) = node {
        let pad = "  ".repeat(indent);
        outputln!("{pad}{} {}", n.kind, n.name);
        for child in g.children(id) {
            print_subtree(g, &child.id, indent + 1)?;
        }
    }
    Ok(())
}

fn run_cycles(
    kinds: &str,
    file: Option<&std::path::Path>,
    path: &std::path::Path,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<()> {
    let snap = nav_snapshot(file, path, calls, language)?;
    let g = crate::Graph::from_snapshot(&snap);
    let ks = parse_kinds(kinds)?;
    let cyc = g.cycles(&ks);
    if cyc.is_empty() {
        outputln!(
            "no cycles over [{}]",
            if ks.is_empty() { "all" } else { &kinds }
        );
        return Ok(());
    }
    outputln!(
        "{} cycle(s) over [{}] (refactor candidates):",
        cyc.len(),
        if ks.is_empty() { "all" } else { &kinds }
    );
    for (i, comp) in cyc.iter().enumerate() {
        outputln!("  cycle {} ({} nodes):", i + 1, comp.len());
        for n in comp {
            outputln!("    {n}");
        }
    }
    Ok(())
}

fn run_diff(
    base: &str,
    head: &str,
    path: &std::path::Path,
    top: usize,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<()> {
    let at = |commitish: &str| {
        if matches!(calls, CallsMode::Required) {
            crate::extract_at_commit_semantic(path, commitish, language.into())
        } else {
            crate::extract_at_commit(path, commitish, language.into())
        }
    };
    let base_snap = at(base).with_context(|| format!("extract at {base}"))?;
    let head_snap = at(head).with_context(|| format!("extract at {head}"))?;
    let d = crate::SnapshotDiff::between(&base_snap, &head_snap);
    let s = d.summary();
    outputln!(
        "diff {base}..{head}  +{an} -{rn} ~{cn} nodes  +{ae} -{re} edges",
        an = s.added_nodes,
        rn = s.removed_nodes,
        cn = s.changed_nodes,
        ae = s.added_edges,
        re = s.removed_edges,
    );
    if !d.added_nodes.is_empty() {
        outputln!("added nodes:");
        for n in &d.added_nodes {
            outputln!("  + {}", crate::diff_node_display(n));
        }
    }
    if !d.removed_nodes.is_empty() {
        outputln!("removed nodes:");
        for n in &d.removed_nodes {
            outputln!("  - {}", crate::diff_node_display(n));
        }
    }
    if !d.changed_nodes.is_empty() {
        outputln!("changed nodes:");
        for c in &d.changed_nodes {
            let flags = match (c.span_moved, c.file_hash_changed) {
                (true, true) => "span+hash",
                (true, false) => "span",
                (false, true) => "hash",
                _ => "?",
            };
            outputln!("  ~ {} [{}] ({})", c.name, c.rel_path, flags);
        }
    }
    if !d.added_edges.is_empty() {
        outputln!("added edges:");
        for e in &d.added_edges {
            outputln!("  + {}", crate::diff_edge_display(e));
        }
    }
    if !d.removed_edges.is_empty() {
        outputln!("removed edges:");
        for e in &d.removed_edges {
            outputln!("  - {}", crate::diff_edge_display(e));
        }
    }
    let hot = d.hotspots(top);
    if !hot.is_empty() {
        outputln!("churn hotspots (non-authoritative, top {top}):");
        for h in &hot {
            outputln!(
                "  {}  +{} -{} (total {})",
                h.node,
                h.added_edges,
                h.removed_edges,
                h.total_change
            );
        }
    }
    Ok(())
}

/// Walk history and write the change stream. Progress goes to stderr so the
/// summary on stdout stays pipeable.
#[allow(clippy::too_many_arguments)]
fn run_history(
    tip: &str,
    path: &std::path::Path,
    selector: SelectorArg,
    limit: Option<usize>,
    out: &std::path::Path,
    language: LanguageArg,
    calls: CallsMode,
    measures: bool,
    twins: bool,
    python_parser: Option<&std::path::Path>,
) -> Result<()> {
    #[cfg(not(unix))]
    anyhow::ensure!(
        python_parser.is_none() && !matches!(language, LanguageArg::Python),
        "Python captured parser requires Unix"
    );
    let options = crate::HistoryOptions {
        #[cfg(unix)]
        python_parser: python_parser
            .map(|path| {
                load_python_parser(
                    path,
                    language.into(),
                    matches!(calls, CallsMode::Required),
                    measures,
                    twins,
                )
            })
            .transpose()?,
        repo: path.to_path_buf(),
        tip: tip.to_string(),
        selector: selector.into(),
        limit,
        out_dir: out.to_path_buf(),
        language: language.into(),
        semantic: matches!(calls, CallsMode::Required),
        measures,
        twins,
    };
    let mut progress = std::io::stderr();
    let summary = crate::walk_history(&options, &mut progress)?;
    outputln!(
        "history {} commits  {} transitions  {} metric moves  {}.{:03}s  -> {}",
        summary.commits,
        summary.transitions,
        summary.metric_moves,
        summary.elapsed_ms / 1000,
        summary.elapsed_ms % 1000,
        out.display(),
    );
    Ok(())
}

#[cfg(unix)]
fn load_python_parser(
    path: &std::path::Path,
    language: crate::schema::Language,
    semantic: bool,
    measures: bool,
    twins: bool,
) -> Result<crate::python_parser::ParserConfig> {
    anyhow::ensure!(
        language == crate::schema::Language::Python,
        "--python-parser requires --language python"
    );
    anyhow::ensure!(
        !semantic,
        "Python source-only parser cannot admit required calls or semantic history"
    );
    anyhow::ensure!(
        !measures && !twins,
        "Python measures, clones and twins have no accepted policy"
    );
    load_python_configuration(path, "parser")
}

#[cfg(unix)]
fn load_python_configuration<T: serde::de::DeserializeOwned>(
    path: &std::path::Path,
    kind: &str,
) -> Result<T> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("open Python {kind} configuration {}", path.display()))?;
    let limit = 32 * 1024 * 1024;
    anyhow::ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= limit,
        "Python {kind} configuration must be a bounded regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= limit,
        "Python {kind} configuration exceeds byte budget"
    );
    serde_json::from_slice(&bytes).with_context(|| format!("parse Python {kind} configuration"))
}

fn extract_python_native_cli(
    options: &ExtractOptions,
    dirty: bool,
    path: &std::path::Path,
) -> Result<crate::Snapshot> {
    anyhow::ensure!(
        options.language == crate::schema::Language::Python && options.calls_required,
        "--python-native requires --language python --calls required"
    );
    anyhow::ensure!(
        !options.measures && !options.twins,
        "Python measures, clones and twins have no accepted policy"
    );
    #[cfg(unix)]
    {
        let config = load_python_configuration(path, "native")?;
        if dirty {
            crate::python_semantic::extract_python_native_worktree(options, &config)
        } else {
            crate::python_semantic::extract_python_native(options, &config)
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (dirty, path);
        anyhow::bail!("Python captured native extraction requires Unix")
    }
}

fn extract_python_cli(
    options: &ExtractOptions,
    dirty: bool,
    parser: Option<&std::path::Path>,
) -> Result<crate::Snapshot> {
    #[cfg(unix)]
    {
        let path = parser.context(
            "Python extraction requires --python-parser with an explicit captured configuration",
        )?;
        let config = load_python_parser(
            path,
            options.language,
            options.calls_required,
            options.measures,
            options.twins,
        )?;
        if dirty {
            crate::python_syntax::extract_python_worktree(options, &config)
        } else {
            crate::python_syntax::extract_python(options, &config)
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (options, dirty, parser);
        anyhow::bail!("Python captured parser requires Unix")
    }
}

struct DiffInputs {
    base: crate::Snapshot,
    diff: crate::SnapshotDiff,
}

fn maybe_diff(
    base: Option<&str>,
    head: Option<&str>,
    path: &std::path::Path,
    language: crate::schema::Language,
) -> Result<Option<DiffInputs>> {
    match (base, head) {
        (Some(b), Some(h)) => {
            let base = crate::extract_at_commit(path, b, language)?;
            let head = crate::extract_at_commit(path, h, language)?;
            let diff = crate::SnapshotDiff::between(&base, &head);
            Ok(Some(DiffInputs { base, diff }))
        }
        (None, None) => Ok(None),
        _ => anyhow::bail!("--diff-base and --diff-head must be given together"),
    }
}

fn run_hotspots(
    top: usize,
    diff_base: Option<&str>,
    diff_head: Option<&str>,
    file: Option<&std::path::Path>,
    path: &std::path::Path,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<()> {
    let snap = nav_snapshot(file, path, calls, language)?;
    let g = crate::Graph::from_snapshot(&snap);
    let d = maybe_diff(diff_base, diff_head, path, snap.identity.language)?;
    let hot = crate::hotspots(&g, d.as_ref().map(|inputs| &inputs.diff), top);
    if hot.is_empty() {
        outputln!("no hotspots");
        return Ok(());
    }
    outputln!("hotspots (non-authoritative, top {top}); score = coupling × (churn+1):");
    for h in &hot {
        outputln!(
            "  {} {}  fan-in={} fan-out={} size={}B churn={} score={}",
            h.kind,
            h.node,
            h.fan_in,
            h.fan_out,
            h.size_bytes,
            h.churn,
            h.score
        );
    }
    Ok(())
}

// One argument per command-line flag; the thresholds are the flags.
#[allow(clippy::too_many_arguments)]
fn run_refactor(
    hub_threshold: usize,
    god_size: usize,
    god_cc: u32,
    diff_base: Option<&str>,
    diff_head: Option<&str>,
    file: Option<&std::path::Path>,
    path: &std::path::Path,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<()> {
    let snap = nav_snapshot(file, path, calls, language)?;
    let g = crate::Graph::from_snapshot(&snap);
    let d = maybe_diff(diff_base, diff_head, path, snap.identity.language)?;
    let base_graph = d
        .as_ref()
        .map(|inputs| crate::Graph::from_snapshot(&inputs.base));
    let sigs = crate::refactor_signals(
        &g,
        base_graph.as_ref(),
        d.as_ref().map(|inputs| &inputs.diff),
        hub_threshold,
        god_size,
        god_cc,
    );
    if sigs.is_empty() {
        outputln!("no refactor signals");
        return Ok(());
    }
    outputln!("refactor signals (non-authoritative):");
    for s in &sigs {
        outputln!("  [{:?}] {}  — {}", s.kind, s.node, s.detail);
    }
    Ok(())
}

fn run_clusters(
    rounds: u32,
    file: Option<&std::path::Path>,
    path: &std::path::Path,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<()> {
    let snap = nav_snapshot(file, path, calls, language)?;
    let g = crate::Graph::from_snapshot(&snap);
    let cs = crate::clusters(&g, rounds);
    if cs.is_empty() {
        outputln!("no clusters");
        return Ok(());
    }
    outputln!("clusters (non-authoritative, seeded label propagation):");
    for c in &cs {
        outputln!(
            "  {} ({} members, cohesion {:.2}):",
            c.label,
            c.members.len(),
            c.cohesion
        );
        for m in &c.members {
            outputln!("    {m}");
        }
    }
    Ok(())
}

fn run_processes(
    max_steps: usize,
    file: Option<&std::path::Path>,
    path: &std::path::Path,
    calls: CallsMode,
    language: LanguageArg,
) -> Result<()> {
    let snap = nav_snapshot(file, path, calls, language)?;
    let g = crate::Graph::from_snapshot(&snap);
    let has_calls = snap.edges.iter().any(|e| e.kind == crate::EdgeKind::Calls);
    if !has_calls {
        eprintln!(
            "note: this snapshot has no canonical `calls` edges (syntax-only). \
                  Every function is an 'entry point', so processes are noisy; \
                  re-extract with --calls=required for meaningful entry detection."
        );
    }
    let procs = crate::processes(&g, max_steps);
    if procs.is_empty() {
        outputln!("no processes detected (no symbols with zero in-repo callers)");
        return Ok(());
    }
    outputln!("processes (non-authoritative, traced from entry points):");
    for p in &procs {
        outputln!("  entry {} ({} steps):", p.entry, p.steps.len());
        for s in &p.steps {
            outputln!("    {s}");
        }
    }
    Ok(())
}

fn print_report(s: &crate::Snapshot) -> Result<()> {
    let r = &s.report;
    outputln!("snapshot: {}", s.identity.snapshot_id);
    outputln!(
        "repository: {} (canonical={})",
        s.identity.repository.id,
        s.identity.repository.is_canonical
    );
    outputln!("commit: {}", s.identity.commit_sha);
    outputln!("tree:   {}", s.identity.tree_sha);
    outputln!(
        "schema: {}  extractor: {}",
        s.identity.schema_version,
        s.identity.extractor_version
    );
    outputln!(
        "files:  scanned={} skipped={} parse_errors={}",
        r.files_scanned,
        r.files_skipped,
        r.parse_errors
    );
    outputln!("nodes:");
    for (k, v) in &r.node_counts {
        outputln!("  {k:>12} {v}");
    }
    outputln!("edges:");
    for (k, v) in &r.edge_counts {
        outputln!("  {k:>12} {v}");
    }
    if !r.call_sites_by_category.is_empty() {
        outputln!("call sites (observations, not canonical edges):");
        for (k, v) in &r.call_sites_by_category {
            outputln!("  {k:>12} {v}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpp_profile_requires_explicit_cpp_semantic_mode() {
        for args in [
            vec![
                "codemap",
                "extract",
                "--language",
                "cpp",
                "--cpp-profile",
                "/absent.json",
            ],
            vec![
                "codemap",
                "extract",
                "--calls",
                "required",
                "--cpp-profile",
                "/absent.json",
            ],
        ] {
            let error = run(Cli::try_parse_from(args).unwrap()).unwrap_err();
            assert!(
                format!("{error:#}")
                    .contains("--cpp-profile requires --language cpp --calls required")
            );
        }
    }

    #[test]
    fn js_ts_project_requires_complete_explicit_semantic_selection() {
        for args in [
            vec!["codemap", "extract", "--js-ts-project", "tsconfig.json"],
            vec!["codemap", "extract", "--js-ts-environment", "/absent.json"],
            vec![
                "codemap",
                "extract",
                "--cpp-profile",
                "/cpp.json",
                "--js-ts-project",
                "tsconfig.json",
                "--js-ts-environment",
                "/js.json",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
        for args in [
            vec![
                "codemap",
                "extract",
                "--language",
                "js-ts",
                "--js-ts-project",
                "tsconfig.json",
                "--js-ts-environment",
                "/absent.json",
            ],
            vec![
                "codemap",
                "extract",
                "--calls",
                "required",
                "--js-ts-project",
                "tsconfig.json",
                "--js-ts-environment",
                "/absent.json",
            ],
        ] {
            let error = run(Cli::try_parse_from(args).unwrap()).unwrap_err();
            assert!(
                format!("{error:#}")
                    .contains("--js-ts-project requires --language js-ts --calls required")
            );
        }
    }

    #[test]
    fn parse_kinds_rejects_unknown_tokens() {
        assert!(parse_kinds("calls,unknown").is_err());
        assert_eq!(parse_kinds("").expect("all kinds"), Vec::new());
    }

    #[cfg(unix)]
    #[test]
    fn python_cli_refuses_missing_or_incompatible_parser_selection() {
        for command in ["extract", "history"] {
            let out = tempfile::tempdir().unwrap();
            let output = out.path().join("output");
            let base = vec![
                "codemap",
                command,
                "--language",
                "python",
                "--out",
                output.to_str().unwrap(),
            ];
            let error = run(Cli::try_parse_from(base.clone()).unwrap()).unwrap_err();
            assert!(format!("{error:#}").contains("explicit captured"));
            for (extra, expected) in [
                (
                    vec!["--calls", "required"],
                    "source-only parser cannot admit",
                ),
                (vec!["--measures"], "no accepted policy"),
                (vec!["--twins"], "no accepted policy"),
            ] {
                let mut args = base.clone();
                args.extend(["--python-parser", "/absent.json"]);
                args.extend(extra);
                let error = run(Cli::try_parse_from(args).unwrap()).unwrap_err();
                assert!(format!("{error:#}").contains(expected));
                assert!(!output.exists());
            }
            let error = run(Cli::try_parse_from([
                "codemap",
                command,
                "--language",
                "rust",
                "--python-parser",
                "/absent.json",
                "--out",
                output.to_str().unwrap(),
            ])
            .unwrap())
            .unwrap_err();
            assert!(format!("{error:#}").contains("requires --language python"));
        }
        assert!(
            Cli::try_parse_from([
                "codemap",
                "extract",
                "--python-parser",
                "/python.json",
                "--cpp-profile",
                "/cpp.json"
            ])
            .is_err()
        );
    }

    #[test]
    fn python_native_cli_requires_compatible_explicit_selection() {
        for extra in [
            vec![],
            vec!["--language", "python"],
            vec!["--calls", "required"],
        ] {
            let mut args = vec!["codemap", "extract", "--python-native", "/absent.json"];
            args.extend(extra);
            assert!(
                run(Cli::try_parse_from(args).unwrap())
                    .unwrap_err()
                    .to_string()
                    .contains("--python-native requires --language python --calls required")
            );
        }
        for flag in ["--measures", "--twins"] {
            let args = [
                "codemap",
                "extract",
                "--language",
                "python",
                "--calls",
                "required",
                "--python-native",
                "/absent.json",
                flag,
            ];
            assert!(
                run(Cli::try_parse_from(args).unwrap())
                    .unwrap_err()
                    .to_string()
                    .contains("no accepted policy")
            );
        }
        for extra in [
            vec!["--python-parser", "/parser.json"],
            vec!["--cpp-profile", "/cpp.json"],
            vec![
                "--js-ts-project",
                "tsconfig.json",
                "--js-ts-environment",
                "/js.json",
            ],
        ] {
            let mut args = vec!["codemap", "extract", "--python-native", "/native.json"];
            args.extend(extra);
            assert!(Cli::try_parse_from(args).is_err());
        }
        assert!(
            Cli::try_parse_from(["codemap", "history", "--python-native", "/native.json"]).is_err()
        );
    }
}
