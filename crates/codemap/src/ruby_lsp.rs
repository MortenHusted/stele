//! Local stdio JSON-RPC client for `ruby-lsp` (LSP) — the Ruby semantic
//! resolver (adapter R2).
//!
//! Mirrors [`crate::rust_analyzer`] in shape, but the truth boundary is
//! enforced differently: ruby-lsp only supplies candidate locations. A
//! `calls` edge is emitted when **codemap's own static model** verifies the
//! receiver→target relation (constant receiver, implicit self or verified construction, target
//! owner inside the receiver's `inherits`/`implements` closure).
//! ruby-lsp's name-guess answers for untyped receivers are never trusted.
//!
//! ## Hermetic launch
//!
//! The server is launched via `bundle exec ruby-lsp` with `BUNDLE_GEMFILE`
//! pointing at an operator-installed tool bundle whose lockfile is
//! byte-identical to codemap's canonical `toolchain/ruby/Gemfile.lock`.
//! `BUNDLE_GEMFILE` skips ruby-lsp's `SetupBundler` (no network fetch, no
//! worktree writes), and `bundle exec` restricts the gem set to the
//! lockfile, which suppresses auto-activated addons — most importantly
//! `ruby-lsp-rails`, which would boot application code. Nothing is
//! installed at extraction time; an absent or mismatched bundle fails
//! closed under `--calls=required`.
//!
//! ## Quiescence
//!
//! ruby-lsp has no server-status notification. It indexes in a background
//! thread after `initialized` and, when the client advertises
//! `window.workDoneProgress`, drives a progress sequence
//! (`window/workDoneProgress/create` request → `$/progress` begin/report/
//! end) for the token `indexing-progress`. The `end` notification — sent
//! after the ancestor caches are rebuilt — is the index-complete signal;
//! definition queries before it would resolve against a partially
//! populated index.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::lsp::{
    DefinitionRequest, DefinitionResult, DefinitionSource, NodeIndex, ServerRequestHook, Transport,
    bounded_probe, document_uri, parse_definition_response,
};
use crate::resolve::{DefinitionIndex, Resolution};
use crate::rust_syntax::ReceiverShape;
use crate::schema::{
    CallSiteCategory, Edge, EdgeKind, FactSource, Node, NodeKind, Provenance, SnapshotIdentity,
    Span, TypeKind, edge_id,
};

/// Whether ruby-lsp is available to the extractor. `Present` carries the
/// tool-bundle Gemfile path whose lockfile satisfied the canonical pin set.
/// `Absent` means no bundle was configured; `Unavailable` preserves why a
/// configured bundle could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Present(PathBuf),
    Absent,
    Unavailable(String),
}

/// Identity of the Ruby semantic environment, captured once per extraction
/// and pinned in snapshot identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RubyToolIdentity {
    pub ruby_lsp: String,
    pub ruby: String,
    pub rbs: String,
    pub tool_bundle: PathBuf,
}

/// Canonical tool-bundle files compiled into the binary. Operators install
/// a bundle whose lockfile is byte-identical to this pin set; extraction
/// refuses to run against a drifted one.
const CANONICAL_LOCKFILE: &str = include_str!("../../../toolchain/ruby/Gemfile.lock");

/// SHA-256 of the canonical lockfile, for snapshot identity.
pub fn canonical_lockfile_sha256() -> String {
    crate::schema::sha256_hex(CANONICAL_LOCKFILE.as_bytes())
}

/// Detect a usable ruby-lsp tool bundle.
///
/// The bundle Gemfile is supplied by the operator through the
/// `CODEMAP_RUBY_TOOL_BUNDLE` environment variable — deliberately the only
/// source. Bundles inside the extracted worktree are never considered: a
/// ruby-lsp launched under the analyzed application's own gems would load
/// application code (e.g. the ruby-lsp-rails addon), violating the
/// code-execution-free policy. A candidate qualifies only when its
/// lockfile is byte-identical to the canonical pin set and `bundle exec
/// ruby-lsp --version` succeeds under it.
pub fn detect() -> Availability {
    let Some(explicit) = std::env::var_os("CODEMAP_RUBY_TOOL_BUNDLE") else {
        return Availability::Absent;
    };
    let gemfile = PathBuf::from(explicit);
    let lockfile = gemfile.with_extension("lock");
    let lock = match std::fs::read_to_string(&lockfile) {
        Ok(lock) => lock,
        Err(error) => {
            return Availability::Unavailable(format!(
                "cannot read configured CODEMAP_RUBY_TOOL_BUNDLE lockfile {}: {error}",
                lockfile.display()
            ));
        }
    };
    if !lockfile_matches_canonical(&lock) {
        return Availability::Unavailable(format!(
            "configured CODEMAP_RUBY_TOOL_BUNDLE lockfile {} does not match Stele's canonical toolchain/ruby/Gemfile.lock",
            lockfile.display()
        ));
    }
    match bundle_version(&gemfile, ["exec", "ruby-lsp", "--version"]) {
        Ok(_) => Availability::Present(gemfile),
        Err(error) => Availability::Unavailable(format!(
            "configured CODEMAP_RUBY_TOOL_BUNDLE {} failed: {error:#}",
            gemfile.display()
        )),
    }
}

fn lockfile_matches_canonical(operator_lock: &str) -> bool {
    canonical_lockfile_sha256() == crate::schema::sha256_hex(operator_lock.as_bytes())
}

/// Probe deadline. A stalled Bundler or Ruby process must not hang
/// `--calls=required` extraction: without this bound, the probe runs before
/// the LSP client's own timeouts exist.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// A `bundle` invocation under the pinned Gemfile. `BUNDLE_FROZEN` keeps
/// Bundler from rewriting the operator's lockfile: extraction must never
/// mutate operator state, and real drift must fail loud instead of being
/// silently re-resolved.
fn bundle_command(gemfile: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("bundle");
    command
        .args(args)
        .env("BUNDLE_GEMFILE", gemfile)
        .env("BUNDLE_FROZEN", "true");
    command
}

/// Capture the full semantic environment identity for snapshot pinning.
/// Fails closed when any version cannot be read from the bundle.
pub fn capture_identity(tool_bundle: &Path) -> Result<RubyToolIdentity> {
    let ruby_lsp = bundle_version(tool_bundle, ["exec", "ruby-lsp", "--version"])
        .context("ruby-lsp version probe failed")?;
    let ruby = bundle_version(tool_bundle, ["exec", "ruby", "--version"])
        .context("ruby version probe failed")?;
    let rbs = bundle_version(tool_bundle, ["exec", "rbs", "--version"])
        .context("rbs version probe failed")?;
    Ok(RubyToolIdentity {
        ruby_lsp,
        ruby,
        rbs,
        tool_bundle: tool_bundle.to_path_buf(),
    })
}

fn bundle_version(gemfile: &Path, args: [&str; 3]) -> Result<String> {
    probe_bundle_version(bundle_command(gemfile, &args), PROBE_TIMEOUT)
        .with_context(|| format!("bundle {}", args.join(" ")))
}

fn probe_bundle_version(command: Command, timeout: Duration) -> Result<String> {
    let out = bounded_probe(command, timeout).context("run bundle version probe")?;
    if !out.status.success() {
        bail!(
            "bundle version probe failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    if out.stdout.is_empty() {
        bail!("bundle version probe returned no version output");
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string())
}

/// An owned LSP client connected to a running ruby-lsp server.
pub struct Client {
    transport: Transport,
    root_uri: String,
    /// The progress token ruby-lsp chose in its
    /// `window/workDoneProgress/create` request. LSP servers own the token;
    /// `progress_end` matches against it so an unrelated progress sequence
    /// can never release the index wait. Recorded by the transport's hook
    /// on the reader thread, before the request is forwarded, so the token
    /// is always known by the time its `$/progress` end can be seen.
    indexing_token: Arc<Mutex<Option<String>>>,
}

impl Client {
    /// Spawn ruby-lsp for `workspace_root` under the hermetic tool bundle
    /// and complete the LSP initialize handshake. Indexing starts on the
    /// `initialized` notification; call [`Self::wait_indexed`] before
    /// definition queries.
    pub fn spawn(workspace_root: &Path, tool_bundle: &Path) -> Result<Self> {
        let indexing_token: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let hook_token = Arc::clone(&indexing_token);
        let hook: ServerRequestHook =
            Box::new(move |request| Some(server_request_reply(request, &hook_token)));
        let mut command = ruby_lsp_command(tool_bundle);
        command.current_dir(workspace_root);
        let transport = Transport::spawn("ruby-lsp", &mut command, Some(hook))?;
        let root_uri = uri_from_path(workspace_root);
        let client = Self {
            transport,
            root_uri,
            indexing_token,
        };

        let _init_result = client
            .transport
            .request("initialize", &initialization_params(&client.root_uri))
            .context("ruby-lsp initialize failed")?;
        client
            .transport
            .notify("initialized", &json!({}))
            .context("notify ruby-lsp initialized")?;
        Ok(client)
    }

    /// Open a document so ruby-lsp serves definition queries against the
    /// pinned source bytes.
    pub fn open_document(&self, rel_path: &str, text: &str) -> Result<()> {
        self.transport.notify(
            "textDocument/didOpen",
            &json!({
                "textDocument": {
                    "uri": document_uri(&self.root_uri, rel_path),
                    "languageId": "ruby",
                    "version": 1,
                    "text": text,
                }
            }),
        )
    }

    /// Block until ruby-lsp reports index completion, or `timeout` elapses.
    pub fn wait_indexed(&self, timeout: Duration) -> Result<()> {
        let rx = self.transport.unsolicited()?;
        wait_indexed(&rx, &self.indexing_token, timeout)
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
        parse_definition_response("ruby-lsp", &value)
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

/// Build a ruby-lsp process under the hermetic tool bundle. `BUNDLE_GEMFILE`
/// skips `SetupBundler` (no network, no worktree writes); `bundle exec`
/// confines the gem set to the pinned lockfile, suppressing addons.
/// `BUNDLE_FROZEN` stops Bundler from rewriting the lockfile during launch —
/// extraction never mutates operator state. stderr is inherited so a failed
/// launch stays diagnosable.
fn ruby_lsp_command(tool_bundle: &Path) -> Command {
    let mut command = Command::new("bundle");
    command
        .args(["exec", "ruby-lsp"])
        .env("BUNDLE_GEMFILE", tool_bundle)
        .env("BUNDLE_FROZEN", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    command
}

fn initialization_params(root_uri: &str) -> Value {
    json!({
        "processId": null,
        "rootUri": root_uri,
        "capabilities": {
            "textDocument": {
                "definition": { "linkSupport": true }
            },
            // Enables the `$/progress` indexing sequence that
            // [`Client::wait_indexed`] uses as its quiescence signal.
            "window": { "workDoneProgress": true }
        },
        "initializationOptions": {},
    })
}

// ───────────────────────── canonical-edge policy model ─────────────────────────

/// Codemap's static model for admitting ruby-lsp candidate locations.
/// Every canonical Ruby `calls` edge must be justified by this model —
/// never by the resolver's own inference (truth boundary).
pub struct PolicyModel {
    /// Every emitted class/module node id.
    class_ids: HashSet<String>,
    /// Class/module node id → direct superclass id (`inherits` edges).
    superclasses: HashMap<String, String>,
    /// Class/module node id → `include`/`prepend` mixin targets
    /// (`implements` edges). These contribute *instance* methods.
    instance_mixins: HashMap<String, BTreeSet<String>>,
    /// Class/module node id → `extend` mixin targets (`extends` edges).
    /// These contribute *singleton* methods and must never feed the
    /// implicit-self (instance) regime.
    singleton_mixins: HashMap<String, BTreeSet<String>>,
    /// Method node id → owning class/module node id.
    owners: HashMap<String, String>,
    /// Every emitted `Method`/`Function` node id. A canonical edge's `from`
    /// must be one of these: a call enclosed by a callable definition.
    callable_ids: HashSet<String>,
    constructor_classes: HashSet<String>,
    blocked_constructors: HashSet<String>,
    uncertain_instance_methods: HashSet<String>,
    unverified_method_owners: HashSet<String>,
}

impl PolicyModel {
    pub fn build(nodes: &[Node], edges: &[Edge]) -> Self {
        let mut class_ids: HashSet<String> = HashSet::new();
        for node in nodes {
            if matches!(
                node.kind,
                NodeKind::Type(TypeKind::Class) | NodeKind::Module
            ) {
                class_ids.insert(node.id.clone());
            }
        }
        let mut superclasses = HashMap::new();
        let mut instance_mixins: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut singleton_mixins: HashMap<String, BTreeSet<String>> = HashMap::new();
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
                EdgeKind::Extends => {
                    singleton_mixins
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
            if node.kind == NodeKind::Method
                && let Some(owner) = derive_owner(&node.id, &class_ids)
            {
                owners.insert(node.id.clone(), owner);
            }
        }
        let blocked_constructors = nodes
            .iter()
            .filter_map(|node| {
                if node.kind != NodeKind::Method {
                    return None;
                }
                let owner = owners.get(&node.id).cloned()?;
                [
                    "::self.new",
                    "::self.included",
                    "::self.prepended",
                    "::self.extended",
                    "::self.inherited",
                ]
                .iter()
                .any(|suffix| node.id.ends_with(suffix))
                .then_some(owner)
            })
            .collect();
        Self {
            class_ids,
            superclasses,
            instance_mixins,
            singleton_mixins,
            owners,
            callable_ids,
            constructor_classes: nodes
                .iter()
                .filter(|node| node.kind == NodeKind::Type(TypeKind::Class))
                .map(|node| node.id.clone())
                .collect(),
            blocked_constructors,
            uncertain_instance_methods: HashSet::new(),
            unverified_method_owners: HashSet::new(),
        }
    }

    /// Verify runtime method owners and constructor sources independently of LSP answers.
    pub fn verify_ruby_source_contexts(
        &mut self,
        sources: &BTreeMap<&str, &[u8]>,
        definitions: &DefinitionIndex,
        node_index: &NodeIndex,
    ) -> Result<()> {
        for (path, source) in sources {
            let raw = crate::ruby_syntax::extract_file(path, source)?;
            for def in &raw.unverified_method_owners {
                if let Some(id) = node_index.find(path, raw.defs[*def].span.start_byte) {
                    self.unverified_method_owners.insert(id.to_owned());
                }
            }
            self.uncertain_instance_methods
                .extend(raw.uncertain_instance_methods);
            if raw.parse_errors != 0 {
                self.blocked_constructors
                    .extend(self.constructor_classes.iter().cloned());
            }
            for inherit in raw.inherits {
                if let Resolution::Unique(child) =
                    definitions.resolve_constant(&inherit.class_name, &[])
                {
                    let complete = matches!(definitions.resolve_constant(&inherit.superclass, &inherit.scope),
                        Resolution::Unique(parent) if parent.kind == NodeKind::Type(TypeKind::Class)
                            && self.superclasses.get(&child.node_id) == Some(&parent.node_id));
                    if !complete {
                        self.blocked_constructors.insert(child.node_id);
                    }
                }
            }
            for mixin in raw.mixins {
                let Some(receiver) = mixin.receiver_name else {
                    self.blocked_constructors
                        .extend(self.constructor_classes.iter().cloned());
                    continue;
                };
                if let Resolution::Unique(class) = definitions.resolve_constant(&receiver, &[]) {
                    let providers = if mixin.mixin == crate::ruby_syntax::MixinKind::Extend {
                        &self.singleton_mixins
                    } else {
                        &self.instance_mixins
                    };
                    let complete = matches!(definitions.resolve_constant(&mixin.target, &mixin.scope),
                        Resolution::Unique(module) if module.kind == NodeKind::Module
                            && providers.get(&class.node_id).is_some_and(|mixins| mixins.contains(&module.node_id)));
                    if !complete {
                        self.blocked_constructors.insert(class.node_id);
                    }
                }
            }
            for (receiver, scope) in raw.constructor_overrides {
                if let Some(receiver) = receiver {
                    if let Resolution::Unique(class) =
                        definitions.resolve_constant(&receiver, &scope)
                    {
                        self.blocked_constructors.insert(class.node_id);
                    }
                } else {
                    self.blocked_constructors
                        .extend(self.constructor_classes.iter().cloned());
                }
            }
        }
        Ok(())
    }

    fn default_constructor_receiver(&self, id: &str, method: &str) -> bool {
        !self.uncertain_instance_methods.contains(method)
            && self.constructor_classes.contains(id)
            && self
                .instance_ancestors(id)
                .iter()
                .all(|ancestor| !self.blocked_constructors.contains(ancestor))
            && self.singleton_ancestors(id).iter().all(|ancestor| {
                !self.blocked_constructors.contains(ancestor)
                    && !self.singleton_mixins.contains_key(ancestor)
            })
    }

    pub fn contains_class(&self, id: &str) -> bool {
        self.class_ids.contains(id)
    }

    /// Instance-method ancestry of `class_id`: the class itself, its
    /// superclass chain, and every `include`/`prepend` mixin mixed into
    /// any of those. `extend` mixins never appear: they contribute
    /// singleton methods, which no implicit-self body can reach.
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

    /// Singleton-method ancestry of `class_id`: the class/module itself,
    /// its superclass chain, and every `extend` mixin mixed into any of
    /// those. `include`/`prepend` mixins never appear.
    fn singleton_ancestors(&self, class_id: &str) -> BTreeSet<String> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut stack = vec![class_id.to_string()];
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(mixins) = self.singleton_mixins.get(&current) {
                stack.extend(mixins.iter().cloned());
            }
            if let Some(parent) = self.superclasses.get(&current) {
                stack.push(parent.clone());
            }
        }
        seen
    }

    /// Admit `target` (a method node id) as the canonical destination of a
    /// call whose receiver is `receiver_class`, per the R2 edge policy:
    /// the target's owner must lie in the receiver's verified ancestry for
    /// the call's regime, and the target's method kind must match it.
    ///
    /// - Implicit self: instance methods only, owner inside the receiver's
    ///   *instance* ancestry. The walker already records `ImplicitSelf`
    ///   exclusively inside instance-method bodies.
    /// - Constant receiver (`C.m`): singleton methods only (`self.`-named),
    ///   owner inside the receiver's *singleton* ancestry.
    /// - Direct construction (`C.new.m`): instance methods only, after the
    ///   constructor provider and source heritage have been verified.
    pub fn admit(&self, receiver_class: &str, shape: &ReceiverShape, target: &str) -> bool {
        if !self.class_ids.contains(receiver_class)
            || self.unverified_method_owners.contains(target)
        {
            return false;
        }
        let singleton_target = target.rsplit("::").next().is_some_and(|name| {
            name.strip_prefix("self.")
                .is_some_and(|rest| !rest.contains("::"))
        });
        let ancestors = match shape {
            ReceiverShape::ImplicitSelf | ReceiverShape::Constructed { .. } => {
                if singleton_target {
                    return false;
                }
                self.instance_ancestors(receiver_class)
            }
            ReceiverShape::Constant(_) => {
                if !singleton_target {
                    return false;
                }
                self.singleton_ancestors(receiver_class)
            }
            // PHP-only shapes; Ruby extraction never emits them.
            #[allow(clippy::match_same_arms)]
            ReceiverShape::Parent | ReceiverShape::Declared(_) => return false,
            ReceiverShape::Other => return false,
        };
        self.owners
            .get(target)
            .is_some_and(|owner| ancestors.contains(owner))
    }

    /// True when `id` is an emitted `Method`/`Function` node. A canonical
    /// edge's `from` must pass this: the spatial index returns the smallest
    /// enclosing node at the call site, which is a class/module for
    /// load-time calls in class bodies — those have no enclosing method, so
    /// codemap cannot attribute them to a callable and they stay
    /// observations.
    pub fn is_callable(&self, id: &str) -> bool {
        self.callable_ids.contains(id)
    }
}

/// Owning class/module id of a method node id. Node ids are
/// `{lang}:method:{rel_path}#{Scope::Name}`; the owner is the same path
/// with the last `::`-segment dropped and the kind rewritten to
/// `type`/`module`, when that id is an emitted class/module.
fn derive_owner(method_id: &str, class_ids: &HashSet<String>) -> Option<String> {
    let head = method_id.split_once('#')?.0;
    let qualified = &method_id[head.len() + 1..];
    let segments: Vec<&str> = qualified.split("::").collect();
    if segments.len() < 2 {
        return None;
    }
    let owner_qual = segments[..segments.len() - 1].join("::");
    for kind in ["type", "module"] {
        let candidate = head.replacen(":method:", &format!(":{kind}:"), 1);
        let candidate = format!("{candidate}#{owner_qual}");
        if class_ids.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Everything one semantic Ruby call resolution needs, shared across sites.
pub struct ResolveContext<'a> {
    pub workspace_root: &'a Path,
    pub identity: &'a SnapshotIdentity,
    pub rel_paths: &'a HashSet<String>,
    pub file_sources: &'a BTreeMap<&'a str, &'a [u8]>,
    pub node_index: &'a NodeIndex,
    pub def_index: &'a DefinitionIndex,
}

/// One pending Ruby call site with its file identity.
pub struct PendingRubyCall {
    pub rel_path: String,
    pub file_hash: String,
    pub call: crate::rust_syntax::RawCall,
}

/// Admit or reject one answered Ruby call site under the R2 policy.
/// Returns the category to report; appends a canonical edge when the
/// static model admits one. Pure over the definition answer, so the
/// number of lanes that asked never reaches the emitted bytes.
pub fn admit_definition(
    result: DefinitionResult,
    constructor_result: Option<&DefinitionResult>,
    model: &PolicyModel,
    ctx: &ResolveContext<'_>,
    pending: &PendingRubyCall,
    emitted_edge_ids: &mut HashSet<String>,
    edges: &mut Vec<Edge>,
) -> CallSiteCategory {
    let rel_path = &pending.rel_path;
    let call = &pending.call;

    // The regime is decided by codemap's syntax model, never by the
    // resolver: untyped receivers stay observations whatever the server
    // answers.
    let verifiable = match call.receiver {
        ReceiverShape::Constant(_) | ReceiverShape::ImplicitSelf => true,
        ReceiverShape::Constructed { .. } => constructor_result.is_some_and(default_new_provider),
        _ => false,
    };

    let locations = match result {
        DefinitionResult::Empty => Vec::new(),
        DefinitionResult::Locations(locs) => locs,
    };

    let mut seen_owned_ranges = BTreeSet::new();
    let locations = locations
        .into_iter()
        .filter(|location| {
            let Some(relative) = rel_path_of_uri(&location.uri, ctx.workspace_root)
                .filter(|path| ctx.rel_paths.contains(path))
            else {
                return true;
            };
            let Ok(file) = ctx.workspace_root.join(relative).canonicalize() else {
                return true;
            };
            seen_owned_ranges.insert((
                file,
                location.start_line,
                location.start_character,
                location.end_line,
                location.end_character,
            ))
        })
        .collect::<Vec<_>>();

    // Map in-repo locations to emitted definition nodes; collect every
    // external location for classification when nothing maps in-repo.
    // Candidate nodes are deduplicated, but distinct definition ranges
    // still prevent canonical admission.
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

    // The R2 memo's policy: multi-location answers stay observations. A
    // response mixing one in-repo location with external locations is
    // ambiguous, so a canonical edge requires exactly one total location
    // that maps in-repo.
    let uniquely_in_repo = locations.len() == 1 && candidates.len() == 1;

    // The `from` of a canonical edge must be an emitted callable: the
    // spatial index returns the smallest enclosing node at the call site,
    // which is a class/module for load-time calls in class bodies. Codemap
    // cannot attribute those to a callable, so they stay observations.
    let from_id = ctx
        .node_index
        .find(rel_path, call.name_start_byte)
        .map(str::to_string)
        .filter(|id| model.is_callable(id));

    let receiver_class = verifiable
        .then(|| receiver_class_id(call, rel_path, ctx, model))
        .flatten();

    match (verifiable, receiver_class.as_deref(), candidates.len()) {
        // Admitted canonical edge: exactly one total definition location
        // (mixed in-repo + external answers stay observations per the R2
        // memo), mapped to a unique candidate whose owner lies in the
        // receiver's verified ancestry with a matching method kind, called
        // from an emitted callable body.
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
        // Any remaining in-repo candidate is explicitly non-authoritative:
        // either the shape is guess regime or codemap could not admit the
        // target. Never an edge.
        (_, _, 1..) => CallSiteCategory::Candidate,
        (_, _, 0) => zero_candidate_category(&external_uris),
    }
}

/// Classify a zero-candidate site by the *set* of external locations, not
/// any single one: ruby-lsp's response ordering is not guaranteed, so
/// "the last location wins" would classify nondeterministically. The set
/// is ordered (`BTreeSet`), so the rule is deterministic for any ordering:
/// no locations → `Unmapped`; every location stdlib-derived →
/// `StandardLibrary`; any gem location present → `ExternalCrate`.
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

/// Provenance for a semantic Ruby `calls` edge: authoritative ruby-lsp
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
        fact_source: FactSource::RubyLsp,
        confidence: 255,
    }
}

/// Classify a definition location outside the repository. Stdlib-derived
/// answers (Ruby's own source tree and the RBS standard-library type
/// surface shipped in the `rbs` gem's `core/` and `stdlib/` directories)
/// are `StandardLibrary`; everything else out-of-repo is a gem.
pub fn classify_external(uri: &str) -> CallSiteCategory {
    let path = crate::lsp::path_from_uri(uri);
    // Ruby's own source lives under `<prefix>/lib/ruby/<version>/`; the
    // `gems/` subtree is installed gems, which are external.
    if path.contains("/lib/ruby/") && !path.contains("/lib/ruby/gems/") {
        return CallSiteCategory::StandardLibrary;
    }
    // The rbs gem bundles the stdlib's type definitions under its
    // `core/` and `stdlib/` directories (`.../gems/rbs-<ver>/core/*.rbs`).
    // Those are standard library, not third-party gem sigs.
    if path
        .split("/gems/")
        .any(|seg| seg.starts_with("rbs-") && (seg.contains("/core/") || seg.contains("/stdlib/")))
    {
        return CallSiteCategory::StandardLibrary;
    }
    CallSiteCategory::ExternalCrate
}

fn receiver_class_id(
    call: &crate::rust_syntax::RawCall,
    rel_path: &str,
    ctx: &ResolveContext<'_>,
    model: &PolicyModel,
) -> Option<String> {
    match &call.receiver {
        ReceiverShape::ImplicitSelf => {
            if call.scope.is_empty() {
                return None;
            }
            enclosing_class_id(&call.scope, rel_path, ctx.identity, model)
        }
        ReceiverShape::Constant(name) => {
            // Ruby-style lexical resolution (see [`DefinitionIndex::resolve_constant`]):
            // walk the enclosing scope chain innermost-first, then top level.
            // Never a repo-wide name search: the last-segment global fallback was
            // removed because it can admit a wrong-class canonical edge when two
            // same-named classes exist in different namespaces.
            match ctx.def_index.resolve_constant(name, &call.scope) {
                Resolution::Unique(def)
                    if matches!(def.kind, NodeKind::Type(TypeKind::Class) | NodeKind::Module) =>
                {
                    Some(def.node_id)
                }
                _ => None,
            }
        }
        ReceiverShape::Constructed { class_name, .. } => {
            match ctx.def_index.resolve_constant(class_name, &call.scope) {
                Resolution::Unique(def)
                    if model.default_constructor_receiver(&def.node_id, &call.raw_name) =>
                {
                    Some(def.node_id)
                }
                _ => None,
            }
        }
        #[allow(clippy::match_same_arms)] // PHP-only shapes vs unknown receiver
        ReceiverShape::Parent | ReceiverShape::Declared(_) => None,
        ReceiverShape::Other => None,
    }
}

/// Default Class#new in the pinned public RBS 4.1.2 source. The provider
/// declaration is required in addition to a unique destination and source
/// heritage checks; a resolver's name-only suggestion cannot establish it.
fn default_new_provider(result: &DefinitionResult) -> bool {
    let DefinitionResult::Locations(locations) = result else {
        return false;
    };
    if locations.len() != 1 {
        return false;
    }
    let location = &locations[0];
    let path = PathBuf::from(crate::lsp::path_from_uri(&location.uri));
    if !path.ends_with("rbs-4.1.2/core/class.rbs")
        || location.start_line != 164
        || location.end_line != 164
        || location.start_character != 2
        || location.end_character != 63
    {
        return false;
    }
    std::fs::read(path).is_ok_and(|source| {
        crate::schema::sha256_hex(&source)
            == "36c7659b5c6a936881a49ca4d1aade1c47198bf93ac33ded7118fb45e6e80ef6"
    })
}

/// Emitted node id of the innermost enclosing class/module for a scope
/// chain (innermost last), checking both `type` and `module` kinds. The
/// entries are fully qualified, so the final entry names the receiver.
fn enclosing_class_id(
    scope: &[String],
    rel_path: &str,
    identity: &SnapshotIdentity,
    model: &PolicyModel,
) -> Option<String> {
    let qualified = scope.last()?;
    let prefix = identity.language.id_prefix();
    for kind in ["type", "module"] {
        let candidate = format!("{prefix}{kind}:{rel_path}#{qualified}");
        if model.contains_class(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn rel_path_of_uri(uri: &str, workspace_root: &Path) -> Option<String> {
    let abs = crate::lsp::path_from_uri(uri);
    let abs_path = Path::new(&abs);
    // ruby-lsp canonicalizes paths; the recorded root may not. Try both.
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    abs_path
        .strip_prefix(&canonical_root)
        .or_else(|_| abs_path.strip_prefix(workspace_root))
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

fn uri_from_path(path: &Path) -> String {
    crate::lsp::uri_from_path(path)
}

/// Reader thread: parse LSP frames from stdout and send each JSON message.
/// True when `msg` is the `$/progress` `end` notification for the indexing
/// token — the index-complete signal. End notifications for any other token
/// (another server task finishing early) must not release the wait; before
/// the server's create request arrives the token is unknown and no progress
/// end qualifies.
fn progress_end(msg: &Value, expected_token: Option<&str>) -> bool {
    if msg.get("method").and_then(Value::as_str) != Some("$/progress") {
        return false;
    }
    let Some(expected) = expected_token else {
        return false;
    };
    msg.get("params")
        .and_then(|p| p.get("token"))
        .and_then(Value::as_str)
        == Some(expected)
        && msg
            .get("params")
            .and_then(|p| p.get("value"))
            .and_then(|v| v.get("kind"))
            .and_then(Value::as_str)
            == Some("end")
}

/// ruby-lsp issues server→client requests (e.g.
/// `window/workDoneProgress/create`) while indexing. Answer every request
/// with `null` so the protocol stays balanced, and record the progress
/// token the server chose so the index wait can tell the indexing sequence
/// apart from any other `$/progress` traffic. Runs on the transport's
/// reader thread.
fn server_request_reply(request: &Value, indexing_token: &Mutex<Option<String>>) -> Value {
    if request.get("method").and_then(Value::as_str) == Some("window/workDoneProgress/create") {
        if let Some(token) = request
            .get("params")
            .and_then(|p| p.get("token"))
            .and_then(Value::as_str)
        {
            *indexing_token.lock().expect("indexing token lock") = Some(token.to_string());
        }
    }
    Value::Null
}

/// Block until the index-complete progress end, or `timeout` elapses.
fn wait_indexed(
    rx: &mpsc::Receiver<Value>,
    indexing_token: &Mutex<Option<String>>,
    timeout: Duration,
) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            bail!("ruby-lsp indexing timed out");
        }
        match rx.recv_timeout(remaining) {
            Ok(msg) => {
                let expected = {
                    let token = indexing_token.lock().expect("indexing token lock");
                    token.clone()
                };
                if progress_end(&msg, expected.as_deref()) {
                    return Ok(());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                bail!("ruby-lsp indexing timed out");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("ruby-lsp stdout reader disconnected before index completion");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{EXTRACTOR_VERSION, FactSource, Provenance};

    #[test]
    fn bundle_version_probe_preserves_launch_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let error = probe_bundle_version(
            Command::new(tmp.path().join("missing-bundle")),
            Duration::from_millis(100),
        )
        .unwrap_err();
        let io_error = error.downcast_ref::<std::io::Error>().unwrap();
        assert_eq!(io_error.kind(), std::io::ErrorKind::NotFound);
        assert!(format!("{error:#}").contains("run bundle version probe"));
    }

    #[test]
    fn bundle_version_probe_preserves_deadline_failure() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let start = std::time::Instant::now();
        let error = probe_bundle_version(command, Duration::from_millis(100)).unwrap_err();
        let io_error = error.downcast_ref::<std::io::Error>().unwrap();
        assert_eq!(io_error.kind(), std::io::ErrorKind::TimedOut);
        assert!(format!("{error:#}").contains("exceeded 100ms"));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    #[cfg(unix)]
    fn bundle_version_probe_rejects_empty_output() {
        let error =
            probe_bundle_version(Command::new("true"), Duration::from_millis(100)).unwrap_err();
        assert!(error.to_string().contains("no version output"));
    }

    #[test]
    #[ignore = "requires the pinned Ruby-LSP operator bundle"]
    fn constructor_provider_requires_the_pinned_class_new_declaration() {
        let Availability::Present(bundle) = detect() else {
            panic!("pinned Ruby-LSP operator is required");
        };
        let root = tempfile::tempdir().unwrap();
        let source = "class Widget; def perform; 1; end; end\nWidget.new.perform\n";
        std::fs::write(root.path().join("app.rb"), source).unwrap();
        let client = Client::spawn(root.path(), &bundle).unwrap();
        client.open_document("app.rb", source).unwrap();
        client.wait_indexed(Duration::from_secs(120)).unwrap();
        let answer = client.definition("app.rb", 1, 7).unwrap();
        client.shutdown();
        assert!(
            default_new_provider(&answer),
            "the real default provider must qualify: {answer:?}"
        );
        assert!(!default_new_provider(&DefinitionResult::Empty));
        assert!(!default_new_provider(&DefinitionResult::Locations(
            Vec::new()
        )));
        let DefinitionResult::Locations(locations) = answer else {
            unreachable!()
        };
        let location = locations[0].clone();
        assert!(!default_new_provider(&DefinitionResult::Locations(vec![
            location.clone(),
            location.clone()
        ])));
        let mut wrong_range = location.clone();
        wrong_range.start_line += 1;
        assert!(!default_new_provider(&DefinitionResult::Locations(vec![
            wrong_range
        ])));
        let bytes = std::fs::read(crate::lsp::path_from_uri(&location.uri)).unwrap();
        let directory = root.path().join("rbs-4.1.2/core");
        std::fs::create_dir_all(&directory).unwrap();
        let wrong_file = directory.join("module.rbs");
        std::fs::write(&wrong_file, &bytes).unwrap();
        let mut substituted = location.clone();
        substituted.uri = uri_from_path(&wrong_file);
        assert!(!default_new_provider(&DefinitionResult::Locations(vec![
            substituted
        ])));
        let drifted = directory.join("class.rbs");
        let mut drifted_bytes = bytes;
        drifted_bytes.extend_from_slice(b"\n# changed provider\n");
        std::fs::write(&drifted, drifted_bytes).unwrap();
        let mut drifted_location = location;
        drifted_location.uri = uri_from_path(&drifted);
        assert!(!default_new_provider(&DefinitionResult::Locations(vec![
            drifted_location
        ])));
    }

    fn prov() -> Provenance {
        Provenance {
            repository: "r".into(),
            commit_sha: "c".into(),
            rel_path: "a.rb".into(),
            span: Span::from_ts(0, 1, (0, 0), (0, 1)),
            file_hash: "h".into(),
            extractor_version: EXTRACTOR_VERSION.to_string(),
            fact_source: FactSource::Prism,
            confidence: u8::MAX,
        }
    }

    fn class_node(id: &str, is_module: bool) -> Node {
        Node {
            python: None,
            id: id.into(),
            kind: if is_module {
                NodeKind::Module
            } else {
                NodeKind::Type(TypeKind::Class)
            },
            name: id.rsplit('#').next().unwrap_or(id).into(),
            symbol_key: id.into(),
            provenance: prov(),
        }
    }

    fn method_node(id: &str) -> Node {
        Node {
            python: None,
            id: id.into(),
            kind: NodeKind::Method,
            name: id.rsplit('#').next().unwrap_or(id).into(),
            symbol_key: id.into(),
            provenance: prov(),
        }
    }

    fn edge(kind: EdgeKind, from: &str, to: &str) -> Edge {
        Edge {
            id: edge_id(kind, from, to, 0),
            kind,
            from: from.into(),
            to: to.into(),
            provenance: prov(),
        }
    }

    /// The 2026-08-06 acceptance finding: `resolve_mixin` used to emit an
    /// `Implements` edge for every flavor, so `extend`-ed modules fed the
    /// instance closure and an `extend`-ed module's methods could admit an
    /// implicit-self canonical edge.
    #[test]
    fn extend_does_not_feed_the_instance_regime() {
        let model = PolicyModel::build(
            &[
                class_node("ruby:type:a.rb#A", false),
                class_node("ruby:module:a.rb#M", true),
                method_node("ruby:method:a.rb#M::mixed"),
            ],
            &[edge(
                EdgeKind::Extends,
                "ruby:type:a.rb#A",
                "ruby:module:a.rb#M",
            )],
        );
        assert!(
            !model.admit(
                "ruby:type:a.rb#A",
                &ReceiverShape::ImplicitSelf,
                "ruby:method:a.rb#M::mixed"
            ),
            "an extend-ed module contributes no instance methods"
        );
    }

    #[test]
    fn extend_feeds_the_singleton_regime() {
        let model = PolicyModel::build(
            &[
                class_node("ruby:type:a.rb#A", false),
                class_node("ruby:module:a.rb#M", true),
                method_node("ruby:method:a.rb#M::self.mixed"),
            ],
            &[edge(
                EdgeKind::Extends,
                "ruby:type:a.rb#A",
                "ruby:module:a.rb#M",
            )],
        );
        assert!(
            model.admit(
                "ruby:type:a.rb#A",
                &ReceiverShape::Constant("A".into()),
                "ruby:method:a.rb#M::self.mixed"
            ),
            "an extend-ed module's instance methods become singleton methods"
        );
    }

    #[test]
    fn include_feeds_the_instance_regime_only() {
        let model = PolicyModel::build(
            &[
                class_node("ruby:type:a.rb#A", false),
                class_node("ruby:module:a.rb#M", true),
                method_node("ruby:method:a.rb#M::mixed"),
                method_node("ruby:method:a.rb#M::self.mixed"),
            ],
            &[edge(
                EdgeKind::Implements,
                "ruby:type:a.rb#A",
                "ruby:module:a.rb#M",
            )],
        );
        assert!(model.admit(
            "ruby:type:a.rb#A",
            &ReceiverShape::ImplicitSelf,
            "ruby:method:a.rb#M::mixed"
        ));
        assert!(
            !model.admit(
                "ruby:type:a.rb#A",
                &ReceiverShape::Constant("A".into()),
                "ruby:method:a.rb#M::self.mixed"
            ),
            "include contributes no singleton methods"
        );
    }

    #[test]
    fn mixin_closures_follow_the_superclass_chain() {
        let model = PolicyModel::build(
            &[
                class_node("ruby:type:a.rb#Child", false),
                class_node("ruby:type:a.rb#Parent", false),
                class_node("ruby:module:a.rb#Inst", true),
                class_node("ruby:module:a.rb#Sing", true),
                method_node("ruby:method:a.rb#Inst::inst"),
                method_node("ruby:method:a.rb#Sing::self.sing"),
            ],
            &[
                edge(
                    EdgeKind::Inherits,
                    "ruby:type:a.rb#Child",
                    "ruby:type:a.rb#Parent",
                ),
                edge(
                    EdgeKind::Implements,
                    "ruby:type:a.rb#Parent",
                    "ruby:module:a.rb#Inst",
                ),
                edge(
                    EdgeKind::Extends,
                    "ruby:type:a.rb#Parent",
                    "ruby:module:a.rb#Sing",
                ),
            ],
        );
        assert!(model.admit(
            "ruby:type:a.rb#Child",
            &ReceiverShape::ImplicitSelf,
            "ruby:method:a.rb#Inst::inst"
        ));
        assert!(model.admit(
            "ruby:type:a.rb#Child",
            &ReceiverShape::Constant("Child".into()),
            "ruby:method:a.rb#Sing::self.sing"
        ));
    }

    #[test]
    fn only_emitted_callables_verify_as_from_nodes() {
        let model = PolicyModel::build(
            &[
                class_node("ruby:type:a.rb#A", false),
                method_node("ruby:method:a.rb#A::body"),
            ],
            &[],
        );
        assert!(model.is_callable("ruby:method:a.rb#A::body"));
        assert!(
            !model.is_callable("ruby:type:a.rb#A"),
            "a class-body load-time call has no enclosing method"
        );
    }

    #[test]
    fn the_hook_records_the_indexing_token_and_answers_every_server_request() {
        let token = Mutex::new(None);
        let create = json!({"jsonrpc": "2.0", "id": 99, "method": "window/workDoneProgress/create", "params": {"token": "indexing-progress"}});
        assert_eq!(server_request_reply(&create, &token), Value::Null);
        assert_eq!(
            *token.lock().expect("token lock"),
            Some("indexing-progress".to_string())
        );

        let other =
            json!({"jsonrpc": "2.0", "id": 7, "method": "workspace/configuration", "params": {}});
        assert_eq!(
            server_request_reply(&other, &token),
            Value::Null,
            "every server request is answered"
        );
        assert_eq!(
            *token.lock().expect("token lock"),
            Some("indexing-progress".to_string()),
            "an unrelated request leaves the token alone"
        );
    }

    #[test]
    fn wait_indexed_releases_only_on_the_indexing_token_end() {
        let (tx, rx) = mpsc::channel::<Value>();
        let token = Mutex::new(Some("indexing-progress".to_string()));

        // A different token finishing first must NOT release the wait.
        tx.send(json!({"method": "$/progress", "params": {"token": "diagnostics", "value": {"kind": "end"}}}))
        .unwrap();
        // A begin for the right token must not release either.
        tx.send(json!({"method": "$/progress", "params": {"token": "indexing-progress", "value": {"kind": "begin"}}}))
        .unwrap();
        // The real end releases.
        tx.send(json!({"method": "$/progress", "params": {"token": "indexing-progress", "value": {"kind": "end"}}}))
        .unwrap();

        wait_indexed(&rx, &token, Duration::from_secs(5)).expect("indexed");
    }

    #[test]
    fn wait_indexed_does_not_release_before_the_token_is_known() {
        let (tx, rx) = mpsc::channel::<Value>();
        let token = Mutex::new(None);

        // An end for any token before the server's create request has been
        // seen by the hook is not the indexing end.
        tx.send(json!({"method": "$/progress", "params": {"token": "indexing-progress", "value": {"kind": "end"}}}))
        .unwrap();
        drop(tx);
        assert!(wait_indexed(&rx, &token, Duration::from_millis(50)).is_err());
    }

    #[test]
    fn progress_end_ignores_foreign_tokens_and_shapes() {
        let end = |token: &str, kind: &str| json!({"method": "$/progress", "params": {"token": token, "value": {"kind": kind}}});
        assert!(progress_end(
            &end("indexing-progress", "end"),
            Some("indexing-progress")
        ));
        assert!(!progress_end(
            &end("other", "end"),
            Some("indexing-progress")
        ));
        assert!(!progress_end(
            &end("indexing-progress", "report"),
            Some("indexing-progress")
        ));
        assert!(
            !progress_end(&end("indexing-progress", "end"), None),
            "no known token means no progress end qualifies"
        );
    }

    #[test]
    fn uri_paths_with_special_characters_round_trip() {
        // `#` and space in a repository path must survive the
        // path→uri→path round trip: `#` is a URI fragment delimiter and
        // must be percent-encoded, never left literal.
        let dir = tempfile::tempdir().expect("temp dir");
        let special = dir.path().join("app #1");
        std::fs::create_dir(&special).expect("create dir");
        let uri = uri_from_path(&special);
        assert!(uri.contains("%23"), "a literal '#' would truncate the URI");
        assert!(!uri.contains(' '));
        assert_eq!(
            crate::lsp::path_from_uri(&uri),
            special
                .canonicalize()
                .expect("canonicalize")
                .to_string_lossy(),
            "the encoded URI must decode back to the real path"
        );
        // Rel-path joining used by open_document/definition keeps the
        // fragment delimiter out of the document URI.
        let joined = format!("{}/{}", uri, crate::lsp::encode_uri_path("lib/a b.rb"));
        assert!(joined.contains("a%20b.rb"));
        assert!(rel_path_of_uri(&joined, &special).is_some());
    }

    #[test]
    fn multi_external_locations_classify_by_set_not_last_element() {
        // Both orders of the same location set must classify identically —
        // the acceptance finding was "last URI wins", which ruby-lsp's
        // unguaranteed ordering makes nondeterministic.
        let stdlib = "file:///opt/ruby/lib/ruby/4.0.0/json.rb";
        let gem = "file:///opt/ruby/lib/ruby/gems/4.0.0/gems/foo-1.0.0/lib/foo.rb";
        for uris in [[stdlib, gem], [gem, stdlib]] {
            let set: BTreeSet<String> = uris.iter().map(|u| (*u).to_string()).collect();
            assert_eq!(
                zero_candidate_category(&set),
                CallSiteCategory::ExternalCrate,
                "any gem location makes the set external: {uris:?}"
            );
        }
        let pure_stdlib: BTreeSet<String> = [stdlib, "file:///opt/ruby/lib/ruby/4.0.0/erb.rb"]
            .iter()
            .map(|u| (*u).to_string())
            .collect();
        assert_eq!(
            zero_candidate_category(&pure_stdlib),
            CallSiteCategory::StandardLibrary
        );
        assert_eq!(
            zero_candidate_category(&BTreeSet::new()),
            CallSiteCategory::Unmapped
        );
    }
    #[test]
    #[allow(clippy::too_many_lines)]
    fn absolute_receivers_admit_the_source_labelled_definition_answers() {
        use crate::lsp::{DefinitionLocation, byte_offset_to_lsp_pos};
        use crate::schema::Language;
        let source = include_str!("../tests/fixtures/ruby_absolute/absolute.rb");
        let expected: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/ruby_absolute/expected.json"
        ))
        .unwrap();
        assert_eq!(
            crate::schema::sha256_hex(source.as_bytes()),
            expected["source_sha256"]
        );
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let path = "lib/absolute.rb";
        std::fs::create_dir(root.join("lib")).unwrap();
        std::fs::write(root.join(path), source).unwrap();
        for args in [
            vec!["init", "--quiet"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        ] {
            assert!(
                Command::new("git")
                    .current_dir(&root)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let snapshot = crate::extract(&crate::ExtractOptions {
            path: root.clone(),
            language: Language::Ruby,
            calls_required: false,
            measures: false,
            twins: false,
        })
        .unwrap();
        let model = PolicyModel::build(&snapshot.nodes, &snapshot.edges);
        let definitions = DefinitionIndex::from_nodes(&snapshot.nodes);
        let node_index = NodeIndex::build(
            snapshot
                .nodes
                .iter()
                .map(|node| (path.to_string(), node.provenance.span, node.id.clone())),
        );
        let paths = HashSet::from([path.to_string()]);
        let sources = BTreeMap::from([(path, source.as_bytes())]);
        let context = ResolveContext {
            workspace_root: &root,
            identity: &snapshot.identity,
            rel_paths: &paths,
            file_sources: &sources,
            node_index: &node_index,
            def_index: &definitions,
        };
        let symbols: BTreeMap<_, _> = snapshot
            .nodes
            .iter()
            .map(|node| {
                (
                    node.id.as_str(),
                    node.symbol_key
                        .split_once('#')
                        .map_or("", |(_, symbol)| symbol),
                )
            })
            .collect();
        let labels: Vec<[String; 2]> = serde_json::from_value(expected["calls"].clone()).unwrap();
        let raw = crate::ruby_syntax::extract_file(path, source.as_bytes()).unwrap();
        let mut admitted = Vec::new();
        for call in raw.calls {
            let caller = node_index.find(path, call.name_start_byte).unwrap();
            let Some(label) = labels.iter().find(|label| {
                label[0] == symbols[caller]
                    && label[1]
                        .rsplit("::")
                        .next()
                        .unwrap()
                        .trim_start_matches("self.")
                        == call.raw_name
            }) else {
                continue;
            };
            let target = snapshot
                .nodes
                .iter()
                .find(|node| symbols[node.id.as_str()] == label[1])
                .unwrap();
            let (line, character) =
                byte_offset_to_lsp_pos(source.as_bytes(), target.provenance.span.start_byte);
            let pending = PendingRubyCall {
                rel_path: path.into(),
                file_hash: crate::schema::sha256_hex(source.as_bytes()),
                call,
            };
            let mut edges = Vec::new();
            assert_eq!(
                admit_definition(
                    DefinitionResult::Locations(vec![DefinitionLocation {
                        uri: uri_from_path(&root.join(path)),
                        start_line: line,
                        start_character: character,
                        end_line: line,
                        end_character: character,
                    }]),
                    None,
                    &model,
                    &context,
                    &pending,
                    &mut HashSet::new(),
                    &mut edges
                ),
                CallSiteCategory::Resolved
            );
            assert_eq!(edges.len(), 1);
            admitted.push([
                symbols[edges[0].from.as_str()].to_string(),
                symbols[edges[0].to.as_str()].to_string(),
            ]);
            let mut absent_edges = Vec::new();
            assert_eq!(
                admit_definition(
                    DefinitionResult::Empty,
                    None,
                    &model,
                    &context,
                    &pending,
                    &mut HashSet::new(),
                    &mut absent_edges
                ),
                CallSiteCategory::Unmapped
            );
            assert_eq!(absent_edges, [] as [Edge; 0]);
            for decoy in ["Base::base_marker", "Outer::Top::decoy_marker"] {
                let target = snapshot
                    .nodes
                    .iter()
                    .find(|node| symbols[node.id.as_str()] == decoy)
                    .unwrap();
                let (line, character) =
                    byte_offset_to_lsp_pos(source.as_bytes(), target.provenance.span.start_byte);
                let mut rejected = Vec::new();
                assert_eq!(
                    admit_definition(
                        DefinitionResult::Locations(vec![DefinitionLocation {
                            uri: uri_from_path(&root.join(path)),
                            start_line: line,
                            start_character: character,
                            end_line: line,
                            end_character: character,
                        }]),
                        None,
                        &model,
                        &context,
                        &pending,
                        &mut HashSet::new(),
                        &mut rejected
                    ),
                    CallSiteCategory::Candidate
                );
                assert_eq!(rejected, [] as [Edge; 0]);
            }
        }
        admitted.sort();
        assert_eq!(admitted, labels);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn only_identical_owned_definition_ranges_are_duplicates() {
        use crate::lsp::DefinitionLocation;
        use crate::rust_syntax::RawCall;
        use crate::schema::{Language, RepoIdentity, ToolVersions};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = "class A\n def run\n  target\n end\n def target\n  1\n end\nend\n";
        std::fs::write(root.join("a.rb"), source).unwrap();
        std::fs::write(root.join("b.rb"), source).unwrap();
        let identity = SnapshotIdentity::build(
            Language::Ruby,
            RepoIdentity {
                id: "https://example.invalid/fixture.git".into(),
                origin: None,
                is_canonical: true,
            },
            "commit".into(),
            "tree".into(),
            ToolVersions::syntax_ruby(),
            false,
        );
        let mut class = class_node("ruby:type:a.rb#A", false);
        class.provenance.span = Span::from_ts(0, source.len(), (0, 0), (7, 3));
        let mut caller = method_node("ruby:method:a.rb#A::run");
        caller.provenance.span = Span::from_ts(
            source.find("def run").unwrap(),
            source.find(" def target").unwrap() - 1,
            (1, 1),
            (3, 4),
        );
        let mut target = method_node("ruby:method:a.rb#A::target");
        target.provenance.span = Span::from_ts(
            source.find("def target").unwrap(),
            source.len() - 4,
            (4, 1),
            (6, 4),
        );
        let mut other_class = class.clone();
        let mut other_target = target.clone();
        for node in [&mut other_class, &mut other_target] {
            node.id = node.id.replace(":a.rb#", ":b.rb#");
            node.symbol_key = node.symbol_key.replace(":a.rb#", ":b.rb#");
            node.provenance.rel_path = "b.rb".into();
        }
        let nodes = vec![class, caller, target, other_class, other_target];
        let model = PolicyModel::build(&nodes, &[]);
        let index = DefinitionIndex::from_nodes(&nodes);
        let node_index = NodeIndex::build(nodes.iter().map(|node| {
            (
                node.provenance.rel_path.clone(),
                node.provenance.span,
                node.id.clone(),
            )
        }));
        let paths = HashSet::from(["a.rb".to_string(), "b.rb".to_string()]);
        let sources = BTreeMap::from([("a.rb", source.as_bytes()), ("b.rb", source.as_bytes())]);
        let ctx = ResolveContext {
            workspace_root: &root,
            identity: &identity,
            rel_paths: &paths,
            file_sources: &sources,
            node_index: &node_index,
            def_index: &index,
        };
        let start = source.find("\n  target").unwrap() + 3;
        let pending = PendingRubyCall {
            rel_path: "a.rb".into(),
            file_hash: "hash".into(),
            call: RawCall {
                raw_name: "target".into(),
                path_root: None,
                span: Span::from_ts(start, start + 6, (2, 2), (2, 8)),
                name_start_byte: start,
                receiver: ReceiverShape::ImplicitSelf,
                scope: vec!["A".into()],
            },
        };
        let uri = uri_from_path(&root);
        let owned = DefinitionLocation {
            uri: format!("{uri}/a.rb"),
            start_line: 4,
            start_character: 5,
            end_line: 4,
            end_character: 11,
        };
        let alias = DefinitionLocation {
            uri: format!("{uri}//a.rb"),
            ..owned
        };
        let other_position = DefinitionLocation {
            start_character: 6,
            ..owned.clone()
        };
        let other_range = DefinitionLocation {
            end_character: 12,
            ..owned.clone()
        };
        let other_file = DefinitionLocation {
            uri: format!("{uri}/b.rb"),
            ..owned
        };
        let external = DefinitionLocation {
            uri: "file:///external/gem/a.rb".into(),
            ..owned
        };
        for (locations, expected) in [
            (vec![owned.clone()], CallSiteCategory::Resolved),
            (
                vec![owned.clone(), alias.clone()],
                CallSiteCategory::Resolved,
            ),
            (
                vec![alias.clone(), owned.clone()],
                CallSiteCategory::Resolved,
            ),
            (
                vec![owned.clone(), other_position],
                CallSiteCategory::Candidate,
            ),
            (
                vec![owned.clone(), other_range],
                CallSiteCategory::Candidate,
            ),
            (vec![owned.clone(), other_file], CallSiteCategory::Candidate),
            (
                vec![owned.clone(), alias, external.clone()],
                CallSiteCategory::Candidate,
            ),
            (vec![owned.clone(), external], CallSiteCategory::Candidate),
        ] {
            let mut edges = Vec::new();
            let category = admit_definition(
                DefinitionResult::Locations(locations),
                None,
                &model,
                &ctx,
                &pending,
                &mut HashSet::new(),
                &mut edges,
            );
            assert_eq!(category, expected);
            assert_eq!(
                edges.len(),
                usize::from(expected == CallSiteCategory::Resolved)
            );
        }
        // A unique destination alone does not prove a constructor's return type.
        let single_index = DefinitionIndex::from_nodes(&nodes[..3]);
        let constructor_context = ResolveContext {
            def_index: &single_index,
            ..ctx
        };
        let mut constructor_call = pending;
        constructor_call.call.receiver = ReceiverShape::Constructed {
            class_name: "A".into(),
            constructor_start: 0,
        };
        let absent = DefinitionResult::Empty;
        for provider in [None, Some(&absent)] {
            let mut edges = Vec::new();
            assert_eq!(
                admit_definition(
                    DefinitionResult::Locations(vec![owned.clone()]),
                    provider,
                    &model,
                    &constructor_context,
                    &constructor_call,
                    &mut HashSet::new(),
                    &mut edges,
                ),
                CallSiteCategory::Candidate
            );
            assert_eq!(edges.len(), 0);
        }
    }
}
