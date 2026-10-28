# C++ toolchain

Semantic C++ extraction uses an operator-owned clangd. Tree-sitter supplies
structure. Clangd supplies candidate locations. The adapter admits a call
only after its own checks, so a unique clangd location is not by itself a
canonical edge.

The acceptance pin is recorded locally under `target/benchmarks/cpp/` after
the feasibility probe. This directory does not vendor clangd, and a compiler
found on `PATH` is not the pin.

## Launch

- Pass `--compile-commands-dir` for the validated database.
- Pass `--enable-config=0` so a project `.clangd` and the user config are ignored.
- Do not pass `--query-driver`. Clangd must not execute the compiler named in the database.
- Pass `--background-index=0` and keep index files in a disposable directory.
- Do not run the database commands, load plugins, or write into the analyzed tree.

Required semantic mode fails when the selected database has a missing input,
two different commands for one translation unit, or a drifted tool pin.
Syntax extraction does not launch clangd.

## Platforms

Syntax extraction depends on the git tree and the pinned parser. The same commit produces the same syntax snapshot on macOS and Linux.

Semantic extraction depends on one compilation profile. The profile fingerprint covers normalized commands and translation-unit bytes. The session also retains a separate input fingerprint for manifested headers, compiler resources, and sysroot files. The host name is not part of the identity. A Darwin SDK and a Linux sysroot are different profiles. Declared source, build, dependency, and sysroot roots are written as logical names, so moving a checkout does not change the fingerprint. `--target`, `-isysroot`, `-std`, and `-D` stay in the fingerprint.

`.C` is a C++ extension. `.c` is not. Inventory uses the case git recorded.

Accepted semantic extraction targets macOS arm64 and native Linux x86_64/aarch64 with explicit runtime profiles. Linux also requires the guarded kernel capabilities described below. A compilation target and an extraction host are separate parts of the profile. Extraction never downloads a compiler. Syntax extraction remains portable; Windows is outside the first acceptance hosts.

The profile loader hashes normalized commands, working directories, and all
translation-unit bytes, including dependency units. `CppSession` combines this
profile with captured, manifested headers, compiler resources, and sysroot
files. The standalone loader is not an input-closure proof.

The runtime pin API (`RuntimeManifest` and `CapturedRuntime`) verifies explicit
operator file digests and the complete declared shared-cache family without
executing a tool. Files have stable logical names, absolute canonical paths,
SHA-256 pins, and roles (`clangd`, `library`, `shared_cache`, `inspector`, or
`sandbox`). Exactly one file has the `clangd` role. Cache-family membership
names a canonical directory and a filename prefix; every matching entry must
have a `shared_cache` pin. Missing, extra, nonregular, substituted, or changed
files fail verification. The fingerprint covers logical names, roles, bytes,
and the family prefix; installation paths stay outside identity.

This API is a launcher prerequisite. It does not attest loaded images or
enable semantic extraction by itself. The launcher must bind the actual
process's loaded libraries and loader to these pins, enforce isolation, and
repeat runtime verification before returning a snapshot.

`DarwinImageCatalog` reads the pinned arm64e cache catalogue and thin arm64
operator Mach-O UUIDs. Its inspector check binds one live clangd PID to both
`sample` and `vmmap`: all loaded paths and addresses must agree, operator images
must match their pinned UUIDs and exact mapped paths, and cached images must
match catalogue UUIDs with one common slide. Unknown, duplicate, missing or
contradictory images fail. The loader must be present. Inspector subprocesses
have a deadline and reports have a size bound.

The retained `catalogue_uuid` comes from the pinned cache header. These
inspectors expose individual image UUIDs rather than the process's global cache
UUID; live binding uses the image UUIDs, exact mappings and common slide.
The caller must compare loaded-image proofs before and after compiler queries
and reverify runtime bytes. `CppSession` performs these checks
through the shared LSP transport under a network, execution and write guard.
The canonical producer and CLI use this guarded session. The adapter passed its
acceptance gates on
2026-10-01.

Normalized commands retain argument boundaries and the working directory.
Logical root markers such as `{source}` are reserved in command arguments;
literal occurrences are rejected so they cannot collide with normalized paths.

## Supported compilation commands

The preparatory loader accepts `c++`, `g++`, `gcc`, `clang++`, and `clang`
driver names. Commands must name exactly the database entry's source file.
Relative source and path-option values resolve against the recorded working
directory, then normalize under declared roots. Include-search order and
argument boundaries remain part of the fingerprint.

Supported path options are `-I`, `-isystem`, `-cxx-isystem`, `-isysroot`,
`-iframework`, `-idirafter`, `-iquote`, `-imacros`, `-include`, `--sysroot`, `-resource-dir`,
and `-o`. Joined forms are supported for `-I`, `-iquote`, `-isysroot`,
`--sysroot=`, and `-resource-dir=`. Other path options require separate values.
Forced-header options `-include` and `-imacros` require absolute paths to avoid
changing include-search semantics. All path values must stay inside declared
roots, including output paths.
The `-I-` search barrier and sysroot-relative `=` and `$SYSROOT` path forms
require preparation as explicit paths.

The initial flag set covers definitions and undefinitions, language and target
selection, standard and standard-library selection, common optimization and
debug levels, warnings, sanitizers, architecture selection, exceptions, RTTI,
position-independent code, threading, and standard-include suppression.
Unknown flags fail with preparation guidance. Compiler wrappers, Clang
passthrough flags, plugin loaders, config files, overlays, precompiled headers,
modules, and assembler/preprocessor/linker passthrough flags are unsupported.
Response files expand as data and pass through the same validation.

This allowlist is one prerequisite for semantic launch. It does not establish
that all header, compiler-resource, or sysroot inputs have been manifested.

## Fixture

`bin/cpp-benchmark --help` describes the reusable labelled benchmark command.
Pass a prepared repository/profile to extract, or `--snapshot` to score an
existing canonical snapshot. The command validates the snapshot through
`stele code validate`, then evaluates the frozen sample. Supply independent
SHA-256 pins for both the manual oracle and the pre-query parser-anchor
record.

The oracle supplies `commit`, `targets` (path, definition line, symbol), `sites`
(unique ID, path, caller, byte offset, classification, regime, optional target),
and `cross_module_queries` (caller path/name, target and site ID). Classifications
are `known_in_repository`, `external`, or `unknown`; `supported` is the only
regime eligible for supported-shape recall. Other named regimes stay in the
known-target denominator. Require at least 60 sites, or an explicit `census:
true`, and ten distinct cross-module query labels. These declarations are
independently reviewed before extraction; the command does not create labels.

The anchor record supplies `files` with path, source SHA-256, call spans/name
spans, and body spans/name spans/normalized names. Supported labels require
unique call and target anchors. Exact credit requires the target's full body
span. For omitted unsupported call anchors, any emitted call spanning the
independent site offset fails precision; omission never earns recall credit.

Reports require zero false positives in the sample, at least 90% supported
recall and at least 50% overall known-target recall. Unknown/external counts
remain separate. Outputs go into a new local directory; corpus evidence stays
unpublished. This command checks the labelled sample, not the entire adapter
acceptance contract. Run its synthetic controls with:

```sh
ruby bin/cpp-benchmark.d/test/score_test.rb
```

`crates/codemap/tests/fixtures/cpp_calls/` is the labelled synthetic oracle.
`expected.json` names the direct calls that may become canonical edges and
the shapes that stay observations. Those labels were written from the
source, not from a clangd response.

## Executable parser-isolation probes

Set `CODEMAP_CPP_CLANGD` to an absolute operator-owned binary and
`CODEMAP_CPP_CLANGD_SHA256` to its recorded SHA-256, then run:

```sh
cargo test -p codemap cpp_isolation_tests --lib -- --ignored
```

The tests use the shared LSP transport and an internally generated LLVM VFS
with `redirecting-with: redirect-only`. Manifested files point to captured
copies. `--skip-preamble-build` is required by this probe: the default preamble
path failed AST construction under the restricted filesystem. The tests verify
all eight independently labelled candidate body locations, invisible
unmanifested files even through `__has_include`, failed direct includes of
unmanifested headers, captured header bytes after original-file changes, and
disabled project configuration. Temporary paths include spaces and non-ASCII
characters. Compilation commands name an inert driver and explicit target,
sysroot, and compiler-resource paths.

The tests exercise `CapturedInputs`, which verifies an operator-provided
`InputManifest` and owns private copies until it is dropped. Manifest entries
map logical filenames, such as `{source}/src/unit.cpp`, to lowercase SHA-256
digests. Its fingerprint covers file contents and aliases without including
physical checkout paths or inode numbers. Symlinks must resolve inside declared
roots. Physical aliases share a copy; distinct files with identical bytes keep
separate copies, preserving `#pragma once` behavior. Verification before and
after resolution rejects original/copy content drift, changed alias relations,
and a changed overlay. Unknown files receive no VFS entry.

A semantic-token probe checks exact call-site token positions and the server’s
advertised legend. It distinguishes concrete and static calls from virtual
calls, including an indirectly inherited virtual method, and keeps a function
pointer as a parameter token. Hover text is not the dispatch witness.

A compiled-ownership probe checks exact qualifier tokens on out-of-line bodies
and their source declaration ranges. A class definition is distinguished from
its forward declaration. A class behind an include guard is
distinguished from a namespace with the same name; their body tokens identify
a method and a free function respectively. This evidence does not change the
syntax adapter's treatment of conditional definitions or recover graph nodes.

The U1 tests retain exact Tree-sitter declarator anchors for body names and
qualifiers, including qualified bodies omitted from syntax snapshots. Return
type qualifiers and body-less prototypes are excluded. Template and conditional
contexts remain visible. The ownership and call/body identity probes compare
these anchors with independent fixture positions before querying clangd.

`CppSession` uses compiled ownership admission in production. Each
qualifier must bind to an exact parser name on a namespace or class body, with
matching kind, spelling, and compiler scope. Class forward declarations,
body-less aliases, member names inside a class span, templates, missing
definition tokens, and conflicting witnesses cannot supply an owner. The
recovered callable keeps its normalized signature, source span, and lexical
parent. Canonical extraction projects admitted bodies and calls with `Clangd` provenance, retaining spelled nodes with their original provenance.

The reusable parser-anchor exporter reads a local source manifest with `files`
containing repository-relative `path`, source `text`, and its SHA-256. It does
not read or trust compiler responses. Set `CODEMAP_CPP_SOURCE_MANIFEST` and
`CODEMAP_CPP_ANCHOR_REPORT` (a new local output), then run:

```sh
cargo test -p codemap export_source_pinned_body_and_call_anchors --lib -- --ignored
```

Generate and independently pin this record before compiler queries. It describes
spelled source anchors only; it does not establish compiled presence, callers,
targets, or adapter acceptance.

## Production session

`CppSession::start` takes a compilation database, declared roots, input and
runtime manifests, and the selected C++ source bytes. Roots are canonicalized;
selected sources must match the captured input bytes. Every application unit
requires an explicit `-x c++` or `-x c++-header`, target, sysroot, compiler
resource directory, `-nostdinc`, and `-nostdinc++`. Include paths must be
prepared explicitly. The private database uses an inert driver and the
session's captured VFS; the operator cannot supply an overlay through the
compilation-profile allowlist.

The production guard supports macOS arm64 and native Linux x86_64/aarch64
with the required kernel capabilities. Other hosts fail before launch.
The session disables configuration and background indexing, denies
network access and child execution, and restricts writes to its private state.
`resolve` requires clean diagnostics and admits bodies and calls from exact
parser anchors, semantic tokens, and compiler identities. Before returning,
it compares loaded-image proofs and reverifies the original compilation
profile, captured inputs, and runtime bytes. Failure returns no partial result.

The ignored live tests use `CODEMAP_CPP_RUNTIME_MANIFEST` to exercise the
independent eight-call fixture and reject original-input drift:

```sh
cargo test -p codemap cpp_lsp::tests::production_session --lib -- --ignored
```

The session tests also terminate the owned server and reject missing or drifted
tool pins. With the same runtime manifest, run the production OS-guard controls:

```sh
cargo test -p codemap production_guard_denies_network_execution_and_outside_writes --lib -- --ignored
```

An extractor-owned test process uses the production policy and attempts a TCP
connection, child execution, and an outside write. All must be denied; a private
state write must succeed. The parent checks the outside bytes and listener.

The session returns compiler evidence. `extract_cpp` and `extract_cpp_worktree`
project admitted bodies and calls into validated snapshots. Acceptance
explicitly executed the compiler, isolation, corpus and consumer gates.

## Snapshot identity contract

The graph schema has an optional `identity.cpp_semantic` block for the
normalized profile, captured inputs, complete pinned runtime, exact clangd
binary, and fixed semantic policy. Each digest is a separate lowercase SHA-256
component. Missing or invalid components, a foreign language, or a different
policy fail validation. The block is omitted from syntax and other-language
snapshots, preserving their bytes and identity hashes.

`Clangd` provenance is limited to verified callable bodies, same-file lexical
containment of recovered bodies, and direct calls between callable bodies.
A call's source span must lie inside its caller. These schema checks validate
producer claims; they do not run a compiler or establish runtime attestation.
The production session establishes the evidence. Canonical extraction also
requires every resolved call observation to match exactly one admitted edge.

## Canonical extraction configuration

Use `extract --language cpp --calls required --cpp-profile /absolute/config.json`.
Add `--dirty` to read working-tree bytes. Required C++ mode without a profile
fails; commit-based semantic diff and history are unsupported under the initial
policy. The configuration is a `CppExtractConfig` JSON object with these fields:

- `compilation_database`: absolute path to the prepared database.
- `query_lanes`: optional concurrent symbol-query count, from 1 to 8 (default 1).
  It controls scheduling and stays outside semantic identity. Responses retain
  source-request order; a failed query aborts the extraction.
- `translation_units`: nonempty list of exact repository-relative implementation
  paths, each present in the source inventory. Duplicates and headers fail.
- `roots`: source, optional build/sysroot, and named dependency roots.
- `inputs`: captured-input manifest.
- `runtime`: complete operator runtime manifest.

Unknown fields fail. The selected implementation paths must exactly match the
normalized database's C++ application implementation units. Missing selected
commands or unexpected implementation commands fail before launch. C and
external-dependency commands may contribute profile identity without becoming
queried application units. Header commands never supply an independently
admitted header context. The source root must match the selected repository.

Snapshots retain spelled structure and add only compiler-proven recovered
callables, their same-file lexical containment, and exact direct calls. The
call census distinguishes `Unsupported`, `Inactive`, and `Unprofiled`.
`Inactive` requires an explicit compiler region; an absent response cannot
establish it. Inventoried implementation files outside `translation_units`
remain spelled facts with `Unprofiled` calls. Header calls are `Unsupported`
under the initial context policy. A selected unit missing a compiler call
observation fails extraction. Neither classification shrinks the observed-site
coverage denominator.

The guarded synthetic canonical test requires the same operator runtime pin
and `CODEMAP_CPP_CLI` set to the absolute path of the freshly built `stele`
binary:

```sh
cargo test -p codemap canonical_extraction_matches_the_independent_labelled_calls --lib -- --ignored
```

It verifies the eight independently labelled exact edges, recovered callable
provenance, inactive/unprofiled/header classifications, and identical repeated
clean/working-tree/CLI snapshot bytes across one and four query lanes, and CLI rejection of incomplete
translation-unit selection without a partial output. These checks do not replace full corpus
acceptance.

## Positive LemmaSpec paths

`stele code positive-path --file snapshot.json --entry-node <exact-id>
--target-node <exact-id> --out path.stele` writes a self-contained typed
artifact. `--out` requires a new file and cannot overwrite an existing snapshot
through a hardlink or symlink. Run `stele spec walk path.stele --json` to evaluate it. Both
selectors require exact callable node IDs, so a shared symbol key never expands
the selection. The consumer validates the semantic C++ snapshot first and
retains snapshot identity and source provenance on definitions and calls.
Selected endpoints are explicitly reviewer-declared.

The artifact proves only a positive path over the supplied canonical call
facts. A failed expectation means this graph did not establish that path;
runtime reachability and whole-program completeness remain unknown. The
existing architecture and symbols absence checkers continue to refuse C++.
Legacy symbol-key selections for other languages are unchanged.

`LEMMASPEC_BIN` selects the installed engine for the ignored positive-path
integration test. Its synthetic transitive path detects dropped call facts
and rules, and a same-symbol-key decoy does not satisfy the exact selection.

For the actual guarded synthetic pipeline, set
`CODEMAP_CPP_CANONICAL_SNAPSHOT_OUT` to a new local JSON path when running the
canonical extraction test, then pass that path as
`CODEMAP_CPP_CANONICAL_SNAPSHOT` with `LEMMASPEC_BIN` when running
`guarded_cpp_fixture_projects_the_independent_oracle_calls -- --ignored`.
This evaluates every independently labelled positive pair and verifies that
removing canonical call facts removes the positive proofs.

### Linux runtime profiles

The C++ `runtime` field retains the original Darwin manifest JSON. On Linux,
use an explicit object with `platform: "linux"`, `runtime`, `kernel`, and
`limits`. The nested runtime is a `LinuxRuntimeManifest`; kernel pins are a
`LinuxKernelPins` value and limits are a complete `LinuxLspLimits` value. Pins
are prepared by the operator. Extraction does not discover or download them.

The selected native clangd and its ELF interpreter have separate `clangd` and
`loader` roles. Every loaded library and consumed runtime data file must also
be pinned. Anonymous execution is denied. Kernel pins name the independently
captured native vDSO and the architecture's vsyscall policy. All limits are
bounded by the shared transport's production maxima; smaller limits change
profile identity and may refuse a workload. A Darwin profile has no Linux
fallback.

The transport permits an idle tool while the parent verifies inputs. Each
pending request retains its absolute deadline, and each frame retains a deadline
once data arrives. The production maximum is 32,768 frames and 64 MiB of wire
data in each direction. These bounds participate in transport policy identity.
After collecting a Linux translation unit's evidence, Codemap closes its
document so clangd can release the AST and in-memory preamble.

Linux grants source reads only to the verified private input copies and overlay,
and to a separate read-only prepared compilation database. Original source and
SDK files receive no read permission. Runtime, kernel images, captured inputs,
and the retained private database are checked before and after queries. A
failed check returns no partial semantic result. Physical private paths do not
enter semantic identity; runtime, kernel, guard and transport policies do.

The guarded compiler can demonstrate that an unmanifested header is absent
through `__has_include`. The current syntax parser rejects that expression
shape for canonical extraction. The native controls verify both behaviors;
compiler success does not override the syntax admission gate.

The macOS guarded regression, both native Linux synthetic suites and the
original pinned application oracle have passed. Complete repeat and independent
checkout snapshots are byte-identical on each Linux host.
Existing canonical oracle thresholds are unchanged. JS/TS and Python Linux
acceptance and the combined platform release remain separate pending units.
