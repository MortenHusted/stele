//! Ordered orchestration: gather git state, parse every tracked Rust file,
//! assign deterministic IDs, build containment/import edges, resolve
//! implements edges, validate, and assemble the immutable snapshot.
//!
//! The public API is one deep operation — [`extract`] — per the plan's
//! architecture direction. Parser and resolver interfaces remain internal.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::clones;
use crate::cpp_syntax;
use crate::git::{self, GitState};
use crate::go_lsp;
use crate::go_syntax;
use crate::js_ts_syntax;
use crate::lsp::{self, NodeIndex};
use crate::measures;
use crate::php_lsp;
use crate::php_syntax;
use crate::resolve::{
    DefinitionIndex, parent_dir, resolve_go_embed, resolve_implements, resolve_inherits,
    resolve_mixin, resolve_php_heritage, resolve_php_trait_use,
};
use crate::ruby_lex;
use crate::ruby_lsp;
use crate::ruby_syntax;
use crate::rust_analyzer;
use crate::rust_syntax::{self, RawCall, RawDef, RawImport};
use crate::schema::{
    self, CallSite, CallSiteCategory, Edge, EdgeKind, FactSource, FileMeasure, FileRecord,
    Language, Measure, Node, NodeKind, Provenance, Report, Snapshot, SnapshotIdentity, Span,
    ToolVersions, edge_id, node_id, node_id_disambiguated, sort_edges, sort_files, sort_nodes,
    symbol_key,
};
use crate::twins;

type RustTypeTargets = BTreeMap<String, BTreeMap<usize, (String, NodeKind)>>;

/// Extraction options. Kept minimal: the user must not configure language
/// plugins, relation policies, or confidence thresholds at runtime.
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Path inside the repository to extract (the whole repo is scanned
    /// regardless; this only selects the worktree to locate).
    pub path: std::path::PathBuf,
    /// When `true`, require rust-analyzer semantic call resolution. v0
    /// syntax-only extraction leaves this `false`.
    pub calls_required: bool,
    /// Source language of the snapshot. One language per extraction run.
    pub language: Language,
    /// Emit per-callable and per-file measures (`measures`, `file_measures`)
    /// under the language's counting policy. Rust only in this release;
    /// any other language fails closed. Off by default, and the default
    /// output is byte-identical whether or not the field exists.
    pub measures: bool,
    /// Also emit `twin_pairs` (callables whose token shingles overlap past
    /// the twin threshold) under the language's twin policy. Needs
    /// `measures`; fails closed without it. The measured output without
    /// this flag is byte-identical whether or not the field exists.
    pub twins: bool,
}

/// A Go file's embeddings with its import table: `(rel_path, file_hash,
/// embeds, imports)`. Embeddings resolve against the file's own imports.
type GoEmbedObservation = (
    String,
    String,
    Vec<go_syntax::RawEmbed>,
    Vec<(String, String)>,
);

struct PendingCall {
    rel_path: String,
    file_hash: String,
    call: RawCall,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            path: std::path::PathBuf::from("."),
            calls_required: false,
            language: Language::Rust,
            measures: false,
            twins: false,
        }
    }
}

/// Run a complete extraction of the clean working tree and return a
/// validated, immutable snapshot.
#[allow(clippy::too_many_lines)]
pub fn extract(opts: &ExtractOptions) -> Result<Snapshot> {
    let state = git::gather(&opts.path, opts.language).context("git state gathering failed")?;
    // Read from the captured commit rather than the mutable worktree. The
    // clean-worktree checks establish that the commit is the user's intended
    // source; commit-object reads keep every byte pinned even during a long
    // semantic extraction.
    let files = read_snapshot_files(&state)?;
    let snapshot = build_snapshot(
        &state,
        &files,
        opts.calls_required,
        opts.measures,
        opts.twins,
    )?;
    git::verify_unchanged(&state).context("repository changed during extraction")?;
    Ok(snapshot)
}

/// Extract the working tree as it is now, without the clean-worktree gate.
///
/// The inventory is tracked plus untracked, non-ignored files present on
/// disk; bytes and per-file hashes come from disk. `tree_sha` is the content
/// hash of the working tree and `dirty` records whether it differs from
/// `HEAD`, so a dirty snapshot can never be mistaken for the commit's own.
/// Semantic resolution already reads the checkout on disk, so `--calls`
/// works unchanged. The end-of-run check refuses a tree that moved
/// underneath the extraction.
pub fn extract_worktree(opts: &ExtractOptions) -> Result<Snapshot> {
    let state =
        git::gather_worktree(&opts.path, opts.language).context("git state gathering failed")?;
    let files = read_worktree_files(&state)?;
    let snapshot = build_snapshot(
        &state,
        &files,
        opts.calls_required,
        opts.measures,
        opts.twins,
    )?;
    git::verify_unchanged(&state).context("repository changed during extraction")?;
    Ok(snapshot)
}

/// Extract a snapshot pinned to an arbitrary `commitish` (SHA/branch/tag/
/// `HEAD~N`), reading committed blobs from the Git object store — no working
/// checkout, no clean-worktree gate. Syntax-only: semantic call resolution at
/// an arbitrary commit would require checking it out for rust-analyzer; the
/// cross-commit diff is structural, which is the deterministic value.
pub fn extract_at_commit(
    repo_path: &Path,
    commitish: &str,
    language: Language,
) -> Result<Snapshot> {
    extract_at_commit_measured(repo_path, commitish, language, false)
}

/// [`extract_at_commit`] with the `--measures` switch.
///
/// `measures` adds the per-callable and per-file counts under the
/// language's counting policy; `false` is byte-for-byte
/// [`extract_at_commit`].
///
/// # Errors
///
/// Git state or blob reads at `commitish` failing, extraction failing, or
/// `measures` on a language without a counting policy.
pub fn extract_at_commit_measured(
    repo_path: &Path,
    commitish: &str,
    language: Language,
    measures: bool,
) -> Result<Snapshot> {
    extract_at_commit_with(repo_path, commitish, language, measures, false)
}

/// [`extract_at_commit_measured`] with the `--twins` switch as well.
///
/// # Errors
///
/// As [`extract_at_commit_measured`], plus `twins` without `measures`.
pub fn extract_at_commit_with(
    repo_path: &Path,
    commitish: &str,
    language: Language,
    measures: bool,
    twins: bool,
) -> Result<Snapshot> {
    let state = git::gather_at(repo_path, commitish, language)
        .with_context(|| format!("git state at {commitish}"))?;
    let files = read_snapshot_files(&state)?;
    build_snapshot(&state, &files, false, measures, twins)
}

/// Extract a **semantic** snapshot pinned to `commitish` by materializing that
/// commit in a throwaway detached worktree.
///
/// rust-analyzer needs a real checkout — it runs `cargo metadata` against a
/// workspace on disk — so the object-store read path [`extract_at_commit`] uses
/// can never reach semantic mode. A detached worktree is the smallest thing
/// that satisfies it: `git worktree add --detach` writes the tree without
/// touching the user's checkout, `HEAD`, or index, and it is clean by
/// construction, so the usual clean-worktree gate applies unchanged rather than
/// being weakened for this path.
///
/// The worktree is removed even when extraction fails. Snapshot bytes do not
/// depend on where the tree was materialized — that is the property the
/// two-checkout determinism gate already asserts.
pub fn extract_at_commit_semantic(
    repo_path: &Path,
    commitish: &str,
    language: Language,
) -> Result<Snapshot> {
    extract_at_commit_semantic_measured(repo_path, commitish, language, false)
}

/// [`extract_at_commit_semantic`] with the `--measures` switch, see
/// [`extract_at_commit_measured`].
///
/// # Errors
///
/// As [`extract_at_commit_semantic`], plus `measures` on a language
/// without a counting policy.
pub fn extract_at_commit_semantic_measured(
    repo_path: &Path,
    commitish: &str,
    language: Language,
    measures: bool,
) -> Result<Snapshot> {
    extract_at_commit_semantic_with(repo_path, commitish, language, measures, false)
}

/// [`extract_at_commit_semantic_measured`] with the `--twins` switch as well.
///
/// # Errors
///
/// As [`extract_at_commit_semantic_measured`], plus `twins` without `measures`.
pub fn extract_at_commit_semantic_with(
    repo_path: &Path,
    commitish: &str,
    language: Language,
    measures: bool,
    twins: bool,
) -> Result<Snapshot> {
    if language == Language::Cpp {
        bail!(
            "C++ semantic commit extraction is unsupported: a current compilation profile cannot attest another revision"
        );
    }
    if language == Language::JsTs {
        bail!(
            "JS/TS semantic commit extraction is unsupported: current prepared inputs cannot attest another revision"
        );
    }
    let root = git::repo_root(repo_path)?;
    let root = Path::new(&root);
    let (commit_sha, _) = git::commit_identity(root, commitish)?;

    // Named for the commit and this process, so concurrent walks cannot collide
    // and a leaked directory is traceable to what made it.
    let worktree = std::env::temp_dir().join(format!(
        "codemap-worktree-{}-{}",
        &commit_sha[..12.min(commit_sha.len())],
        std::process::id()
    ));
    git::add_detached_worktree(root, &worktree, &commit_sha)?;

    let snapshot = extract(&ExtractOptions {
        path: worktree.clone(),
        calls_required: true,
        measures,
        twins,
        language,
    });

    // Remove before propagating: a failed extraction must not leak a worktree,
    // and a failed cleanup must not mask why the extraction failed.
    if let Err(error) = git::remove_worktree(root, &worktree) {
        if snapshot.is_ok() {
            return Err(error);
        }
        eprintln!(
            "warning: could not remove {}: {error:#}",
            worktree.display()
        );
    }
    snapshot.with_context(|| format!("semantic extraction at {commitish}"))
}

/// Read every inventoried Rust file from the exact captured commit. A missing
/// or unreadable blob invalidates the snapshot; exact extraction never skips a
/// tracked source file.
pub fn read_snapshot_files(state: &GitState) -> Result<Vec<(String, Vec<u8>)>> {
    // No added context: `read_files_at` already names the exact commit and
    // path it could not read, and wrapping would bury that behind a summary.
    let blobs = git::read_files_at(
        Path::new(&state.repo_root),
        &state.commit_sha,
        &state.source_files,
    )?;
    Ok(state.source_files.iter().cloned().zip(blobs).collect())
}

/// Read every inventoried file from the working tree on disk. A missing or
/// unreadable file invalidates the snapshot, exactly as a missing blob does.
pub fn read_worktree_files(state: &GitState) -> Result<Vec<(String, Vec<u8>)>> {
    let root = Path::new(&state.repo_root);
    state
        .source_files
        .iter()
        .map(|rel_path| {
            let bytes = std::fs::read(root.join(rel_path))
                .with_context(|| format!("read working-tree file {rel_path}"))?;
            Ok((rel_path.clone(), bytes))
        })
        .collect()
}

/// Everything `--measures` collects while files are read, and what clone
/// detection needs once every file has been: token streams and line
/// classes per file, and each measured callable's lines.
#[derive(Default)]
struct MeasureSink {
    callable_measures: Vec<Measure>,
    file_measures: Vec<FileMeasure>,
    token_streams: Vec<(String, Vec<clones::Token>)>,
    line_classes: BTreeMap<String, Vec<measures::LineClass>>,
    callable_lines: Vec<(String, u32, u32)>,
}

impl MeasureSink {
    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        rel_path: &str,
        file_node_id: &str,
        defs: &[RawDef],
        def_ids: &[String],
        raw_measures: &[measures::RawMeasure],
        file_lines: measures::LineCounts,
        line_classes: &[measures::LineClass],
        tokens: &[clones::Token],
    ) {
        for measure in raw_measures {
            let span = defs[measure.def].span;
            self.callable_measures.push(Measure {
                node_id: def_ids[measure.def].clone(),
                cc: measure.cc,
                sloc: measure.lines.sloc,
                loc: measure.lines.loc,
                clone_group_ids: Vec::new(),
                params: measure.form.map(|form| form.params),
                nesting: measure.form.map(|form| form.nesting),
                chain: measure.form.map(|form| form.chain),
                ivars: measure.form.and_then(|form| form.ivars),
            });
            self.callable_lines
                .push((rel_path.to_string(), span.start_line, span.end_line));
        }
        self.file_measures.push(FileMeasure {
            file_id: file_node_id.to_string(),
            loc: file_lines.loc,
            sloc: file_lines.sloc,
            blank: file_lines.blank,
            comment: file_lines.comment,
            clone_sloc: 0,
        });
        self.token_streams
            .push((rel_path.to_string(), tokens.to_vec()));
        self.line_classes
            .insert(rel_path.to_string(), line_classes.to_vec());
    }

    /// Run clone detection over every recorded file and attach the result:
    /// group ids onto the callables an occurrence overlaps, the union of
    /// clone lines onto each file. Returns the groups.
    fn attach_clones(&mut self) -> Vec<schema::CloneGroup> {
        let attached = clones::attach(
            &self.token_streams,
            &self.line_classes,
            &self.callable_lines,
        );
        for (measure, ids) in self
            .callable_measures
            .iter_mut()
            .zip(attached.callable_groups)
        {
            measure.clone_group_ids = ids;
        }
        for (file, (rel_path, _)) in self.file_measures.iter_mut().zip(&self.token_streams) {
            file.clone_sloc = attached
                .file_clone_sloc
                .get(rel_path.as_str())
                .copied()
                .unwrap_or(0);
        }
        attached.groups
    }
}

/// Build, validate, and return a snapshot from a git state and pre-read file
/// contents. Shared by the working-tree and commit-pinned extraction paths.
#[allow(clippy::too_many_lines)]
pub fn build_snapshot(
    state: &GitState,
    files_with_bytes: &[(String, Vec<u8>)],
    calls_required: bool,
    measures: bool,
    twins: bool,
) -> Result<Snapshot> {
    // Twins are found over the measured token streams, so they exist only
    // with measures; asking for one without the other is refused rather
    // than quietly measuring more than was asked.
    if twins && !measures {
        bail!("--twins needs --measures: twin pairs are found over the measured token streams");
    }
    // Measures exist only under a counting policy the language defines.
    // Without one there is nothing honest to emit, so the run refuses
    // rather than producing zeros or a guess.
    let measures_policy = match (measures, schema::measures_policy(state.language)) {
        (false, _) => None,
        (true, Some(policy)) => Some(policy),
        (true, None) => bail!(
            "--measures: {} has no counting policy yet, so \
             codemap emits no measures for it rather than guessing. Re-run without --measures.",
            state.language.tag()
        ),
    };
    // Semantic call resolution is language-specific: rust-analyzer for
    // Rust, ruby-lsp for Ruby. A language without a language-aware
    // resolver never guesses. Detection happens up front and fails closed;
    // codemap never silently falls back to name-only call edges.
    let workspace_root = Path::new(&state.repo_root);
    let rust_semantic: Option<(String, rust_analyzer::RustcIdentity, String)> =
        if calls_required && state.language == Language::Rust {
            match rust_analyzer::detect_at(workspace_root) {
                rust_analyzer::Availability::Present(v) => Some(v),
                rust_analyzer::Availability::Absent => {
                    bail!(
                        "rust-analyzer is not available for the repository's toolchain, \
                         and call edges are never guessed from names. Install it \
                         (`rustup component add rust-analyzer` in the repository), or \
                         extract without `--calls=required` where a syntax-only \
                         snapshot is enough."
                    );
                }
            }
            .map(|ra| {
                let rustc = rust_analyzer::companion_rustc(workspace_root)?;
                let cargo = rust_analyzer::companion_cargo(workspace_root)?;
                Ok::<_, anyhow::Error>((ra, rustc, cargo))
            })
            .transpose()?
        } else {
            None
        };
    let ruby_semantic: Option<ruby_lsp::RubyToolIdentity> = if calls_required
        && state.language == Language::Ruby
    {
        match ruby_lsp::detect() {
            ruby_lsp::Availability::Present(bundle) => Some(ruby_lsp::capture_identity(&bundle)?),
            ruby_lsp::Availability::Unavailable(reason) => {
                bail!("ruby-lsp tool bundle is unavailable: {reason}");
            }
            ruby_lsp::Availability::Absent => {
                bail!(
                    "no ruby-lsp tool bundle is available, and call edges are never \
                         guessed from names. Set CODEMAP_RUBY_TOOL_BUNDLE to a Gemfile \
                         whose lockfile matches Stele's canonical \
                         toolchain/ruby/Gemfile.lock (installed once by the operator), \
                         or extract without `--calls=required` where a syntax-only \
                         snapshot is enough."
                );
            }
        }
    } else {
        None
    };
    let php_semantic: Option<php_lsp::PhpToolIdentity> =
        if calls_required && state.language == Language::Php {
            match php_lsp::detect() {
                php_lsp::Availability::Present(bundle) => Some(php_lsp::capture_identity(&bundle)?),
                php_lsp::Availability::Absent => {
                    bail!(
                        "no phpactor tool bundle is available, and call edges are \
                     never guessed from names. Set CODEMAP_PHP_TOOL_BUNDLE to a \
                     directory containing a composer.lock that matches Stele's \
                     canonical toolchain/php/composer.lock (installed once by the \
                     operator), or extract without `--calls=required` where a \
                     syntax-only snapshot is enough."
                    );
                }
            }
        } else {
            None
        };
    let go_semantic: Option<go_lsp::GoToolIdentity> =
        if calls_required && state.language == Language::Go {
            match go_lsp::detect() {
                go_lsp::Availability::Present(bundle) => Some(go_lsp::capture_identity(&bundle)?),
                go_lsp::Availability::Absent => {
                    bail!(
                        "no gopls tool bundle is available, and call edges are never \
                     guessed from names. Set CODEMAP_GO_TOOL_BUNDLE to a directory \
                     whose go.sum matches Stele's canonical toolchain/go/go.sum and \
                     whose bin/gopls the operator installed once \
                     (`cd toolchain/go && GOBIN=$PWD/bin go install tool`), or \
                     extract without `--calls=required` where a syntax-only \
                     snapshot is enough."
                    );
                }
            }
        } else {
            None
        };

    let tools = match (&rust_semantic, &ruby_semantic, &php_semantic, &go_semantic) {
        (Some((ra, rustc, cargo)), None, None, None) => ToolVersions::semantic(
            ra.clone(),
            rustc.version.clone(),
            cargo.clone(),
            rustc.host_triple.clone(),
            state.cargo_lock_sha256.clone(),
        ),
        (None, Some(ruby), None, None) => ToolVersions::semantic_ruby(
            ruby.ruby_lsp.clone(),
            ruby.ruby.clone(),
            ruby.rbs.clone(),
            ruby_lsp::canonical_lockfile_sha256(),
        ),
        (None, None, Some(php), None) => ToolVersions::semantic_php(
            php.phpactor.clone(),
            php.php.clone(),
            php_lsp::canonical_lockfile_sha256(),
        ),
        (None, None, None, Some(go)) => ToolVersions::semantic_go(
            go.gopls.clone(),
            go.go.clone(),
            go_lsp::canonical_lockfile_sha256(),
        ),
        (None, None, None, None) => match state.language {
            Language::Rust => ToolVersions::syntax(),
            Language::Ruby => ToolVersions::syntax_ruby(),
            Language::Php => ToolVersions::syntax_php(),
            Language::Go => ToolVersions::syntax_go(),
            Language::Cpp => ToolVersions::syntax_cpp(),
            Language::JsTs => ToolVersions::syntax_js_ts(),
            Language::Python => ToolVersions::syntax_python(),
        },
        _ => unreachable!("one language per extraction"),
    };
    let identity = SnapshotIdentity::build(
        state.language,
        state.identity.clone(),
        state.commit_sha.clone(),
        state.tree_sha.clone(),
        tools,
        state.dirty,
    );
    let identity = match measures_policy {
        Some(policy) => identity.with_measures_policy(policy),
        None => identity,
    };
    let identity = if twins {
        identity.with_twins_policy(
            schema::twins_policy(state.language)
                .expect("twins are only emitted for a language with a twin policy"),
        )
    } else {
        identity
    };
    let mut sink = MeasureSink::default();

    let mut files: Vec<FileRecord> = Vec::with_capacity(files_with_bytes.len());
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut parse_errors = 0usize;
    let mut parse_error_files = BTreeMap::new();
    let mut first_parse_errors = BTreeMap::new();
    let files_skipped = 0usize;
    let mut impl_obs: Vec<(String, String, rust_syntax::RawImpl)> = Vec::new();
    let mut mixin_obs: Vec<(String, String, ruby_syntax::RawMixin)> = Vec::new();
    let mut inherit_obs: Vec<(String, String, ruby_syntax::RawInherit)> = Vec::new();
    let mut php_heritage_obs: Vec<(String, String, php_syntax::RawHeritage)> = Vec::new();
    let mut php_trait_obs: Vec<(String, String, php_syntax::RawTraitUse)> = Vec::new();
    let mut go_embed_obs: Vec<GoEmbedObservation> = Vec::new();
    let mut raw_calls: Vec<PendingCall> = Vec::new();
    let mut callable_targets: BTreeMap<String, BTreeMap<usize, String>> = BTreeMap::new();
    let mut type_targets = RustTypeTargets::new();
    let mut observations: BTreeMap<String, usize> = BTreeMap::new();
    let cpp_index = if state.language == Language::Cpp {
        cpp_syntax::ScopeIndex::build(files_with_bytes)?
    } else {
        cpp_syntax::ScopeIndex::default()
    };
    // PHP methods declared `static`, by node id: the only targets a
    // constant receiver may reach.
    let mut php_static_methods: HashSet<String> = HashSet::new();
    let file_sources: BTreeMap<&str, &[u8]> = if calls_required {
        files_with_bytes
            .iter()
            .map(|(rel_path, bytes)| (rel_path.as_str(), bytes.as_slice()))
            .collect()
    } else {
        BTreeMap::new()
    };

    if state.language == Language::Cpp && calls_required {
        bail!(
            "C++ --calls=required needs an explicit --cpp-profile with its compilation database, input and runtime manifests. Re-run without --calls for syntax extraction."
        );
    }

    if state.language == Language::JsTs && calls_required {
        bail!("JS/TS required calls need an explicit --js-ts-project and --js-ts-environment");
    }

    for (rel_path, bytes) in files_with_bytes {
        let file_hash = schema::sha256_hex(bytes);
        files.push(FileRecord {
            rel_path: rel_path.clone(),
            sha256: file_hash.clone(),
            byte_len: bytes.len(),
        });

        let file_node = build_file_node(rel_path, bytes, &file_hash, &identity);
        let file_node_id = file_node.id.clone();
        nodes.push(file_node);

        match state.language {
            Language::Python => {
                bail!("Python extraction requires an explicit captured parser configuration")
            }
            Language::Rust => {
                let raw = rust_syntax::extract_file(rel_path, bytes)
                    .with_context(|| format!("extraction failed for {rel_path}"))?;
                parse_errors += raw.parse_errors;
                if raw.parse_errors > 0 {
                    parse_error_files.insert(rel_path.clone(), raw.parse_errors);
                }
                if let Some(span) = raw.first_parse_error {
                    first_parse_errors.insert(rel_path.clone(), span);
                }
                let def_ids = assign_and_push(
                    &raw.defs,
                    &raw.imports,
                    &identity,
                    &file_hash,
                    &mut nodes,
                    &mut edges,
                    &file_node_id,
                    rel_path,
                );
                for &(def, name_start) in &raw.type_declarations {
                    type_targets
                        .entry(rel_path.clone())
                        .or_default()
                        .insert(name_start, (def_ids[def].clone(), raw.defs[def].kind));
                }
                for &(def, name_start) in &raw.callable_declarations {
                    callable_targets
                        .entry(rel_path.clone())
                        .or_default()
                        .insert(name_start, def_ids[def].clone());
                }
                if measures {
                    sink.record(
                        rel_path,
                        &file_node_id,
                        &raw.defs,
                        &def_ids,
                        &raw.measures,
                        raw.file_lines,
                        &raw.line_classes,
                        &raw.tokens,
                    );
                }
                for c in &raw.calls {
                    raw_calls.push(PendingCall {
                        rel_path: rel_path.clone(),
                        file_hash: file_hash.clone(),
                        call: c.clone(),
                    });
                }
                for imp in &raw.impls {
                    impl_obs.push((rel_path.clone(), file_hash.clone(), imp.clone()));
                }
            }
            Language::Cpp => {
                let raw = cpp_syntax::extract_file(rel_path, bytes, &cpp_index)
                    .with_context(|| format!("extraction failed for {rel_path}"))?;
                parse_errors += raw.parse_errors;
                let def_ids = assign_and_push(
                    &raw.defs,
                    &raw.imports,
                    &identity,
                    &file_hash,
                    &mut nodes,
                    &mut edges,
                    &file_node_id,
                    rel_path,
                );
                for call in &raw.calls {
                    raw_calls.push(PendingCall {
                        rel_path: rel_path.clone(),
                        file_hash: file_hash.clone(),
                        call: call.clone(),
                    });
                }
                for link in &raw.inherits {
                    let from = &def_ids[link.child];
                    let to = &def_ids[link.base];
                    let prov = provenance(&identity, rel_path, link.span, &file_hash);
                    edges.push(schema::Edge {
                        id: schema::edge_id(
                            schema::EdgeKind::Inherits,
                            from,
                            to,
                            link.span.start_byte,
                        ),
                        kind: schema::EdgeKind::Inherits,
                        from: from.clone(),
                        to: to.clone(),
                        provenance: prov,
                    });
                }
                for (kind, count) in raw.observations {
                    *observations.entry(kind.to_string()).or_default() += count;
                }
            }
            Language::JsTs => {
                let raw = js_ts_syntax::extract_file(rel_path, bytes)
                    .with_context(|| format!("extraction failed for {rel_path}"))?;
                parse_errors += raw.parse_errors;
                assign_and_push(
                    &raw.defs,
                    &raw.imports,
                    &identity,
                    &file_hash,
                    &mut nodes,
                    &mut edges,
                    &file_node_id,
                    rel_path,
                );
                for call in raw.calls {
                    raw_calls.push(PendingCall {
                        rel_path: rel_path.clone(),
                        file_hash: file_hash.clone(),
                        call,
                    });
                }
                for (kind, count) in raw.observations {
                    *observations.entry(kind.to_owned()).or_default() += count;
                }
            }
            Language::Ruby => {
                let raw = ruby_syntax::extract_file(rel_path, bytes)
                    .with_context(|| format!("extraction failed for {rel_path}"))?;
                parse_errors += raw.parse_errors;
                let def_ids = assign_and_push(
                    &raw.defs,
                    &raw.imports,
                    &identity,
                    &file_hash,
                    &mut nodes,
                    &mut edges,
                    &file_node_id,
                    rel_path,
                );
                if measures {
                    // Prism lexes the file a second time here; the token
                    // stream is only needed for clone detection.
                    let tokens = ruby_lex::token_stream(bytes);
                    sink.record(
                        rel_path,
                        &file_node_id,
                        &raw.defs,
                        &def_ids,
                        &raw.measures,
                        raw.file_lines,
                        &raw.line_classes,
                        &tokens,
                    );
                }
                for c in &raw.calls {
                    raw_calls.push(PendingCall {
                        rel_path: rel_path.clone(),
                        file_hash: file_hash.clone(),
                        call: c.clone(),
                    });
                }
                for m in &raw.mixins {
                    mixin_obs.push((rel_path.clone(), file_hash.clone(), m.clone()));
                }
                for i in &raw.inherits {
                    inherit_obs.push((rel_path.clone(), file_hash.clone(), i.clone()));
                }
            }
            Language::Php => {
                let raw = php_syntax::extract_file(rel_path, bytes)
                    .with_context(|| format!("extraction failed for {rel_path}"))?;
                parse_errors += raw.parse_errors;
                let def_ids = assign_and_push(
                    &raw.defs,
                    &raw.imports,
                    &identity,
                    &file_hash,
                    &mut nodes,
                    &mut edges,
                    &file_node_id,
                    rel_path,
                );
                php_static_methods.extend(raw.static_methods.iter().map(|&i| def_ids[i].clone()));
                if measures {
                    sink.record(
                        rel_path,
                        &file_node_id,
                        &raw.defs,
                        &def_ids,
                        &raw.measures,
                        raw.file_lines,
                        &raw.line_classes,
                        &raw.tokens,
                    );
                }
                for c in &raw.calls {
                    raw_calls.push(PendingCall {
                        rel_path: rel_path.clone(),
                        file_hash: file_hash.clone(),
                        call: c.clone(),
                    });
                }
                for h in &raw.heritage {
                    php_heritage_obs.push((rel_path.clone(), file_hash.clone(), h.clone()));
                }
                for t in &raw.trait_uses {
                    php_trait_obs.push((rel_path.clone(), file_hash.clone(), t.clone()));
                }
            }
            Language::Go => {
                let raw = go_syntax::extract_file(rel_path, bytes)
                    .with_context(|| format!("extraction failed for {rel_path}"))?;
                parse_errors += raw.parse_errors;
                let def_ids = assign_and_push(
                    &raw.defs,
                    &raw.imports,
                    &identity,
                    &file_hash,
                    &mut nodes,
                    &mut edges,
                    &file_node_id,
                    rel_path,
                );
                for &(def, name_start) in &raw.callable_declarations {
                    callable_targets
                        .entry(rel_path.clone())
                        .or_default()
                        .insert(name_start, def_ids[def].clone());
                }
                if measures {
                    sink.record(
                        rel_path,
                        &file_node_id,
                        &raw.defs,
                        &def_ids,
                        &raw.measures,
                        raw.file_lines,
                        &raw.line_classes,
                        &raw.tokens,
                    );
                }
                for c in &raw.calls {
                    raw_calls.push(PendingCall {
                        rel_path: rel_path.clone(),
                        file_hash: file_hash.clone(),
                        call: c.clone(),
                    });
                }
                if !raw.embeds.is_empty() {
                    go_embed_obs.push((
                        rel_path.clone(),
                        file_hash.clone(),
                        raw.embeds.clone(),
                        raw.package_imports.clone(),
                    ));
                }
            }
        }
    }
    deduplicate_pending_calls(&mut raw_calls);
    let _ = files_skipped; // skipped files are simply absent from the input vec

    let index = DefinitionIndex::from_nodes(&nodes);
    let mut call_sites: Vec<schema::CallSite> = Vec::new();
    for (rel_path, file_hash, imp) in &impl_obs {
        if calls_required && imp.trait_name.is_some() {
            continue;
        }
        let prov = provenance(&identity, rel_path, imp.span, file_hash);
        match resolve_implements(imp, &index, &prov) {
            Some(edge) => edges.push(edge),
            None => call_sites.push(schema::CallSite {
                rel_path: rel_path.clone(),
                span: imp.span,
                raw_name: imp
                    .trait_name
                    .clone()
                    .unwrap_or_else(|| format!("impl {}", imp.type_name)),
                category: schema::CallSiteCategory::Unmapped,
            }),
        }
    }

    for (rel_path, file_hash, mixin) in &mixin_obs {
        let prov = provenance(&identity, rel_path, mixin.span, file_hash);
        match resolve_mixin(mixin, &index, &prov) {
            Some(edge) => edges.push(edge),
            None => call_sites.push(schema::CallSite {
                rel_path: rel_path.clone(),
                span: mixin.span,
                raw_name: format!("{} {}", mixin.mixin.report_key(), mixin.target),
                category: schema::CallSiteCategory::Unmapped,
            }),
        }
    }
    for (rel_path, file_hash, inherit) in &inherit_obs {
        let prov = provenance(&identity, rel_path, inherit.span, file_hash);
        match resolve_inherits(inherit, &index, &prov) {
            Some(edge) => edges.push(edge),
            None => call_sites.push(schema::CallSite {
                rel_path: rel_path.clone(),
                span: inherit.span,
                raw_name: inherit.superclass.clone(),
                category: schema::CallSiteCategory::Unmapped,
            }),
        }
    }

    for (rel_path, file_hash, heritage) in &php_heritage_obs {
        let prov = provenance(&identity, rel_path, heritage.span, file_hash);
        match resolve_php_heritage(heritage, &index, &prov) {
            Some(edge) => edges.push(edge),
            None => call_sites.push(schema::CallSite {
                rel_path: rel_path.clone(),
                span: heritage.span,
                raw_name: format!(
                    "{} {}",
                    match heritage.kind {
                        EdgeKind::Inherits => "extends",
                        _ => "implements",
                    },
                    heritage.target
                ),
                category: schema::CallSiteCategory::Unmapped,
            }),
        }
    }

    for (rel_path, file_hash, trait_use) in &php_trait_obs {
        let prov = provenance(&identity, rel_path, trait_use.span, file_hash);
        match resolve_php_trait_use(trait_use, &index, &prov) {
            Some(edge) => edges.push(edge),
            None => call_sites.push(schema::CallSite {
                rel_path: rel_path.clone(),
                span: trait_use.span,
                raw_name: format!("use {}", trait_use.trait_name),
                category: schema::CallSiteCategory::Unmapped,
            }),
        }
    }

    let go_module = if go_embed_obs.is_empty() {
        None
    } else {
        go_module_path(state)?
    };
    for (rel_path, file_hash, embeds, package_imports) in &go_embed_obs {
        for embed in embeds {
            let prov = provenance(&identity, rel_path, embed.span, file_hash);
            match resolve_go_embed(
                embed,
                parent_dir(rel_path),
                package_imports,
                go_module.as_deref(),
                &index,
                &prov,
            ) {
                Some(edge) => edges.push(edge),
                None => call_sites.push(schema::CallSite {
                    rel_path: rel_path.clone(),
                    span: embed.span,
                    raw_name: format!(
                        "embeds {}",
                        embed.target.package.as_ref().map_or_else(
                            || embed.target.name.clone(),
                            |package| format!("{package}.{}", embed.target.name)
                        )
                    ),
                    category: schema::CallSiteCategory::Unmapped,
                }),
            }
        }
    }

    if calls_required {
        match state.language {
            Language::Python => bail!("Python calls require an explicit captured native profile"),
            Language::Rust => resolve_calls_semantic(
                state,
                &identity,
                &nodes,
                &callable_targets,
                &type_targets,
                &impl_obs,
                &raw_calls,
                &file_sources,
                &mut edges,
                &mut call_sites,
            )?,
            Language::Ruby => {
                let ruby = ruby_semantic
                    .as_ref()
                    .context("ruby semantic identity lost before call resolution")?;
                resolve_calls_semantic_ruby(
                    state,
                    &identity,
                    ruby,
                    &nodes,
                    &index,
                    &raw_calls,
                    &file_sources,
                    &mut edges,
                    &mut call_sites,
                )?;
            }
            Language::Php => {
                let php = php_semantic
                    .as_ref()
                    .context("php semantic identity lost before call resolution")?;
                resolve_calls_semantic_php(
                    state,
                    &identity,
                    php,
                    &nodes,
                    &index,
                    &raw_calls,
                    &php_static_methods,
                    &file_sources,
                    &mut edges,
                    &mut call_sites,
                )?;
            }
            Language::Go => {
                let go = go_semantic
                    .as_ref()
                    .context("go semantic identity lost before call resolution")?;
                resolve_calls_semantic_go(
                    state,
                    &identity,
                    go,
                    &nodes,
                    &callable_targets,
                    &raw_calls,
                    &file_sources,
                    &mut edges,
                    &mut call_sites,
                )?;
            }
            Language::JsTs => bail!("JS/TS required calls need an explicit prepared environment"),
            Language::Cpp => bail!(
                "C++ --calls=required needs an explicit --cpp-profile with its compilation database, input and runtime manifests. Re-run without --calls for syntax extraction."
            ),
        }
    } else {
        for pending in &raw_calls {
            call_sites.push(schema::CallSite {
                rel_path: pending.rel_path.clone(),
                span: pending.call.span,
                raw_name: pending.call.raw_name.clone(),
                category: schema::CallSiteCategory::Observed,
            });
        }
    }
    sort_files(&mut files);
    sort_nodes(&mut nodes);
    sort_edges(&mut edges);
    call_sites.sort_by(|a, b| {
        a.rel_path
            .cmp(&b.rel_path)
            .then(a.span.start_byte.cmp(&b.span.start_byte))
    });

    // Clones are found over every file at once, so they attach after the
    // file loop: group ids onto the callables an occurrence overlaps, the
    // union of clone lines onto each file.
    let clone_groups = if measures && crate::schema::clone_measures_available(state.language) {
        sink.attach_clones()
    } else {
        Vec::new()
    };
    // Twins read the same token streams once every file is in, keyed by
    // the callables the measures name.
    let twin_pairs = if twins {
        let spans: Vec<twins::CallableSpan<'_>> = sink
            .callable_measures
            .iter()
            .zip(&sink.callable_lines)
            .map(
                |(measure, (rel_path, start_line, end_line))| twins::CallableSpan {
                    node_id: measure.node_id.as_str(),
                    rel_path: rel_path.as_str(),
                    start_line: *start_line,
                    end_line: *end_line,
                },
            )
            .collect();
        twins::detect(&sink.token_streams, &spans)
    } else {
        Vec::new()
    };
    let MeasureSink {
        callable_measures,
        file_measures,
        ..
    } = sink;

    let files_skipped = state
        .source_files
        .len()
        .saturating_sub(files_with_bytes.len());
    let mut report = build_report(
        &files,
        &nodes,
        &edges,
        &call_sites,
        files_skipped,
        parse_errors,
        observations,
    );

    report.parse_error_files = parse_error_files;
    report.first_parse_errors = first_parse_errors;

    let snapshot = Snapshot {
        identity,
        files,
        nodes,
        edges,
        call_sites,
        measures: callable_measures,
        file_measures,
        clone_groups,
        twin_pairs,
        report,
    };

    crate::validate::validate(&snapshot).context("snapshot validation failed")?;
    Ok(snapshot)
}

/// The root module path from the tracked `go.mod`, read from the same
/// source as the inventory (the commit object store, or the working tree
/// for a worktree extraction). `None` when the repository has no root
/// `go.mod`: then no import path names a directory and only bare
/// same-package references resolve. Nested modules are out of scope.
fn go_module_path(state: &GitState) -> Result<Option<String>> {
    let root = Path::new(&state.repo_root);
    let bytes = if state.worktree {
        match std::fs::read(root.join("go.mod")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("read go.mod from the working tree"),
        }
    } else {
        match git::read_file_at(root, &state.commit_sha, "go.mod") {
            Ok(bytes) => bytes,
            Err(_) => return Ok(None),
        }
    };
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("module "))
        .map(|module| module.trim().trim_matches('"').to_string()))
}

fn deduplicate_pending_calls(calls: &mut Vec<PendingCall>) {
    calls.sort_by(|left, right| {
        (
            &left.rel_path,
            left.call.name_start_byte,
            &left.call.raw_name,
            &left.call.path_root,
            left.call.span.start_byte,
            left.call.span.end_byte,
        )
            .cmp(&(
                &right.rel_path,
                right.call.name_start_byte,
                &right.call.raw_name,
                &right.call.path_root,
                right.call.span.start_byte,
                right.call.span.end_byte,
            ))
    });
    calls.dedup_by(|left, right| {
        left.rel_path == right.rel_path
            && left.call.name_start_byte == right.call.name_start_byte
            && left.call.raw_name == right.call.raw_name
            && left.call.path_root == right.call.path_root
    });
}

fn push_unique_edge(
    edges: &mut Vec<Edge>,
    emitted_edge_ids: &mut std::collections::HashSet<String>,
    edge: Edge,
) {
    if emitted_edge_ids.insert(edge.id.clone()) {
        edges.push(edge);
    }
}

/// Assign deterministic IDs to raw defs, emit nodes + lexical `contains`
/// edges + `imports` edges. Collision disambiguation via source span.
#[allow(clippy::too_many_arguments)] // shared per-file fact assembly
fn assign_and_push(
    defs: &[RawDef],
    imports: &[RawImport],
    identity: &SnapshotIdentity,
    file_hash: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    file_node_id: &str,
    rel_path: &str,
) -> Vec<String> {
    // Map from def index → assigned node id (for contains parent links),
    // returned so a caller can name a def's node without recomputing it.
    let mut def_ids: Vec<String> = Vec::with_capacity(defs.len());
    // Track seen ids for collision disambiguation within the file.
    let mut seen: HashMap<String, usize> = HashMap::new();

    for def in defs {
        let base = node_id(identity.language, def.kind, rel_path, &def.scope, &def.name);
        let id = match seen.get(&base) {
            None => {
                seen.insert(base.clone(), 1);
                base
            }
            Some(_) => node_id_disambiguated(
                identity.language,
                def.kind,
                rel_path,
                &def.scope,
                &def.name,
                def.span.start_byte,
            ),
        };
        def_ids.push(id.clone());

        let prov = provenance(identity, rel_path, def.span, file_hash);
        nodes.push(Node {
            python: None,
            id: id.clone(),
            kind: def.kind,
            name: def.name.clone(),
            symbol_key: symbol_key(identity.language, rel_path, &def.scope, &def.name),
            provenance: prov.clone(),
        });

        let parent_id = def
            .parent
            .map_or_else(|| file_node_id.to_string(), |p| def_ids[p].clone());
        edges.push(Edge {
            id: edge_id(EdgeKind::Contains, &parent_id, &id, def.span.start_byte),
            kind: EdgeKind::Contains,
            from: parent_id,
            to: id,
            provenance: prov,
        });
    }

    // Imports: file → ImportRef placeholder (no dangling).
    for imp in imports {
        let ref_id = format!(
            "{}importref:{rel_path}#{}@{}",
            identity.language.id_prefix(),
            imp.path,
            imp.span.start_byte
        );
        let prov = provenance(identity, rel_path, imp.span, file_hash);
        nodes.push(Node {
            python: None,
            id: ref_id.clone(),
            kind: NodeKind::ImportRef,
            name: imp.path.clone(),
            symbol_key: format!(
                "{}importref:{rel_path}#{}",
                identity.language.id_prefix(),
                imp.path
            ),
            provenance: prov.clone(),
        });
        edges.push(Edge {
            id: edge_id(
                EdgeKind::Imports,
                file_node_id,
                &ref_id,
                imp.span.start_byte,
            ),
            kind: EdgeKind::Imports,
            from: file_node_id.to_string(),
            to: ref_id,
            provenance: prov,
        });
    }
    def_ids
}

fn build_file_node(
    rel_path: &str,
    source: &[u8],
    file_hash: &str,
    identity: &SnapshotIdentity,
) -> Node {
    let mut end = (0, 0);
    for byte in source {
        if *byte == b'\n' {
            end = (end.0 + 1, 0);
        } else {
            end.1 += 1;
        }
    }
    let span = Span::from_ts(0, source.len(), (0, 0), end);
    Node {
        python: None,
        id: node_id(identity.language, NodeKind::File, rel_path, &[], ""),
        kind: NodeKind::File,
        name: rel_path.to_string(),
        symbol_key: format!("{}file:{rel_path}", identity.language.id_prefix()),
        provenance: provenance(identity, rel_path, span, file_hash),
    }
}

fn provenance(
    identity: &SnapshotIdentity,
    rel_path: &str,
    span: Span,
    file_hash: &str,
) -> Provenance {
    Provenance {
        repository: identity.repository.id.clone(),
        commit_sha: identity.commit_sha.clone(),
        rel_path: rel_path.to_string(),
        span,
        file_hash: file_hash.to_string(),
        extractor_version: identity.extractor_version.clone(),
        fact_source: identity.language.structural_fact_source(),
        confidence: 255,
    }
}

/// Provenance for a semantic `calls` edge: authoritative rust-analyzer source.
fn calls_provenance(
    identity: &SnapshotIdentity,
    rel_path: &str,
    span: Span,
    file_hash: &str,
) -> Provenance {
    Provenance {
        repository: identity.repository.id.clone(),
        commit_sha: identity.commit_sha.clone(),
        rel_path: rel_path.to_string(),
        span,
        file_hash: file_hash.to_string(),
        extractor_version: identity.extractor_version.clone(),
        fact_source: FactSource::RustAnalyzer,
        confidence: 255,
    }
}

/// Resolve every call site via rust-analyzer and emit `calls` edges only
/// when the target maps uniquely to an in-repository node. All other call
/// sites are reported as categorized observations, never canonical edges.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn resolve_calls_semantic(
    state: &GitState,
    identity: &SnapshotIdentity,
    nodes: &[Node],
    target_index: &BTreeMap<String, BTreeMap<usize, String>>,
    type_targets: &RustTypeTargets,
    implementations: &[(String, String, rust_syntax::RawImpl)],
    raw_calls: &[PendingCall],
    file_sources: &BTreeMap<&str, &[u8]>,
    edges: &mut Vec<Edge>,
    call_sites: &mut Vec<CallSite>,
) -> Result<()> {
    let workspace_root = Path::new(&state.repo_root)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&state.repo_root).to_path_buf());

    // In-repository file set (repo-relative) for classification.
    let rel_paths: std::collections::HashSet<String> = state.source_files.iter().cloned().collect();

    // Callers use enclosing callable bodies; targets use declaration names.
    let node_index = NodeIndex::build(
        nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
            .map(|n| {
                (
                    n.provenance.rel_path.clone(),
                    n.provenance.span,
                    n.id.clone(),
                )
            }),
    );

    let target_triple = identity
        .tools
        .target_triple
        .as_deref()
        .context("semantic identity is missing target triple")?;
    // Semantic extraction has two costs with different cures: getting
    // rust-analyzer to a state where it can answer, and then asking it a great
    // many questions. Only the second is parallelizable, so the split decides
    // whether a concurrent client is worth building or whether the lever is
    // pipelining indexing across commits. Report it rather than guess.
    let indexing_started = Instant::now();
    let client = rust_analyzer::Client::spawn(&workspace_root, target_triple)
        .context("failed to start rust-analyzer")?;

    // Open every tracked file so rust-analyzer indexes the pinned sources.
    for (rel_path, source) in file_sources {
        let text = std::str::from_utf8(source)
            .with_context(|| format!("tracked Rust file {rel_path} is not valid UTF-8"))?;
        client
            .open_document(rel_path, text)
            .with_context(|| format!("open {rel_path} in rust-analyzer"))?;
    }
    client
        .wait_quiescent(std::time::Duration::from_secs(120))
        .context("rust-analyzer did not index opened documents")?;
    ensure_complete_crate_graph(&client)
        .context("semantic extraction requires a complete rust-analyzer crate graph")?;
    let indexing = indexing_started.elapsed();
    let resolving_started = Instant::now();
    let dependency_roots = rust_analyzer::external_crate_roots(&workspace_root)
        .context("Cargo dependency inventory failed")?;
    let mut emitted_edge_ids: std::collections::HashSet<String> =
        edges.iter().map(|edge| edge.id.clone()).collect();

    // Ask every question at once, then fold the answers in request order. The
    // fold stays sequential and the answers arrive in order, so concurrency
    // cannot reach the emitted bytes — only the wall clock.
    let mut requests = Vec::with_capacity(raw_calls.len());
    for pending in raw_calls {
        let source = file_sources
            .get(pending.rel_path.as_str())
            .with_context(|| {
                format!(
                    "missing pinned source bytes for semantic call in {}",
                    pending.rel_path
                )
            })?;
        let (line, character) = lsp::byte_offset_to_lsp_pos(source, pending.call.name_start_byte);
        requests.push(lsp::DefinitionRequest {
            rel_path: pending.rel_path.clone(),
            line,
            character,
        });
    }
    // One server, many lanes: rust-analyzer answers concurrently, so every
    // worker thread drives the same client.
    let workers = definition_workers().clamp(1, 64).min(requests.len().max(1));
    let lane: &dyn lsp::DefinitionSource = &client;
    let lanes = vec![lane; workers];
    let answers = lsp::definition_batch(&requests, &lanes)
        .context("rust-analyzer definition requests failed")?;

    for (pending, result) in raw_calls.iter().zip(answers) {
        let rel_path = &pending.rel_path;
        let file_hash = &pending.file_hash;
        let call = &pending.call;

        let category = match result {
            lsp::DefinitionResult::Empty => {
                classify_unresolved_target(rel_path, call.path_root.as_deref(), &dependency_roots)
            }
            lsp::DefinitionResult::Locations(locs) => {
                // Partition into in-repo and external targets.
                let in_repo: Vec<_> = locs
                    .iter()
                    .filter_map(|loc| {
                        rel_path_of_uri(&loc.uri, &workspace_root)
                            .filter(|rp| rel_paths.contains(rp))
                            .map(|rp| (rp, loc))
                    })
                    .collect();
                match in_repo.len() {
                    0 => locs.first().map_or(CallSiteCategory::Unmapped, |loc| {
                        rust_analyzer::classify_location(loc, &std::collections::HashSet::new())
                    }),
                    1 if locs.len() == 1 => {
                        let (target_rel, loc) = &in_repo[0];
                        let target_source = file_sources.get(target_rel.as_str());
                        let target_byte = target_source.map_or(0, |s| {
                            lsp::lsp_pos_to_byte(s, loc.start_line, loc.start_character)
                        });
                        target_index
                            .get(target_rel)
                            .and_then(|targets| targets.get(&target_byte))
                            .map_or(CallSiteCategory::Unmapped, |to_id| {
                                node_index.find(rel_path, call.name_start_byte).map_or(
                                    CallSiteCategory::Candidate,
                                    |from_id| {
                                        let prov = calls_provenance(
                                            identity, rel_path, call.span, file_hash,
                                        );
                                        let id = edge_id(
                                            EdgeKind::Calls,
                                            from_id,
                                            to_id,
                                            call.name_start_byte,
                                        );
                                        push_unique_edge(
                                            edges,
                                            &mut emitted_edge_ids,
                                            Edge {
                                                id,
                                                kind: EdgeKind::Calls,
                                                from: from_id.to_string(),
                                                to: to_id.clone(),
                                                provenance: prov,
                                            },
                                        );
                                        CallSiteCategory::Resolved
                                    },
                                )
                            })
                    }
                    _ => CallSiteCategory::Ambiguous,
                }
            }
        };
        call_sites.push(CallSite {
            rel_path: rel_path.clone(),
            span: call.span,
            raw_name: call.raw_name.clone(),
            category,
        });
    }

    resolve_rust_implementations(
        &client,
        &workspace_root,
        identity,
        type_targets,
        implementations,
        file_sources,
        edges,
        call_sites,
    )?;

    let resolving = resolving_started.elapsed();
    client.shutdown();

    let requests = call_sites.len().max(1) as u128;
    let indexing_ms = indexing.as_millis();
    let resolving_ms = resolving.as_millis();
    let total_ms = (indexing_ms + resolving_ms).max(1);
    eprintln!(
        "semantic phases: indexing {}.{:03}s, resolving {}.{:03}s over {requests} requests \
         ({}us each) — resolving is {}% of the total",
        indexing.as_secs(),
        indexing.subsec_millis(),
        resolving.as_secs(),
        resolving.subsec_millis(),
        resolving.as_micros() / requests,
        resolving_ms * 100 / total_ms,
    );
    Ok(())
}

/// How many definition requests to keep in flight.
///
/// rust-analyzer serves read requests from a thread pool, so the useful ceiling
/// is roughly the machine's parallelism; two cores are left for the server's own
/// main loop and this process. `CODEMAP_DEFINITION_WORKERS` overrides it, which
/// is how the sequential baseline (`=1`) is reproduced for the determinism gate.
fn definition_workers() -> usize {
    if let Ok(configured) = std::env::var("CODEMAP_DEFINITION_WORKERS") {
        if let Ok(workers) = configured.parse::<usize>() {
            return workers.max(1);
        }
    }
    std::thread::available_parallelism().map_or(4, |cores| cores.get().saturating_sub(2).max(1))
}

/// A compiler answer is a declaration identity only at an exact emitted name.
fn rust_implementation_target(
    answer: &lsp::DefinitionResult,
    workspace_root: &Path,
    sources: &BTreeMap<&str, &[u8]>,
    targets: &RustTypeTargets,
    trait_endpoint: bool,
) -> Option<String> {
    let lsp::DefinitionResult::Locations(locations) = answer else {
        return None;
    };
    if locations.len() != 1 {
        return None;
    }
    let location = &locations[0];
    let path = rel_path_of_uri(&location.uri, workspace_root)?;
    let source = sources.get(path.as_str())?;
    let byte = lsp::lsp_pos_to_byte(source, location.start_line, location.start_character);
    let (id, kind) = targets.get(&path)?.get(&byte)?;
    let correct_kind = if trait_endpoint {
        *kind == NodeKind::Trait
    } else {
        matches!(
            kind,
            NodeKind::Type(
                schema::TypeKind::Struct | schema::TypeKind::Enum | schema::TypeKind::Union
            )
        )
    };
    correct_kind.then(|| id.clone())
}

#[allow(clippy::too_many_arguments)]
fn resolve_rust_implementations(
    client: &dyn lsp::DefinitionSource,
    workspace_root: &Path,
    identity: &SnapshotIdentity,
    targets: &RustTypeTargets,
    implementations: &[(String, String, rust_syntax::RawImpl)],
    sources: &BTreeMap<&str, &[u8]>,
    edges: &mut Vec<Edge>,
    observations: &mut Vec<CallSite>,
) -> Result<()> {
    for (path, file_hash, implementation) in implementations {
        let Some(trait_name) = &implementation.trait_name else {
            continue;
        };
        let positions = implementation
            .type_name_start
            .zip(implementation.trait_name_start)
            .filter(|_| {
                !implementation.negative && !implementation.generic && !implementation.visible_glob
            });
        let resolved = if let Some(positions) = positions {
            let source = sources
                .get(path.as_str())
                .context("missing Rust implementation source")?;
            let mut answers = Vec::with_capacity(2);
            for byte in <[usize; 2]>::from(positions) {
                let (line, character) = lsp::byte_offset_to_lsp_pos(source, byte);
                answers.push(client.definition(&lsp::DefinitionRequest {
                    rel_path: path.clone(),
                    line,
                    character,
                })?);
            }
            rust_implementation_target(&answers[0], workspace_root, sources, targets, false).zip(
                rust_implementation_target(&answers[1], workspace_root, sources, targets, true),
            )
        } else {
            None
        };
        if let Some((from, to)) = resolved {
            let id = edge_id(
                EdgeKind::Implements,
                &from,
                &to,
                implementation.span.start_byte,
            );
            edges.push(Edge {
                id,
                kind: EdgeKind::Implements,
                from,
                to,
                provenance: calls_provenance(identity, path, implementation.span, file_hash),
            });
        } else {
            observations.push(CallSite {
                rel_path: path.clone(),
                span: implementation.span,
                raw_name: trait_name.clone(),
                category: CallSiteCategory::Unmapped,
            });
        }
    }
    Ok(())
}

/// Resolve every Ruby call site via ruby-lsp and emit `calls` edges only
/// when codemap's static model verifies the receiver→target relation
/// (constant receiver, implicit self or verified construction; target owner inside the receiver's
/// `inherits`/`implements` closure). Every other call site is reported as
/// a categorized observation — guess-regime sites with in-repo name
/// candidates are `Candidate`s — never canonical edges.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn resolve_calls_semantic_ruby(
    state: &GitState,
    identity: &SnapshotIdentity,
    ruby: &ruby_lsp::RubyToolIdentity,
    nodes: &[Node],
    def_index: &DefinitionIndex,
    raw_calls: &[PendingCall],
    file_sources: &BTreeMap<&str, &[u8]>,
    edges: &mut Vec<Edge>,
    call_sites: &mut Vec<CallSite>,
) -> Result<()> {
    let workspace_root = Path::new(&state.repo_root)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&state.repo_root).to_path_buf());

    let rel_paths: std::collections::HashSet<String> = state.source_files.iter().cloned().collect();

    // Spatial index over definition-bearing nodes for target mapping.
    let node_index = NodeIndex::build(
        nodes
            .iter()
            .filter(|n| !matches!(n.kind, NodeKind::File | NodeKind::ImportRef))
            .map(|n| {
                (
                    n.provenance.rel_path.clone(),
                    n.provenance.span,
                    n.id.clone(),
                )
            }),
    );

    // ruby-lsp answers one request at a time, so the definition phase runs
    // over a pool of servers, each indexing the same pinned sources. They
    // index concurrently in their own processes; the waits below overlap.
    let workers = definition_workers()
        .clamp(1, RUBY_DEFINITION_WORKERS_CAP)
        .min(raw_calls.len().max(1));
    let mut clients = Vec::with_capacity(workers);
    for _ in 0..workers {
        let client = ruby_lsp::Client::spawn(&workspace_root, &ruby.tool_bundle)
            .context("failed to start ruby-lsp")?;
        // Open every tracked file so ruby-lsp serves the pinned sources.
        for (rel_path, source) in file_sources {
            let text = std::str::from_utf8(source)
                .with_context(|| format!("tracked Ruby file {rel_path} is not valid UTF-8"))?;
            client
                .open_document(rel_path, text)
                .with_context(|| format!("open {rel_path} in ruby-lsp"))?;
        }
        clients.push(client);
    }
    for client in &clients {
        client
            .wait_indexed(std::time::Duration::from_secs(120))
            .context("ruby-lsp did not finish indexing the opened documents")?;
    }

    // The policy model is codemap's own static verification layer, built
    // from the emitted structural facts; ruby-lsp answers are candidate
    // material only.
    let mut model = ruby_lsp::PolicyModel::build(nodes, edges);
    model.verify_ruby_source_contexts(file_sources, def_index, &node_index)?;
    let ctx = ruby_lsp::ResolveContext {
        workspace_root: &workspace_root,
        identity,
        rel_paths: &rel_paths,
        file_sources,
        node_index: &node_index,
        def_index,
    };
    let mut emitted_edge_ids: std::collections::HashSet<String> =
        edges.iter().map(|edge| edge.id.clone()).collect();

    let pending: Vec<ruby_lsp::PendingRubyCall> = raw_calls
        .iter()
        .map(|pending| ruby_lsp::PendingRubyCall {
            rel_path: pending.rel_path.clone(),
            file_hash: pending.file_hash.clone(),
            call: pending.call.clone(),
        })
        .collect();
    // Ask every question at once, then fold the answers in request order:
    // the fold is sequential, so concurrency reaches the wall clock and
    // never the emitted bytes.
    let requests = pending
        .iter()
        .map(|site| {
            let source = file_sources
                .get(site.rel_path.as_str())
                .context("missing pinned source bytes for ruby semantic call")?;
            let (line, character) = lsp::byte_offset_to_lsp_pos(source, site.call.name_start_byte);
            Ok(lsp::DefinitionRequest {
                rel_path: site.rel_path.clone(),
                line,
                character,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let lanes: Vec<&dyn lsp::DefinitionSource> = clients
        .iter()
        .map(|client| client as &dyn lsp::DefinitionSource)
        .collect();
    let answers =
        lsp::definition_batch(&requests, &lanes).context("ruby-lsp definition requests failed")?;
    let constructor_answers: BTreeMap<_, _> = pending
        .iter()
        .zip(&answers)
        .filter(|(site, _)| site.call.raw_name == "new")
        .map(|(site, answer)| {
            (
                (site.rel_path.as_str(), site.call.name_start_byte),
                answer.clone(),
            )
        })
        .collect();
    for (site, answer) in pending.iter().zip(answers) {
        let constructor = match &site.call.receiver {
            rust_syntax::ReceiverShape::Constructed {
                constructor_start, ..
            } => constructor_answers.get(&(site.rel_path.as_str(), *constructor_start)),
            _ => None,
        };
        let category = ruby_lsp::admit_definition(
            answer,
            constructor,
            &model,
            &ctx,
            site,
            &mut emitted_edge_ids,
            edges,
        );
        call_sites.push(CallSite {
            rel_path: site.rel_path.clone(),
            span: site.call.span,
            raw_name: site.call.raw_name.clone(),
            category,
        });
    }

    for client in &clients {
        client.shutdown();
    }
    Ok(())
}

/// Upper bound on pooled ruby-lsp servers. Each server indexes the whole
/// application itself, so the pool pays that once per server, and beyond
/// four servers the indexing cost outweighs the parallel gain.
const RUBY_DEFINITION_WORKERS_CAP: usize = 4;

/// Resolve every PHP call site via phpactor and emit `calls` edges only
/// when codemap's verified-edge policy admits them. Mirrors
/// [`resolve_calls_semantic_ruby`]; the policy inputs differ (see
/// `php_lsp::PolicyModel`).
#[allow(clippy::too_many_arguments)]
fn resolve_calls_semantic_php(
    state: &GitState,
    identity: &SnapshotIdentity,
    php: &php_lsp::PhpToolIdentity,
    nodes: &[Node],
    def_index: &DefinitionIndex,
    raw_calls: &[PendingCall],
    static_methods: &HashSet<String>,
    file_sources: &BTreeMap<&str, &[u8]>,
    edges: &mut Vec<Edge>,
    call_sites: &mut Vec<CallSite>,
) -> Result<()> {
    let workspace_root = Path::new(&state.repo_root)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&state.repo_root).to_path_buf());

    let rel_paths: std::collections::HashSet<String> = state.source_files.iter().cloned().collect();

    let node_index = NodeIndex::build(
        nodes
            .iter()
            .filter(|n| !matches!(n.kind, NodeKind::File | NodeKind::ImportRef))
            .map(|n| {
                (
                    n.provenance.rel_path.clone(),
                    n.provenance.span,
                    n.id.clone(),
                )
            }),
    );

    // phpactor serves one request at a time, so the definition phase runs
    // over a pool of servers, each reading the one index the workspace
    // built before any of them started. Answers are folded in request
    // order, so the pool size never reaches the snapshot bytes.
    let workspace = php_lsp::Workspace::prepare(&workspace_root, &php.tool_bundle)
        .context("failed to prepare phpactor")?;
    let workers = definition_workers()
        .clamp(1, PHP_DEFINITION_WORKERS_CAP)
        .min(raw_calls.len().max(1));
    let mut clients = Vec::with_capacity(workers);
    for _ in 0..workers {
        let client = workspace
            .spawn_client()
            .context("failed to start phpactor")?;
        for (rel_path, source) in file_sources {
            let text = std::str::from_utf8(source)
                .with_context(|| format!("tracked PHP file {rel_path} is not valid UTF-8"))?;
            client
                .open_document(rel_path, text)
                .with_context(|| format!("open {rel_path} in phpactor"))?;
        }
        clients.push(client);
    }

    let model = php_lsp::PolicyModel::build(nodes, edges, static_methods);
    let ctx = php_lsp::ResolveContext {
        workspace_root: &workspace_root,
        identity,
        rel_paths: &rel_paths,
        file_sources,
        node_index: &node_index,
        def_index,
    };
    let mut emitted_edge_ids: std::collections::HashSet<String> =
        edges.iter().map(|edge| edge.id.clone()).collect();

    let pending: Vec<php_lsp::PendingPhpCall> = raw_calls
        .iter()
        .map(|pending| php_lsp::PendingPhpCall {
            rel_path: pending.rel_path.clone(),
            file_hash: pending.file_hash.clone(),
            call: pending.call.clone(),
        })
        .collect();
    // Ask every question over the pool, one lane per server, then fold
    // the answers in request order: the fold is sequential, so concurrency
    // reaches the wall clock and never the emitted bytes.
    let requests = pending
        .iter()
        .map(|site| {
            let source = file_sources
                .get(site.rel_path.as_str())
                .context("missing pinned source bytes for php semantic call")?;
            let (line, character) = lsp::byte_offset_to_lsp_pos(source, site.call.name_start_byte);
            Ok(lsp::DefinitionRequest {
                rel_path: site.rel_path.clone(),
                line,
                character,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let lanes: Vec<&dyn lsp::DefinitionSource> = clients
        .iter()
        .map(|client| client as &dyn lsp::DefinitionSource)
        .collect();
    let answers =
        lsp::definition_batch(&requests, &lanes).context("phpactor definition requests failed")?;
    for (site, answer) in pending.iter().zip(answers) {
        let category =
            php_lsp::admit_definition(answer, &model, &ctx, site, &mut emitted_edge_ids, edges);
        call_sites.push(CallSite {
            rel_path: site.rel_path.clone(),
            span: site.call.span,
            raw_name: site.call.raw_name.clone(),
            category,
        });
    }

    for client in &clients {
        client.shutdown();
    }
    Ok(())
}

/// Upper bound on pooled phpactor servers: each is a PHP process holding
/// the whole index (roughly 150–360 MB on a 1,600-file application), and
/// the definition phase saturates cores well before this.
const PHP_DEFINITION_WORKERS_CAP: usize = 8;

/// Resolve every Go call site via gopls and emit `calls` edges only when
/// codemap's gates admit them: a unique in-repository answer that is an
/// emitted callable, reached from inside an emitted callable. gopls
/// answers concurrently, so one server is driven over many lanes, as for
/// rust-analyzer; answers fold in request order, so the lane count never
/// reaches the emitted bytes.
#[allow(clippy::too_many_arguments)]
fn resolve_calls_semantic_go(
    state: &GitState,
    identity: &SnapshotIdentity,
    go: &go_lsp::GoToolIdentity,
    nodes: &[Node],
    callable_targets: &BTreeMap<String, BTreeMap<usize, String>>,
    raw_calls: &[PendingCall],
    file_sources: &BTreeMap<&str, &[u8]>,
    edges: &mut Vec<Edge>,
    call_sites: &mut Vec<CallSite>,
) -> Result<()> {
    let workspace_root = Path::new(&state.repo_root)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&state.repo_root).to_path_buf());

    let rel_paths: std::collections::HashSet<String> = state.source_files.iter().cloned().collect();

    let node_index = NodeIndex::build(
        nodes
            .iter()
            .filter(|n| !matches!(n.kind, NodeKind::File | NodeKind::ImportRef))
            .map(|n| {
                (
                    n.provenance.rel_path.clone(),
                    n.provenance.span,
                    n.id.clone(),
                )
            }),
    );

    let env = go_lsp::GoEnv::capture().context("go toolchain environment")?;
    let client = go_lsp::Client::spawn(&workspace_root, &go.tool_bundle, &env)
        .context("failed to start gopls")?;

    // Open every tracked file so gopls serves the pinned sources, then
    // wait for the workspace to load; a workspace it cannot load fails
    // here rather than resolving against part of it.
    let mut opened: Vec<String> = Vec::with_capacity(file_sources.len());
    for (rel_path, source) in file_sources {
        let text = std::str::from_utf8(source)
            .with_context(|| format!("tracked Go file {rel_path} is not valid UTF-8"))?;
        client
            .open_document(rel_path, text)
            .with_context(|| format!("open {rel_path} in gopls"))?;
        opened.push((*rel_path).to_string());
    }
    client
        .wait_loaded(&opened, std::time::Duration::from_secs(300))
        .context("gopls did not load the workspace")?;

    let model = go_lsp::AdmissionModel::build(nodes);
    let ctx = go_lsp::ResolveContext {
        workspace_root: &workspace_root,
        identity,
        rel_paths: &rel_paths,
        file_sources,
        node_index: &node_index,
        callable_targets,
        env: &env,
    };
    let mut emitted_edge_ids: std::collections::HashSet<String> =
        edges.iter().map(|edge| edge.id.clone()).collect();

    let pending: Vec<go_lsp::PendingGoCall> = raw_calls
        .iter()
        .map(|pending| go_lsp::PendingGoCall {
            rel_path: pending.rel_path.clone(),
            file_hash: pending.file_hash.clone(),
            call: pending.call.clone(),
        })
        .collect();
    let requests = pending
        .iter()
        .map(|site| {
            let source = file_sources
                .get(site.rel_path.as_str())
                .context("missing pinned source bytes for go semantic call")?;
            let (line, character) = lsp::byte_offset_to_lsp_pos(source, site.call.name_start_byte);
            Ok(lsp::DefinitionRequest {
                rel_path: site.rel_path.clone(),
                line,
                character,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let workers = definition_workers().clamp(1, 64).min(requests.len().max(1));
    let lane: &dyn lsp::DefinitionSource = &client;
    let lanes = vec![lane; workers];
    let answers =
        lsp::definition_batch(&requests, &lanes).context("gopls definition requests failed")?;
    for (site, answer) in pending.iter().zip(answers) {
        let category =
            go_lsp::admit_definition(answer, &model, &ctx, site, &mut emitted_edge_ids, edges);
        call_sites.push(CallSite {
            rel_path: site.rel_path.clone(),
            span: site.call.span,
            raw_name: site.call.raw_name.clone(),
            category,
        });
    }

    client.shutdown();
    Ok(())
}

/// rust-analyzer builds its crate graph once at startup, before its file
/// loader has populated the VFS, which yields an EMPTY graph: every crate
/// root file is missing at construction time. With autoreload enabled the
/// server self-heals — it re-fetches and rebuilds the graph when the loader
/// delivers the crate roots — but the first quiescence can be reported while
/// that rebuild is still pending (or never happens, e.g. a sysroot without
/// sources). Poll the crate graph until it contains the workspace crates and
/// the sysroot, waiting out further quiescence cycles; fail closed if it
/// stays incomplete, because definition queries against an empty graph
/// silently misclassify most call sites.
fn ensure_complete_crate_graph(client: &rust_analyzer::Client) -> Result<()> {
    const ATTEMPTS: usize = 4;
    for attempt in 0..ATTEMPTS {
        let dot = client
            .view_crate_graph()
            .context("rust-analyzer crate graph inspection failed")?;
        if crate_graph_is_complete(&dot) {
            return Ok(());
        }
        if attempt + 1 < ATTEMPTS {
            client
                .wait_quiescent(std::time::Duration::from_secs(60))
                .context("rust-analyzer did not finish rebuilding its crate graph")?;
        }
    }
    bail!(
        "rust-analyzer crate graph remained incomplete after {ATTEMPTS} checks \
         (missing workspace or sysroot crates); refusing to resolve calls against it"
    )
}

/// A crate graph is complete once it holds at least one workspace crate next
/// to the sysroot. The broken startup graph has zero nodes; a graph without
/// the sysroot cannot infer receivers through `std`, so `std` membership is
/// the acceptance condition. Graph nodes render as `[label="<name>"...]`.
fn crate_graph_is_complete(dot: &str) -> bool {
    let node_count = dot.matches("[label=").count();
    let has_std = dot.contains("label=\"std\"");
    node_count >= 2 && has_std
}

/// Convert a `file://` URI to a repo-relative path if it lives under
/// `workspace_root`, else `None`.
fn rel_path_of_uri(uri: &str, workspace_root: &Path) -> Option<String> {
    let abs = lsp::path_from_uri(uri);
    let abs_path = Path::new(&abs);
    abs_path
        .strip_prefix(workspace_root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

fn classify_unresolved_target(
    caller_rel_path: &str,
    path_root: Option<&str>,
    dependency_roots: &rust_analyzer::WorkspaceDependencyRoots,
) -> CallSiteCategory {
    match path_root {
        Some("std" | "core" | "alloc") => CallSiteCategory::StandardLibrary,
        Some(root) if dependency_roots.is_external(caller_rel_path, root) => {
            CallSiteCategory::ExternalCrate
        }
        _ => CallSiteCategory::Unmapped,
    }
}

/// Serialize a snapshot to canonical JSON (byte-deterministic).
pub fn to_json(snapshot: &Snapshot) -> Result<String> {
    schema::canonical_json(snapshot).context("snapshot serialization failed")
}

pub fn build_report(
    files: &[FileRecord],
    nodes: &[Node],
    edges: &[Edge],
    call_sites: &[schema::CallSite],
    files_skipped: usize,
    parse_errors: usize,
    observations: BTreeMap<String, usize>,
) -> Report {
    let mut node_counts: BTreeMap<String, usize> = BTreeMap::new();
    for n in nodes {
        let key = n.kind.to_string();
        *node_counts.entry(key).or_default() += 1;
    }
    let mut edge_counts: BTreeMap<String, usize> = BTreeMap::new();
    for e in edges {
        *edge_counts.entry(e.kind.tag().to_string()).or_default() += 1;
    }
    let mut call_sites_by_category: BTreeMap<String, usize> = BTreeMap::new();
    for c in call_sites {
        *call_sites_by_category
            .entry(c.category.report_key().to_string())
            .or_default() += 1;
    }
    let import_ref_nodes = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::ImportRef)
        .count();

    Report {
        files_scanned: files.len(),
        files_skipped,
        parse_errors,
        parse_error_files: BTreeMap::new(),
        first_parse_errors: BTreeMap::new(),
        node_counts,
        edge_counts,
        call_sites_by_category,
        import_ref_nodes,
        observations,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::{
        ExtractOptions, Language, PendingCall, deduplicate_pending_calls, extract,
        push_unique_edge, read_snapshot_files,
    };
    use crate::git;

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-C"])
            .arg(dir)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn implementation_answers_require_one_exact_concrete_declaration() {
        use super::rust_implementation_target;
        use crate::{
            lsp,
            schema::{self, NodeKind},
        };
        use std::collections::BTreeMap;
        let root = tempfile::tempdir().unwrap();
        let source = b"trait Marker {}\nstruct Item;\ntype Alias = Item;\nfn f<Item>() {}\n";
        let marker = 6;
        let item = 23;
        let alias = 34;
        let generic = 53;
        let sources = BTreeMap::from([("a.rs", source.as_slice())]);
        let targets = BTreeMap::from([(
            "a.rs".to_string(),
            BTreeMap::from([
                (marker, ("marker".into(), NodeKind::Trait)),
                (
                    item,
                    ("item".into(), NodeKind::Type(schema::TypeKind::Struct)),
                ),
                (
                    alias,
                    ("alias".into(), NodeKind::Type(schema::TypeKind::TypeAlias)),
                ),
            ]),
        )]);
        let location = |byte| {
            let (line, character) = lsp::byte_offset_to_lsp_pos(source, byte);
            lsp::DefinitionLocation {
                uri: lsp::uri_from_path(&root.path().join("a.rs")),
                start_line: line,
                start_character: character,
                end_line: line,
                end_character: character + 1,
            }
        };
        assert_eq!(
            rust_implementation_target(
                &lsp::DefinitionResult::Locations(vec![location(item)]),
                root.path(),
                &sources,
                &targets,
                false
            )
            .as_deref(),
            Some("item")
        );
        assert_eq!(
            rust_implementation_target(
                &lsp::DefinitionResult::Locations(vec![location(marker)]),
                root.path(),
                &sources,
                &targets,
                true
            )
            .as_deref(),
            Some("marker")
        );
        let foreign = lsp::DefinitionLocation {
            uri: "file:///external/a.rs".into(),
            ..location(item)
        };
        for answer in [
            lsp::DefinitionResult::Empty,
            lsp::DefinitionResult::Locations(vec![]),
            lsp::DefinitionResult::Locations(vec![location(alias)]),
            lsp::DefinitionResult::Locations(vec![location(generic)]),
            lsp::DefinitionResult::Locations(vec![location(item + 1)]),
            lsp::DefinitionResult::Locations(vec![location(marker)]),
            lsp::DefinitionResult::Locations(vec![location(item), location(item)]),
            lsp::DefinitionResult::Locations(vec![location(item), foreign]),
        ] {
            assert_eq!(
                rust_implementation_target(&answer, root.path(), &sources, &targets, false),
                None
            );
        }
        assert_eq!(
            rust_implementation_target(
                &lsp::DefinitionResult::Locations(vec![location(item)]),
                root.path(),
                &sources,
                &targets,
                true
            ),
            None
        );
    }

    fn committed_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("tempdir");
        run_git(repo.path(), &["init", "-q"]);
        run_git(repo.path(), &["config", "user.name", "Codemap Test"]);
        run_git(
            repo.path(),
            &["config", "user.email", "codemap@example.invalid"],
        );
        fs::write(repo.path().join("lib.rs"), "fn committed() {}\n").expect("write source");
        run_git(repo.path(), &["add", "lib.rs"]);
        run_git(repo.path(), &["commit", "-qm", "fixture"]);
        repo
    }

    #[test]
    fn extract_options_default_is_syntax_only() {
        let opts = ExtractOptions::default();
        assert!(!opts.calls_required);
    }

    #[test]
    fn snapshot_reads_use_the_captured_commit_not_worktree_bytes() {
        let repo = committed_repo();
        let state = git::gather(repo.path(), Language::Rust).expect("gather clean state");
        fs::write(repo.path().join("lib.rs"), "fn drifted() {}\n").expect("drift source");

        let files = read_snapshot_files(&state).expect("read captured commit");
        assert_eq!(
            files,
            vec![("lib.rs".to_string(), b"fn committed() {}\n".to_vec())]
        );
    }

    #[test]
    fn ruby_extraction_produces_namespaced_structural_facts() {
        let repo = committed_repo();
        fs::write(
            repo.path().join("app.rb"),
            "require \"json\"\nclass User < Record\n  include Auditable\n  def save\n  end\nend\n",
        )
        .expect("write ruby");
        fs::write(repo.path().join("lib.rs"), "fn rust() {}\n").expect("write rust");
        let status = Command::new("git")
            .args(["-C"])
            .arg(repo.path())
            .args(["add", "app.rb", "lib.rs"])
            .status()
            .expect("stage files");
        assert!(status.success());
        let status = Command::new("git")
            .args(["-C"])
            .arg(repo.path())
            .args(["commit", "-qm", "ruby"])
            .status()
            .expect("commit files");
        assert!(status.success());

        let snapshot = extract(&ExtractOptions {
            path: repo.path().to_path_buf(),
            calls_required: false,
            measures: false,
            twins: false,
            language: Language::Ruby,
        })
        .expect("ruby extraction");

        assert_eq!(snapshot.identity.language, Language::Ruby);
        assert_eq!(snapshot.identity.tools.ruby_parser, "prism 1.9.0");
        assert_eq!(snapshot.identity.tools.tree_sitter_rust, "");
        assert_eq!(snapshot.identity.tools.tree_sitter, "");
        // Only the ruby file is inventoried; every id is ruby-namespaced.
        assert_eq!(snapshot.files.len(), 1);
        assert!(snapshot.nodes.iter().all(|n| n.id.starts_with("ruby:")));
        let ids: Vec<_> = snapshot.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"ruby:file:app.rb"));
        assert!(ids.contains(&"ruby:type:app.rb#User"));
        assert!(ids.contains(&"ruby:method:app.rb#User::save"));
        // Inherits and mixins resolve against unique in-repo definitions
        // only; `Record`/`Auditable` are absent here, so no edges — but the
        // require still becomes an imports edge with an ImportRef node.
        assert!(
            snapshot
                .edges
                .iter()
                .all(|e| e.kind != crate::schema::EdgeKind::Calls)
        );
        assert!(
            snapshot
                .edges
                .iter()
                .any(|e| e.kind == crate::schema::EdgeKind::Imports)
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|n| n.kind == crate::schema::NodeKind::ImportRef)
        );
    }

    #[test]

    fn snapshot_reads_fail_when_an_inventoried_blob_is_unreadable() {
        let repo = committed_repo();
        let mut state = git::gather(repo.path(), Language::Rust).expect("gather clean state");
        state.source_files.push("missing.rs".to_string());

        let error = read_snapshot_files(&state).expect_err("missing blob must fail");
        assert!(error.to_string().contains("missing.rs"), "{error:#}");
    }

    #[test]
    fn produced_file_record_and_file_node_cover_exact_source_bytes() {
        let repo = committed_repo();
        let snapshot = extract(&ExtractOptions {
            path: repo.path().to_path_buf(),
            calls_required: false,
            measures: false,
            twins: false,
            language: Language::Rust,
        })
        .expect("extract snapshot");
        let file = &snapshot.files[0];
        assert_eq!(file.byte_len, b"fn committed() {}\n".len());
        let file_node = snapshot
            .nodes
            .iter()
            .find(|node| node.kind == crate::schema::NodeKind::File)
            .expect("file node");
        assert_eq!(file_node.provenance.span.start_byte, 0);
        assert_eq!(file_node.provenance.span.end_byte, file.byte_len);
    }

    #[test]
    fn exact_duplicate_call_observations_are_coalesced() {
        let call = crate::rust_syntax::RawCall {
            raw_name: "same".into(),
            path_root: None,
            span: crate::schema::Span::from_ts(4, 10, (0, 4), (0, 10)),
            name_start_byte: 4,
            receiver: crate::rust_syntax::ReceiverShape::Other,
            scope: Vec::new(),
        };
        let mut calls = vec![
            PendingCall {
                rel_path: "a.rs".into(),
                file_hash: "h".into(),
                call: call.clone(),
            },
            PendingCall {
                rel_path: "a.rs".into(),
                file_hash: "h".into(),
                call: crate::rust_syntax::RawCall {
                    span: crate::schema::Span::from_ts(4, 12, (0, 4), (0, 12)),
                    ..call
                },
            },
        ];
        deduplicate_pending_calls(&mut calls);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn duplicate_canonical_edge_ids_are_emitted_once() {
        let mut edges = Vec::new();
        let mut ids = std::collections::HashSet::new();
        let edge = crate::schema::Edge {
            id: "calls:caller->callee@4".into(),
            kind: crate::schema::EdgeKind::Calls,
            from: "caller".into(),
            to: "callee".into(),
            provenance: crate::schema::Provenance {
                repository: "repo".into(),
                commit_sha: "commit".into(),
                rel_path: "a.rs".into(),
                span: crate::schema::Span::from_ts(4, 10, (0, 4), (0, 10)),
                file_hash: "hash".into(),
                extractor_version: "version".into(),
                fact_source: crate::schema::FactSource::RustAnalyzer,
                confidence: 255,
            },
        };

        push_unique_edge(&mut edges, &mut ids, edge.clone());
        push_unique_edge(&mut edges, &mut ids, edge);

        assert_eq!(edges.len(), 1);
    }
}

#[cfg(test)]
mod crate_graph_completeness_tests {
    use super::crate_graph_is_complete;

    #[test]
    fn empty_startup_graph_is_rejected() {
        assert!(!crate_graph_is_complete(
            "digraph rust_analyzer_crate_graph {\n}\n"
        ));
    }

    #[test]
    fn graph_without_sysroot_is_rejected() {
        let dot = "digraph { 0 [label=\"mycrate\"] 1 [label=\"mydep\"] }";
        assert!(!crate_graph_is_complete(dot));
    }

    #[test]
    fn single_node_graph_is_rejected_even_with_std() {
        assert!(!crate_graph_is_complete("digraph { 0 [label=\"std\"] }"));
    }

    #[test]
    fn workspace_plus_sysroot_is_accepted() {
        let dot = "digraph { 0 [label=\"mycrate\"] 1 [label=\"std\"] 2 [label=\"std_detect\"] }";
        assert!(crate_graph_is_complete(dot));
    }

    #[test]
    fn std_prefix_crates_do_not_satisfy_the_std_check() {
        let dot = "digraph { 0 [label=\"mycrate\"] 1 [label=\"std_detect\"] }";
        assert!(!crate_graph_is_complete(dot));
    }
}
