//! Go semantic call resolution via gopls, under codemap's verified-edge
//! policy.
//!
//! Mirrors `rust_analyzer.rs` in regime: gopls type-checks the workspace
//! with the Go compiler's own front end, so its `textDocument/definition`
//! answer is exact when the workspace loads, and codemap admits it without
//! a receiver-shape model. Two gates stay on codemap's side: the target
//! must be an emitted `Function`/`Method` node (a conversion resolves to a
//! type, a call through a function-typed field or a method value to the
//! field or the variable), and the call must be enclosed by an emitted
//! callable. The server runs from an operator bundle
//! (`CODEMAP_GO_TOOL_BUNDLE`) with the network cut off; a workspace gopls
//! cannot load fails the run instead of resolving against part of it.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::lsp::{
    DefinitionRequest, DefinitionResult, DefinitionSource, NodeIndex, ServerRequestHook, Transport,
    bounded_probe, document_uri, parse_definition_response, uri_from_path,
};
use crate::schema::{
    CallSiteCategory, Edge, EdgeKind, FactSource, Node, NodeKind, Provenance, SnapshotIdentity,
    Span, edge_id,
};

/// Whether gopls is available to the extractor. `Present` carries the
/// tool-bundle directory whose `go.sum` satisfied the canonical pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Present(PathBuf),
    Absent,
}

/// Identity of the Go semantic environment, pinned in snapshot identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoToolIdentity {
    pub gopls: String,
    pub go: String,
    pub tool_bundle: PathBuf,
}

/// Canonical tool-bundle lockfile compiled into the binary. Operators
/// install a bundle whose `go.sum` is byte-identical to this pin;
/// extraction refuses to run against a drifted one.
const CANONICAL_LOCKFILE: &str = include_str!("../../../toolchain/go/go.sum");

/// SHA-256 of the canonical lockfile, for snapshot identity.
pub fn canonical_lockfile_sha256() -> String {
    crate::schema::sha256_hex(CANONICAL_LOCKFILE.as_bytes())
}

/// Detect a usable gopls tool bundle.
///
/// The bundle directory is supplied by the operator through
/// `CODEMAP_GO_TOOL_BUNDLE` — deliberately the only source. A bundle inside
/// the analyzed worktree is never considered. A candidate qualifies only
/// when its `go.sum` is byte-identical to the canonical pin set and the
/// installed binary answers `version`.
pub fn detect() -> Availability {
    let Some(explicit) = std::env::var_os("CODEMAP_GO_TOOL_BUNDLE") else {
        return Availability::Absent;
    };
    let dir = PathBuf::from(explicit);
    let Ok(lock) = std::fs::read_to_string(dir.join("go.sum")) else {
        return Availability::Absent;
    };
    if lockfile_matches_canonical(&lock) && probe_bundle(&gopls_binary(&dir)) {
        Availability::Present(dir)
    } else {
        Availability::Absent
    }
}

fn lockfile_matches_canonical(operator_lock: &str) -> bool {
    canonical_lockfile_sha256() == crate::schema::sha256_hex(operator_lock.as_bytes())
}

/// Where the operator's `go install tool` put the binary.
fn gopls_binary(tool_bundle: &Path) -> PathBuf {
    tool_bundle.join("bin/gopls")
}

/// Verify the pinned gopls binary launches. `version` reads nothing but
/// the binary's own build information.
fn probe_bundle(binary: &Path) -> bool {
    let mut probe = Command::new(binary);
    probe.arg("version").env("GOTELEMETRY", "off");
    bounded_probe(probe, PROBE_TIMEOUT)
        .is_ok_and(|out| out.status.success() && !out.stdout.is_empty())
}

/// Probe deadline: a stalled toolchain must not hang `--calls=required`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// The operator's Go toolchain, as `go env` reports it: gopls needs the
/// standard library (`GOROOT`), the module cache for non-vendored
/// dependencies (`GOMODCACHE`), and `GOPATH` to find both, and codemap
/// needs `GOROOT` and `GOMODCACHE` to classify definitions that land
/// outside the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoEnv {
    pub goroot: PathBuf,
    pub gopath: PathBuf,
    pub gomodcache: PathBuf,
}

impl GoEnv {
    /// Read the toolchain environment once, in the operator's environment.
    pub fn capture() -> Result<Self> {
        let mut probe = Command::new("go");
        probe.args(["env", "GOROOT", "GOPATH", "GOMODCACHE"]);
        let out = bounded_probe(probe, PROBE_TIMEOUT).context("spawn go env probe")?;
        if !out.status.success() {
            bail!("go env GOROOT GOPATH GOMODCACHE failed ({})", out.status);
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines().map(str::trim);
        let (Some(goroot), Some(gopath), Some(gomodcache)) =
            (lines.next(), lines.next(), lines.next())
        else {
            bail!("go env did not report GOROOT, GOPATH, and GOMODCACHE");
        };
        if goroot.is_empty() || gopath.is_empty() || gomodcache.is_empty() {
            bail!("go env reported an empty GOROOT, GOPATH, or GOMODCACHE");
        }
        Ok(Self {
            goroot: PathBuf::from(goroot),
            gopath: PathBuf::from(gopath),
            gomodcache: PathBuf::from(gomodcache),
        })
    }
}

/// Capture the full semantic environment identity for snapshot pinning.
/// Fails closed when any version cannot be read.
pub fn capture_identity(tool_bundle: &Path) -> Result<GoToolIdentity> {
    let mut gopls_probe = Command::new(gopls_binary(tool_bundle));
    gopls_probe.arg("version").env("GOTELEMETRY", "off");
    let gopls = first_line_of(gopls_probe).context("gopls version probe failed")?;
    let mut go_probe = Command::new("go");
    go_probe.arg("version");
    let go = first_line_of(go_probe).context("go version probe failed")?;
    Ok(GoToolIdentity {
        gopls,
        go,
        tool_bundle: tool_bundle.to_path_buf(),
    })
}

/// Run a version probe with the bounded discipline and keep its first line.
fn first_line_of(probe: Command) -> Result<String> {
    let description = format!("{probe:?}");
    let out = bounded_probe(probe, PROBE_TIMEOUT).context("spawn version probe")?;
    if !out.status.success() {
        bail!("{description} failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string())
}

/// Hands out a distinct number to every throwaway home this process names.
static HOME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A throwaway home for one gopls process, removed when dropped. gopls
/// keeps a file cache under the user cache directory and the `go` command
/// a build cache; pointing `HOME` and `GOCACHE` here keeps both out of
/// operator state and out of the next run.
struct GoHome(PathBuf);

impl GoHome {
    fn create() -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let sequence = HOME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codemap-gopls-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(path.join("go-build"))
            .with_context(|| format!("create {}", path.display()))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for GoHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The gopls process as codemap launches it: the pinned binary with the
/// network cut off and its state confined to `home`.
///
/// `GOPROXY=off` refuses every module download and `GOTOOLCHAIN=local`
/// every toolchain download, so a dependency the operator has not
/// vendored or cached fails the run instead of being fetched. `GOFLAGS` is
/// dropped so the repository's own `vendor/` directory or module cache
/// decides the build mode exactly as it would for `go build`. `GOROOT`,
/// `GOPATH`, and `GOMODCACHE` come from the operator's `go env`, and the
/// toolchain's own `bin` leads `PATH` so gopls finds that `go` rather than
/// a version-manager shim that would read the redirected home.
pub fn gopls_command(tool_bundle: &Path, home: &Path, env: &GoEnv) -> Command {
    let mut command = Command::new(gopls_binary(tool_bundle));
    let path = {
        let mut entries = vec![env.goroot.join("bin")];
        if let Some(existing) = std::env::var_os("PATH") {
            entries.extend(std::env::split_paths(&existing));
        }
        std::env::join_paths(entries).unwrap_or_default()
    };
    command
        .env("HOME", home)
        .env("GOCACHE", home.join("go-build"))
        .env("GOROOT", &env.goroot)
        .env("GOPATH", &env.gopath)
        .env("GOMODCACHE", &env.gomodcache)
        .env("GOPROXY", "off")
        .env("GOTOOLCHAIN", "local")
        .env("GOTELEMETRY", "off")
        .env("PATH", path)
        .env_remove("GOFLAGS");
    command
}

/// An owned LSP client connected to a running gopls server. gopls answers
/// definition requests concurrently, so one client serves every lane.
pub struct Client {
    transport: Transport,
    root_uri: String,
    _home: GoHome,
}

impl Client {
    /// Spawn gopls for `workspace_root` under the hermetic tool bundle and
    /// complete the LSP initialize handshake. The workspace load starts on
    /// the `initialized` notification; call [`Self::wait_loaded`] after
    /// opening the documents and before definition queries. Every
    /// server-initiated request is answered with `null`: codemap never
    /// serves workspace queries back.
    pub fn spawn(workspace_root: &Path, tool_bundle: &Path, env: &GoEnv) -> Result<Self> {
        let home = GoHome::create()?;
        let mut command = gopls_command(tool_bundle, home.path(), env);
        command.current_dir(workspace_root).stderr(Stdio::inherit());
        let hook: ServerRequestHook = Box::new(|_| Some(Value::Null));
        let transport = Transport::spawn("gopls", &mut command, Some(hook))?;
        let root_uri = uri_from_path(workspace_root);
        let client = Self {
            transport,
            root_uri,
            _home: home,
        };

        let _init_result = client
            .transport
            .request("initialize", &initialization_params(&client.root_uri))
            .context("gopls initialize failed")?;
        client
            .transport
            .notify("initialized", &json!({}))
            .context("notify gopls initialized")?;
        Ok(client)
    }

    /// Open a document so gopls serves definition queries against the
    /// pinned source bytes.
    pub fn open_document(&self, rel_path: &str, text: &str) -> Result<()> {
        self.transport.notify(
            "textDocument/didOpen",
            &json!({
                "textDocument": {
                    "uri": document_uri(&self.root_uri, rel_path),
                    "languageId": "go",
                    "version": 1,
                    "text": text,
                }
            }),
        )
    }

    /// Block until gopls has loaded the workspace and diagnosed every
    /// opened document, or `timeout` elapses. A workspace gopls could not
    /// load fails here, before any definition is asked.
    pub fn wait_loaded(&self, opened: &[String], timeout: Duration) -> Result<()> {
        let expected: BTreeSet<String> = opened
            .iter()
            .map(|rel_path| document_uri(&self.root_uri, rel_path))
            .collect();
        let rx = self.transport.unsolicited()?;
        wait_loaded(&rx, &expected, timeout)
    }

    /// Request the definition of the symbol at `(rel_path, line, character)`.
    /// `line` and `character` are 0-based; `character` is a UTF-16 offset.
    pub fn definition(
        &self,
        rel_path: &str,
        line: u32,
        character: u32,
    ) -> Result<DefinitionResult> {
        let params = json!({
            "textDocument": { "uri": document_uri(&self.root_uri, rel_path) },
            "position": { "line": line, "character": character },
        });
        let value = self.transport.request("textDocument/definition", &params)?;
        parse_definition_response("gopls", &value)
    }

    /// Cleanly shut down the server.
    pub fn shutdown(&self) {
        self.transport.shutdown();
    }
}

impl DefinitionSource for Client {
    fn definition(&self, request: &DefinitionRequest) -> Result<DefinitionResult> {
        Self::definition(self, &request.rel_path, request.line, request.character)
    }
}

fn initialization_params(root_uri: &str) -> Value {
    json!({
        "processId": null,
        "rootUri": root_uri,
        "workspaceFolders": [{ "uri": root_uri, "name": "codemap" }],
        "capabilities": {
            "textDocument": {
                "definition": { "linkSupport": false },
                "publishDiagnostics": {}
            },
            // Enables the `$/progress` workspace-load sequence that
            // [`Client::wait_loaded`] uses as its quiescence signal.
            "window": { "workDoneProgress": true },
            "workspace": { "workspaceFolders": true }
        },
        "initializationOptions": {},
    })
}

/// The title gopls gives the progress sequence of its initial workspace
/// load ("Setting up workspace", "Loading packages...", "Finished loading
/// packages.").
const WORKSPACE_LOAD_TITLE: &str = "Setting up workspace";

/// Whether a diagnostic or message names a workspace gopls could not load
/// rather than an ordinary compile error in the analyzed code.
fn load_failure(message: &str) -> bool {
    [
        "could not import",
        "no required module provides package",
        "cannot find module",
        "is not in std",
        "error loading workspace",
        "packages.Load error",
        "go.mod file not found",
        "inconsistent vendoring",
        "cannot find package",
    ]
    .iter()
    .any(|signature| message.contains(signature))
}

/// Block until the workspace-load progress ends and every expected
/// document has been diagnosed, collecting load failures on the way.
fn wait_loaded(
    rx: &mpsc::Receiver<Value>,
    expected: &BTreeSet<String>,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut load_token: Option<Value> = None;
    let mut loaded = false;
    let mut diagnosed: BTreeSet<String> = BTreeSet::new();
    let mut failures: Vec<String> = Vec::new();
    while !(loaded && expected.iter().all(|uri| diagnosed.contains(uri))) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!(
                "gopls did not finish loading the workspace: load {}, {} of {} documents diagnosed",
                if loaded { "finished" } else { "unfinished" },
                diagnosed.len(),
                expected.len()
            );
        }
        let msg = match rx.recv_timeout(remaining) {
            Ok(msg) => msg,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("gopls stdout reader disconnected before the workspace loaded");
            }
        };
        let params = msg.get("params");
        match msg.get("method").and_then(Value::as_str) {
            Some("$/progress") => {
                let value = params.and_then(|p| p.get("value"));
                let kind = value.and_then(|v| v.get("kind")).and_then(Value::as_str);
                let token = params.and_then(|p| p.get("token")).cloned();
                match kind {
                    Some("begin")
                        if value
                            .and_then(|v| v.get("title"))
                            .and_then(Value::as_str)
                            .is_some_and(|title| title.starts_with(WORKSPACE_LOAD_TITLE)) =>
                    {
                        load_token = token;
                    }
                    Some("end") if token.is_some() && token == load_token => {
                        loaded = true;
                        if let Some(message) =
                            value.and_then(|v| v.get("message")).and_then(Value::as_str)
                        {
                            if load_failure(message) {
                                failures.push(message.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some("window/showMessage") => {
                let is_error =
                    params.and_then(|p| p.get("type")).and_then(Value::as_u64) == Some(1);
                if let Some(message) = params
                    .and_then(|p| p.get("message"))
                    .and_then(Value::as_str)
                {
                    if is_error || load_failure(message) {
                        failures.push(message.to_string());
                    }
                }
            }
            Some("textDocument/publishDiagnostics") => {
                if let Some(uri) = params.and_then(|p| p.get("uri")).and_then(Value::as_str) {
                    diagnosed.insert(uri.to_string());
                }
                for diagnostic in params
                    .and_then(|p| p.get("diagnostics"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(message) = diagnostic.get("message").and_then(Value::as_str) {
                        if load_failure(message) {
                            failures.push(message.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if !failures.is_empty() {
        failures.sort();
        failures.dedup();
        bail!(
            "gopls could not load the workspace; codemap never resolves calls against \
             part of it. Vendor the dependencies or run `go mod download` as operator \
             setup, then re-run. gopls reported:\n  - {}",
            failures.join("\n  - ")
        );
    }
    Ok(())
}

// ───────────────────────── admission ─────────────────────────

/// Codemap's gates over gopls answers. The resolver is exact; the gates
/// keep an edge from pointing at anything but a callable definition
/// reached from inside a callable definition.
pub struct AdmissionModel {
    /// Every emitted `Method`/`Function` node id, for caller admission.
    callables: HashSet<String>,
}

impl AdmissionModel {
    pub fn build(nodes: &[Node]) -> Self {
        Self {
            callables: nodes
                .iter()
                .filter(|node| matches!(node.kind, NodeKind::Method | NodeKind::Function))
                .map(|node| node.id.clone())
                .collect(),
        }
    }

    pub fn is_callable(&self, id: &str) -> bool {
        self.callables.contains(id)
    }
}

/// Everything one semantic Go call resolution needs, shared across sites.
pub struct ResolveContext<'a> {
    pub workspace_root: &'a Path,
    pub identity: &'a SnapshotIdentity,
    pub rel_paths: &'a HashSet<String>,
    pub file_sources: &'a BTreeMap<&'a str, &'a [u8]>,
    pub node_index: &'a NodeIndex,
    pub callable_targets: &'a BTreeMap<String, BTreeMap<usize, String>>,
    pub env: &'a GoEnv,
}

/// One pending Go call site with its file identity.
pub struct PendingGoCall {
    pub rel_path: String,
    pub file_hash: String,
    pub call: crate::rust_syntax::RawCall,
}

/// Admit or reject one answered call site under the G2 policy. Returns
/// the category to report; appends a canonical edge when admitted. Pure:
/// the same answer always yields the same category and edge.
pub fn admit_definition(
    result: DefinitionResult,
    model: &AdmissionModel,
    ctx: &ResolveContext<'_>,
    pending: &PendingGoCall,
    emitted_edge_ids: &mut HashSet<String>,
    edges: &mut Vec<Edge>,
) -> CallSiteCategory {
    let rel_path = &pending.rel_path;
    let call = &pending.call;

    let locations = match result {
        DefinitionResult::Empty => Vec::new(),
        DefinitionResult::Locations(locs) => locs,
    };

    // Targets must land at an exact parser declaration-name start.
    let mut in_repo: Vec<Option<String>> = Vec::new();
    let mut external_uris: BTreeSet<String> = BTreeSet::new();
    for loc in &locations {
        match rel_path_of_uri(&loc.uri, ctx.workspace_root).filter(|rp| ctx.rel_paths.contains(rp))
        {
            Some(target_rel) => {
                let source = ctx.file_sources.get(target_rel.as_str()).copied();
                let start = source.map_or(0, |s| {
                    crate::lsp::lsp_pos_to_byte(s, loc.start_line, loc.start_character)
                });
                in_repo.push(
                    ctx.callable_targets
                        .get(&target_rel)
                        .and_then(|targets| targets.get(&start))
                        .cloned(),
                );
            }
            None => {
                external_uris.insert(loc.uri.clone());
            }
        }
    }

    match in_repo.as_slice() {
        [] => zero_candidate_category(&external_uris, ctx.env),
        [Some(to_id)] if external_uris.is_empty() && model.is_callable(to_id) => {
            let Some(from_id) = ctx
                .node_index
                .find(rel_path, call.name_start_byte)
                .filter(|id| model.is_callable(id))
            else {
                // Resolved to a callable, but the call has no callable
                // around it (a package-level initializer): not an edge.
                return CallSiteCategory::Candidate;
            };
            let edge = Edge {
                id: edge_id(EdgeKind::Calls, from_id, to_id, call.name_start_byte),
                kind: EdgeKind::Calls,
                from: from_id.to_string(),
                to: to_id.clone(),
                provenance: calls_provenance(ctx.identity, rel_path, call.span, &pending.file_hash),
            };
            if emitted_edge_ids.insert(edge.id.clone()) {
                edges.push(edge);
            }
            CallSiteCategory::Resolved
        }
        // A type (conversion), a field, a variable or parameter declared
        // inside a callable (function value, method value), or a position
        // no definition covers.
        [_] if external_uris.is_empty() => CallSiteCategory::Unmapped,
        _ => CallSiteCategory::Ambiguous,
    }
}

/// Classify a zero-candidate site by the *set* of external locations:
/// deterministic regardless of gopls's response ordering. No locations
/// → `Unmapped`; every location under `GOROOT` → `StandardLibrary`; any
/// location under the module cache or the repository's `vendor/` →
/// `ExternalCrate`.
fn zero_candidate_category(external_uris: &BTreeSet<String>, env: &GoEnv) -> CallSiteCategory {
    if external_uris.is_empty() {
        return CallSiteCategory::Unmapped;
    }
    let classes: Vec<CallSiteCategory> = external_uris
        .iter()
        .map(|uri| classify_external(uri, env))
        .collect();
    if classes.contains(&CallSiteCategory::ExternalCrate) {
        CallSiteCategory::ExternalCrate
    } else if classes.contains(&CallSiteCategory::StandardLibrary) {
        CallSiteCategory::StandardLibrary
    } else {
        CallSiteCategory::Unmapped
    }
}

/// Classify a definition location outside the repository inventory: the
/// standard library lives under `GOROOT`; dependencies under the module
/// cache or a `vendor` directory.
pub fn classify_external(uri: &str, env: &GoEnv) -> CallSiteCategory {
    let path = PathBuf::from(crate::lsp::path_from_uri(uri));
    if path.starts_with(&env.goroot) {
        return CallSiteCategory::StandardLibrary;
    }
    if path.starts_with(&env.gomodcache)
        || path
            .components()
            .any(|component| component.as_os_str() == "vendor")
    {
        return CallSiteCategory::ExternalCrate;
    }
    CallSiteCategory::Unmapped
}

/// Provenance for a semantic Go `calls` edge: authoritative gopls source.
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
        fact_source: FactSource::Gopls,
        confidence: 255,
    }
}

fn rel_path_of_uri(uri: &str, workspace_root: &Path) -> Option<String> {
    let abs = crate::lsp::path_from_uri(uri);
    let abs_path = Path::new(&abs);
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    abs_path
        .strip_prefix(&canonical_root)
        .or_else(|_| abs_path.strip_prefix(workspace_root))
        .ok()
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::DefinitionLocation;

    fn env() -> GoEnv {
        GoEnv {
            goroot: PathBuf::from("/toolchains/go"),
            gopath: PathBuf::from("/home/op/go"),
            gomodcache: PathBuf::from("/home/op/go/pkg/mod"),
        }
    }

    #[test]
    fn every_gopls_launch_is_cut_off_from_the_network_and_operator_state() {
        let bundle = Path::new("/bundle");
        let home = Path::new("/isolated-home");
        let command = gopls_command(bundle, home, &env());

        assert_eq!(
            command.get_program(),
            bundle.join("bin/gopls").as_os_str(),
            "the pinned binary, never one from PATH"
        );
        let envs: BTreeMap<_, _> = command.get_envs().collect();
        let value = |key: &str| {
            envs.get(std::ffi::OsStr::new(key))
                .copied()
                .flatten()
                .map(|v| v.to_string_lossy().to_string())
        };
        assert_eq!(value("GOPROXY").as_deref(), Some("off"));
        assert_eq!(value("GOTOOLCHAIN").as_deref(), Some("local"));
        assert_eq!(value("GOTELEMETRY").as_deref(), Some("off"));
        assert_eq!(value("HOME").as_deref(), Some("/isolated-home"));
        assert_eq!(value("GOCACHE").as_deref(), Some("/isolated-home/go-build"));
        assert_eq!(value("GOROOT").as_deref(), Some("/toolchains/go"));
        assert_eq!(value("GOMODCACHE").as_deref(), Some("/home/op/go/pkg/mod"));
        assert_eq!(
            envs.get(std::ffi::OsStr::new("GOFLAGS")),
            Some(&None),
            "the repository decides vendor mode, not the operator's GOFLAGS"
        );
        assert!(
            value("PATH")
                .unwrap_or_default()
                .starts_with("/toolchains/go/bin"),
            "the toolchain's own go leads PATH"
        );
    }

    #[test]
    fn a_go_home_is_a_fresh_directory_that_leaves_with_its_owner() {
        let path = {
            let home = GoHome::create().expect("create");
            assert!(home.path().join("go-build").is_dir());
            home.path().to_path_buf()
        };
        assert!(!path.exists(), "dropped home must be removed");
    }

    #[test]
    fn load_failure_signatures_name_missing_workspace_pieces_not_compile_errors() {
        assert!(load_failure(
            "could not import github.com/x/y (no required module provides package \"github.com/x/y\")"
        ));
        assert!(load_failure("error loading workspace: err: exit status 1"));
        assert!(!load_failure("undefined: helper"));
        assert!(!load_failure("declared and not used: x"));
    }

    fn progress(token: &str, kind: &str, title: Option<&str>, message: Option<&str>) -> Value {
        let mut value = json!({"kind": kind});
        if let Some(title) = title {
            value["title"] = json!(title);
        }
        if let Some(message) = message {
            value["message"] = json!(message);
        }
        json!({"method": "$/progress", "params": {"token": token, "value": value}})
    }

    fn diagnostics(uri: &str, messages: &[&str]) -> Value {
        let diagnostics: Vec<Value> = messages
            .iter()
            .map(|m| json!({"severity": 1, "message": m}))
            .collect();
        json!({"method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": diagnostics}})
    }

    #[test]
    fn wait_loaded_releases_after_the_load_ends_and_every_document_is_diagnosed() {
        let (tx, rx) = mpsc::channel::<Value>();
        let expected: BTreeSet<String> = ["file:///r/a.go", "file:///r/b.go"]
            .iter()
            .map(ToString::to_string)
            .collect();
        // A foreign progress sequence, then the workspace load, then the
        // diagnostics gopls publishes after it.
        tx.send(progress("other", "begin", Some("Indexing"), None))
            .unwrap();
        tx.send(progress("other", "end", None, None)).unwrap();
        tx.send(progress(
            "load",
            "begin",
            Some(WORKSPACE_LOAD_TITLE),
            Some("Loading packages..."),
        ))
        .unwrap();
        tx.send(progress(
            "load",
            "end",
            None,
            Some("Finished loading packages."),
        ))
        .unwrap();
        tx.send(diagnostics("file:///r/a.go", &["declared and not used: x"]))
            .unwrap();
        tx.send(diagnostics("file:///r/b.go", &[])).unwrap();
        wait_loaded(&rx, &expected, Duration::from_secs(5)).expect("loaded");
    }

    #[test]
    fn wait_loaded_fails_closed_on_a_workspace_gopls_could_not_load() {
        let (tx, rx) = mpsc::channel::<Value>();
        let expected: BTreeSet<String> = std::iter::once("file:///r/a.go".to_string()).collect();
        tx.send(progress("load", "begin", Some(WORKSPACE_LOAD_TITLE), None))
            .unwrap();
        tx.send(progress(
            "load",
            "end",
            None,
            Some("Finished loading packages."),
        ))
        .unwrap();
        tx.send(diagnostics(
            "file:///r/a.go",
            &["could not import example.invalid/missing (no required module provides package \"example.invalid/missing\")"],
        ))
        .unwrap();
        let error = wait_loaded(&rx, &expected, Duration::from_secs(5)).expect_err("load failure");
        assert!(
            format!("{error:#}").contains("no required module"),
            "{error:#}"
        );

        let (tx, rx) = mpsc::channel::<Value>();
        tx.send(json!({"method": "window/showMessage", "params": {"type": 1, "message": "error loading workspace: go.mod file not found"}}))
            .unwrap();
        tx.send(progress("load", "begin", Some(WORKSPACE_LOAD_TITLE), None))
            .unwrap();
        tx.send(progress("load", "end", None, None)).unwrap();
        tx.send(diagnostics("file:///r/a.go", &[])).unwrap();
        let error = wait_loaded(&rx, &expected, Duration::from_secs(5)).expect_err("load failure");
        assert!(
            format!("{error:#}").contains("go.mod file not found"),
            "{error:#}"
        );
    }

    #[test]
    fn wait_loaded_times_out_when_a_document_is_never_diagnosed() {
        let (tx, rx) = mpsc::channel::<Value>();
        let expected: BTreeSet<String> = std::iter::once("file:///r/a.go".to_string()).collect();
        tx.send(progress("load", "begin", Some(WORKSPACE_LOAD_TITLE), None))
            .unwrap();
        tx.send(progress("load", "end", None, None)).unwrap();
        let error = wait_loaded(&rx, &expected, Duration::from_millis(50)).expect_err("timed out");
        assert!(
            format!("{error:#}").contains("0 of 1 documents"),
            "{error:#}"
        );
    }

    #[test]
    fn external_locations_classify_by_goroot_module_cache_and_vendor() {
        let env = env();
        assert_eq!(
            classify_external("file:///toolchains/go/src/os/proc.go", &env),
            CallSiteCategory::StandardLibrary
        );
        assert_eq!(
            classify_external(
                "file:///home/op/go/pkg/mod/github.com/x/y@v1.0.0/y.go",
                &env
            ),
            CallSiteCategory::ExternalCrate
        );
        assert_eq!(
            classify_external("file:///work/repo/vendor/github.com/x/y/y.go", &env),
            CallSiteCategory::ExternalCrate
        );
        assert_eq!(
            classify_external("file:///elsewhere/z.go", &env),
            CallSiteCategory::Unmapped
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one fixture, several answers, one assertion each
    fn admission_is_a_pure_function_of_the_answer() {
        use crate::rust_syntax::{RawCall, ReceiverShape};
        use crate::schema::{Language, RepoIdentity, ToolVersions, TypeKind, node_id, symbol_key};

        let language = Language::Go;
        let identity = SnapshotIdentity::build(
            language,
            RepoIdentity {
                id: "https://example.invalid/fixture.git".into(),
                origin: None,
                is_canonical: true,
            },
            "c0ffee".into(),
            "tree".into(),
            ToolVersions::syntax_go(),
            false,
        );
        let span = |start: usize, end: usize| Span {
            start_byte: start,
            end_byte: end,
            start_line: 1,
            start_col: 0,
            end_line: 1,
            end_col: 0,
        };
        let node = |kind: NodeKind, rel_path: &str, scope: &[&str], name: &str, s: Span| {
            let scope: Vec<String> = scope.iter().map(ToString::to_string).collect();
            Node {
                python: None,
                id: node_id(language, kind, rel_path, &scope, name),
                kind,
                name: name.to_string(),
                symbol_key: symbol_key(language, rel_path, &scope, name),
                provenance: Provenance {
                    repository: identity.repository.id.clone(),
                    commit_sha: identity.commit_sha.clone(),
                    rel_path: rel_path.to_string(),
                    span: s,
                    file_hash: "h".into(),
                    extractor_version: identity.extractor_version.clone(),
                    fact_source: FactSource::TreeSitter,
                    confidence: u8::MAX,
                },
            }
        };
        let target = node(
            NodeKind::Function,
            "lib/callee.go",
            &["lib"],
            "New",
            span(100, 200),
        );
        let kind = node(
            NodeKind::Type(TypeKind::Defined),
            "lib/callee.go",
            &["lib"],
            "ID",
            span(210, 230),
        );
        let origin = node(
            NodeKind::Function,
            "cmd/main.go",
            &["main"],
            "main",
            span(0, 100),
        );
        let nodes = vec![target.clone(), kind, origin.clone()];
        let model = AdmissionModel::build(&nodes);

        let workspace = tempfile::tempdir().expect("workspace");
        // `New` is declared at bytes 150..153 and `ID` at 220..222; the
        // rest of the callee file is filler.
        let mut target_bytes = vec![b'x'; 300];
        target_bytes[150..153].copy_from_slice(b"New");
        target_bytes[170..173].copy_from_slice(b"New");
        target_bytes[220..222].copy_from_slice(b"ID");
        let origin_bytes = vec![b'y'; 200];
        let file_sources: BTreeMap<&str, &[u8]> = [
            ("lib/callee.go", target_bytes.as_slice()),
            ("cmd/main.go", origin_bytes.as_slice()),
        ]
        .into_iter()
        .collect();
        let rel_paths: HashSet<String> = file_sources.keys().map(ToString::to_string).collect();
        let node_index = NodeIndex::build(nodes.iter().map(|n| {
            (
                n.provenance.rel_path.clone(),
                n.provenance.span,
                n.id.clone(),
            )
        }));
        let env = env();
        let callable_targets = BTreeMap::from([(
            "lib/callee.go".to_owned(),
            BTreeMap::from([(150, target.id.clone())]),
        )]);
        let ctx = ResolveContext {
            workspace_root: workspace.path(),
            identity: &identity,
            rel_paths: &rel_paths,
            file_sources: &file_sources,
            node_index: &node_index,
            callable_targets: &callable_targets,
            env: &env,
        };
        let pending_at = |name_start: usize| PendingGoCall {
            rel_path: "cmd/main.go".into(),
            file_hash: "h".into(),
            call: RawCall {
                raw_name: "lib.New".into(),
                path_root: None,
                span: span(name_start, name_start + 7),
                name_start_byte: name_start,
                receiver: ReceiverShape::Other,
                scope: Vec::new(),
            },
        };
        let at = |character: u32| DefinitionLocation {
            uri: format!("{}/lib/callee.go", uri_from_path(workspace.path())),
            start_line: 0,
            start_character: character,
            end_line: 0,
            end_character: character + if character == 220 { 2 } else { 3 },
        };

        let mut emitted = HashSet::new();
        let mut edges = Vec::new();
        // Inside `main`, resolving to the function `New`: an edge.
        let category = admit_definition(
            DefinitionResult::Locations(vec![at(150)]),
            &model,
            &ctx,
            &pending_at(10),
            &mut emitted,
            &mut edges,
        );
        assert_eq!(category, CallSiteCategory::Resolved);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (edges[0].from.as_str(), edges[0].to.as_str()),
            (origin.id.as_str(), target.id.as_str())
        );
        assert_eq!(edges[0].provenance.fact_source, FactSource::Gopls);

        // Resolving to the defined type `ID`: a conversion, never an edge.
        let category = admit_definition(
            DefinitionResult::Locations(vec![at(220)]),
            &model,
            &ctx,
            &pending_at(10),
            &mut emitted,
            &mut edges,
        );
        assert_eq!(category, CallSiteCategory::Unmapped);

        // Resolving to something declared inside `New` (a local, a
        // parameter): even the same name is not the callable declaration.
        let local_category = admit_definition(
            DefinitionResult::Locations(vec![at(170)]),
            &model,
            &ctx,
            &pending_at(10),
            &mut emitted,
            &mut edges,
        );

        // Outside any callable (byte 150 of main.go is past `main`):
        // resolved but not admissible.
        let category = admit_definition(
            DefinitionResult::Locations(vec![at(150)]),
            &model,
            &ctx,
            &pending_at(150),
            &mut emitted,
            &mut edges,
        );
        assert_eq!(category, CallSiteCategory::Candidate);

        // Nothing, the standard library, a dependency, two in-repo answers.
        let category = admit_definition(
            DefinitionResult::Empty,
            &model,
            &ctx,
            &pending_at(10),
            &mut emitted,
            &mut edges,
        );
        assert_eq!(category, CallSiteCategory::Unmapped);
        let stdlib = DefinitionLocation {
            uri: "file:///toolchains/go/src/builtin/builtin.go".into(),
            start_line: 178,
            start_character: 5,
            end_line: 178,
            end_character: 8,
        };
        let category = admit_definition(
            DefinitionResult::Locations(vec![stdlib.clone()]),
            &model,
            &ctx,
            &pending_at(10),
            &mut emitted,
            &mut edges,
        );
        assert_eq!(category, CallSiteCategory::StandardLibrary);
        let mixed_category = admit_definition(
            DefinitionResult::Locations(vec![at(150), stdlib]),
            &model,
            &ctx,
            &pending_at(10),
            &mut emitted,
            &mut edges,
        );
        let vendored = DefinitionLocation {
            uri: format!(
                "{}/vendor/github.com/x/y/y.go",
                uri_from_path(workspace.path())
            ),
            start_line: 0,
            start_character: 0,
            end_line: 0,
            end_character: 1,
        };
        let category = admit_definition(
            DefinitionResult::Locations(vec![vendored]),
            &model,
            &ctx,
            &pending_at(10),
            &mut emitted,
            &mut edges,
        );
        assert_eq!(category, CallSiteCategory::ExternalCrate);
        let category = admit_definition(
            DefinitionResult::Locations(vec![at(150), at(220)]),
            &model,
            &ctx,
            &pending_at(10),
            &mut emitted,
            &mut edges,
        );
        assert_eq!(category, CallSiteCategory::Ambiguous);
        assert_eq!(
            (local_category, mixed_category),
            (CallSiteCategory::Unmapped, CallSiteCategory::Ambiguous),
            "same-name locals and mixed answers cannot prove a callable target"
        );
        assert_eq!(edges.len(), 1, "only the admitted answer became an edge");
    }
}
