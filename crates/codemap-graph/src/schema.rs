//! Versioned snapshot schema: identity, nodes, edges, provenance, and
//! canonical byte-deterministic serialization.
//!
//! This module owns *facts only* — it never parses source, runs Git, or
//! performs transport. Every emitted node and edge carries source
//! provenance so a downstream consumer can never mistake a parser guess
//! for a structural fact.
//!
//! ## Snapshot contract v2
//!
//! A snapshot is identified as `codemap.snapshot.v8`. It is byte-stable
//! for the same clean commit, extractor version, grammar version, and
//! options. Determinism is enforced by:
//!
//! - sorting `files`, `nodes`, and `edges` by their deterministic keys;
//! - serializing via [`canonical_json`], which orders struct fields by
//!   declaration and rejects maps (only `BTreeMap`/sorted `Vec` are used);
//! - deriving [`SnapshotIdentity::snapshot_id`] as a SHA-256 of the
//!   canonical identity block, never wall-clock time or randomness.
//!
//! `repository` and `commit` are snapshot *identity* fields, not codemap
//! fact nodes. A consumer that needs repository or commit nodes builds them
//! from that identity itself.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Schema version tag stamped onto every snapshot.
pub const SCHEMA_VERSION: &str = "codemap.snapshot.v8";

/// The counting policy behind Rust measures.
///
/// Cyclomatic complexity and source lines per callable, line classes per
/// file, and clone groups over the normalised token stream, under the
/// counting rules each language module keeps next to its grammar, and the
/// form measures ([`Measure::params`],
/// [`Measure::nesting`], [`Measure::chain`]) over the same body. The name
/// travels in [`SnapshotIdentity::measures_policy`]; a change to any
/// counting rule is a new version, so two snapshots counted under
/// different rules never compare by accident. `cc-sloc/rust/1` was the
/// name before clone groups joined the measured snapshot and
/// `cc-sloc-clones/rust/1` the name before the form measures did; the CC,
/// line, and clone rules did not change either time.
pub const MEASURES_POLICY_RUST_FORM_V1: &str = "cc-sloc-clones-form/rust/1";
/// The PHP counting policy.
///
/// Plan section 4's PHP rules, the same line rule, the same clone
/// thresholds, and the form measures ([`Measure::params`],
/// [`Measure::nesting`], [`Measure::chain`]) over the same body.
/// `cc-sloc-clones/php/1` was the name before the form measures joined;
/// the CC, line, and clone rules did not change.
pub const MEASURES_POLICY_PHP_FORM_V1: &str = "cc-sloc-clones-form/php/1";
/// The Go counting policy.
///
/// `if`, `for`, cases beyond the first, `&&` and `||`; the same line
/// rule, the same clone thresholds, and the form measures
/// ([`Measure::params`], [`Measure::nesting`], [`Measure::chain`]) over
/// the same body. `cc-sloc-clones/go/1` was the name before the form
/// measures joined; the CC, line, and clone rules did not change.
pub const MEASURES_POLICY_GO_FORM_V1: &str = "cc-sloc-clones-form/go/1";
/// The Ruby counting policy.
///
/// CC and line counts from Prism's tree, clone groups over Prism's own
/// token stream (`ruby_lex`) under the same clone thresholds, and the four
/// form measures ([`Measure::params`], [`Measure::nesting`],
/// [`Measure::chain`], [`Measure::ivars`]) counted over the same `def`
/// body. `cc-sloc/ruby/1` was the name before clone groups joined and
/// `cc-sloc-clones/ruby/1` the name before the form measures did; the CC,
/// line, and clone rules did not change either time.
pub const MEASURES_POLICY_RUBY_FORM_V1: &str = "cc-sloc-clones-form/ruby/1";

/// The counting policy a language's measures are emitted under, if the
/// language has one.
#[must_use]
pub const fn measures_policy(language: Language) -> Option<&'static str> {
    match language {
        Language::Rust => Some(MEASURES_POLICY_RUST_FORM_V1),
        Language::Php => Some(MEASURES_POLICY_PHP_FORM_V1),
        Language::Go => Some(MEASURES_POLICY_GO_FORM_V1),
        Language::Ruby => Some(MEASURES_POLICY_RUBY_FORM_V1),
        Language::Cpp | Language::JsTs | Language::Python => None,
    }
}

/// Whether the language's current counting policy carries the form
/// measures.
///
/// [`Measure::params`], [`Measure::nesting`], [`Measure::chain`], and
/// [`Measure::ivars`] where the language has instance variables. Every
/// current policy does, one language per slice under its own oracle; the
/// question stays a function so a consumer keeps asking it rather than
/// assuming, as with [`clone_measures_available`].
#[must_use]
pub const fn form_measures_available(language: Language) -> bool {
    match language {
        Language::Ruby | Language::Rust | Language::Php | Language::Go => true,
        Language::Cpp | Language::JsTs | Language::Python => false,
    }
}

/// Whether the language's current counting policy includes clone
/// detection. Every current policy does; the question stays a function so
/// a consumer keeps asking it rather than assuming.
#[must_use]
pub const fn clone_measures_available(language: Language) -> bool {
    match language {
        Language::Rust | Language::Php | Language::Go | Language::Ruby => true,
        Language::Cpp | Language::JsTs | Language::Python => false,
    }
}

/// Clone thresholds under every counting policy that carries clone
/// groups: a run of equal normalised tokens is a clone when it spans at
/// least this many tokens and every occurrence covers at least this many
/// lines. jscpd's defaults; `GitClear`'s duplicated-block definition uses
/// the same line count (plan section 2.5).
pub const CLONE_MIN_TOKENS: usize = 50;
/// See [`CLONE_MIN_TOKENS`].
pub const CLONE_MIN_LINES: u32 = 5;

/// Twin thresholds (`extract --measures --twins`).
///
/// Two callables are twins when the [`TWIN_SHINGLE_TOKENS`]-token windows
/// they share cover at least [`TWIN_MIN_CONTAINMENT_PERCENT`] of the
/// smaller body's windows, both bodies holding at least
/// [`TWIN_MIN_TOKENS`] normalised tokens. A window seen in more than
/// [`TWIN_NOISE_CALLABLES`] callables is boilerplate and seeds no
/// candidate pair. Chosen on 2026-09-15 over a 5,192-method Rails
/// repository: these values named 885 pairs (17 exact, 868 edited copies)
/// and the one pair a reader had found by hand; eighty percent lost that
/// pair, a fifty-token floor tripled the count with short boilerplate
/// (`crates/codemap/src/twins.rs`).
pub const TWIN_SHINGLE_TOKENS: usize = 5;
/// See [`TWIN_SHINGLE_TOKENS`].
pub const TWIN_MIN_TOKENS: usize = 80;
/// See [`TWIN_SHINGLE_TOKENS`].
pub const TWIN_MIN_CONTAINMENT_PERCENT: u32 = 70;
/// See [`TWIN_SHINGLE_TOKENS`].
pub const TWIN_NOISE_CALLABLES: usize = 64;

/// The twin policy a language's pairs are emitted under.
///
/// Every language with a counting policy has a token stream, so every
/// language has one; the name travels in
/// [`SnapshotIdentity::twins_policy`] so two snapshots paired under
/// different rules never compare by accident.
#[must_use]
pub const fn twins_policy(language: Language) -> Option<&'static str> {
    match language {
        Language::Rust => Some("twins/rust/1"),
        Language::Ruby => Some("twins/ruby/1"),
        Language::Php => Some("twins/php/1"),
        Language::Go => Some("twins/go/1"),
        Language::Cpp | Language::JsTs | Language::Python => None,
    }
}

/// What a language's structural facts describe, when that is narrower than
/// the compiled program.
///
/// C++ syntax facts describe the source as spelled: the preprocessor is not
/// run and no macro is expanded, so a macro from any header or the command
/// line can make the compiled program differ from the facts. The policy is
/// stamped on the identity so no consumer reads a spelled fact as a
/// compiled one. Other languages have no preprocessor and carry none.
#[must_use]
pub const fn structure_policy(language: Language) -> Option<&'static str> {
    match language {
        Language::Cpp => Some("spelled/cpp/1"),
        Language::JsTs => Some("spelled/js_ts/1"),
        Language::Python => Some("spelled/python/1"),
        Language::Rust | Language::Ruby | Language::Php | Language::Go => None,
    }
}

/// Source language of a snapshot. One language per extraction run; the
/// validator enforces that every fact's ID namespace matches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Ruby,
    Php,
    Go,
    /// C++ source and headers selected explicitly with `--language cpp`.
    /// `.C` is included; `.c` is not.
    Cpp,
    /// JavaScript, TypeScript, JSX and TSX share one explicit source family.
    #[serde(rename = "js_ts")]
    JsTs,
    Python,
}

impl Language {
    /// Stable lowercase tag used in identity serialization.
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Ruby => "ruby",
            Self::Php => "php",
            Self::Go => "go",
            Self::Cpp => "cpp",
            Self::JsTs => "js_ts",
            Self::Python => "python",
        }
    }

    /// Node-ID / symbol-key namespace prefix, separator included.
    pub const fn id_prefix(self) -> &'static str {
        match self {
            Self::Rust => "rust:",
            Self::Ruby => "ruby:",
            Self::Php => "php:",
            Self::Go => "go:",
            Self::Cpp => "cpp:",
            Self::JsTs => "js_ts:",
            Self::Python => "python:",
        }
    }

    /// The authoritative structural fact source for this language's
    /// nodes and syntax-derived edges.
    pub const fn structural_fact_source(self) -> FactSource {
        match self {
            Self::Rust | Self::Php | Self::Go | Self::Cpp | Self::JsTs => FactSource::TreeSitter,
            Self::Ruby => FactSource::Prism,
            Self::Python => FactSource::Cpython,
        }
    }
}

/// Extractor version, baked in at compile time.
pub const EXTRACTOR_VERSION: &str = env!("CARGO_PKG_VERSION");

const fn is_empty_string(s: &str) -> bool {
    s.is_empty()
}

/// Grammar/tool versions recorded in snapshot identity so two extractions
/// that differ only by toolchain cannot claim the same snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolVersions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mypyc: Option<String>,
    /// `tree-sitter` crate version (empty when the language parser is not
    /// tree-sitter based).
    #[serde(default, skip_serializing_if = "is_empty_string")]
    pub tree_sitter: String,
    /// `tree-sitter-rust` grammar version (Rust snapshots).
    #[serde(default, skip_serializing_if = "is_empty_string")]
    pub tree_sitter_rust: String,
    /// Ruby parser identity (e.g. `prism 1.9.0`); empty for non-Ruby
    /// snapshots.
    #[serde(default, skip_serializing_if = "is_empty_string")]
    pub ruby_parser: String,
    /// `rust-analyzer` version, present only in semantic (`--calls`) mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust_analyzer: Option<String>,
    /// Companion `rustc` version selected for semantic analysis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rustc: Option<String>,
    /// Companion `cargo` version used for dependency inventory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo: Option<String>,
    /// Effective compilation target. Codemap's fixed semantic policy uses
    /// the companion compiler's host target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_triple: Option<String>,
    /// Fixed Cargo feature policy used by rust-analyzer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_feature_policy: Option<String>,
    /// SHA-256 of the tracked root `Cargo.lock`, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_lock_sha256: Option<String>,
    /// Fixed, code-execution-free rust-analyzer launch policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust_analyzer_policy: Option<RustAnalyzerPolicy>,
    /// `tree-sitter-php` grammar version (PHP snapshots).
    #[serde(default, skip_serializing_if = "is_empty_string")]
    pub tree_sitter_php: String,
    /// `ruby-lsp` version, present only in Ruby semantic (`--calls`) mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruby_lsp: Option<String>,
    /// Companion Ruby interpreter version selected for semantic analysis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruby: Option<String>,
    /// Companion RBS version used by ruby-lsp's type index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rbs: Option<String>,
    /// SHA-256 of codemap's canonical Ruby tool-bundle lockfile
    /// (`toolchain/ruby/Gemfile.lock`). The extraction refuses to run
    /// unless the operator's bundle satisfies exactly this pinned set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruby_tool_bundle_sha256: Option<String>,
    /// `phpactor` version, present only in PHP semantic (`--calls`) mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phpactor: Option<String>,
    /// Companion PHP interpreter version selected for semantic analysis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub php: Option<String>,
    /// SHA-256 of codemap's canonical PHP tool-bundle lockfile
    /// (`toolchain/php/composer.lock`). Same pin discipline as the Ruby
    /// bundle: extraction refuses to run unless the operator's bundle
    /// satisfies exactly this pinned set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub php_tool_bundle_sha256: Option<String>,
    /// `tree-sitter-go` grammar version (Go snapshots).
    #[serde(default, skip_serializing_if = "is_empty_string")]
    pub tree_sitter_go: String,
    /// `gopls` version, present only in Go semantic (`--calls`) mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gopls: Option<String>,
    /// Companion `go` toolchain version gopls loaded the workspace with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go: Option<String>,
    /// SHA-256 of codemap's canonical Go tool-bundle lockfile
    /// (`toolchain/go/go.sum`). Same pin discipline as the Ruby and PHP
    /// bundles: extraction refuses to run unless the operator's bundle
    /// satisfies exactly this pinned set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go_tool_bundle_sha256: Option<String>,
    /// `tree-sitter-cpp` grammar version (C++ snapshots).
    #[serde(default, skip_serializing_if = "is_empty_string")]
    pub tree_sitter_cpp: String,
    #[serde(default, skip_serializing_if = "is_empty_string")]
    pub tree_sitter_javascript: String,
    #[serde(default, skip_serializing_if = "is_empty_string")]
    pub tree_sitter_typescript: String,
}

/// Semantic rust-analyzer policy. These values are fixed by the extractor,
/// not runtime options, and therefore participate in snapshot identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RustAnalyzerPolicy {
    pub cargo_offline: bool,
    pub build_scripts_enabled: bool,
    pub proc_macros_enabled: bool,
    pub background_checks_enabled: bool,
}

pub const CARGO_FEATURE_POLICY: &str = "default";

impl RustAnalyzerPolicy {
    pub const fn fixed() -> Self {
        Self {
            cargo_offline: true,
            build_scripts_enabled: false,
            proc_macros_enabled: false,
            background_checks_enabled: false,
        }
    }
}

impl ToolVersions {
    pub fn syntax_python() -> Self {
        Self {
            python: Some("3.14.7".to_owned()),
            ..Self::default()
        }
    }
    /// Syntax-only tool versions for Rust snapshots (no rust-analyzer).
    pub fn syntax() -> Self {
        Self {
            python: None,
            mypyc: None,
            tree_sitter: TREE_SITTER_VERSION.to_string(),
            tree_sitter_rust: TREE_SITTER_RUST_VERSION.to_string(),
            ruby_parser: String::new(),
            rust_analyzer: None,
            rustc: None,
            cargo: None,
            target_triple: None,
            cargo_feature_policy: None,
            cargo_lock_sha256: None,
            rust_analyzer_policy: None,
            tree_sitter_php: String::new(),
            ruby_lsp: None,
            ruby: None,
            rbs: None,
            ruby_tool_bundle_sha256: None,
            phpactor: None,
            php: None,
            php_tool_bundle_sha256: None,
            tree_sitter_go: String::new(),
            gopls: None,
            go: None,
            go_tool_bundle_sha256: None,
            tree_sitter_cpp: String::new(),
            tree_sitter_javascript: String::new(),
            tree_sitter_typescript: String::new(),
        }
    }

    /// Syntax-only tool versions for Ruby snapshots (Prism-based; no
    /// tree-sitter involvement).
    pub fn syntax_ruby() -> Self {
        Self {
            python: None,
            mypyc: None,
            tree_sitter: String::new(),
            tree_sitter_rust: String::new(),
            ruby_parser: RUBY_PARSER_PRISM_VERSION.to_string(),
            rust_analyzer: None,
            rustc: None,
            cargo: None,
            target_triple: None,
            cargo_feature_policy: None,
            cargo_lock_sha256: None,
            rust_analyzer_policy: None,
            tree_sitter_php: String::new(),
            ruby_lsp: None,
            ruby: None,
            rbs: None,
            ruby_tool_bundle_sha256: None,
            phpactor: None,
            php: None,
            php_tool_bundle_sha256: None,
            tree_sitter_go: String::new(),
            gopls: None,
            go: None,
            go_tool_bundle_sha256: None,
            tree_sitter_cpp: String::new(),
            tree_sitter_javascript: String::new(),
            tree_sitter_typescript: String::new(),
        }
    }

    /// Syntax-only tool versions for PHP snapshots (tree-sitter-php; no
    /// semantic tool yet).
    pub fn syntax_php() -> Self {
        Self {
            python: None,
            mypyc: None,
            tree_sitter: TREE_SITTER_VERSION.to_string(),
            tree_sitter_rust: String::new(),
            ruby_parser: String::new(),
            rust_analyzer: None,
            rustc: None,
            cargo: None,
            target_triple: None,
            cargo_feature_policy: None,
            cargo_lock_sha256: None,
            rust_analyzer_policy: None,
            tree_sitter_php: TREE_SITTER_PHP_VERSION.to_string(),
            ruby_lsp: None,
            ruby: None,
            rbs: None,
            ruby_tool_bundle_sha256: None,
            phpactor: None,
            php: None,
            php_tool_bundle_sha256: None,
            tree_sitter_go: String::new(),
            gopls: None,
            go: None,
            go_tool_bundle_sha256: None,
            tree_sitter_cpp: String::new(),
            tree_sitter_javascript: String::new(),
            tree_sitter_typescript: String::new(),
        }
    }

    /// Complete semantic environment identity.
    pub fn semantic(
        rust_analyzer: String,
        rustc: String,
        cargo: String,
        target_triple: String,
        cargo_lock_sha256: Option<String>,
    ) -> Self {
        Self {
            python: None,
            mypyc: None,
            tree_sitter: TREE_SITTER_VERSION.to_string(),
            tree_sitter_rust: TREE_SITTER_RUST_VERSION.to_string(),
            ruby_parser: String::new(),
            rust_analyzer: Some(rust_analyzer),
            rustc: Some(rustc),
            cargo: Some(cargo),
            target_triple: Some(target_triple),
            cargo_feature_policy: Some(CARGO_FEATURE_POLICY.to_string()),
            cargo_lock_sha256,
            rust_analyzer_policy: Some(RustAnalyzerPolicy::fixed()),
            tree_sitter_php: String::new(),
            ruby_lsp: None,
            ruby: None,
            rbs: None,
            ruby_tool_bundle_sha256: None,
            phpactor: None,
            php: None,
            php_tool_bundle_sha256: None,
            tree_sitter_go: String::new(),
            gopls: None,
            go: None,
            go_tool_bundle_sha256: None,
            tree_sitter_cpp: String::new(),
            tree_sitter_javascript: String::new(),
            tree_sitter_typescript: String::new(),
        }
    }

    /// Complete Ruby semantic environment identity.
    pub fn semantic_ruby(
        ruby_lsp: String,
        ruby: String,
        rbs: String,
        tool_bundle_sha256: String,
    ) -> Self {
        Self {
            python: None,
            mypyc: None,
            tree_sitter: String::new(),
            tree_sitter_rust: String::new(),
            ruby_parser: RUBY_PARSER_PRISM_VERSION.to_string(),
            rust_analyzer: None,
            rustc: None,
            cargo: None,
            target_triple: None,
            cargo_feature_policy: None,
            cargo_lock_sha256: None,
            rust_analyzer_policy: None,
            tree_sitter_php: String::new(),
            ruby_lsp: Some(ruby_lsp),
            ruby: Some(ruby),
            rbs: Some(rbs),
            ruby_tool_bundle_sha256: Some(tool_bundle_sha256),
            phpactor: None,
            php: None,
            php_tool_bundle_sha256: None,
            tree_sitter_go: String::new(),
            gopls: None,
            go: None,
            go_tool_bundle_sha256: None,
            tree_sitter_cpp: String::new(),
            tree_sitter_javascript: String::new(),
            tree_sitter_typescript: String::new(),
        }
    }

    /// Complete PHP semantic environment identity (phpactor + PHP runtime
    /// + verifying operator-bundle pin).
    pub fn semantic_php(phpactor: String, php: String, tool_bundle_sha256: String) -> Self {
        Self {
            python: None,
            mypyc: None,
            tree_sitter: TREE_SITTER_VERSION.to_string(),
            tree_sitter_rust: String::new(),
            ruby_parser: String::new(),
            rust_analyzer: None,
            rustc: None,
            cargo: None,
            target_triple: None,
            cargo_feature_policy: None,
            cargo_lock_sha256: None,
            rust_analyzer_policy: None,
            tree_sitter_php: TREE_SITTER_PHP_VERSION.to_string(),
            ruby_lsp: None,
            ruby: None,
            rbs: None,
            ruby_tool_bundle_sha256: None,
            phpactor: Some(phpactor),
            php: Some(php),
            php_tool_bundle_sha256: Some(tool_bundle_sha256),
            tree_sitter_go: String::new(),
            gopls: None,
            go: None,
            go_tool_bundle_sha256: None,
            tree_sitter_cpp: String::new(),
            tree_sitter_javascript: String::new(),
            tree_sitter_typescript: String::new(),
        }
    }

    /// Syntax-only tool versions for Go snapshots (tree-sitter-go; no
    /// semantic tool).
    pub fn syntax_go() -> Self {
        Self {
            tree_sitter_go: TREE_SITTER_GO_VERSION.to_string(),
            ..Self::tree_sitter_base()
        }
    }

    /// Complete Go semantic environment identity (gopls + the companion
    /// `go` toolchain + verifying operator-bundle pin).
    pub fn semantic_go(gopls: String, go: String, tool_bundle_sha256: String) -> Self {
        Self {
            tree_sitter_go: TREE_SITTER_GO_VERSION.to_string(),
            gopls: Some(gopls),
            go: Some(go),
            go_tool_bundle_sha256: Some(tool_bundle_sha256),
            ..Self::tree_sitter_base()
        }
    }

    /// Syntax-only identity for C++. The grammar crate is pinned; clangd is
    /// not part of a syntax snapshot.
    pub fn syntax_cpp() -> Self {
        Self {
            tree_sitter_cpp: TREE_SITTER_CPP_VERSION.to_string(),
            ..Self::tree_sitter_base()
        }
    }

    pub fn syntax_js_ts() -> Self {
        Self {
            tree_sitter_javascript: TREE_SITTER_JAVASCRIPT_VERSION.to_owned(),
            tree_sitter_typescript: TREE_SITTER_TYPESCRIPT_VERSION.to_owned(),
            ..Self::tree_sitter_base()
        }
    }

    /// The identity of a tree-sitter-parsed language before its grammar and
    /// semantic tool are filled in: the shared runtime version and nothing
    /// else.
    fn tree_sitter_base() -> Self {
        Self {
            python: None,
            mypyc: None,
            tree_sitter: TREE_SITTER_VERSION.to_string(),
            tree_sitter_rust: String::new(),
            ruby_parser: String::new(),
            rust_analyzer: None,
            rustc: None,
            cargo: None,
            target_triple: None,
            cargo_feature_policy: None,
            cargo_lock_sha256: None,
            rust_analyzer_policy: None,
            tree_sitter_php: String::new(),
            ruby_lsp: None,
            ruby: None,
            rbs: None,
            ruby_tool_bundle_sha256: None,
            phpactor: None,
            php: None,
            php_tool_bundle_sha256: None,
            tree_sitter_go: String::new(),
            gopls: None,
            go: None,
            go_tool_bundle_sha256: None,
            tree_sitter_cpp: String::new(),
            tree_sitter_javascript: String::new(),
            tree_sitter_typescript: String::new(),
        }
    }
}

// Pinned compile-time versions. These are the crate versions declared in
// Cargo.toml; updating a dependency intentionally changes snapshot identity.
pub const TREE_SITTER_VERSION: &str = "0.25";
pub const TREE_SITTER_RUST_VERSION: &str = "0.24.2";
/// Ruby is parsed by Prism (the `CRuby` reference parser), not tree-sitter;
/// the identity records the parser rather than a grammar.
pub const RUBY_PARSER_PRISM_VERSION: &str = "prism 1.9.0";
/// Pinned tree-sitter-php grammar crate version.
pub const TREE_SITTER_PHP_VERSION: &str = "0.24.2";
/// Pinned tree-sitter-go grammar crate version.
pub const TREE_SITTER_GO_VERSION: &str = "0.25.0";
/// Pinned tree-sitter-cpp grammar crate version.
pub const TREE_SITTER_JAVASCRIPT_VERSION: &str = "0.25.0";
pub const TREE_SITTER_TYPESCRIPT_VERSION: &str = "0.23.2";

pub const TREE_SITTER_CPP_VERSION: &str = "0.23.4";

/// Normalized repository identity. The origin URL is preferred; a stable
/// path-derived fallback is used when no remote is configured (e.g. a
/// local-only repo). The fallback is derived from the absolute repo root
/// so it is stable across runs in the same checkout, but it is explicitly
/// marked as non-canonical via [`RepoIdentity::is_canonical`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoIdentity {
    /// Normalized identifier: origin URL when present, else
    /// `local:<sha256-of-abspath-prefix>`.
    pub id: String,
    /// Human-readable origin URL when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// `true` when `id` is a real remote origin, `false` for a local fallback.
    pub is_canonical: bool,
}

/// Snapshot identity block. Every emitted fact inherits `repository` and
/// `commit` so a snapshot fragment is self-describing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonInputs>,
    pub schema_version: String,
    /// Source language of every fact in the snapshot.
    pub language: Language,
    pub extractor_version: String,
    pub repository: RepoIdentity,
    pub commit_sha: String,
    pub tree_sha: String,
    pub tools: ToolVersions,
    /// `true` only when the working tree differed from `HEAD` at extraction
    /// time. Default extraction refuses dirty worktrees, so this is `false`
    /// unless the caller asked for a working-tree snapshot; then `tree_sha`
    /// names the tree that was actually read, and a consumer keyed by commit
    /// must not treat the snapshot as that commit's canonical map.
    pub dirty: bool,
    /// Name and version of the counting policy behind `measures` and
    /// `file_measures`, present exactly when the snapshot carries them
    /// (`extract --measures`). Absent by default, so a snapshot without
    /// measures serializes and hashes exactly as it did before the field
    /// existed; present, it changes `snapshot_id`, so a consumer keyed by
    /// id never mistakes a measured snapshot for a plain one and never
    /// compares two snapshots counted under different rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measures_policy: Option<String>,
    /// Name and version of the twin policy behind `twin_pairs`, present
    /// exactly when the snapshot carries them (`extract --measures
    /// --twins`). Same discipline as `measures_policy`: absent by default,
    /// so a snapshot without twins hashes exactly as before the field
    /// existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub twins_policy: Option<String>,
    /// What the structural facts describe, from [`structure_policy`]. Set
    /// by [`SnapshotIdentity::build`] from the language; absent for a
    /// language without one, so those snapshots hash exactly as before the
    /// field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structure_policy: Option<String>,
    /// Present only for guarded C++ semantic extraction. Each component
    /// identifies a different prerequisite; the profile alone is not input
    /// closure, and runtime pins alone are not loaded-image attestation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpp_semantic: Option<CppSemanticInputs>,
    /// Content identities for the guarded, explicitly selected JS/TS project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js_ts_semantic: Option<JsTsSemanticInputs>,
    /// SHA-256 of the canonical serialization of every field above
    /// *except* `snapshot_id` itself. Deterministic across processes.
    pub snapshot_id: String,
}

/// The fixed compiler-query and admission policy for C++ semantic facts.
pub const CPP_SEMANTIC_POLICY: &str = "clangd-isolated/cpp/1";

pub const JS_TS_SEMANTIC_POLICY: &str = "typescript-bindings/js-ts/1";

pub const PYTHON_PARSER_POLICY: &str = "python-parser-profile/1";
pub const PYTHON_NATIVE_POLICY: &str = "mypyc-native/single-unit/2.3.1";

/// Content identities, without physical operator paths. The producer must
/// attest execution and validate original-source evidence before attaching them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonInputs {
    pub parser_profile_sha256: String,
    pub policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<PythonNativeInputs>,
}

impl PythonInputs {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.policy == PYTHON_PARSER_POLICY
            && valid_sha256(&self.parser_profile_sha256)
            && self
                .native
                .as_ref()
                .is_none_or(PythonNativeInputs::is_valid)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonNativeInputs {
    pub profile_sha256: String,
    pub evidence_sha256: String,
    pub policy: String,
}

impl PythonNativeInputs {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.policy == PYTHON_NATIVE_POLICY
            && valid_sha256(&self.profile_sha256)
            && valid_sha256(&self.evidence_sha256)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsTsSemanticInputs {
    pub project: String,
    pub inputs_sha256: String,
    pub runtime_sha256: String,
    pub compiler_sha256: String,
    pub helper_sha256: String,
    pub evidence_sha256: String,
    pub node_version: String,
    pub typescript_version: String,
    pub policy: String,
}

impl JsTsSemanticInputs {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.policy == JS_TS_SEMANTIC_POLICY
            && !self.node_version.is_empty()
            && !self.typescript_version.is_empty()
            && !self.project.is_empty()
            && !self.project.contains(['\\', '\0'])
            && self
                .project
                .split('/')
                .all(|part| !part.is_empty() && !matches!(part, "." | ".."))
            && [
                &self.inputs_sha256,
                &self.runtime_sha256,
                &self.compiler_sha256,
                &self.helper_sha256,
                &self.evidence_sha256,
            ]
            .into_iter()
            .all(|digest| valid_sha256(digest))
    }
}

/// Content identities for one guarded C++ compilation profile. Physical roots
/// and installation paths stay outside snapshot identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CppSemanticInputs {
    pub profile_sha256: String,
    pub inputs_sha256: String,
    pub runtime_sha256: String,
    pub clangd_sha256: String,
    pub policy: String,
}

impl CppSemanticInputs {
    /// Whether every content identity and the fixed policy are present.
    /// This checks the identity contract, not the runtime artifacts themselves.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.policy == CPP_SEMANTIC_POLICY
            && valid_sha256(&self.profile_sha256)
            && valid_sha256(&self.inputs_sha256)
            && valid_sha256(&self.runtime_sha256)
            && valid_sha256(&self.clangd_sha256)
    }
}

pub(crate) fn valid_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A tracked Rust source file in the snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    /// Repository-relative, forward-slash-normalized path.
    pub rel_path: String,
    /// SHA-256 of file contents, hex lowercase.
    pub sha256: String,
    /// Exact file size in bytes.
    pub byte_len: usize,
}

/// Concrete node kind. `Type` carries a subtype distinguishing
/// struct/enum/union/type-alias per the snapshot contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "subtype")]
pub enum NodeKind {
    File,
    Module,
    #[serde(rename = "type")]
    Type(TypeKind),
    Trait,
    Function,
    Method,
    Macro,
    /// Synthesized node representing an in-repository `use` target that
    /// could not be mapped to a concrete definition. Required so an
    /// `imports` edge never dangles. Distinct from `ExternalSymbol` in
    /// that it is a *local* placeholder, not a foreign-crate symbol.
    ImportRef,
}

/// Subtype for [`NodeKind::Type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TypeKind {
    Struct,
    Enum,
    Union,
    TypeAlias,
    /// Ruby `class` definition.
    Class,
    /// PHP `interface` definition.
    Interface,
    /// Go defined type whose underlying type is neither a struct nor an
    /// interface (`type ID int`, `type Handler func()`). Distinct from an
    /// alias: a defined type has its own method set.
    Defined,
}

impl NodeKind {
    /// Stable lowercase tag used in node IDs and reports.
    pub const fn tag(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Type(_) => "type",
            Self::Trait => "trait",
            Self::Function => "function",
            Self::Method => "method",
            Self::Macro => "macro",
            Self::ImportRef => "importref",
        }
    }
}

/// Edge kind. `Calls` is only emitted in semantic mode; in syntax-only
/// mode call sites are reported as observations, never as edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    Contains,
    Imports,
    Implements,
    /// Superclass relation (`class A < B` in Ruby). Mixins map to
    /// [`EdgeKind::Implements`] (`include`/`prepend`) or
    /// [`EdgeKind::Extends`] (`extend`) instead; only direct inheritance
    /// gets this kind. Syntax-derived like `Implements`: emitted only when
    /// both ends resolve to unique in-repository definitions.
    Inherits,
    /// Ruby `extend Mod`: the module's *instance* methods become the
    /// receiver's *singleton* methods. Distinct from
    /// [`EdgeKind::Implements`] (`include`/`prepend`, which affect
    /// instance methods) because the two flavors feed different call
    /// regimes; lumping them together would admit implicit-self call
    /// edges into bodies the module never contributes instance
    /// methods to. Syntax-derived: emitted only when both ends resolve
    /// to unique in-repository definitions.
    Extends,
    /// Authoritative call edge, only when a language-aware resolver
    /// mapped the target to a unique in-repository definition.
    Calls,
}

impl EdgeKind {
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Imports => "imports",
            Self::Implements => "implements",
            Self::Inherits => "inherits",
            Self::Extends => "extends",
            Self::Calls => "calls",
        }
    }
}

/// A byte span plus line/column span. Both are recorded so a consumer can
/// render either form without re-reading source. Lines and columns are
/// 1-indexed per the tree-sitter convention adjusted for human display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// Where a fact came from. This is the truth-boundary tag: a consumer can
/// reject any edge whose `fact_source` is not authoritative for its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactSource {
    /// Tree-sitter structural parse. Authoritative for Rust `contains`,
    /// `imports`, and `implements` syntax. **Never** authoritative for
    /// `calls`.
    TreeSitter,
    /// rust-analyzer semantic resolution. Authoritative for Rust `calls`.
    /// Also establishes Rust implementation endpoints at exact declarations.
    RustAnalyzer,
    /// Prism structural parse (the `CRuby` reference parser). Authoritative
    /// for Ruby `contains`, `imports`, `inherits`, and `implements`
    /// syntax. **Never** authoritative for `calls`.
    Prism,
    /// ruby-lsp semantic resolution, admitted only after codemap's own
    /// static model verifies the receiver→target relation. Authoritative
    /// for Ruby `calls`.
    RubyLsp,
    /// phpactor semantic resolution, admitted only after codemap's own
    /// static model verifies the receiver→target relation. Authoritative
    /// for PHP `calls`.
    Phpactor,
    /// gopls semantic resolution: the Go type checker's answer, admitted
    /// only for a callable target reached from inside a callable.
    /// Authoritative for Go `calls`.
    Gopls,
    /// clangd evidence independently admitted against exact C++ source bodies.
    /// Authoritative only for verified callable bodies, lexical containment of
    /// recovered bodies, and exact direct calls under the C++ semantic policy.
    Clangd,
    /// Pinned TypeScript compiler evidence admitted against exact JS/TS source witnesses.
    TypeScript,
    /// Pinned `CPython` AST structure. Never authoritative for calls.
    #[serde(rename = "CPython")]
    Cpython,
    /// Guarded mypyc direct-call evidence admitted against original bodies.
    Mypyc,
}

impl FactSource {
    pub const fn tag(self) -> &'static str {
        match self {
            Self::TreeSitter => "tree-sitter-rust",
            Self::RustAnalyzer => "rust-analyzer",
            Self::Prism => "prism",
            Self::RubyLsp => "ruby-lsp",
            Self::Phpactor => "phpactor",
            Self::Gopls => "gopls",
            Self::Clangd => "clangd",
            Self::TypeScript => "typescript",
            Self::Cpython => "cpython",
            Self::Mypyc => "mypyc",
        }
    }
}

/// Provenance carried by every node and edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub repository: String,
    pub commit_sha: String,
    pub rel_path: String,
    pub span: Span,
    /// SHA-256 of the file the fact was derived from, so file drift is
    /// detectable without a separate index.
    pub file_hash: String,
    pub extractor_version: String,
    pub fact_source: FactSource,
    /// Normalized confidence in `[0, 255]`. Structural facts are `255`
    /// (maximum); semantic `calls` edges are `255` only when uniquely
    /// resolved.
    pub confidence: u8,
}

/// A snapshot node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonDefinition>,
    /// Deterministic, never wall-clock/path-random. See [`node_id`].
    pub id: String,
    pub kind: NodeKind,
    /// Declared name (`Repository`, `commit_pending`).
    pub name: String,
    /// Semantic symbol key, e.g. `rust:src/repository.rs#Repository::commit_pending`.
    pub symbol_key: String,
    pub provenance: Provenance,
}

/// Syntax observations on a Python definition; no runtime decorator or base claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonDefinition {
    pub is_async: bool,
    pub ordinal: usize,
    pub ambiguous: bool,
    pub decorators: Vec<PythonExpression>,
    pub bases: Vec<PythonExpression>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonExpression {
    pub spelling: String,
    pub provenance: Provenance,
}

/// A snapshot edge. `from` and `to` must resolve to emitted node IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// Deterministic edge id.
    pub id: String,
    pub kind: EdgeKind,
    pub from: String,
    pub to: String,
    pub provenance: Provenance,
}

/// Per-call-site observation, reported (never emitted as an edge) in
/// syntax-only mode. Categories drive the omission report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallSiteCategory {
    /// Receiver-qualified or free call observed by the parser.
    Observed,
    /// Resolved to a unique in-repository definition (semantic mode only).
    Resolved,
    /// Target is in an external crate.
    ExternalCrate,
    /// Target is in the standard library.
    StandardLibrary,
    /// Multiple in-repository candidates; cannot be canonical.
    Ambiguous,
    /// No in-repository candidate matched.
    Unmapped,
    /// rust-analyzer returned an error or no result.
    Failed,
    /// Guess-regime call site (semantic mode only): the receiver is not
    /// statically verifiable, but the in-repository definition index holds
    /// one or more name candidates. The candidates are explicitly
    /// non-authoritative — they never become edges — and are drawn only
    /// from the extractor's own definition index, never from a resolver's
    /// unscoped name search.
    Candidate,
    /// Source shape or dispatch regime excluded by the semantic policy.
    Unsupported,
    /// Compiler-reported inactive preprocessor region in the selected profile.
    Inactive,
    /// Source outside the explicitly selected semantic compilation profile.
    Unprofiled,
}

impl CallSiteCategory {
    /// Stable key used by [`Report::call_sites_by_category`].
    pub const fn report_key(self) -> &'static str {
        match self {
            Self::Observed => "Observed",
            Self::Resolved => "Resolved",
            Self::ExternalCrate => "ExternalCrate",
            Self::StandardLibrary => "StandardLibrary",
            Self::Ambiguous => "Ambiguous",
            Self::Unmapped => "Unmapped",
            Self::Failed => "Failed",
            Self::Candidate => "Candidate",
            Self::Unsupported => "Unsupported",
            Self::Inactive => "Inactive",
            Self::Unprofiled => "Unprofiled",
        }
    }
}

/// A single observed call site, accumulated for the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSite {
    pub rel_path: String,
    pub span: Span,
    pub raw_name: String,
    pub category: CallSiteCategory,
}

/// Extraction report: counts and omission accounting.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Report {
    pub files_scanned: usize,
    pub files_skipped: usize,
    pub parse_errors: usize,
    pub node_counts: BTreeMap<String, usize>,
    pub edge_counts: BTreeMap<String, usize>,
    pub call_sites_by_category: BTreeMap<String, usize>,
    pub import_ref_nodes: usize,
    /// Constructs the adapter saw and did not emit, counted by kind (C++:
    /// body-less declarations, unresolved bases, unresolved qualifiers).
    /// Omitted when empty, so languages that count nothing are unchanged.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub observations: BTreeMap<String, usize>,
}

/// The immutable snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub identity: SnapshotIdentity,
    pub files: Vec<FileRecord>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub call_sites: Vec<CallSite>,
    /// One record per callable node, present only under
    /// `extract --measures` (see [`Measure`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub measures: Vec<Measure>,
    /// One record per file in the inventory, present only under
    /// `extract --measures` (see [`FileMeasure`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_measures: Vec<FileMeasure>,
    /// Clone groups over the normalised token streams, present only under
    /// `extract --measures` and only when there are any (see
    /// [`CloneGroup`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clone_groups: Vec<CloneGroup>,
    /// Twin pairs over the same token streams, present only under
    /// `extract --measures --twins` and only when there are any (see
    /// [`TwinPair`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub twin_pairs: Vec<TwinPair>,
    pub report: Report,
}

/// Per-callable measures.
///
/// Integers a named function in the language module computed from the
/// parse tree, under the counting policy the identity names. The record
/// cites the node it measures, so provenance
/// is the node's. Metrics (erosion, budgets) are a consumer's business;
/// the producer emits counts only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Measure {
    /// The `Function`, `Method`, or `Macro` node measured.
    pub node_id: String,
    /// Cyclomatic complexity: 1 plus the decision points in the
    /// callable's own body. Closures count toward the callable that
    /// encloses them; nested named definitions count toward themselves.
    pub cc: u32,
    /// Source lines: lines in the span that are neither blank nor
    /// comment-only.
    pub sloc: u32,
    /// Lines in the span, first through last inclusive.
    pub loc: u32,
    /// Clone groups with an occurrence overlapping this callable's span,
    /// sorted; empty when none does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clone_group_ids: Vec<String>,
    /// Parameters the definition declares: required, optional, and
    /// post-rest positionals, keywords, one each for a rest, a keyword
    /// rest, a block, or a forwarding `...`; `**nil` declares none.
    /// Present under a policy with form measures
    /// ([`form_measures_available`]), absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<u32>,
    /// The deepest stack of control structures any node of the body sits
    /// under: conditionals (an `elsif` at its parent's depth), loops,
    /// `case`, and block or lambda bodies, each one level; a straight
    /// body is 0. Present as [`Measure::params`] is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nesting: Option<u32>,
    /// The longest receiver spine of any call in the body: dispatches
    /// with an explicit receiver along the receiver chain (`a.b.c.d` is
    /// 3); a body with no such call is 0. Present as [`Measure::params`]
    /// is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<u32>,
    /// Distinct instance variables the body writes, in any write form;
    /// reads do not count. Present under a policy with form measures for
    /// a language with instance variables (Ruby), absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ivars: Option<u32>,
}

/// One stretch of one file that a clone group covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloneOccurrence {
    pub rel_path: String,
    /// First and last line of the token run, 1-based inclusive.
    pub start_line: u32,
    pub end_line: u32,
    /// Lines in the range that are neither blank nor comment-only: the
    /// one line rule [`FileMeasure`] uses, so a clone never counts a line
    /// SLOC does not.
    pub sloc: u32,
}

/// Two or more occurrences with equal normalised token sequences
/// (identifiers and literals replaced by placeholders): a Type-1 or
/// Type-2 clone. Found by the producer under [`CLONE_MIN_TOKENS`] and
/// [`CLONE_MIN_LINES`]; what it means is a consumer's business.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloneGroup {
    /// `clone:` plus the leading hex of the SHA-256 over the normalised
    /// tokens, so the same duplicated text has the same id in every
    /// snapshot; a numeric suffix keeps ids unique in the one case where
    /// two runs carry equal tokens.
    pub id: String,
    /// Sorted by path, then line.
    pub occurrences: Vec<CloneOccurrence>,
    /// Normalised tokens in the run.
    pub tokens: u32,
    /// Lines of the shortest occurrence: what [`CLONE_MIN_LINES`] held.
    pub lines: u32,
}

/// Two callables whose normalised token shingles overlap past the twin
/// threshold.
///
/// A copy that was edited (a Type-3 clone), or an exact twin that is also
/// a clone group. Found by the producer under [`TWIN_SHINGLE_TOKENS`],
/// [`TWIN_MIN_TOKENS`], and [`TWIN_MIN_CONTAINMENT_PERCENT`]; what it
/// means is a consumer's business. Integers only, so the snapshot stays
/// byte-stable; the containment ratio is the consumer's to derive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TwinPair {
    /// The callable with the smaller node id.
    pub a: String,
    /// The callable with the larger node id.
    pub b: String,
    /// Shingles the two bodies share (multiset intersection).
    pub shared: u32,
    /// Shingles in `a`.
    pub shingles_a: u32,
    /// Shingles in `b`.
    pub shingles_b: u32,
}

impl TwinPair {
    /// `shared` over the smaller body's shingles, in `[0, 1]`.
    #[must_use]
    pub fn containment(&self) -> f64 {
        let smaller = self.shingles_a.min(self.shingles_b);
        if smaller == 0 {
            0.0
        } else {
            f64::from(self.shared) / f64::from(smaller)
        }
    }

    /// Whether the two bodies are token-identical.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        self.shared == self.shingles_a && self.shingles_a == self.shingles_b
    }
}

/// Per-file line classes.
///
/// One eligibility rule shared with [`Measure::sloc`]: a line is blank,
/// comment-only, or source. Files are the only honest source of repository LOC; summing callable spans
/// would miss top-level code and count nested definitions twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeasure {
    /// The `File` node measured.
    pub file_id: String,
    /// Lines in the file (`blank + comment + sloc`).
    pub loc: u32,
    /// Lines that are neither blank nor comment-only.
    pub sloc: u32,
    /// Lines holding only whitespace.
    pub blank: u32,
    /// Lines whose only non-whitespace content is comment text.
    pub comment: u32,
    /// Source lines covered by at least one clone occurrence, each line
    /// counted once however many groups cover it; never above `sloc`.
    #[serde(default)]
    pub clone_sloc: u32,
}

#[derive(Serialize)]
struct TransportedFactSet<'a> {
    files: Vec<&'a FileRecord>,
    nodes: Vec<&'a Node>,
    edges: Vec<&'a Edge>,
}

impl Snapshot {
    /// Digest the complete fact set carried by transport adapters.
    ///
    /// Call-site observations and the derived report are intentionally omitted:
    /// transport adapters carry only files, canonical nodes, and canonical edges.
    /// Sorting makes the digest independent of storage/query row order.
    pub fn transported_fact_set_sha256(&self) -> String {
        let mut files = self.files.iter().collect::<Vec<_>>();
        files.sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
        let mut nodes = self.nodes.iter().collect::<Vec<_>>();
        nodes.sort_by(|left, right| left.id.cmp(&right.id));
        let mut edges = self.edges.iter().collect::<Vec<_>>();
        edges.sort_by(|left, right| left.id.cmp(&right.id));
        let facts = TransportedFactSet {
            files,
            nodes,
            edges,
        };
        let canonical = canonical_json(&facts).expect("transported fact set serializes");
        sha256_hex(canonical.as_bytes())
    }
}

// ───────────────────────── deterministic IDs ─────────────────────────

/// Build a deterministic node ID.
///
/// - file:        `{lang}:file:{rel_path}`
/// - symbol:      `{lang}:{kind}:{rel_path}#{qualified}` where `{lang}` is
///   the snapshot language tag and `qualified` is the `::`-joined scope
///   chain plus name.
///
/// When two declarations collide on `(kind, rel_path, qualified)`, append
/// `@{start_byte}` as a deterministic source-span disambiguator.
pub fn node_id(
    language: Language,
    kind: NodeKind,
    rel_path: &str,
    scope: &[String],
    name: &str,
) -> String {
    if kind == NodeKind::File {
        format!("{}file:{rel_path}", language.id_prefix())
    } else {
        let qualified = qualified_name(scope, name);
        format!(
            "{}{}:{}#{}",
            language.id_prefix(),
            kind.tag(),
            rel_path,
            qualified
        )
    }
}

/// Disambiguated node ID for the collision case.
pub fn node_id_disambiguated(
    language: Language,
    kind: NodeKind,
    rel_path: &str,
    scope: &[String],
    name: &str,
    start_byte: usize,
) -> String {
    format!(
        "{}@{start_byte}",
        node_id(language, kind, rel_path, scope, name)
    )
}

/// Semantic symbol key: `{lang}:{rel_path}#{qualified}`.
pub fn symbol_key(language: Language, rel_path: &str, scope: &[String], name: &str) -> String {
    format!(
        "{}{rel_path}#{}",
        language.id_prefix(),
        qualified_name(scope, name)
    )
}

fn qualified_name(scope: &[String], name: &str) -> String {
    if scope.is_empty() {
        name.to_string()
    } else {
        format!("{}::{name}", scope.join("::"))
    }
}

/// Deterministic edge ID: `{kind}:{from}->{to}@{start_byte}`.
pub fn edge_id(kind: EdgeKind, from: &str, to: &str, start_byte: usize) -> String {
    format!("{}:{}->{}@{start_byte}", kind.tag(), from, to)
}

// ───────────────────────── canonical serialization ─────────────────────────

/// Serialize a value as canonical JSON: no whitespace, struct fields in
/// declaration order, and all collections pre-sorted by the caller. This
/// is the single function responsible for byte-determinism.
pub fn canonical_json<T: Serialize>(value: &T) -> anyhow::Result<String> {
    let s = serde_json::to_string(value)?;
    Ok(s)
}

/// SHA-256 hex digest of `data`, lowercase.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    hex_lower(&h.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

impl SnapshotIdentity {
    /// Whether the snapshot was extracted with the declared language's
    /// resolver, so its `calls` edges and call-site categories are
    /// semantic facts. A syntax snapshot carries no verified call; a
    /// consumer must say "unavailable", never "zero", about its coupling.
    #[must_use]
    pub fn is_semantic(&self) -> bool {
        match self.language {
            Language::Rust => self.tools.rust_analyzer.is_some(),
            Language::Ruby => self.tools.ruby_lsp.is_some(),
            Language::Php => self.tools.phpactor.is_some(),
            Language::Go => self.tools.gopls.is_some(),
            Language::JsTs => self
                .js_ts_semantic
                .as_ref()
                .is_some_and(JsTsSemanticInputs::is_valid),
            Language::Cpp => self
                .cpp_semantic
                .as_ref()
                .is_some_and(CppSemanticInputs::is_valid),
            Language::Python => self.python.as_ref().is_some_and(|inputs| {
                inputs.is_valid()
                    && inputs.native.is_some()
                    && self.tools.python.as_deref() == Some("3.14.7")
                    && self.tools.mypyc.as_deref() == Some("2.3.1")
            }),
        }
    }

    /// Compute `snapshot_id` as SHA-256 of the canonical serialization of
    /// the identity block *without* the `snapshot_id` field.
    pub fn compute_snapshot_id(&self) -> String {
        let mut block = self.clone();
        block.snapshot_id = String::new();
        let canon = canonical_json(&block).expect("identity serializes");
        sha256_hex(canon.as_bytes())
    }

    /// Build a complete identity, filling in `snapshot_id`.
    pub fn build(
        language: Language,
        repository: RepoIdentity,
        commit_sha: String,
        tree_sha: String,
        tools: ToolVersions,
        dirty: bool,
    ) -> Self {
        let mut id = Self {
            python: None,
            schema_version: SCHEMA_VERSION.to_string(),
            language,
            extractor_version: EXTRACTOR_VERSION.to_string(),
            repository,
            commit_sha,
            tree_sha,
            tools,
            dirty,
            measures_policy: None,
            twins_policy: None,
            structure_policy: structure_policy(language).map(ToString::to_string),
            cpp_semantic: None,
            js_ts_semantic: None,
            snapshot_id: String::new(),
        };
        id.snapshot_id = id.compute_snapshot_id();
        id
    }

    /// Attach compiler input identity; validation enforces language and policy.
    #[must_use]
    pub fn with_cpp_semantic(mut self, inputs: CppSemanticInputs) -> Self {
        self.cpp_semantic = Some(inputs);
        self.snapshot_id = self.compute_snapshot_id();
        self
    }

    #[must_use]
    pub fn with_js_ts_semantic(mut self, inputs: JsTsSemanticInputs) -> Self {
        self.js_ts_semantic = Some(inputs);
        self.snapshot_id = self.compute_snapshot_id();
        self
    }

    #[must_use]
    pub fn with_python(mut self, inputs: PythonInputs) -> Self {
        self.python = Some(inputs);
        self.snapshot_id = self.compute_snapshot_id();
        self
    }

    /// The same identity carrying a twin policy, `snapshot_id` recomputed
    /// so the id changes exactly when twin pairs are present.
    #[must_use]
    pub fn with_twins_policy(mut self, policy: &str) -> Self {
        self.twins_policy = Some(policy.to_string());
        self.snapshot_id = self.compute_snapshot_id();
        self
    }

    /// The same identity carrying a measures policy, `snapshot_id`
    /// recomputed so the id changes exactly when measures are present.
    #[must_use]
    pub fn with_measures_policy(mut self, policy: &str) -> Self {
        self.measures_policy = Some(policy.to_string());
        self.snapshot_id = self.compute_snapshot_id();
        self
    }
}

impl Span {
    /// Build a span from tree-sitter byte/point ranges. tree-sitter points
    /// are 0-indexed; we store 1-indexed lines and 0-indexed columns
    /// (byte offsets) for compact, unambiguous provenance.
    pub const fn from_ts(
        start_byte: usize,
        end_byte: usize,
        start: (usize, usize),
        end: (usize, usize),
    ) -> Self {
        Self {
            start_byte,
            end_byte,
            start_line: start.0 as u32 + 1,
            start_col: start.1 as u32,
            end_line: end.0 as u32 + 1,
            end_col: end.1 as u32,
        }
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Type(t) => write!(f, "type:{}", t.tag()),
            other => f.write_str(other.tag()),
        }
    }
}

impl TypeKind {
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Union => "union",
            Self::TypeAlias => "alias",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Defined => "defined",
        }
    }
}

/// Order nodes by id for canonical serialization.
pub fn sort_nodes(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
}

/// Order edges by (kind tag, from, to, `start_byte`) for canonical serialization.
pub fn sort_edges(edges: &mut [Edge]) {
    edges.sort_by(|a, b| {
        a.kind
            .tag()
            .cmp(b.kind.tag())
            .then(a.from.cmp(&b.from))
            .then(a.to.cmp(&b.to))
            .then(
                a.provenance
                    .span
                    .start_byte
                    .cmp(&b.provenance.span.start_byte),
            )
    });
}

/// Order files by `rel_path`.
pub fn sort_files(files: &mut [FileRecord]) {
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_ts_environment_identity_requires_every_component_and_relative_project() {
        let inputs = JsTsSemanticInputs {
            project: "packages/app/tsconfig.json".to_owned(),
            inputs_sha256: "1".repeat(64),
            runtime_sha256: "2".repeat(64),
            compiler_sha256: "3".repeat(64),
            helper_sha256: "4".repeat(64),
            evidence_sha256: "5".repeat(64),
            node_version: "v22.22.2".to_owned(),
            typescript_version: "5.9.3".to_owned(),
            policy: JS_TS_SEMANTIC_POLICY.to_owned(),
        };
        let plain = SnapshotIdentity::build(
            Language::JsTs,
            RepoIdentity {
                id: "r".to_owned(),
                origin: None,
                is_canonical: false,
            },
            "a".repeat(40),
            "b".repeat(40),
            ToolVersions::syntax_js_ts(),
            false,
        );
        assert!(!plain.is_semantic());
        assert!(!canonical_json(&plain).unwrap().contains("js_ts_semantic"));
        let semantic = plain.clone().with_js_ts_semantic(inputs.clone());
        assert!(semantic.is_semantic());
        assert_ne!(plain.snapshot_id, semantic.snapshot_id);
        let value = serde_json::to_value(inputs).unwrap();
        for field in [
            "inputs_sha256",
            "runtime_sha256",
            "compiler_sha256",
            "helper_sha256",
            "evidence_sha256",
            "node_version",
            "typescript_version",
            "policy",
        ] {
            let mut changed = value.clone();
            changed[field] = serde_json::json!("");
            let invalid: JsTsSemanticInputs = serde_json::from_value(changed).unwrap();
            assert!(!invalid.is_valid(), "{field}");
        }
        for project in [
            "/tsconfig.json",
            "../tsconfig.json",
            "x/../a.json",
            "a\\b",
            "a//b",
            "",
        ] {
            let mut changed = value.clone();
            changed["project"] = serde_json::json!(project);
            let invalid: JsTsSemanticInputs = serde_json::from_value(changed).unwrap();
            assert!(!invalid.is_valid(), "{project}");
        }
        for field in [
            "project",
            "inputs_sha256",
            "runtime_sha256",
            "compiler_sha256",
            "helper_sha256",
            "evidence_sha256",
            "node_version",
            "typescript_version",
        ] {
            let mut changed = value.clone();
            changed[field] = serde_json::json!(format!("{}x", changed[field].as_str().unwrap()));
            let changed: JsTsSemanticInputs = serde_json::from_value(changed).unwrap();
            assert_ne!(
                semantic.snapshot_id,
                plain.clone().with_js_ts_semantic(changed).snapshot_id,
                "{field}"
            );
        }
    }

    #[test]
    fn node_id_is_deterministic_and_path_relative() {
        let scope = vec!["Repository".to_string()];
        let id = node_id(
            Language::Rust,
            NodeKind::Method,
            "src/repository.rs",
            &scope,
            "seal",
        );
        assert_eq!(id, "rust:method:src/repository.rs#Repository::seal");
        // same inputs → same id, no randomness
        assert_eq!(
            id,
            node_id(
                Language::Rust,
                NodeKind::Method,
                "src/repository.rs",
                &scope,
                "seal"
            )
        );
        // the language tag namespaces the id
        let ruby = node_id(
            Language::Ruby,
            NodeKind::Method,
            "app/models/user.rb",
            &scope,
            "seal",
        );
        assert_eq!(ruby, "ruby:method:app/models/user.rb#Repository::seal");
    }

    #[test]
    fn symbol_key_joins_language_path_scope_and_name() {
        let scope = vec!["Repository".to_string()];
        let key = symbol_key(
            Language::Rust,
            "src/store/repository.rs",
            &scope,
            "commit_pending",
        );
        assert_eq!(
            key,
            "rust:src/store/repository.rs#Repository::commit_pending"
        );
    }

    #[test]
    fn snapshot_id_is_deterministic_across_instances() {
        let repo = RepoIdentity {
            id: "git@x:y".into(),
            origin: Some("git@x:y".into()),
            is_canonical: true,
        };
        let a = SnapshotIdentity::build(
            Language::Rust,
            repo.clone(),
            "deadbeef".into(),
            "cafebabe".into(),
            ToolVersions::syntax(),
            false,
        );
        let b = SnapshotIdentity::build(
            Language::Rust,
            repo,
            "deadbeef".into(),
            "cafebabe".into(),
            ToolVersions::syntax(),
            false,
        );
        assert_eq!(a.snapshot_id, b.snapshot_id);
        assert_ne!(a.snapshot_id, "");
    }

    #[test]
    fn canonical_json_is_byte_stable() {
        let repo = RepoIdentity {
            id: "r".into(),
            origin: None,
            is_canonical: false,
        };
        let id = SnapshotIdentity::build(
            Language::Rust,
            repo,
            "c".into(),
            "t".into(),
            ToolVersions::syntax(),
            false,
        );
        let s1 = canonical_json(&id).unwrap();
        let s2 = canonical_json(&id).unwrap();
        assert_eq!(s1, s2);
        // no pretty whitespace
        assert!(!s1.contains('\n'));
    }

    #[test]
    fn v7_requires_file_lengths_and_language_but_keeps_optional_semantic_tool_fields() {
        assert_eq!(SCHEMA_VERSION, "codemap.snapshot.v8");

        let file = serde_json::from_value::<FileRecord>(serde_json::json!({
            "rel_path": "a.rs",
            "sha256": "h"
        }));
        assert!(file.is_err(), "v4 file records must carry byte_len");

        // Identity requires the language field (no default: a snapshot must
        // declare which language its facts belong to).
        let identity = serde_json::from_value::<SnapshotIdentity>(serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "extractor_version": EXTRACTOR_VERSION,
            "repository": { "id": "r", "is_canonical": false },
            "commit_sha": "c",
            "tree_sha": "t",
            "tools": {
                "tree_sitter": TREE_SITTER_VERSION,
                "tree_sitter_rust": TREE_SITTER_RUST_VERSION
            },
            "dirty": false,
            "snapshot_id": "s"
        }));
        assert!(identity.is_err(), "v4 identity must carry language");

        let tools: ToolVersions = serde_json::from_value(serde_json::json!({
            "tree_sitter": TREE_SITTER_VERSION,
            "tree_sitter_rust": TREE_SITTER_RUST_VERSION,
            "rust_analyzer": "legacy-ra"
        }))
        .expect("legacy tool versions");
        assert_eq!(tools.rust_analyzer.as_deref(), Some("legacy-ra"));
        assert!(tools.rustc.is_none());
        assert!(tools.rust_analyzer_policy.is_none());
    }

    #[test]
    fn language_serializes_as_lowercase_tag() {
        assert_eq!(serde_json::to_string(&Language::Rust).unwrap(), "\"rust\"");
        assert_eq!(serde_json::to_string(&Language::Ruby).unwrap(), "\"ruby\"");
        assert_eq!(serde_json::to_string(&Language::Go).unwrap(), "\"go\"");
        assert_eq!(
            serde_json::from_str::<Language>("\"ruby\"").unwrap(),
            Language::Ruby
        );
        assert_eq!(
            serde_json::from_str::<Language>("\"go\"").unwrap(),
            Language::Go
        );
    }

    #[test]
    fn go_identities_carry_the_grammar_and_only_semantic_mode_names_gopls() {
        let syntax = ToolVersions::syntax_go();
        assert_eq!(syntax.tree_sitter, TREE_SITTER_VERSION);
        assert_eq!(syntax.tree_sitter_go, TREE_SITTER_GO_VERSION);
        assert!(syntax.gopls.is_none() && syntax.go.is_none());
        assert!(syntax.tree_sitter_rust.is_empty() && syntax.tree_sitter_php.is_empty());
        assert_eq!(syntax.ruby_parser, "");

        let semantic = ToolVersions::semantic_go("gopls v1".into(), "go1".into(), "sum".into());
        assert_eq!(semantic.gopls.as_deref(), Some("gopls v1"));
        assert_eq!(semantic.go.as_deref(), Some("go1"));
        assert_eq!(semantic.go_tool_bundle_sha256.as_deref(), Some("sum"));
        let a = SnapshotIdentity::build(
            Language::Go,
            RepoIdentity {
                id: "r".into(),
                origin: None,
                is_canonical: false,
            },
            "c".into(),
            "t".into(),
            syntax,
            false,
        );
        let b = SnapshotIdentity::build(
            Language::Go,
            a.repository.clone(),
            "c".into(),
            "t".into(),
            semantic,
            false,
        );
        assert_ne!(a.snapshot_id, b.snapshot_id);
    }

    fn semantic_tools(overrides: &[(&str, serde_json::Value)]) -> ToolVersions {
        let mut value = serde_json::json!({
            "tree_sitter": TREE_SITTER_VERSION,
            "tree_sitter_rust": TREE_SITTER_RUST_VERSION,
            "rust_analyzer": "rust-analyzer 1",
            "rustc": "rustc 1",
            "cargo": "cargo 1",
            "target_triple": "x86_64-unknown-linux-gnu",
            "cargo_feature_policy": "default",
            "cargo_lock_sha256": "lock-a",
            "rust_analyzer_policy": {
                "cargo_offline": true,
                "build_scripts_enabled": false,
                "proc_macros_enabled": false,
                "background_checks_enabled": false
            }
        });
        for (field, replacement) in overrides {
            value[*field] = replacement.clone();
        }
        serde_json::from_value(value).expect("semantic tool identity")
    }

    fn semantic_snapshot_id(tools: ToolVersions) -> String {
        SnapshotIdentity::build(
            Language::Rust,
            RepoIdentity {
                id: "r".into(),
                origin: None,
                is_canonical: false,
            },
            "c".into(),
            "t".into(),
            tools,
            false,
        )
        .snapshot_id
    }

    #[test]
    fn is_semantic_follows_the_declared_language_resolver() {
        let repository = RepoIdentity {
            id: "r".into(),
            origin: None,
            is_canonical: false,
        };
        let build = |language: Language, tools: ToolVersions| {
            SnapshotIdentity::build(
                language,
                repository.clone(),
                "c".into(),
                "t".into(),
                tools,
                false,
            )
        };
        assert!(!build(Language::Rust, ToolVersions::syntax()).is_semantic());
        assert!(build(Language::Rust, semantic_tools(&[])).is_semantic());
        assert!(!build(Language::Ruby, ToolVersions::syntax_ruby()).is_semantic());
        assert!(
            !build(Language::Ruby, semantic_tools(&[])).is_semantic(),
            "a rust-analyzer version says nothing about a Ruby snapshot"
        );
    }

    #[test]
    fn semantic_identity_includes_companion_rustc() {
        let a = semantic_snapshot_id(semantic_tools(&[]));
        let b = semantic_snapshot_id(semantic_tools(&[("rustc", serde_json::json!("rustc 2"))]));
        assert_ne!(a, b);
    }

    #[test]
    fn semantic_identity_includes_companion_cargo() {
        let a = semantic_snapshot_id(semantic_tools(&[]));
        let b = semantic_snapshot_id(semantic_tools(&[("cargo", serde_json::json!("cargo 2"))]));
        assert_ne!(a, b);
    }

    #[test]
    fn semantic_identity_includes_target_triple() {
        let a = semantic_snapshot_id(semantic_tools(&[]));
        let b = semantic_snapshot_id(semantic_tools(&[(
            "target_triple",
            serde_json::json!("aarch64-apple-darwin"),
        )]));
        assert_ne!(a, b);
    }

    #[test]
    fn semantic_identity_includes_feature_policy() {
        let a = semantic_snapshot_id(semantic_tools(&[]));
        let b = semantic_snapshot_id(semantic_tools(&[(
            "cargo_feature_policy",
            serde_json::json!("all"),
        )]));
        assert_ne!(a, b);
    }

    #[test]
    fn semantic_identity_includes_cargo_lock_hash() {
        let a = semantic_snapshot_id(semantic_tools(&[]));
        let b = semantic_snapshot_id(semantic_tools(&[(
            "cargo_lock_sha256",
            serde_json::json!("lock-b"),
        )]));
        assert_ne!(a, b);
    }

    #[test]
    fn semantic_identity_includes_rust_analyzer_policy() {
        let a = semantic_snapshot_id(semantic_tools(&[]));
        let b = semantic_snapshot_id(semantic_tools(&[(
            "rust_analyzer_policy",
            serde_json::json!({
                "cargo_offline": true,
                "build_scripts_enabled": true,
                "proc_macros_enabled": false,
                "background_checks_enabled": false
            }),
        )]));
        assert_ne!(a, b);
    }
}
