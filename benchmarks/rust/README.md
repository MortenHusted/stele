# Public Rust corpus gate

`ripgrep.json` pins BurntSushi/ripgrep 15.2.0 at
`e89fff89ac9af12e8d4ce9d5fd07beb408ca730f`, Rust 1.99.0 and the unchanged
synthetic call oracle. Ripgrep is MIT OR Unlicense; `LICENSE.ripgrep` preserves
its upstream MIT notice. The oracle contains symbol/span/hash metadata, not
copied function bodies. Fetch the source separately and retain its notices.

`ripgrep-oracle.json` was labelled from public source and syntax-only anchors,
then independently reviewed before the first rust-analyzer query. Its 13
complete production callers cover escaping, pattern loading, receiver methods,
associated functions and cross-crate calls through workspace re-exports.
All 17 required source-span edges must match exactly, with no extra selected
edges. Four explicit wrong-target decoys and three local-closure sites must
remain excluded. Every labelled site must appear exactly once as an observation.
This is a selected-caller oracle, not application-wide precision or recall.

`From`/trait-implementation calls are outside required recall under the existing
definition-only adapter. The one labelled `io::Error::from(err)` site may either
admit its exact anchored implementation or retain a noncanonical observation.
Every other owned target is forbidden. The result reports its category and
whether the owned implementation was admitted; this exception applies only to
that site and target.

Prepare dependencies explicitly, before the offline gate. Install the declared
Rust toolchain with its `rust-analyzer` and `rust-src` components. Use a new,
isolated Cargo home named `.cargo`, without personal credentials or custom
configuration. For example, after checking out the exact ripgrep commit:

```sh
rustup component add --toolchain 1.99.0 rust-analyzer rust-src
mkdir -p /local/rust-operator/.cargo
CARGO_HOME=/local/rust-operator/.cargo cargo +1.99.0 fetch --locked --manifest-path /local/stele/Cargo.toml
CARGO_HOME=/local/rust-operator/.cargo cargo +1.99.0 fetch --locked --manifest-path /local/stele/crates/codemap/tests/fixtures/calls_cargo/Cargo.toml
CARGO_HOME=/local/rust-operator/.cargo cargo +1.99.0 fetch --locked --manifest-path /local/ripgrep/Cargo.toml
```

Fetching dependencies is preparation, not a semantic result. Do not build or
execute ripgrep to prepare the gate. Run from the Stele checkout:

```sh
bin/public-corpus-benchmark --language rust --repo /local/ripgrep --cargo-home /local/rust-operator/.cargo --out /local/new-rust-results
```

The command builds Stele offline, runs exactly the named synthetic oracle and
the live execution-policy control, then creates two independent source clones.
It validates syntax and semantic snapshots, checks the pinned source anchors,
requires more than 90 Rust files, forbids syntax calls and requires identical
semantic bytes across clones. The analyzer uses offline Cargo with build
scripts, procedural macros and background checks disabled. The controlled
synthetic positive copy deliberately executes its own build script and macro
to prove the traps work; a fresh semantic copy must trigger neither and must
still resolve an ordinary call. The real application is never executed.

Snapshots, logs, executable/input digests and results remain in the new output
directory outside Git. `result.json` is written only after every gate passes.
Hosted CI tests the scorer's refusals, prepares rust-analyzer, rust-src and
the locked fixture dependencies, and runs the four native Rust semantic
tests offline with named passing witnesses. The full public application gate
requires its separately prepared operator.

The first run against the reviewed oracle exposed a target-admission defect
in extractor 0.5.0: three local-closure definitions mapped to their enclosing
function, and an enum variant mapped to its enum. All 17 required calls were
present, but those four extra edges correctly failed the gate. Extractor
0.5.1 admits a Rust target only at the parser's exact function/method name
start and attributes calls only to enclosing functions/methods. Its separate
regression fixture includes indirect bindings, parameters, fields, globals,
constructors and same-name closures, alongside real recursive, nested and
receiver calls. Rust semantic snapshots accepted before this fix may contain
these false canonical edges and should be regenerated. The earlier fixture
and incoming-edge checks did not establish their absence.

This also changes visible coverage: macro invocations no longer emit `calls`
edges to macro nodes. Calls whose definitions are required trait methods no
longer emit edges to the enclosing trait; the parser currently emits no
callable node for a required method declaration. Supporting those declarations
as exact targets needs a separate implementation and labelled gate. Genuine
recursive calls still target their exact function declarations.

For Rust call expressions, `Unmapped` means that no exact emitted function/method declaration
was available. It includes empty or unusable resolver answers and definitions
that point at local bindings, parameters, fields, globals, constructors,
macros or unrepresented required trait methods. It does not mean that the
resolver returned nothing. `Candidate` distinguishes a resolved callable
target whose call site has no enclosing emitted function/method. Multiple in-repository
or mixed owned/external definition answers remain `Ambiguous`. The regression fixture pins
these distinctions, including the loss of macro and required-trait-method
edges. Observation categories do not claim a complete unsupported-reason
taxonomy.

Extractor 0.5.5 also removes global bare-name resolution of Rust `implements`
relations. That lookup could bind a foreign trait or imported foreign type to
an unrelated local declaration with the same name. Regenerate older Rust
snapshots that contain implementation relations.

With `--calls required`, rust-analyzer resolves both identifiers in each
supported `impl Trait for Type` header. Each answer must uniquely identify the
exact name start of an emitted declaration: a concrete struct, enum or union
for the implementor and a trait for the target. Compiler-backed relations carry
`RustAnalyzer` provenance and require semantic identity. Mixed, external,
ambiguous or unrepresented endpoints remain observations. Implementations with
type, lifetime or const parameters, or applied type arguments, are unsupported
in 0.5.5 and remain observations even for real in-repository generic structs;
the graph does not represent their constraints or specializations. The fixed
no-execution/offline policy also applies to these queries.

Syntax mode proves only bare-name relations between declarations in the same
file and lexical module, with no visible import or glob that could bind either
name. Callable-local definitions and implementations do not establish that
proof. Qualified paths, aliases and cross-file relations require semantic mode;
their omission from syntax mode is a coverage change. A visible glob import is
an explicit unsupported boundary in this first repair, even when it imports a
real local trait. Such an omitted relation is not a negative runtime claim.
The source-reviewed control retains required local, qualified, alias and
same-name trait cases alongside foreign-name decoys. The public call oracle
and its acceptance thresholds stay unchanged.
