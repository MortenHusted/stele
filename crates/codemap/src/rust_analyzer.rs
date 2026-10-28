//! Local stdio JSON-RPC client for `rust-analyzer` (LSP).
//!
//! Owns the LSP process lifecycle and the `textDocument/definition`
//! request that turns a call-site span into a resolved target. It never
//! guesses: if rust-analyzer is absent, returns [`Availability::Absent`]
//! so the extractor can fail closed. It never auto-installs or mutates the
//! caller's rustup/Cargo environment.
//!
//! The client is deliberately synchronous: one request at a time, with a
//! reader thread that drains the server's stdout and a channel the caller
//! reads until its response id arrives (skipping server notifications).

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::lsp::{
    DefinitionLocation, DefinitionRequest, DefinitionResult, DefinitionSource, Transport,
    document_uri, parse_definition_response, path_from_uri, uri_from_path,
};
use crate::schema::{CallSiteCategory, RustAnalyzerPolicy};

/// Whether rust-analyzer is available to the extractor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Present(String),
    Absent,
}

/// Companion compiler identity selected for the semantic workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustcIdentity {
    pub version: String,
    pub host_triple: String,
}

/// Detect the rust-analyzer selected for `workspace_root`.
pub fn detect_at(workspace_root: &Path) -> Availability {
    let out = Command::new("rust-analyzer")
        .current_dir(workspace_root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("RUSTUP_NO_UPDATE_CHECK", "1")
        .arg("--version")
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("rust-analyzer (unknown)")
                .trim()
                .to_string();
            Availability::Present(v)
        }
        _ => Availability::Absent,
    }
}

/// Capture the rustc selected for the same semantic workspace. Semantic
/// extraction fails closed when its version or host triple cannot be read.
pub fn companion_rustc(workspace_root: &Path) -> Result<RustcIdentity> {
    let output = Command::new("rustc")
        .current_dir(workspace_root)
        .env("RUSTUP_NO_UPDATE_CHECK", "1")
        .arg("-vV")
        .output()
        .context("failed to run companion rustc -vV")?;
    if !output.status.success() {
        bail!(
            "companion rustc -vV failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8(output.stdout).context("rustc -vV was not UTF-8")?;
    let version = stdout
        .lines()
        .next()
        .filter(|line| !line.trim().is_empty())
        .context("rustc -vV omitted version")?
        .trim()
        .to_string();
    let host_triple = stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| !host.is_empty())
        .context("rustc -vV omitted host triple")?
        .to_string();
    Ok(RustcIdentity {
        version,
        host_triple,
    })
}

/// Capture the Cargo selected for dependency inventory in the semantic
/// workspace. Its version participates in snapshot identity.
pub fn companion_cargo(workspace_root: &Path) -> Result<String> {
    let output = Command::new("cargo")
        .current_dir(workspace_root)
        .env("RUSTUP_NO_UPDATE_CHECK", "1")
        .arg("-V")
        .output()
        .context("failed to run companion cargo -V")?;
    if !output.status.success() {
        bail!(
            "companion cargo -V failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout)
        .context("cargo -V was not UTF-8")?
        .lines()
        .next()
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .map(str::to_owned)
        .context("cargo -V omitted version")
}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct CargoPackage {
    id: String,
    manifest_path: std::path::PathBuf,
    dependencies: Vec<CargoDependency>,
}

#[derive(Deserialize)]
struct CargoDependency {
    name: String,
    rename: Option<String>,
    path: Option<std::path::PathBuf>,
}

#[derive(Debug)]
pub struct WorkspaceDependencyRoots {
    packages: Vec<PackageDependencyRoots>,
}

#[derive(Debug)]
struct PackageDependencyRoots {
    rel_root: std::path::PathBuf,
    roots: HashSet<String>,
}

impl WorkspaceDependencyRoots {
    pub fn is_external(&self, caller_rel_path: &str, root: &str) -> bool {
        let caller = Path::new(caller_rel_path);
        self.packages
            .iter()
            .find(|package| caller.starts_with(&package.rel_root))
            .is_some_and(|package| package.roots.contains(root))
    }
}

/// Explicit dependency roots available to workspace packages. Cargo metadata
/// supplies rename and path semantics; only dependencies outside the
/// repository can classify an unresolved qualified call as external.
pub fn external_crate_roots(workspace_root: &Path) -> Result<WorkspaceDependencyRoots> {
    let output = Command::new("cargo")
        .current_dir(workspace_root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("RUSTUP_NO_UPDATE_CHECK", "1")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .output()
        .context("failed to run cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let metadata: CargoMetadata =
        serde_json::from_slice(&output.stdout).context("parse cargo metadata")?;
    Ok(dependency_roots_from_metadata(&metadata, workspace_root))
}

fn dependency_roots_from_metadata(
    metadata: &CargoMetadata,
    workspace_root: &Path,
) -> WorkspaceDependencyRoots {
    let workspace_members: HashSet<_> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let mut packages: Vec<_> = metadata
        .packages
        .iter()
        .filter(|package| workspace_members.contains(package.id.as_str()))
        .filter_map(|package| {
            let rel_root = package
                .manifest_path
                .parent()?
                .strip_prefix(workspace_root)
                .ok()?
                .to_path_buf();
            let roots = package
                .dependencies
                .iter()
                .filter(|dependency| {
                    dependency
                        .path
                        .as_ref()
                        .is_none_or(|path| !path.starts_with(workspace_root))
                })
                .map(|dependency| {
                    dependency
                        .rename
                        .as_deref()
                        .unwrap_or(&dependency.name)
                        .replace('-', "_")
                })
                .collect();
            Some(PackageDependencyRoots { rel_root, roots })
        })
        .collect();
    packages.sort_by(|left, right| {
        right
            .rel_root
            .components()
            .count()
            .cmp(&left.rel_root.components().count())
            .then(left.rel_root.cmp(&right.rel_root))
    });
    WorkspaceDependencyRoots { packages }
}

/// An owned LSP client connected to a running rust-analyzer server.
///
/// rust-analyzer serves read requests from a thread pool over an immutable
/// analysis snapshot, so one client is shared by every definition lane once
/// the server is quiescent and the crate graph complete; the transport's
/// per-request routing is what makes that safe.
pub struct Client {
    transport: Transport,
    root_uri: String,
}

impl Client {
    /// Spawn rust-analyzer for `workspace_root` and complete the LSP
    /// initialize handshake.
    pub fn spawn(workspace_root: &Path, target_triple: &str) -> Result<Self> {
        let mut command = rust_analyzer_command();
        command.current_dir(workspace_root);
        let transport = Transport::spawn("rust-analyzer", &mut command, None)?;
        let root_uri = uri_from_path(workspace_root);
        let client = Self {
            transport,
            root_uri,
        };

        let _init_result = client
            .transport
            .request(
                "initialize",
                &initialization_params(&client.root_uri, target_triple),
            )
            .context("rust-analyzer initialize failed")?;
        client
            .transport
            .notify("initialized", &json!({}))
            .context("notify rust-analyzer initialized")?;
        Ok(client)
    }

    /// Open a document so rust-analyzer indexes it.
    pub fn open_document(&self, rel_path: &str, text: &str) -> Result<()> {
        self.transport.notify(
            "textDocument/didOpen",
            &json!({
                "textDocument": {
                    "uri": document_uri(&self.root_uri, rel_path),
                    "languageId": "rust",
                    "version": 1,
                    "text": text,
                }
            }),
        )
    }

    /// Block until rust-analyzer reports it is quiescent (indexing
    /// complete), or `timeout` elapses. Drains server notifications; safe
    /// to call when no request is pending.
    pub fn wait_quiescent(&self, timeout: Duration) -> Result<()> {
        let rx = self.transport.unsolicited()?;
        wait_for_quiescent(&rx, timeout)
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
        parse_definition_response("rust-analyzer", &value)
    }

    /// Fetch the server's crate graph as Graphviz DOT. Used to verify that
    /// the graph is complete (workspace crates plus the sysroot `std` crate)
    /// before definition queries are trusted.
    pub fn view_crate_graph(&self) -> Result<String> {
        let value = self
            .transport
            .request("rust-analyzer/viewCrateGraph", &json!({ "full": true }))?;
        value
            .as_str()
            .map(str::to_string)
            .context("rust-analyzer viewCrateGraph returned a non-string result")
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

/// Build a rust-analyzer process that cannot fetch crates and whose stderr
/// cannot fill an unread pipe. The LSP configuration below additionally turns
/// off build scripts, proc macros, and background checks.
fn rust_analyzer_command() -> Command {
    let policy = RustAnalyzerPolicy::fixed();
    let mut command = Command::new("rust-analyzer");
    command
        .env("CARGO_NET_OFFLINE", policy.cargo_offline.to_string())
        .env("RUSTUP_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    command
}

fn initialization_params(root_uri: &str, target_triple: &str) -> Value {
    let policy = RustAnalyzerPolicy::fixed();
    json!({
        "processId": null,
        "rootUri": root_uri,
        "capabilities": {
            "textDocument": {
                "definition": { "linkSupport": true }
            },
            "experimental": {
                "serverStatusNotification": true
            }
        },
        "workspaceFolders": null,
        // `autoreload` must stay enabled even though the extracted worktree
        // is immutable: rust-analyzer builds its crate graph once, at
        // startup, before its file loader has populated the VFS, which
        // produces an EMPTY graph (every crate root file is missing). The
        // server only rebuilds the graph when the loader later delivers
        // those files if workspace autoreload is permitted; without that
        // rebuild the standard library is never attached and nearly all
        // receiver inference fails. The rebuild re-runs `cargo metadata`
        // only — no build scripts, proc macros, or checks execute — so the
        // code-execution-free policy is unchanged.
        "initializationOptions": {
            "cargo": {
                "buildScripts": { "enable": policy.build_scripts_enabled },
                "autoreload": true,
                "features": [],
                "noDefaultFeatures": false,
                "target": target_triple
            },
            "procMacro": { "enable": policy.proc_macros_enabled },
            // The key is `checkOnSave`; there is no `check.enable` setting,
            // so disabling checks must use this exact name or rust-analyzer
            // silently keeps background `cargo check` running — which would
            // execute build scripts against the extracted worktree.
            "checkOnSave": policy.background_checks_enabled
        },
    })
}

fn wait_for_quiescent(rx: &mpsc::Receiver<Value>, timeout: Duration) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    let quiet_window = timeout.min(Duration::from_millis(500));
    let mut indexing_started = false;
    let mut quiet_deadline = None;
    loop {
        let now = std::time::Instant::now();
        if quiet_deadline.is_some_and(|quiet: std::time::Instant| now >= quiet) {
            return Ok(());
        }
        let remaining = quiet_deadline
            .map_or(deadline, |quiet| quiet.min(deadline))
            .saturating_duration_since(now);
        if remaining.is_zero() {
            bail!("rust-analyzer quiescence timed out");
        }
        match rx.recv_timeout(remaining) {
            Ok(msg) => {
                let quiescent = (msg.get("method").and_then(Value::as_str)
                    == Some("experimental/serverStatus"))
                .then(|| {
                    msg.get("params")
                        .and_then(|params| params.get("quiescent"))
                        .and_then(Value::as_bool)
                })
                .flatten();
                match quiescent {
                    Some(false) => {
                        indexing_started = true;
                        quiet_deadline = None;
                    }
                    Some(true) if indexing_started => {
                        quiet_deadline = Some(std::time::Instant::now() + quiet_window);
                    }
                    _ if quiet_deadline.is_some() => {
                        quiet_deadline = Some(std::time::Instant::now() + quiet_window);
                    }
                    _ => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if quiet_deadline.is_some() {
                    return Ok(());
                }
                bail!("rust-analyzer quiescence timed out");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("rust-analyzer stdout reader disconnected before quiescence");
            }
        }
    }
}

// ───────────────────────── classification + mapping ─────────────────────────

/// Classify a resolved definition location against the workspace root and
/// the set of in-repository files.
pub fn classify_location(
    loc: &DefinitionLocation,
    in_repo_files: &std::collections::HashSet<String>,
) -> CallSiteCategory {
    let path = path_from_uri(&loc.uri);
    if in_repo_files.contains(&path) {
        return CallSiteCategory::Resolved;
    }
    // Standard library sources live under rustup toolchain / rust/library.
    if path.contains("rust/library")
        || path.contains("toolchains/")
        || path.contains("/lib/rustlib/")
    {
        return CallSiteCategory::StandardLibrary;
    }
    // External crates live in the cargo registry / git checkouts.
    if path.contains("/.cargo/")
        || path.contains("registry/src")
        || path.contains("registry/src/index")
    {
        return CallSiteCategory::ExternalCrate;
    }
    CallSiteCategory::ExternalCrate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_std_and_external() {
        let mut files = std::collections::HashSet::new();
        files.insert("/repo/src/lib.rs".to_string());
        let std_loc = DefinitionLocation {
            uri: "file:///Users/x/.rustup/toolchains/1.97/src/rust/library/std/src/lib.rs".into(),
            start_line: 0,
            start_character: 0,
            end_line: 0,
            end_character: 10,
        };
        assert_eq!(
            classify_location(&std_loc, &files),
            CallSiteCategory::StandardLibrary
        );
        let ext_loc = DefinitionLocation {
            uri: "file:///Users/x/.cargo/registry/src/index.crates.io/serde/lib.rs".into(),
            start_line: 0,
            start_character: 0,
            end_line: 0,
            end_character: 10,
        };
        assert_eq!(
            classify_location(&ext_loc, &files),
            CallSiteCategory::ExternalCrate
        );
        let in_loc = DefinitionLocation {
            uri: "file:///repo/src/lib.rs".into(),
            start_line: 0,
            start_character: 0,
            end_line: 0,
            end_character: 10,
        };
        assert_eq!(
            classify_location(&in_loc, &files),
            CallSiteCategory::Resolved
        );
    }

    #[test]
    fn initialization_disables_code_execution_and_network_fetches() {
        let policy = RustAnalyzerPolicy::fixed();
        let params = initialization_params("file:///repo", "test-target");
        assert_eq!(
            params.pointer("/initializationOptions/cargo/buildScripts/enable"),
            Some(&json!(policy.build_scripts_enabled))
        );
        assert_eq!(
            params.pointer("/initializationOptions/procMacro/enable"),
            Some(&json!(policy.proc_macros_enabled))
        );
        assert_eq!(
            params.pointer("/initializationOptions/checkOnSave"),
            Some(&json!(policy.background_checks_enabled))
        );
        assert_eq!(
            params.pointer("/initializationOptions/cargo/features"),
            Some(&json!([]))
        );
        // Autoreload must remain enabled: it is the only mechanism that
        // rebuilds the startup crate graph after the file loader delivers
        // the crate roots (see `initialization_params`). Disabling it
        // silently leaves an empty crate graph with no `std` attachment.
        assert_eq!(
            params.pointer("/initializationOptions/cargo/autoreload"),
            Some(&json!(true))
        );
        assert_eq!(
            params.pointer("/initializationOptions/cargo/noDefaultFeatures"),
            Some(&json!(false))
        );
        assert_eq!(
            params.pointer("/initializationOptions/cargo/target"),
            Some(&json!("test-target"))
        );
        assert_eq!(
            params.pointer("/capabilities/experimental/serverStatusNotification"),
            Some(&json!(true))
        );
        assert_eq!(
            params.pointer("/capabilities/textDocument/definition/linkSupport"),
            Some(&json!(true))
        );

        let command = rust_analyzer_command();
        let offline = command
            .get_envs()
            .find(|(key, _)| *key == "CARGO_NET_OFFLINE")
            .and_then(|(_, value)| value)
            .and_then(std::ffi::OsStr::to_str);
        let expected_offline = policy.cargo_offline.to_string();
        assert_eq!(offline, Some(expected_offline.as_str()));
    }

    #[test]
    fn a_disconnected_reader_fails_the_quiescence_wait() {
        let (quiescent_tx, quiescent_rx) = mpsc::channel::<Value>();
        drop(quiescent_tx);
        assert!(wait_for_quiescent(&quiescent_rx, Duration::from_millis(10)).is_err());
    }

    #[test]
    fn quiescent_status_completes_only_after_indexing_started() {
        let (tx, rx) = mpsc::channel();
        tx.send(json!({
            "method": "experimental/serverStatus",
            "params": { "quiescent": true }
        }))
        .expect("send stale initial status");
        tx.send(json!({
            "method": "experimental/serverStatus",
            "params": { "quiescent": false }
        }))
        .expect("send indexing status");
        tx.send(json!({
            "method": "experimental/serverStatus",
            "params": { "quiescent": true }
        }))
        .expect("send completed status");
        wait_for_quiescent(&rx, Duration::from_millis(10)).expect("quiescent");
    }

    #[test]
    fn stale_initial_quiescence_does_not_complete_wait() {
        let (tx, rx) = mpsc::channel();
        tx.send(json!({
            "method": "experimental/serverStatus",
            "params": { "quiescent": true }
        }))
        .expect("send stale status");
        drop(tx);
        assert!(wait_for_quiescent(&rx, Duration::from_millis(10)).is_err());
    }

    #[test]
    fn detect_returns_present_or_absent() {
        // Don't assert which; just that it doesn't panic.
        let _ = detect_at(Path::new("."));
    }

    #[test]
    fn cargo_metadata_excludes_workspace_path_dependencies() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root");
        let roots = external_crate_roots(&workspace_root).expect("workspace dependency roots");
        assert!(roots.is_external("crates/codemap/src/lib.rs", "serde_json"));
        assert!(!roots.is_external("crates/codemap/src/lib.rs", "codemap_graph"));
    }

    #[test]
    fn cargo_dependency_roots_are_scoped_to_the_callers_package() {
        let metadata: CargoMetadata = serde_json::from_value(json!({
            "workspace_members": ["root", "a", "b"],
            "packages": [
                {
                    "id": "root",
                    "manifest_path": "/repo/Cargo.toml",
                    "dependencies": [{"name": "root-only", "rename": null, "path": null}]
                },
                {
                    "id": "a",
                    "manifest_path": "/repo/crates/a/Cargo.toml",
                    "dependencies": [{"name": "serde-json", "rename": "json", "path": null}]
                },
                {
                    "id": "b",
                    "manifest_path": "/repo/crates/b/Cargo.toml",
                    "dependencies": [{"name": "regex", "rename": null, "path": null}]
                }
            ]
        }))
        .expect("metadata fixture");
        let roots = dependency_roots_from_metadata(&metadata, Path::new("/repo"));

        assert!(roots.is_external("crates/a/src/lib.rs", "json"));
        assert!(!roots.is_external("crates/b/src/lib.rs", "json"));
        assert!(roots.is_external("crates/b/src/lib.rs", "regex"));
        assert!(!roots.is_external("crates/a/src/lib.rs", "regex"));
        assert!(!roots.is_external("crates/b/src/lib.rs", "root_only"));
    }
}
