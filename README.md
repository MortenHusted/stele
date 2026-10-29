# Stele

Stele joins `codemap`, a standalone, deterministic, source-addressed
code-architecture extractor, with LemmaSpec, an engine that checks typed,
deterministic specifications against the facts codemap extracts. Spec files
take the `.stele` extension, and one command, `stele`, runs both.

`codemap` answers structural questions about a pinned source tree: what
files, symbols, modules, imports, and implementations exist at a given
commit, with provenance and confidence on every emitted fact. One source
language per snapshot. Rust, Ruby, PHP, Go, C++, JavaScript/TypeScript and Python
use their documented structural and semantic profiles. Canonical calls and
implementations require the corresponding language-aware verification; ambiguous
or unsupported targets are reported without being promoted to exact edges.
It has no runtime dependency on an LLM, a graph database, remote storage,
or authentication, and produces byte-deterministic output for the same
clean commit.

See [`AGENTS.md`](AGENTS.md) for the project contract and truth boundary.

## Status

Each accepted language ships a pinned public corpus and a labelled oracle or
synthetic precision fixture, with reproducible preparation and gate commands.
The gates distinguish exact edges, forbidden observations, known unsupported
cases and snapshot identity/determinism. Their scope is explicit; a selected
caller or translation-unit oracle is not application-wide precision or recall.

| Language family | Public corpus | Reproducible gate and scope |
| --- | --- | --- |
| Rust | ripgrep | [Prepared rust-analyzer and selected callers](benchmarks/rust/README.md) |
| Ruby | Mastodon | [Verified receiver ancestry and selected callers](benchmarks/ruby/README.md) |
| PHP | monica | [Synthetic precision and real-application identity/determinism](#public-php-and-go-corpus-gates) |
| Go | lazygit | [Callable-target controls and real-application identity/determinism](#public-php-and-go-corpus-gates) |
| C++ | double-conversion | [Selected translation units with declared unsupported sites](benchmarks/cpp/README.md) |
| JavaScript/TypeScript | Excalidraw | [Frozen selected-caller oracle and prepared operator](benchmarks/js_ts/README.md) |
| Python | Black | [Labelled compiled-call regime and prepared operator](benchmarks/python/README.md) |

C++ syntax uses `spelled/cpp/1` and emits no calls. Required semantic mode uses
an explicit compilation profile and pinned, isolated clangd. C++ and JS/TS
semantics support macOS arm64 and native Linux x86_64/aarch64 under their
required runtime profiles and kernel capabilities; syntax is portable. JS/TS
covers JavaScript, TypeScript, JSX, TSX and module extensions. Python's compiled
call regime has its own prepared-environment and admission boundary. See the
[C++](toolchain/cpp/README.md), [JS/TS](toolchain/js_ts/README.md) and
[Python](toolchain/python/README.md) toolchain guides for configuration and
unsupported shapes.

- versioned snapshot (`codemap.snapshot.v7`) with a declared source
  `language`, deterministic identity, SHA-256 `snapshot_id`, and canonical
  byte-stable serialization in both syntax and semantic modes; the validator
  enforces that every fact lives in the declared language's ID namespace;
- Git provenance: clean-worktree enforcement, commit/tree identity, tracked
  source inventory parameterized per language;
- Tree-sitter Rust extraction: definitions, lexical `contains`, `use`
  imports (with non-dangling `ImportRef` placeholders), and `implements`
  edges resolved only when both sides map to unique in-repository
  definitions;
- Prism Ruby extraction (structural, plus semantic `calls` under the R2
  verified-edge policy): module/class/method definitions
  (`def self.name` recorded as `self.name`), lexical `contains`,
  `require`/`require_relative`/`load`/`autoload` imports with `ImportRef`
  placeholders, `inherits` edges from superclasses, `implements` edges from
  `include`/`prepend`, and `extends` edges from `extend`, each resolved only
  when both sides map to unique in-repository definitions. Prism (the
  `CRuby` reference parser, via the official `ruby-prism` bindings) tracks
  local scope at parse time, so bare paren-less method calls (`params`,
  route helpers, chained receivers) are captured as observations; attribute
  writes and operator calls are excluded. Semantic mode launches ruby-lsp
  under an operator-installed, hash-pinned tool bundle
  (`CODEMAP_RUBY_TOOL_BUNDLE`, `BUNDLE_FROZEN` — no network, no worktree
  writes) and emits a `calls` edge only when codemap's own static model
  verifies the receiver→target relation (constant-receiver, implicit-self or
  verified direct-constructor shape, target owner inside the receiver's
  verified ancestry, call enclosed by an emitted callable); everything else
  stays a categorized observation.
  Direct construction (`Widget.new(...).perform`) additionally requires the
  pinned default `Class#new` definition and complete source constructor ancestry;
  overrides, reflective constructor definitions, evaluated code,
  constructor-affecting inheritance/inclusion hooks, external or
  unresolved heritage, class extensions, modules,
  and ambiguous reopened classes stay unverified. Assigned variables and
  arbitrary factory/chained return types are not inferred.
  ruby-lsp answers one request at a time, so definitions resolve over a
  pool of servers (capped at four) whose answers fold in request order;
- Tree-sitter PHP extraction (structural, plus semantic `calls` under the
  P2 and P3 verified-edge policy): namespaces as modules, class/enum, interface,
  trait, function, and method definitions, lexical `contains`, namespace
  `use` imports with `ImportRef` placeholders (aliases recorded as `Path as
  Alias`, group imports flattened), `inherits` edges from `extends`, and
  `implements` edges from `implements` clauses and trait `use`, each
  resolved only when both sides map to unique in-repository definitions
  under PHP's namespace rules. Magic dispatch (`__call`, `__get`), dynamic
  calls (`$x->$m()`, `call_user_func`), and `new $class` stay
  observations. Semantic mode launches phpactor under an operator-installed,
  hash-pinned tool bundle (`CODEMAP_PHP_TOOL_BUNDLE`) with its composer
  extension and auto-configuration off, a throwaway XDG home, and no
  worktree writes; the extractor builds phpactor's index first and resolves
  over a pool of servers whose answers fold in request order. A `calls`
  edge is emitted only when codemap's own static model verifies the
  receiver→target relation (implicit self `$this`/`self::`/`static::`/
  `parent::` reaching an instance method in the receiver's ancestry, a
  constant receiver `Cls::m()` reaching a static method in its chain, or a
  declared receiver, a typed property through `$this` or a typed
  parameter the body never assigns, reaching a method in the declared
  type's ancestry); everything else stays a categorized observation;
- Tree-sitter Go extraction (structural, plus semantic `calls` under the
  G2 verified-edge policy): one module per file from the `package`
  clause, struct, interface, defined, and alias types, functions, methods
  scoped under their receiver type, interface methods under their
  interface, imports verbatim as `ImportRef` placeholders (`path as
  alias`, `path as .`, `path as _`), `inherits` edges from struct
  embedding and `implements` edges from interface embedding, each resolved
  only when both sides are unique in their package directory (the
  embedded type found in the same directory or in the one its import path
  names under the root `go.mod`). `vendor/` and `testdata/` never enter
  the inventory, as the Go toolchain never compiles them. Implicit
  interface satisfaction is never an edge; method values and
  function-typed fields stay observations. Semantic mode launches gopls
  from an operator-installed, hash-pinned tool bundle
  (`CODEMAP_GO_TOOL_BUNDLE`) with `GOPROXY=off`, `GOTOOLCHAIN=local`, a
  throwaway home and build cache, and the repository deciding vendor
  mode; a workspace gopls cannot load fails the run. gopls type-checks, so
  its answer is admitted the way rust-analyzer's is, behind two gates: the
  target must start exactly at the parser's function or method declaration
  name, including declared interface methods, and the call must sit inside
  an emitted callable. Conversions and function-valued bindings remain
  observations; mixed owned/external definition answers are ambiguous;
- **semantic `calls` edges** via a local LSP client (rust-analyzer for
  Rust, ruby-lsp for Ruby, phpactor for PHP, gopls for Go, all over one
  stdio transport): a `calls` edge is emitted only when
  `textDocument/definition` resolves the target to a unique in-repository
  node (Ruby and PHP: additionally admitted only through codemap's own
  static model; Go: only when that node is a callable declaration);
  std-library and external-crate calls are reported as observations,
  never as canonical edges;
- call sites in syntax mode are **observations only** — zero canonical
  `calls` edges;
- validator: duplicate IDs, dangling endpoints, invalid spans, file-hash
  drift, language purity, and `calls`-source gates;
- **measures** behind `extract --measures` (Rust, Ruby, PHP, and Go): cyclomatic
  complexity and line counts per callable and line classes per file, and
  clone groups over the normalised token stream (fifty tokens, five lines,
  identifiers and literals as placeholders) with the callables and files
  they touch, under a named counting policy per language
  (`cc-sloc-clones-form/rust/1`, `cc-sloc-clones-form/ruby/1`, `cc-sloc-clones-form/php/1`,
  `cc-sloc-clones-form/go/1`) the identity records, so the
  snapshot id changes exactly when measures are present and the default
  output stays byte-identical. Counts only, computed from the parse tree
  by a hand-counted oracle; metrics over them are a consumer's business.
  Ruby's
  token stream comes from Prism's own lexer over FFI (`ruby_lex.rs`), the
  library `ruby-prism-sys` already builds, so no new toolchain step.
  `--twins` (with `--measures`) adds twin pairs: two callables of eighty or
  more tokens whose five-token shingles overlap by seventy percent or more
  of the smaller body, which is the copy-then-edit shape the run detector
  cannot see; the identity records `twins/<lang>/1` exactly when they are
  present, so the measured output without the flag is byte-identical;
- `stele code extract`, `validate`, and `report`; `--calls=required` for
  semantic mode (fails closed if rust-analyzer, or the Ruby, PHP, or Go
  tool bundle, is absent or drifted from its pinned lockfile, or if gopls
  cannot load the workspace).

## Install

On Apple Silicon macOS, install the prebuilt bundle with Homebrew:

```sh
brew install mortenhusted/tap/stele
```

The formula retains the license, dependency notices and build receipt under
`$(brew --prefix stele)/share/stele`. It also includes the pinned Ruby resolver
Gemfiles. To prepare verified Ruby calls, use a separate writable tool directory
and a Ruby installation with Bundler 4.0.18:

```sh
mkdir -p "$HOME/.local/share/stele/ruby"
cp "$(brew --prefix stele)/share/stele/ruby/"Gemfile* "$HOME/.local/share/stele/ruby/"
BUNDLE_GEMFILE="$HOME/.local/share/stele/ruby/Gemfile" bundle _4.0.18_ config set --local path ./gems
BUNDLE_GEMFILE="$HOME/.local/share/stele/ruby/Gemfile" BUNDLE_FROZEN=true bundle _4.0.18_ install
export CODEMAP_RUBY_TOOL_BUNDLE="$HOME/.local/share/stele/ruby/Gemfile"
```

This tool bundle belongs outside the application and uses its own dependencies;
extraction does not boot Rails. Other language resolvers are prepared separately
as described under Development.

Get a prebuilt bundle from [Releases](https://github.com/MortenHusted/stele/releases),
or build from source with the toolchain pinned in `rust-toolchain.toml`.
The `stele` binary groups every command: `stele code`
for code facts and their evidence projection, `stele spec` for the LemmaSpec
engine, `stele check` for both in one step, `stele plan` and `stele intent`
for plans and catalogs, and `stele slop` and `stele topology` for those
consumers.

```sh
cargo install --locked --path crates/stele
```

`scripts/release-package.sh --version X --target T` builds
`stele-X-T.tar.gz`, containing a `stele-X-T/` directory with the binary,
project/dependency notices and build receipts, for one of
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (glibc 2.31 or
newer), `aarch64-apple-darwin`, and `x86_64-apple-darwin`. The version is
the workspace version in `Cargo.toml`. Extract into a staging directory and
retain the complete bundle; put its directory on PATH so the notices stay with
the executable. See [release payloads](publication/README.md#release-payloads).

The semantic tool bundles (rust-analyzer, ruby-lsp, phpactor, gopls) are
installed separately, as described under Development.

## Agents

`stele intro` orients a coding agent, the Stele skill tells it which
command answers which question, and the `stele-onboard` skill walks it
through backfilling a repository's catalog for a human to approve. The
skills live in `plugins/stele/skills`, the binary carries the copies it
was built with, and this repository is their plugin marketplace.

```sh
stele agent install                # into ./.claude/skills and ./.codex/skills
stele agent install --dir ../app   # into another project
stele agent install --marketplace  # through Claude Code's and Codex's marketplaces
```

The marketplace can also be added by hand:

```sh
claude plugin marketplace add MortenHusted/stele && claude plugin install stele@stele
codex plugin marketplace add MortenHusted/stele && codex plugin add stele@stele
```

Agents that read plain `SKILL.md` collections, such as Cursor and Pi, take
them with `npx skills add MortenHusted/stele --skill stele --skill
stele-onboard`. Antigravity loads `plugins/stele` from a checkout as a
plugin directory.

## Plans

A plan puts the decisions behind a change and its ordered steps beside
the intents they serve, and `stele plan check` checks it against the
catalog and the code. `intent/` holds Stele's own catalog of what it is
for, and `plans/stele-plans.stele` is the plan for Stele plans:

```sh
stele plan check plans/stele-plans.stele
```

`stele intent` backs the catalog with evidence: tests tagged as examples
of a statement, their failing and passing runs (`stele intent record`),
approvals by name (`stele intent approve`), and the code that realizes
each statement. `stele intent report` gives every statement its standing,
and `stele intent impact main` lists the statements a change touches.
The skill's `references/plans.md` and `references/catalog.md` describe
the formats.

Standalone Ruby projects can record examples with
`stele intent record --runner minitest --example <id>`. This is explicit Unix
execution of tracked `test/**/*_test.rb` methods in direct `Minitest::Test`
subclasses. Cargo remains the default runner. Rails execution needs a separate
profile; the standalone runner refuses Rails roots and performs no Bundler or
database setup. See [the catalog guide](plugins/stele/skills/stele/references/catalog.md#standalone-minitest)
for supported runtimes and example tags.

## Usage

```sh
# Extract a snapshot from a clean worktree (canonical JSON to stdout)
stele code extract --path .

# Extract a Ruby repository (structural facts)
stele code extract --path path/to/ruby-app --language=ruby

# Extract the working tree as it is, uncommitted changes included
stele code extract --path . --dirty

# Add per-callable complexity and line counts and per-file line classes (Rust)
stele code extract --path . --measures

# Ruby semantic mode: verified call edges via ruby-lsp (needs the tool bundle)
CODEMAP_RUBY_TOOL_BUNDLE=toolchain/ruby/Gemfile \
  stele code extract --path path/to/ruby-app --language=ruby --calls=required

# PHP: structural facts, then verified call edges via phpactor (needs the tool bundle)
stele code extract --path path/to/php-app --language=php
CODEMAP_PHP_TOOL_BUNDLE=toolchain/php \
  stele code extract --path path/to/php-app --language=php --calls=required

# Go: structural facts, then verified call edges via gopls (needs the tool bundle
# and the repository's dependencies vendored or in the module cache)
stele code extract --path path/to/go-app --language=go
CODEMAP_GO_TOOL_BUNDLE=toolchain/go \
  stele code extract --path path/to/go-app --language=go --calls=required

# Semantic mode: resolve exact call-impact via rust-analyzer
stele code extract --path . --calls=required

# Write to a file
stele code extract --path . --out snapshot.json

# Show elapsed status on terminal or regular-file stderr (snapshot JSON is unchanged)
stele code extract --path . --progress --out snapshot.json

# Validate a written snapshot
stele code validate --file snapshot.json

# Print a human-readable report
stele code report --file snapshot.json

# ── Layer 1 navigation (deterministic graph queries) ──
# Load --file or extract --path; use --calls=required for call-graph queries.
stele code context <symbol>                      # 360° view: container, in/out edges, fan-in/out
stele code impact <symbol> --direction upstream  # transitive blast radius (callers)
stele code trace <from> <to> --kinds calls       # shortest call path
stele code search <needle>                       # find symbols by name
stele code structure [root]                      # containment tree
stele code cycles --kinds calls                  # SCCs = refactor signals

# Source-backed queries accept --language; the default is rust.
# With --file, the snapshot carries its own language.

# ── Cross-commit structural diff (deterministic; unique to codemap) ──
stele code diff <base> <head> [--top N]          # exact node/edge delta + churn hotspots
stele code diff HEAD~1 HEAD --path path/to/ruby-app --language ruby

# ── Cross-commit history: the structural change stream ──
stele code history --path <repo> --out <dir>     # walk commits, emit transitions + metric moves
stele code history --out <dir> --calls=required --limit 5   # ...with semantic call edges (slow)
stele code history --out <dir> --measures --limit 40        # ...keeping a measured snapshot per commit for stele slop series
stele code diff <base> <head> --calls=required   # call-edge delta between two commits
bin/timetravel when <symbol> -d <dir>            # when a symbol appeared, vanished, moved
bin/timetravel at <commit> -d <dir>              # everything that changed at one commit
bin/timetravel churn -d <dir>                    # facts that keep changing
bin/timetravel stats -d <dir>                    # shape of the walked history
bin/timetravel slop -d <dir> --window 20         # per-commit erosion and clone series with the slope per author label

# ── Layer 1.5 heuristic analysis (derived, non-authoritative) ──
stele code hotspots [--top N]                    # the "heaviest bits" (coupling x churn)
stele code refactor [--diff-base B --diff-head H]  # cycles/hubs/bases/gods/orphans + new-cycle/growing-hub; gods by cc on a measured snapshot
stele code clusters [--rounds N]                 # functional communities (seeded label propagation)
stele code processes [--max-steps N]             # execution flows traced from entry points
```

Extraction refuses a dirty worktree so commit provenance is always honest.

`--dirty` is the one deliberate exception: it reads the working tree as it
is. The inventory is tracked plus untracked, non-ignored files present on
disk, bytes and per-file hashes come from disk, and `tree_sha` is the content
hash of the working tree (the tree git would write for `HEAD` plus every
uncommitted change, computed in a private index that never touches yours).
The identity carries `dirty: true` whenever that tree differs from `HEAD`, so
a dirty snapshot has its own `snapshot_id` and can never be mistaken for the
commit's. On a clean tree `--dirty` produces byte-identical output to the
default. It stays deterministic for the same content and refuses a tree that
changed underneath the extraction; what it does not promise is that
`commit_sha` describes the bytes. A consumer keyed by commit must therefore
refuse a snapshot whose identity says `dirty: true`.

## Consumers

codemap emits one thing: the versioned snapshot JSON that `stele code
extract` writes and `stele code validate` checks. Every consumer reads that
file and owns its own projection of it, and the `codemap` crate never learns
a consumer's format.
The LemmaSpec evidence projection lives here as the separate
`codemap-lemmaspec` crate, which depends only on `codemap-graph` and ships
the four checkers its evidence is written for.

`stele check` runs the whole sequence in one process: it extracts the
repository (verified calls for `architecture` and `symbols`, measures for
`hotspots`), projects the evidence and checks it. It prints the report and
exits as `stele spec check` would, or exits 2 with the reason when extraction
or projection fails. It takes the evidence options below, `--snapshot FILE`
in place of `--path` to reuse a snapshot, and `--json`:

```sh
stele check symbols --path . --public-api "rust:src/lib.rs#extract"
stele check architecture --path . --core src/eval.rs --presentation src/html.rs
stele check hotspots --path path/to/ruby-app --language ruby
stele check symbols --snapshot snapshot.json --json
```

The same steps one at a time:

```sh
# Semantic snapshot, then evidence for the symbol-level liveness checker
stele code extract --path . --calls=required --out snapshot.json
stele code evidence --file snapshot.json \
  --checker symbols --public-api "rust:src/lib.rs#extract" --out evidence.stele
stele code checker symbols --out symbols.stele
stele spec check symbols.stele evidence.stele

# File-level layering: declare the core and the presentation files
stele code evidence --file snapshot.json \
  --checker architecture --core src/eval.rs --presentation src/html.rs \
  --out evidence.stele

# Complexity at the request layer: no entry point above the cutoff (needs --measures)
stele code extract --path . --language ruby --measures --out measured.json
stele code evidence --file measured.json --checker hotspots --out evidence.stele
stele code checker hotspots --out hotspots.stele
stele spec check hotspots.stele evidence.stele   # names every action, job, or main over cc 10
```

Every fact in the evidence says where it came from: a source location for
what the snapshot extracted, `policy:<name>/<language>` for what a named
rule in `crates/codemap-lemmaspec/src/policy.rs` classified, and no
provenance for a review decision passed on the command line. Entry points
default by convention: a Rust binary root `main`, Rails controller actions,
job and worker `perform`, and PHP controller actions, job and command
`handle`; anything else is declared with `--entry`. A symbol the
absence checker reports means “no path in observed graph under entry-point
policy …” within the recorded extraction scope; `--public-api` declares
additional symbol roots. Unresolved dispatch can hide a real runtime caller.

Every evidence file carries snapshot identity, source-file scope, all recognized
call-site categories, skipped-file and parse-error counts, and the entry-point
policy. Unsupported constructs and excluded-source inventory are explicitly
unknown where the snapshot does not measure them. The named admission policy
`observed-call-coverage/v1` defaults to 1000 per mille exact in-repository
`Resolved` sites divided by **all recognized sites**, including external,
standard-library, ambiguous, candidate, and unmapped sites. It also requires
semantic extraction, a nonempty census, and zero parse errors or skipped files.
`--min-call-coverage-per-mille 0..1000` can lower the threshold while retaining
these guards and the conditional scope. Even 1000 is an admission policy,
never a proof that extraction captured all calls.

Architecture and symbols require a positive coverage-admission expectation as
well as zero findings. Missing, empty, syntax-only, or below-threshold evidence
is inconclusive: `stele spec check` reports its existing incomplete status and
exits nonzero, even when guarded absence findings are empty. Hotspots retains
its complexity-based behavior; census metadata adds no coverage gate to it.

With LemmaSpec 0.4, `stele code evidence --readable` emits explicit
`snapshot`, `policy`, and `reviewer_declared` evidence bases, callable labels
and relative source links, and maintainer notes carrying the extraction
census. The producer sets
the basis; a provenance string cannot authenticate an arbitrary claim. Labels
preserve lexical IDs and do not affect findings. Legacy evidence remains the
default for LemmaSpec 0.3 compatibility. For symbols, `--selection-out
selection.json` writes the actual expanded entry-point/public-API node IDs and
coverage policy from the same projection for consumers that verify selections.

Rule-card citation checks require LemmaSpec 0.4 and follow the narrow grammar
in [CITATIONS.md](crates/codemap-lemmaspec/CITATIONS.md):

```sh
stele code evidence --file snapshot.json \
  --checker citations --rule-cards BUSINESS_RULES.md --out citations-evidence.stele
stele code checker citations --out citations.stele
stele spec check citations.stele citations-evidence.stele --json
```

Every card has separate `location_valid(rule, 0|1)` and
`semantic_support_unverified(rule)` results. Location validity means only that
the full cited range fits a recorded declaration, using `stele code locate`'s
snapshot-only containment. Missing, malformed, and out-of-snapshot locations
remain explicit invalid results. Executable classification is unsupported:
comments, blank lines, and executable lines inside the same declaration cannot
be distinguished. A valid location cannot establish whether a rule is true;
all claims remain reviewer-declared and this checker reports incomplete.
Semantic support needs independent review. No source bytes are read.

The slop metrics projection is the separate `codemap-slop` crate, also over
`codemap-graph` only. It reads snapshots
written with `--measures` and answers "what became harder to maintain in
this change" before it prints any ratio:

```sh
stele code extract --path . --measures --out head.json
stele slop diff --base base.json --head head.json   # per-callable deltas, cutoff crossings, hotspots, clone groups introduced
stele slop report --file head.json                  # erosion and verbosity_clone with their counts, hotspots, per-file erosion
stele slop report --file head.json --exclude db/migrate/ --exclude script/   # held to a scope; the all-files line still prints
stele code extract --path . --measures --twins --out head.json              # twin pairs beside the clone figure
stele slop report --file head.json --exclude test/                          # ...near pairs first, both callables in scope
stele slop gate --base base.json --head head.json --budget slop.toml        # exit 1 on a breach
stele slop report --file head.json --budget slop.toml                       # one snapshot held to the shape targets: the backlog per target
stele code history --path . --measures --limit 40 --out history             # one measured snapshot per commit
stele slop series --history history --window 20                             # one line per commit, slope per author label
```

Every threshold is a named constant with a citation in
`crates/codemap-slop/src/policy.rs`. A ratio never prints without the
count of callables over the cutoff, the maximum complexity, and the mass
above the cutoff beside it, and an improved ratio over a worse hotspot is
flagged on the same line. Coupling is verified `calls` edges only, never
`contains` or `inherits`, printed beside the call sites the resolver left
unverified (in-repository candidates, external targets, unresolved names);
on a syntax snapshot it reads `n/a`, never `0`, beside the sites observed.
Run-once code can be left out of what a report is held to with
`--exclude <dir>/` (a repository-relative directory prefix, repeatable, or
`exclude = "<dir>/"` lines in `slop.toml`): hotspots, findings, and the
gate then read the files in scope, and the all-files erosion with its
counts prints on the line above, with how much the scope left out. A
prefix that matches no file is refused, so a typo cannot widen the scope.
For an engine under `engines/shop/`, exclude its tests with
`--exclude engines/shop/test/`; `--exclude test/` covers only the root test directory.
Clone groups and their introduced/removed lists describe the whole repository;
clone-line ratios and twin pairs use the selected scope.
Twin pairs (`--twins`) print as their own count beside the clone figure,
never inside it, near pairs (edited copies) before exact ones, with both
callables located and a note when the pair already sits in a clone group;
a snapshot extracted without `--twins` reads `n/a`, never `0`. The clones
line says what a clone is (a run of fifty or more equal normalised tokens
over five or more lines, exact or renamed), so the ratio is never read as
"duplication". `stele code refactor` lists each side of a twin pair as a
`Twin` signal.

A Rails repository is best held to its runtime code; a checked-in
`slop.toml` such as

```toml
# advisory limits per change, applied by `stele slop gate`
max_new_high_cc_callables = 0
max_erosion_delta = 0.005
max_loc_delta = 2000
# run-once and scaffolding code, kept out of the figures (the all-files line still prints)
exclude = "test/"
exclude = "db/migrate/"
exclude = "script/"

# the shape the app is held to, seeded from a codebase that has it
[target.controller_method_cc]   # every controller action under cc 6
prefix = "app/controllers/"
measure = "cc"
stat = "each"
max = 6

[target.model_methods]          # no model over twenty-five methods
prefix = "app/models/"
measure = "methods_per_class"
stat = "max"
max = 25

# Sandi Metz's rules, on the form measures a Ruby snapshot carries
[target.metz_params]            # rule 3: no more than four parameters
prefix = "app/"
measure = "params"
stat = "each"
max = 4

[target.metz_controller_ivars]  # rule 4: an action hands its view one instance variable
prefix = "app/controllers/"
measure = "ivars"
stat = "each"
max = 1

[target.metz_method_lines]      # rule 2: five-line methods, held at the median
prefix = "app/"
measure = "sloc"
stat = "p50"
max = 5
```

reads the runtime figures, since test scaffolding and migrations tend to
carry most of a Rails app's clone lines. Every command refuses a snapshot without a
counting policy, two snapshots under different policies, a language without
one, or an empty inventory. The gate is advisory until the plan's
experiment has run; `slop.toml` is flat `key = number` lines
(`max_erosion_delta`, `max_loc_delta`, `max_new_high_cc_callables`),
repeatable `exclude = "<dir>/"` lines, and `[target.<name>]` sections.

A target is a ceiling under a directory prefix on a measure the snapshot
already carries: `cc` or `sloc` per callable, `methods_per_class`,
`class_loc`, or `inherits_depth` per class-like definition (from
`contains` and `inherits` edges and the type's span), `twins_per_1k`
(twin pairs with both callables under the prefix, per thousand callables
there; `n/a` without `--twins`), or one of the form measures a policy
with them carries (Ruby, `cc-sloc-clones-form/ruby/1`): `params`
(parameters declared), `nesting` (the deepest stack of conditionals,
loops, `case`, and block bodies any node sits under), `chain` (the
longest dotted call spine, `a.b.c.d` being 3), and `ivars` (distinct
instance variables a method writes). Under a policy without them a form
target reads `n/a`, never met. `stat` says how the sample is held to
`max`: `each` item on its own, or its `p50`, `p90`, or `max` (nearest
rank, never interpolated); `twins_per_1k` takes `value`. The gate applies
the ratchet (`policy::TARGET_RATCHET`): a target is breached only when it
is over the ceiling **and** worse than the base, and an `each` target only
by a callable or class the change added to the prefix or made worse.
Everything else over the ceiling prints as the backlog with its count, so
a repository can adopt another codebase's shape on day one: the gate
shouts at new slop, and `report --budget slop.toml` shows the distance to
the shape on one snapshot.
`stele slop series` folds a walk made with `stele code history --measures`
into one line per commit and fits the slope of each figure per author label
over a window; the label is what the commit says, and nothing in the crate
claims it identifies a person or an agent. The walk keeps the snapshots and
computes no metric, so the producer never depends on the consumer.

## As a library

```no_run
use codemap::{extract, ExtractOptions};
let snapshot = extract(&ExtractOptions::default()).unwrap();
```

The public API is one deep operation (`extract`) plus the versioned
`schema` types and `validate`. Parser, resolver, and git internals are
internal test seams.

Python structural extraction is available on Unix through
`python_syntax::{extract_python, extract_python_worktree, extract_python_at_commit}`
with an explicit `python_parser::ParserConfig`. The configuration names an
operator-prepared, captured CPython runtime; extraction never installs tooling
or imports application modules. These APIs inventory regular `.py` files and
preserve lexical ownership, async definitions, repeated-name ordinals, and
source-backed decorator/base expressions. Source links and linked ancestors
are refused. Historical extraction reads immutable Git blobs.

The v8 neutral schema adds the `python` namespace and optional parser/native
input identities. Syntax snapshots contain observations for calls, with no
canonical call or inheritance edges. Python measures, clones and twins remain
unsupported. Native snapshots admit calls only after an owned compiler query
and independent CPython census agree on complete original call and definition
witnesses. Python is accepted within this compiled-call regime;
a native input descriptor alone does not prove an execution. The accepted
runtime/compiler profile uses captured CPython 3.14.7 and mypyc 2.3.1 on Darwin
arm64. Ordinary CPython call bindings remain unsupported.

The CLI uses the same explicit configuration for Python syntax extraction and
history, with no ambient interpreter fallback:

```sh
stele code extract --language python --python-parser /prepared/parser.json --path .
stele code extract --language python --python-parser /prepared/parser.json --path . --dirty
stele code history --language python --python-parser /prepared/parser.json --path . --out /local/history
```

The JSON is a `ParserConfig` containing `roots`, captured operator `inputs`, and
the selected `runtime` manifest. Configuration files must be regular files of
at most 32 MiB. The source-only parser refuses required calls. Python measures,
clones, twins, and semantic history are refused. Syntax history reads original commit blobs and ignores
other-language-only changes; it validates the first snapshot before creating
output files.

Required native extraction uses an explicit `PythonNativeExtractConfig`:

```sh
stele code extract --language python --calls required --python-native /prepared/native.json --path .
```

The configuration contains captured `parser` and `native` configurations,
`package_profile` with ordered source roots, a sorted nonempty `native_sources`
list of repository-relative `.py` files, and sorted `declarations` of `.pyi`
files. Both selections must belong to the captured Git tree. The parser covers
every inventoried `.py` file plus selected declarations; the compiler receives
only the selected compilation unit. Outside-unit and rejected calls remain
visible as `Unsupported` sites. Declarations have no implementation bodies or
call edges. Call facts describe the selected mypyc compiled execution profile;
ordinary CPython call bindings remain unsupported. `--dirty` uses the same admission rules against the captured working
tree. Native configuration files follow the same bounded regular-file rule and
cannot be combined with other adapter selectors or `--python-parser`.

The corresponding APIs are
`python_semantic::{extract_python_native, extract_python_native_worktree}`.
They obtain both witnesses themselves; caller-supplied parser records or native
JSON cannot establish canonical calls.

Python native snapshots support the existing source navigation and caller-impact
queries. `stele code positive-path` accepts exact Python callable node IDs
under `python-positive-path/v1`, retaining the original source provenance.
It proves only a selected path in the observed graph; it makes no completeness
or runtime-reachability claim. Python absence checkers and measures remain
unsupported.

For a mixed Python/C++/JS repository, extract each language separately at the
same clean commit (or the same captured dirty tree). Consumers may present those
validated snapshots together only when repository, commit/tree and dirty status
agree, preserving each language's snapshot and semantic-profile identity.
Matching names across snapshots do not establish cross-language call edges.

## Development

```sh
cargo test                 # hermetic tests (unit + integration)
BUNDLE_GEMFILE=toolchain/ruby/Gemfile bundle install   # ruby-lsp tool bundle (operator setup, once)
composer install --working-dir=toolchain/php           # phpactor tool bundle (operator setup, once)
(cd toolchain/go && GOBIN=$PWD/bin go install tool)    # gopls tool bundle (operator setup, once)
cargo test -- --ignored    # semantic/failure tests requiring local tools
CODEMAP_DEFINITION_WORKERS=1 ...   # force sequential definition resolution (Rust and Go lanes; Ruby and PHP server pools)
cargo clippy --all-targets   # no warnings under pedantic+nursery lints
```

GitHub Actions runs formatting, strict workspace Clippy, the locked workspace
tests, `stele plan check` and `stele intent report` on GitHub-hosted Ubuntu
24.04, using the toolchain pinned in `rust-toolchain.toml`. Pull requests
check out their head commit with its history: the example ledger names that
source tree, rather than GitHub's synthetic merge tree. Before merging, rebase
onto the fresh default branch and rerun the gates.

The workflow explicitly prepares rust-analyzer, rust-src and the locked Rust
fixture dependencies, then runs the four native Rust semantic tests offline.
Each test needs a named passing witness, so an empty filtered run fails CI.
Other operator-backed suites, public corpus runs and language-specific Ruby
benchmark runner tests still need their prepared operators. Linux runtime
profiles, macOS and cross-target runtime acceptance require their own evidence
before publication.

### Identifier scrub

`bin/scrub` checks an explicit Git export and optional unpacked artifact
directories against a local deny-list. Install its Ruby dependencies with
`gem install tty-option tty-prompt tty-command`. The list contains one
case-sensitive ASCII token per line; matching uses whole `[A-Za-z0-9_]+`
tokens, with punctuation and non-ASCII bytes as delimiters. It does not
normalize names or match substrings. A token scan and hash-set lookup keep
the work independent of the number of list entries per byte scanned.

```sh
bin/scrub --repo EXPORT --deny-list /private/deny.txt \
  --report /private/new-report.json /private/unpacked-artifacts
ruby bin/scrub.d/test/scan_test.rb
```

The scanner reads tracked and untracked non-ignored paths, symlink target
text without following links, and all locally stored Git objects, including
removed content, commit messages and objects no ref reaches. It refuses
shallow history. It does not fetch remote or pull-request refs or read GitHub
conversation text; supply those as local unpacked artifact files when needed.
Recognized packed formats (ZIP, gzip, bzip2, xz, 7z, zstd and tar) require
unpacking before scanning. This is an identifier check, not an inspection of
every possible encoding, a secret scanner, or proof of provenance.

Keep the list and report outside every Git repository and scanned input.
Console JSON contains counts for tree, history and artifacts, plus the list
digest and export HEAD; matching entries and locations go only to the
optional new report file, created with mode 0600. Artifact paths are scanned
relative to their root; report locations use `artifact[index]/relative-path`
without including the operator's root directory. Counts are distinct matching
entries per file or Git object, rather than repeated occurrences. Exit 0
means no listed tokens found, 1 means matches need review, and 2 means an
invalid or incomplete scan. No match is deleted automatically. GitHub CI
runs the synthetic scrub tests; private lists and findings stay local.

### Public PHP and Go corpus gates

[`benchmarks/php.json`](benchmarks/php.json) and
[`benchmarks/go.json`](benchmarks/go.json) pin the public Monica and lazygit
commits, their upstream license locations, the shipped synthetic labelled
oracles, tool lock digests and application size thresholds. No corpus source
text is included. Fetch the upstream source separately and keep its license
with that checkout.

Prepare the tool bundle before running the gate:

```sh
composer install --working-dir=toolchain/php --no-interaction
(cd toolchain/go && GOBIN="$PWD/bin" GOFLAGS=-mod=readonly go install tool)
```

Check out the exact commit named in the corresponding JSON file, then run:

```sh
bin/public-corpus-benchmark --language php --repo /local/monica --out /local/new-php-results
bin/public-corpus-benchmark --language go --repo /local/lazygit --out /local/new-go-results
```

`--bundle` selects another prepared operator bundle with the same pinned lock.
The command builds Stele and runs the existing ignored labelled-fixture test
for exact canonical edges and forbidden observations. It then extracts syntax
and semantic snapshots from independent clean checkouts at the public pin,
validates them, rejects wrong identity, undersized or empty semantic output,
and requires byte-identical semantic snapshots. The temporary clones use the
declared public upstream URL as their repository identity.

Precision is established by the synthetic fixture, not by a complete oracle
over Monica or lazygit. The real application gate covers identity, scale and
determinism. Results, snapshots and logs stay in the explicit new output
directory outside Git; `result.json` records input and executable digests,
`files_scanned` and `calls` from the validated semantic snapshot.
The older `scripts/benchmark-monica.sh` and `scripts/benchmark-lazygit.sh`
delegate to this command. CI tests the gate's refusal cases, while actual
semantic runs require the prepared operator tools.

Go's gate also runs the source-reviewed declaration-admission fixture in
`crates/codemap/tests/fixtures/go_callable_targets`. It covers same-name local
closures, parameters, aliases and method locals, alongside real recursion,
interface dispatch, generics, promoted methods and calls inside literals.
In-repository non-callable definitions are `Unmapped`; an exact callable
target outside any callable caller is `Candidate`. Extractor 0.5.2 fixes
false self-edges that could occur in earlier Go semantic snapshots when a
local binding shared its enclosing callable's name, and refuses mixed
owned/external answers. Regenerate those older snapshots.

Rust's pinned public ripgrep corpus, independently reviewed selected-caller
oracle and offline preparation commands ship under
[`benchmarks/rust`](benchmarks/rust/README.md). The same runner requires exact
source-span edges for the selected callers, explicit forbidden targets and
two-checkout determinism. `From`/trait-implementation calls are outside
required recall; one explicitly labelled definition-only site is reported
separately, with only its exact owned target permitted.

Ruby's pinned public Mastodon corpus, source-reviewed selected-caller oracle and
separate operator preparation ship under
[`benchmarks/ruby`](benchmarks/ruby/README.md). Its gate retains the original
R2 fixture and bundle-failure tests, requires the selected production calls
exactly, excludes explicit same-name decoys, and compares independent
checkouts with one and three definition workers. Extractor 0.5.3 repairs Ruby
superclass and mixin lookup that could bind qualified references to unrelated
same-name definitions; regenerate older Ruby snapshots.

C++'s selected public double-conversion oracle, explicit operator/profile
preparation and one-command guarded acceptance procedure are described in
[`benchmarks/cpp/README.md`](benchmarks/cpp/README.md). The original fixture,
known unsupported classes and unchanged precision/recall thresholds remain
separate from whole-library completeness. Generated evidence stays local.

Black's frozen labelled oracle, source anchors and native query hashes ship
under [`benchmarks/python`](benchmarks/python/README.md). `bin/prepare-black`
checks the public source and pinned parser, applies an exact frozen deletion
list and materializes local inputs for the existing Python benchmark. It
does not establish native semantic acceptance. `bin/prepare-black-operator`
builds its closed compiler/runtime inputs from pinned public archives and an
explicitly selected host cache family; the guarded benchmark establishes
execution and semantic results separately.

Excalidraw's frozen public oracle, source anchors and logical input inventory
ship under [`benchmarks/js_ts`](benchmarks/js_ts/README.md).
`bin/prepare-js-ts` verifies a pinned checkout with its prepared dependencies
and canonical compiler bundle, then writes a local environment with an
explicit operator runtime profile. Runtime execution witnesses and semantic
acceptance are established by the guarded extractor and existing benchmark,
separately from this preparation step.

## Architecture

| Module | Owns | Must not own |
|---|---|---|
| `schema` | versioned facts, IDs, provenance, canonical serialization | parsing, Git, transport |
| `git` | clean-worktree check, working-tree identity, commit and worktree file inventory | fact derivation |
| `rust_syntax` / `ruby_syntax` / `php_syntax` / `go_syntax` | parse, definitions, containment, import syntax, heritage and embedding candidates, call observations | target guessing, LSP lifecycle |
| `lsp` | LSP stdio transport, response routing, server-request hook, definition parsing, URI/position mapping, batched definition fan-out over lanes | launch policy, initialization params, indexing wait, edge admission |
| `rust_analyzer` | rust-analyzer launch policy, `initialize` parameters, indexing quiescence, location classification | framing, fact derivation, persistence |
| `ruby_lsp` / `php_lsp` / `go_lsp` | tool-bundle detection and identity, launch isolation, indexing wait, the admission model for `calls` edges (a static receiver model for Ruby and PHP, callable gates over the type checker for Go) | framing, fact derivation |
| `resolve` | definition index, `implements` resolution, omission accounting | parser traversal, persistence |
| `extract` | ordered orchestration, validation, report | CLI flags, graph upload |
| `validate` | truth-boundary enforcement on a complete snapshot | fact derivation |
| `nav` | deterministic graph queries (impact/trace/context/cycles/search) over a snapshot | edge derivation, mutation |
| `diff` | cross-commit structural diff of two commit-pinned snapshots | fact derivation |
| `cli` | the commands `stele code` runs: `extract`, `validate`, `report` and the graph queries | domain logic |

## License

[Stele Source-Available License 1.0](LICENSE). Personal projects, education,
research, public institutions and internal business use are free, including
paid consulting, audits, reports and specifications. Work you create or check
with Stele remains yours.

Selling Stele or LemmaSpec, embedding their covered code in products supplied
to others, or providing hosted or managed access to their functionality needs
separate written permission, whether or not access is paid. Free standalone
copies and source forks may be shared under the same terms. Contact
[MortenHusted](https://github.com/MortenHusted/stele) for commercial permission.

This is source-available software, not an OSI open-source license. Earlier
MIT grants and separately licensed components retain their permissions; see
[NOTICE](NOTICE) and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
See [CONTRIBUTING.md](CONTRIBUTING.md) before proposing a contribution.
