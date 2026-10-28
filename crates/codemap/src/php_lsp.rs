//! PHP semantic call resolution via phpactor, under codemap's
//! verified-edge policy.
//!
//! Mirrors `ruby_lsp.rs` in role, with PHP-adapted inputs: an LSP client
//! over a hermetically pinned phpactor process (operator bundle via
//! `CODEMAP_PHP_TOOL_BUNDLE`), and a static policy model that admits a
//! canonical `calls` edge only when codemap's own structural facts verify
//! the receiver→target relation. phpactor's answers are candidate
//! material, never authority.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::lsp::{
    DefinitionRequest, DefinitionResult, DefinitionSource, NodeIndex, ServerRequestHook, Transport,
    bounded_probe, document_uri, parse_definition_response, uri_from_path,
};
use crate::resolve::{DefinitionIndex, Resolution};
use crate::rust_syntax::ReceiverShape;
use crate::schema::{
    CallSiteCategory, Edge, EdgeKind, FactSource, Node, NodeKind, Provenance, SnapshotIdentity,
    Span, TypeKind, edge_id,
};

/// Whether phpactor is available to the extractor. `Present` carries the
/// tool-bundle directory whose `composer.lock` satisfied the canonical pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Present(PathBuf),
    Absent,
}

/// Identity of the PHP semantic environment, pinned in snapshot identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhpToolIdentity {
    pub phpactor: String,
    pub php: String,
    pub tool_bundle: PathBuf,
}

/// Canonical tool-bundle lockfile compiled into the binary. Operators
/// install a bundle whose `composer.lock` is byte-identical to this pin;
/// extraction refuses to run against a drifted one.
const CANONICAL_LOCKFILE: &str = include_str!("../../../toolchain/php/composer.lock");

/// SHA-256 of the canonical lockfile, for snapshot identity.
pub fn canonical_lockfile_sha256() -> String {
    crate::schema::sha256_hex(CANONICAL_LOCKFILE.as_bytes())
}

/// Detect a usable phpactor tool bundle.
///
/// The bundle directory is supplied by the operator through
/// `CODEMAP_PHP_TOOL_BUNDLE` — deliberately the only source. A bundle
/// inside the analyzed worktree is never considered: phpactor plugins or
/// autoloaded project code under the analyzed app would violate the
/// code-execution-free policy. A candidate qualifies only when its
/// `composer.lock` is byte-identical to the canonical pin set and the
/// pinned binary answers `--version`.
pub fn detect() -> Availability {
    let Some(explicit) = std::env::var_os("CODEMAP_PHP_TOOL_BUNDLE") else {
        return Availability::Absent;
    };
    let dir = PathBuf::from(explicit);
    let lockfile = dir.join("composer.lock");
    let Ok(lock) = std::fs::read_to_string(&lockfile) else {
        return Availability::Absent;
    };
    let binary = dir.join("vendor/bin/phpactor");
    if lockfile_matches_canonical(&lock) && probe_bundle(&binary) {
        Availability::Present(dir)
    } else {
        Availability::Absent
    }
}

fn lockfile_matches_canonical(operator_lock: &str) -> bool {
    canonical_lockfile_sha256() == crate::schema::sha256_hex(operator_lock.as_bytes())
}

/// Verify the pinned phpactor binary launches. `--version` performs no
/// indexing and never reads the analyzed worktree.
fn probe_bundle(binary: &Path) -> bool {
    let bundle = binary
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent);
    let (Some(bundle), Ok(home), Ok(php)) = (bundle, XdgHome::create(), php_interpreter()) else {
        return false;
    };
    let mut probe = phpactor_command(bundle, home.path(), &php);
    probe.arg("--version");
    bounded_probe(probe, PROBE_TIMEOUT)
        .is_ok_and(|out| out.status.success() && !out.stdout.is_empty())
}

/// The PHP interpreter behind `php` on `PATH`, as an absolute path.
///
/// phpactor is a PHP script, and `php` on a developer machine is often a
/// version-manager shim that consults the operator's XDG configuration to
/// pick an interpreter. codemap launches phpactor with that configuration
/// cut off, so it asks the interpreter for its own path once, in the
/// operator's environment, and runs the script through that path.
fn php_interpreter() -> Result<PathBuf> {
    let mut probe = Command::new("php");
    probe.args(["-r", "echo PHP_BINARY;"]);
    let out = bounded_probe(probe, PROBE_TIMEOUT).context("spawn php interpreter probe")?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || path.is_empty() {
        bail!("php -r 'echo PHP_BINARY;' did not name an interpreter");
    }
    Ok(PathBuf::from(path))
}

/// A throwaway XDG home for one phpactor process, removed when dropped.
/// Pointing `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, and `XDG_CACHE_HOME` at an
/// empty directory keeps the operator's configuration, trust records, and
/// any earlier index out of the launch.
struct XdgHome(PathBuf);

impl XdgHome {
    fn create() -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path =
            std::env::temp_dir().join(format!("codemap-phpactor-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for XdgHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Probe deadline: a stalled php binary must not hang `--calls=required`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// phpactor configuration passed on every launch.
///
/// `composer.enable` off: the composer extension would otherwise `include`
/// the analyzed repository's `vendor/autoload.php` or class maps, which is
/// application code running inside the extractor. Class location comes
/// from phpactor's own index of the sources, built without executing
/// anything.
///
/// `language_server_configuration.auto_config` off: on `initialized`
/// phpactor otherwise inspects the project's `composer.lock`, prompts the
/// client about every tool it recognises (phpstan, psalm, ...), and writes
/// the answer into `.phpactor.json` in the analyzed tree. codemap never
/// writes into what it extracts; the end-of-run clean check would refuse
/// the snapshot.
pub const PHPACTOR_CONFIG_EXTRA: &str =
    r#"{"composer.enable":false,"language_server_configuration.auto_config":false}"#;

/// The phpactor process as codemap launches it: the pinned script under
/// `php`, run from the tool bundle, with every configuration source
/// outside this crate cut off. `xdg_home` replaces the operator's XDG
/// config and data homes, so neither the operator's `phpactor.json` nor a
/// trust record for the analyzed project reaches the process; without
/// trust phpactor never loads the project's own `.phpactor.json`.
/// `PHPACTOR_UNCONDITIONAL_TRUST` is dropped for the same reason. Every
/// launch adds [`PHPACTOR_CONFIG_EXTRA`].
pub fn phpactor_command(tool_bundle: &Path, xdg_home: &Path, php: &Path) -> Command {
    let mut command = Command::new(php);
    command
        .arg(tool_bundle.join("vendor/bin/phpactor"))
        .current_dir(tool_bundle)
        .env_remove("PHPACTOR_UNCONDITIONAL_TRUST")
        .env("XDG_CONFIG_HOME", xdg_home)
        .env("XDG_DATA_HOME", xdg_home)
        .env("XDG_CACHE_HOME", xdg_home)
        .arg("--config-extra")
        .arg(PHPACTOR_CONFIG_EXTRA);
    command
}

/// Capture the full semantic environment identity for snapshot pinning.
/// Fails closed when any version cannot be read.
pub fn capture_identity(tool_bundle: &Path) -> Result<PhpToolIdentity> {
    let home = XdgHome::create()?;
    let php_binary = php_interpreter()?;
    let mut phpactor_probe = phpactor_command(tool_bundle, home.path(), &php_binary);
    phpactor_probe.arg("--version");
    let phpactor = first_line_of(phpactor_probe).context("phpactor version probe failed")?;
    // The interpreter is `php` on PATH (pinned to php 8.5.10 via
    // toolchain/php/composer.json config.platform), recorded from the
    // resolved binary that actually runs phpactor.
    let mut php_probe = Command::new(&php_binary);
    php_probe.arg("--version");
    let php = first_line_of(php_probe).context("php interpreter version probe failed")?;
    Ok(PhpToolIdentity {
        phpactor,
        php,
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

/// One analyzed checkout, prepared for phpactor: a throwaway XDG home
/// holding a complete index of it, which every server the pool starts
/// reads and none of them needs to build.
///
/// phpactor answers `textDocument/definition` from its workspace index,
/// and the language server only fills that index in the background, so
/// the extractor builds it here, once, before any server starts: every
/// run resolves against one complete index of this checkout, never
/// against whatever an earlier run left behind. phpactor serves one
/// request at a time, so parallelism means several servers over the
/// same prepared home.
pub struct Workspace {
    root: PathBuf,
    tool_bundle: PathBuf,
    php: PathBuf,
    xdg_home: Arc<XdgHome>,
}

impl Workspace {
    /// Build the index for `workspace_root` under the hermetic tool
    /// bundle.
    pub fn prepare(workspace_root: &Path, tool_bundle: &Path) -> Result<Self> {
        // A fresh, empty XDG home per extraction: no operator
        // configuration, no trust record, nothing the analyzed project
        // can be trusted by, and no index from another run.
        let xdg_home = XdgHome::create()?;
        let php = php_interpreter()?;
        let indexed = phpactor_command(tool_bundle, xdg_home.path(), &php)
            .args(["index:build", "--reset"])
            .current_dir(workspace_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .context("failed to run phpactor index:build")?;
        if !indexed.success() {
            bail!(
                "phpactor index:build failed for {} ({indexed})",
                workspace_root.display()
            );
        }
        Ok(Self {
            root: workspace_root.to_path_buf(),
            tool_bundle: tool_bundle.to_path_buf(),
            php,
            xdg_home: Arc::new(xdg_home),
        })
    }

    /// Start one language server over the prepared home and complete the
    /// LSP initialize handshake. Every server-initiated request is answered
    /// with `null`: codemap never serves workspace queries back.
    pub fn spawn_client(&self) -> Result<Client> {
        let mut command = phpactor_command(&self.tool_bundle, self.xdg_home.path(), &self.php);
        command
            .arg("language-server")
            .current_dir(&self.root)
            .stderr(Stdio::inherit());
        let hook: ServerRequestHook = Box::new(|_| Some(Value::Null));
        let transport = Transport::spawn("phpactor", &mut command, Some(hook))?;
        let root_uri = uri_from_path(&self.root);
        let client = Client {
            transport,
            root_uri,
            _xdg_home: Arc::clone(&self.xdg_home),
        };

        let _init_result = client
            .transport
            .request("initialize", &initialization_params(&client.root_uri))
            .context("phpactor initialize failed")?;
        client
            .transport
            .notify("initialized", &json!({}))
            .context("notify phpactor initialized")?;
        Ok(client)
    }
}

/// An owned LSP client connected to a running phpactor server.
pub struct Client {
    transport: Transport,
    root_uri: String,
    /// The prepared home outlives every server over it; removed when the
    /// last one is gone.
    _xdg_home: Arc<XdgHome>,
}

impl Client {
    /// Open a document so phpactor serves definition queries against the
    /// pinned source bytes.
    pub fn open_document(&self, rel_path: &str, text: &str) -> Result<()> {
        self.transport.notify(
            "textDocument/didOpen",
            &json!({
                "textDocument": {
                    "uri": document_uri(&self.root_uri, rel_path),
                    "languageId": "php",
                    "version": 1,
                    "text": text,
                }
            }),
        )
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
        parse_definition_response("phpactor", &value)
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
        "capabilities": {
            "textDocument": {
                "definition": { "linkSupport": false }
            }
        },
        "initializationOptions": {},
    })
}

// ───────────────────────── canonical-edge policy model ─────────────────────────

/// Codemap's static model for admitting phpactor candidate locations.
/// Every canonical PHP `calls` edge must be justified by this model —
/// never by the resolver's own inference (truth boundary).
///
/// Policy inputs, PHP-adapted from the Ruby model:
/// - instance regime: `$this->m()` inside an instance method body; the
///   target's owner must lie in the receiver's **instance ancestry** =
///   `extends` chain + trait `use` (Implements edges on the class).
/// - singleton regime: `Cls::m()` with a constant receiver; the target
///   must be a **static** method of the receiver's singleton ancestry
///   (the class itself and its `extends` chain). Trait contributions via
///   `use` add instance methods only; PHP trait statics exist but are
///   folded through the class, so they're admitted when on the chain.
/// - `parent::m()`: receiver becomes the verified superclass; the
///   instance ancestry of that superclass applies (parent can call both
///   inherited statics and overridden instances — codemap admits only
///   owners in the chain; phpactor must land somewhere the ancestry
///   covers).
/// - declared regime (P3): `$this->prop->m()` through a typed property
///   or `$param->m()` through a typed parameter the body never assigns.
///   The declared type must be one in-repo class, enum, or interface;
///   the target's owner must lie in that type's **instance ancestry**.
///   PHP enforces declared types at every write, so the receiver is an
///   instance of the declared type or something beneath it, and a method
///   found on the declared chain is the one that runs or is overridden
///   by a subclass — either way the declared owner is the verified
///   target. Union and intersection types, `self`/`static`, `mixed`,
///   `object`, docblock-only types, inherited properties, and locals
///   assigned from `new` are not declared receivers and stay
///   observations.
pub struct PolicyModel {
    /// Every emitted class/trait/interface/enum node id.
    class_ids: HashSet<String>,
    /// Class node id → direct superclass id (`inherits` edges).
    superclasses: HashMap<String, String>,
    /// Class node id → trait targets (`implements` edges from trait use)
    /// and interface ids. These contribute instance methods to the class.
    instance_mixins: HashMap<String, BTreeSet<String>>,
    /// Method node id → owning class node id.
    owners: HashMap<String, String>,
    /// Every emitted `Method`/`Function` node id. A canonical edge's `from`
    /// must be one of these: a call enclosed by a callable definition.
    callable_ids: HashSet<String>,
    /// Method node ids declared `static`. A constant receiver
    /// (`Cls::m()`) names a class, not an instance, so it may only reach
    /// these; PHP 8 refuses a static call to an instance method.
    static_method_ids: HashSet<String>,
}

impl PolicyModel {
    pub fn build(nodes: &[Node], edges: &[Edge], static_methods: &HashSet<String>) -> Self {
        let mut class_ids: HashSet<String> = HashSet::new();
        for node in nodes {
            if matches!(
                node.kind,
                NodeKind::Type(TypeKind::Class | TypeKind::Interface | TypeKind::Enum)
                    | NodeKind::Trait
            ) {
                class_ids.insert(node.id.clone());
            }
        }
        let mut superclasses = HashMap::new();
        let mut instance_mixins: HashMap<String, BTreeSet<String>> = HashMap::new();
        for edge in edges {
            match edge.kind {
                EdgeKind::Inherits => {
                    superclasses.insert(edge.from.clone(), edge.to.clone());
                }
                EdgeKind::Implements => {
                    instance_mixins
                        .entry(edge.from.clone())
                        .or_default()
                        .insert(edge.to.clone());
                }
                _ => {}
            }
        }
        let mut owners = HashMap::new();
        let mut callable_ids: HashSet<String> = HashSet::new();
        for node in nodes {
            if matches!(node.kind, NodeKind::Method | NodeKind::Function) {
                callable_ids.insert(node.id.clone());
            }
            if node.kind == NodeKind::Method {
                if let Some(owner) = derive_owner(&node.id, &class_ids) {
                    owners.insert(node.id.clone(), owner);
                }
            }
        }
        Self {
            class_ids,
            superclasses,
            instance_mixins,
            owners,
            callable_ids,
            static_method_ids: static_methods.clone(),
        }
    }

    pub fn contains_class(&self, id: &str) -> bool {
        self.class_ids.contains(id)
    }

    /// Verified direct superclass (`inherits` target) of `class_id`.
    pub fn superclass_of(&self, class_id: &str) -> Option<String> {
        self.superclasses.get(class_id).cloned()
    }

    /// Instance-method ancestry of `class_id`: the class itself, its
    /// `extends` chain, and every trait/interface mixed into any of those.
    fn instance_ancestors(&self, class_id: &str) -> BTreeSet<String> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut stack = vec![class_id.to_string()];
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(mixins) = self.instance_mixins.get(&current) {
                stack.extend(mixins.iter().cloned());
            }
            if let Some(parent) = self.superclasses.get(&current) {
                stack.push(parent.clone());
            }
        }
        seen
    }

    /// Singleton ancestry: class plus `extends` chain only. Traits' static
    /// methods (rare) are admitted transitively through the class edge — a
    /// conservative simplification matching PHP's flattening semantics.
    fn singleton_ancestors(&self, class_id: &str) -> BTreeSet<String> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut current = Some(class_id.to_string());
        while let Some(c) = current {
            if !seen.insert(c.clone()) {
                break;
            }
            current = self.superclasses.get(&c).cloned();
        }
        seen
    }

    /// Admit `target` as the canonical destination of a call whose
    /// receiver is `receiver_class`, per the P2 edge policy.
    ///
    /// - Implicit self (`$this->m()`, `parent::m()`, `self::m()`,
    ///   `static::m()` inside an instance method): the target's owner must
    ///   lie in the receiver's *instance* ancestry. Instance methods only.
    /// - Constant receiver (`Cls::m()`): the target must be a static
    ///   method whose owner lies in the receiver's *singleton* ancestry.
    /// - Declared receiver (`$this->prop->m()`, `$param->m()`): the
    ///   target's owner must lie in the declared type's *instance*
    ///   ancestry; instance and static methods alike, since PHP allows
    ///   both through an instance.
    pub fn admit(&self, receiver_class: &str, shape: &ReceiverShape, target: &str) -> bool {
        if !self.class_ids.contains(receiver_class) {
            return false;
        }
        let ancestors = match shape {
            #[allow(clippy::match_same_arms)] // ImplicitSelf ≠ Parent: receiver class differs
            ReceiverShape::ImplicitSelf => self.instance_ancestors(receiver_class),
            ReceiverShape::Constant(_) => {
                if !self.static_method_ids.contains(target) {
                    return false;
                }
                self.singleton_ancestors(receiver_class)
            }
            // `parent::m()`: receiver_class is already the superclass, and a
            // declared receiver is already the declared type; the instance
            // ancestry of that class applies to both.
            ReceiverShape::Parent | ReceiverShape::Declared(_) => {
                self.instance_ancestors(receiver_class)
            }
            ReceiverShape::Other | ReceiverShape::Constructed { .. } => return false,
        };
        self.owners
            .get(target)
            .is_some_and(|owner| ancestors.contains(owner))
    }

    /// True when `id` is an emitted `Method`/`Function` node. A canonical
    /// edge's `from` must pass this: load-time calls in class bodies have
    /// no enclosing callable and stay observations.
    pub fn is_callable(&self, id: &str) -> bool {
        self.callable_ids.contains(id)
    }
}

/// Owning class/trait id of a method node id: `{lang}:method:{path}#{Q}` →
/// `{lang}:{type|trait}:{path}#{owner-Q}` where the owner is `Q` minus the
/// last `::` segment.
fn derive_owner(method_id: &str, class_ids: &HashSet<String>) -> Option<String> {
    let head = method_id.split_once('#')?.0;
    let qualified = &method_id[head.len() + 1..];
    let segments: Vec<&str> = qualified.split("::").collect();
    if segments.len() < 2 {
        return None;
    }
    let owner_qual = segments[..segments.len() - 1].join("::");
    for kind in ["type", "trait"] {
        let candidate = head.replacen(":method:", &format!(":{kind}:"), 1);
        let candidate = format!("{candidate}#{owner_qual}");
        if class_ids.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Everything one semantic PHP call resolution needs, shared across sites.
pub struct ResolveContext<'a> {
    pub workspace_root: &'a Path,
    pub identity: &'a SnapshotIdentity,
    pub rel_paths: &'a HashSet<String>,
    pub file_sources: &'a BTreeMap<&'a str, &'a [u8]>,
    pub node_index: &'a NodeIndex,
    pub def_index: &'a DefinitionIndex,
}

/// One pending PHP call site with its file identity.
pub struct PendingPhpCall {
    pub rel_path: String,
    pub file_hash: String,
    pub call: crate::rust_syntax::RawCall,
}

/// Admit or reject one answered call site under the P2 policy. Returns
/// the category to report; appends a canonical edge when the static
/// model admits one. Pure: the same answer always yields the same
/// category and edge.
pub fn admit_definition(
    result: DefinitionResult,
    model: &PolicyModel,
    ctx: &ResolveContext<'_>,
    pending: &PendingPhpCall,
    emitted_edge_ids: &mut HashSet<String>,
    edges: &mut Vec<Edge>,
) -> CallSiteCategory {
    let rel_path = &pending.rel_path;
    let call = &pending.call;

    // The regime is decided by codemap's syntax model, never by the
    // resolver: untyped receivers stay observations whatever the server
    // answers.
    let verifiable = matches!(
        call.receiver,
        ReceiverShape::Constant(_)
            | ReceiverShape::ImplicitSelf
            | ReceiverShape::Parent
            | ReceiverShape::Declared(_)
    );

    let locations = match result {
        DefinitionResult::Empty => Vec::new(),
        DefinitionResult::Locations(locs) => locs,
    };

    // Map in-repo locations to emitted definition nodes; external
    // locations collected for zero-candidate classification.
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    let mut external_uris: BTreeSet<String> = BTreeSet::new();
    for loc in &locations {
        match rel_path_of_uri(&loc.uri, ctx.workspace_root).filter(|rp| ctx.rel_paths.contains(rp))
        {
            Some(target_rel) => {
                let target_byte = ctx.file_sources.get(target_rel.as_str()).map_or(0, |s| {
                    crate::lsp::lsp_pos_to_byte(s, loc.start_line, loc.start_character)
                });
                if let Some(id) = ctx.node_index.find(&target_rel, target_byte) {
                    candidates.insert(id.to_string());
                }
            }
            None => {
                external_uris.insert(loc.uri.clone());
            }
        }
    }

    let uniquely_in_repo = locations.len() == 1 && candidates.len() == 1;

    let from_id = ctx
        .node_index
        .find(rel_path, call.name_start_byte)
        .map(str::to_string)
        .filter(|id| model.is_callable(id));

    let receiver_class = verifiable
        .then(|| receiver_class_id(call, rel_path, ctx, model))
        .flatten();

    match (verifiable, receiver_class.as_deref(), candidates.len()) {
        (true, Some(receiver), 1)
            if uniquely_in_repo
                && from_id.is_some()
                && model.admit(
                    receiver,
                    &call.receiver,
                    candidates.first().expect("one candidate"),
                ) =>
        {
            let from_id = from_id.expect("verified callable");
            let to_id = candidates.first().expect("one candidate");
            let edge = Edge {
                id: edge_id(EdgeKind::Calls, &from_id, to_id, call.name_start_byte),
                kind: EdgeKind::Calls,
                from: from_id,
                to: to_id.clone(),
                provenance: calls_provenance(ctx.identity, rel_path, call.span, &pending.file_hash),
            };
            if emitted_edge_ids.insert(edge.id.clone()) {
                edges.push(edge);
            }
            CallSiteCategory::Resolved
        }
        (_, _, 1..) => CallSiteCategory::Candidate,
        (_, _, 0) => zero_candidate_category(&external_uris),
    }
}

/// Classify a zero-candidate site by the *set* of external locations:
/// deterministic regardless of phpactor's response ordering. No locations
/// → `Unmapped`; any vendor location → `ExternalCrate`; PHP's own runtime
/// stubs or spl extension sources → `StandardLibrary`.
fn zero_candidate_category(external_uris: &BTreeSet<String>) -> CallSiteCategory {
    if external_uris.is_empty() {
        return CallSiteCategory::Unmapped;
    }
    if external_uris
        .iter()
        .all(|uri| classify_external(uri) == CallSiteCategory::StandardLibrary)
    {
        CallSiteCategory::StandardLibrary
    } else {
        CallSiteCategory::ExternalCrate
    }
}

/// Provenance for a semantic PHP `calls` edge: authoritative phpactor
/// source (admitted only through codemap's static model).
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
        fact_source: FactSource::Phpactor,
        confidence: 255,
    }
}

/// Classify a definition location outside the repository. Vendor packages
/// (`/vendor/`) are external; anything else outside the repo (e.g. PHP
/// runtime stubs under the phpactor install) is treated as the standard
/// library surface.
pub fn classify_external(uri: &str) -> CallSiteCategory {
    let path = crate::lsp::path_from_uri(uri);
    if path.contains("/vendor/") {
        return CallSiteCategory::ExternalCrate;
    }
    CallSiteCategory::StandardLibrary
}

/// The class a call's receiver denotes, per codemap's verified facts.
fn receiver_class_id(
    call: &crate::rust_syntax::RawCall,
    rel_path: &str,
    ctx: &ResolveContext<'_>,
    model: &PolicyModel,
) -> Option<String> {
    match &call.receiver {
        ReceiverShape::ImplicitSelf => {
            // The syntax layer recorded the enclosing type chain in
            // `call.scope`; the innermost entry is the receiver class.
            let inner = call.scope.last()?;
            enclosing_class_id(inner, rel_path, ctx.identity, model)
        }
        ReceiverShape::Parent => {
            // The enclosing class's verified superclass.
            let inner = call.scope.last()?;
            let class_id = enclosing_class_id(inner, rel_path, ctx.identity, model)?;
            model.superclass_of(&class_id)
        }
        ReceiverShape::Constant(name) => {
            // The syntax layer qualified the written name from the file's
            // imports and namespace, so the lookup is exact: no leaf-name
            // fallback, which would admit a same-named class elsewhere.
            match ctx.def_index.resolve_php_class(name) {
                Resolution::Unique(def)
                    if matches!(
                        def.kind,
                        NodeKind::Type(TypeKind::Class | TypeKind::Enum) | NodeKind::Trait
                    ) =>
                {
                    Some(def.node_id)
                }
                _ => None,
            }
        }
        ReceiverShape::Declared(name) => {
            // A declared type names a class, enum, or interface; a trait
            // cannot be a declared type in PHP. Exact lookup, as above.
            match ctx.def_index.resolve_php_class(name) {
                Resolution::Unique(def)
                    if matches!(
                        def.kind,
                        NodeKind::Type(TypeKind::Class | TypeKind::Enum | TypeKind::Interface)
                    ) =>
                {
                    Some(def.node_id)
                }
                _ => None,
            }
        }
        ReceiverShape::Other | ReceiverShape::Constructed { .. } => None,
    }
}

/// The node id of the class named by the qualified `inner` scope entry
/// (e.g. `App\Model::User`), scoped to this file.
fn enclosing_class_id(
    inner: &str,
    rel_path: &str,
    identity: &SnapshotIdentity,
    model: &PolicyModel,
) -> Option<String> {
    let prefix = identity.language.id_prefix();
    for kind in ["type", "trait"] {
        let candidate = format!("{prefix}{kind}:{rel_path}#{inner}");
        if model.contains_class(&candidate) {
            return Some(candidate);
        }
    }
    None
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

    #[test]
    fn every_phpactor_launch_is_cut_off_from_operator_and_project_configuration() {
        let bundle = Path::new("/bundle");
        let home = Path::new("/isolated-home");
        let php = Path::new("/interpreters/php");
        let command = phpactor_command(bundle, home, php);

        assert_eq!(command.get_program(), php.as_os_str());
        assert_eq!(command.get_current_dir(), Some(bundle));
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                bundle.join("vendor/bin/phpactor").as_os_str(),
                std::ffi::OsStr::new("--config-extra"),
                std::ffi::OsStr::new(PHPACTOR_CONFIG_EXTRA),
            ]
        );
        let extra = serde_json::from_str::<Value>(PHPACTOR_CONFIG_EXTRA).expect("valid json");
        assert_eq!(extra["composer.enable"], Value::Bool(false));
        assert_eq!(
            extra["language_server_configuration.auto_config"],
            Value::Bool(false),
            "auto-configuration writes .phpactor.json into the analyzed tree"
        );

        let env: BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env.get(std::ffi::OsStr::new("XDG_CONFIG_HOME")),
            Some(&Some(home.as_os_str()))
        );
        assert_eq!(
            env.get(std::ffi::OsStr::new("XDG_DATA_HOME")),
            Some(&Some(home.as_os_str()))
        );
        assert_eq!(
            env.get(std::ffi::OsStr::new("XDG_CACHE_HOME")),
            Some(&Some(home.as_os_str())),
            "the index must be built fresh, never reused from another run"
        );
        assert_eq!(
            env.get(std::ffi::OsStr::new("PHPACTOR_UNCONDITIONAL_TRUST")),
            Some(&None),
            "an operator-wide trust override must not reach the server"
        );
    }

    #[test]
    fn an_xdg_home_is_a_fresh_empty_directory_that_leaves_with_its_owner() {
        let path = {
            let home = XdgHome::create().expect("create");
            assert!(home.path().is_dir());
            assert_eq!(std::fs::read_dir(home.path()).expect("list").count(), 0);
            home.path().to_path_buf()
        };
        assert!(!path.exists(), "dropped home must be removed");
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one fixture, three answers, one assertion each
    fn admission_is_a_pure_function_of_the_answer() {
        use crate::resolve::DefinitionIndex;
        use crate::rust_syntax::RawCall;
        use crate::schema::{
            Language, RepoIdentity, SnapshotIdentity, ToolVersions, node_id, symbol_key,
        };

        let language = Language::Php;
        let identity = SnapshotIdentity::build(
            language,
            RepoIdentity {
                id: "https://example.invalid/fixture.git".into(),
                origin: None,
                is_canonical: true,
            },
            "c0ffee".into(),
            "tree".into(),
            ToolVersions::syntax_php(),
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
        let class = node(
            NodeKind::Type(TypeKind::Class),
            "lib/callee.php",
            &["Demo"],
            "Helper",
            span(0, 300),
        );
        let make = node(
            NodeKind::Method,
            "lib/callee.php",
            &["Demo::Helper"],
            "make",
            span(100, 200),
        );
        let run = node(
            NodeKind::Function,
            "lib/caller.php",
            &["Demo"],
            "run",
            span(0, 100),
        );
        let nodes = vec![class, make.clone(), run.clone()];
        let statics: HashSet<String> = std::iter::once(make.id.clone()).collect();
        let model = PolicyModel::build(&nodes, &[], &statics);

        let workspace = tempfile::tempdir().expect("workspace");
        let target_bytes = vec![b'x'; 300];
        let origin_bytes = vec![b'y'; 100];
        let file_sources: BTreeMap<&str, &[u8]> = [
            ("lib/callee.php", target_bytes.as_slice()),
            ("lib/caller.php", origin_bytes.as_slice()),
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
        let def_index = DefinitionIndex::from_nodes(&nodes);
        let ctx = ResolveContext {
            workspace_root: workspace.path(),
            identity: &identity,
            rel_paths: &rel_paths,
            file_sources: &file_sources,
            node_index: &node_index,
            def_index: &def_index,
        };
        let pending = PendingPhpCall {
            rel_path: "lib/caller.php".into(),
            file_hash: "h".into(),
            call: RawCall {
                raw_name: "Helper::make()".into(),
                path_root: None,
                span: span(8, 22),
                name_start_byte: 16,
                receiver: ReceiverShape::Constant("Demo\\Helper".into()),
                scope: Vec::new(),
            },
        };
        let at_make = DefinitionLocation {
            uri: format!("{}/lib/callee.php", uri_from_path(workspace.path())),
            start_line: 0,
            start_character: 150,
            end_line: 0,
            end_character: 154,
        };

        let mut emitted = HashSet::new();
        let mut edges = Vec::new();
        let category = admit_definition(
            DefinitionResult::Locations(vec![at_make.clone()]),
            &model,
            &ctx,
            &pending,
            &mut emitted,
            &mut edges,
        );
        assert_eq!(category, CallSiteCategory::Resolved);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (edges[0].from.as_str(), edges[0].to.as_str()),
            (run.id.as_str(), make.id.as_str())
        );

        let category = admit_definition(
            DefinitionResult::Empty,
            &model,
            &ctx,
            &pending,
            &mut emitted,
            &mut edges,
        );
        assert_eq!(category, CallSiteCategory::Unmapped);

        let category = admit_definition(
            DefinitionResult::Locations(vec![at_make.clone(), at_make]),
            &model,
            &ctx,
            &pending,
            &mut emitted,
            &mut edges,
        );
        assert_eq!(
            category,
            CallSiteCategory::Candidate,
            "two answers are never one edge"
        );
        assert_eq!(
            edges.len(),
            1,
            "the same site admitted twice emits one edge"
        );
    }

    #[test]
    fn a_constant_receiver_reaches_only_static_methods() {
        let language = crate::schema::Language::Php;
        let make_node = |kind: NodeKind, scope: &[&str], name: &str| {
            let scope: Vec<String> = scope.iter().map(ToString::to_string).collect();
            Node {
                python: None,
                id: crate::schema::node_id(language, kind, "lib/a.php", &scope, name),
                kind,
                name: name.to_string(),
                symbol_key: crate::schema::symbol_key(language, "lib/a.php", &scope, name),
                provenance: Provenance {
                    repository: "r".into(),
                    commit_sha: "c".into(),
                    rel_path: "lib/a.php".into(),
                    span: Span {
                        start_byte: 0,
                        end_byte: 1,
                        start_line: 1,
                        start_col: 0,
                        end_line: 1,
                        end_col: 1,
                    },
                    file_hash: "h".into(),
                    extractor_version: "v".into(),
                    fact_source: FactSource::TreeSitter,
                    confidence: u8::MAX,
                },
            }
        };
        let class = make_node(NodeKind::Type(TypeKind::Class), &["Demo"], "Helper");
        let static_method = make_node(NodeKind::Method, &["Demo::Helper"], "make");
        let instance_method = make_node(NodeKind::Method, &["Demo::Helper"], "assist");
        let statics: HashSet<String> = std::iter::once(static_method.id.clone()).collect();
        let model = PolicyModel::build(
            &[
                class.clone(),
                static_method.clone(),
                instance_method.clone(),
            ],
            &[],
            &statics,
        );

        let constant = ReceiverShape::Constant("Demo\\Helper".into());
        assert!(model.admit(&class.id, &constant, &static_method.id));
        assert!(
            !model.admit(&class.id, &constant, &instance_method.id),
            "a static call cannot reach an instance method"
        );
        assert!(model.admit(&class.id, &ReceiverShape::ImplicitSelf, &instance_method.id));
        assert!(
            model.admit(&class.id, &ReceiverShape::ImplicitSelf, &static_method.id),
            "$this and self:: may reach static methods"
        );
    }

    #[test]
    fn a_declared_receiver_reaches_its_instance_ancestry_and_nothing_else() {
        let language = crate::schema::Language::Php;
        let make_node = |kind: NodeKind, scope: &[&str], name: &str| {
            let scope: Vec<String> = scope.iter().map(ToString::to_string).collect();
            Node {
                python: None,
                id: crate::schema::node_id(language, kind, "lib/a.php", &scope, name),
                kind,
                name: name.to_string(),
                symbol_key: crate::schema::symbol_key(language, "lib/a.php", &scope, name),
                provenance: Provenance {
                    repository: "r".into(),
                    commit_sha: "c".into(),
                    rel_path: "lib/a.php".into(),
                    span: Span {
                        start_byte: 0,
                        end_byte: 1,
                        start_line: 1,
                        start_col: 0,
                        end_line: 1,
                        end_col: 1,
                    },
                    file_hash: "h".into(),
                    extractor_version: "v".into(),
                    fact_source: FactSource::TreeSitter,
                    confidence: u8::MAX,
                },
            }
        };
        let renderer = make_node(NodeKind::Type(TypeKind::Interface), &["Demo"], "Renderer");
        let render = make_node(NodeKind::Method, &["Demo::Renderer"], "render");
        let base = make_node(NodeKind::Type(TypeKind::Class), &["Demo"], "Base");
        let work = make_node(NodeKind::Method, &["Demo::Base"], "work");
        let service = make_node(NodeKind::Type(TypeKind::Class), &["Demo"], "Service");
        let run = make_node(NodeKind::Method, &["Demo::Service"], "run");
        let stranger = make_node(NodeKind::Type(TypeKind::Class), &["Demo"], "Stranger");
        let visit = make_node(NodeKind::Method, &["Demo::Stranger"], "visit");
        let edge = |kind: EdgeKind, from: &Node, to: &Node| Edge {
            id: edge_id(kind, &from.id, &to.id, 0),
            kind,
            from: from.id.clone(),
            to: to.id.clone(),
            provenance: from.provenance.clone(),
        };
        let nodes = [
            renderer.clone(),
            render.clone(),
            base.clone(),
            work.clone(),
            service.clone(),
            run.clone(),
            stranger,
            visit.clone(),
        ];
        let edges = [
            edge(EdgeKind::Inherits, &service, &base),
            edge(EdgeKind::Implements, &service, &renderer),
        ];
        let model = PolicyModel::build(&nodes, &edges, &HashSet::new());

        let declared = ReceiverShape::Declared("Demo\\Service".into());
        assert!(model.admit(&service.id, &declared, &run.id), "own method");
        assert!(
            model.admit(&service.id, &declared, &work.id),
            "inherited method"
        );
        assert!(
            model.admit(&service.id, &declared, &render.id),
            "interface method"
        );
        assert!(
            !model.admit(&service.id, &declared, &visit.id),
            "a method outside the declared ancestry is never admitted"
        );
        let as_interface = ReceiverShape::Declared("Demo\\Renderer".into());
        assert!(
            model.admit(&renderer.id, &as_interface, &render.id),
            "an interface-typed receiver reaches the interface method"
        );
        assert!(
            !model.admit(&renderer.id, &as_interface, &run.id),
            "an interface-typed receiver does not reach an implementation"
        );
        assert!(
            !model.admit(&service.id, &ReceiverShape::Other, &run.id),
            "an undeclared receiver is never admitted"
        );
    }
}
