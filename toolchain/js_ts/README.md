# JS/TS compiler tooling

Portable syntax extraction and guarded required-call extraction share one JS/TS
family. The U1 probe returns compiler candidates; production Rust admission
checks exact source witnesses before emitting canonical calls. Full application
acceptance passed within the documented supported regime.

The probe uses Node 22.22.2 and TypeScript 5.9.3. `package-lock.json` pins the
compiler package; `manifest.json` pins the executable compiler content. The
operator supplies a separate Node content pin in `runtime.json`:

```json
{"node_sha256": "<SHA-256 of the explicit Node executable>"}
```

Prepare an operator directory outside the analyzed source. Copy `package.json`,
`package-lock.json`, `probe.mjs`, `collect_candidates.mjs` and `resolve.mjs` into it, then install with `npm ci
--ignore-scripts --no-audit --no-fund` using the pinned Node. Record its executable
digest in `runtime.json`. Installation is preparation, never extraction.

Run the actual compiler gates with explicit paths:

```sh
CODEMAP_JS_TS_PROBE_NODE=/absolute/node \
CODEMAP_JS_TS_PROBE_BUNDLE=/absolute/operator-bundle \
cargo test -p codemap --test js_ts_probe -- --ignored
```

The Rust harness checks Node, helper, compiler and lockfile content before
launch, clears the child environment, disables addons, bounds execution and
response size, and rechecks tool pins afterward. The helper reads an explicit
JSON request with `root` and a relative `project`; `report_files` optionally
limits reported sites without narrowing the program-wide write analysis.

The compiler host reads source and configuration as data, refuses outside-root
content and aliases, disables emit, fingerprints consumed bytes and checks for
drift. The compiler API does not load the target's language-service plugins;
the executable plugin trap verifies this. A parsed target file that throws on
execution verifies that application source is not imported. These controls do
not replace the production runtime and input-isolation gates in U3.

The initial candidate policy requires one concrete function body or a `const`
function/arrow initializer, an exact enclosing body, a non-`any` callee and a
known signature. It resolves static aliases, rejects relevant assignment,
destructuring, update, loop writes and declaration initializers, and conservatively rejects files affected
by direct eval or `with`. Unproven script globals, receiver calls, computed
properties, declaration-only targets, default arguments and class initializers
remain observations. Namespace-derived bindings are withheld rather than
assuming their properties are immutable. Nested project configurations resolve
relative paths from their own directory. CommonJS lexical calls are probed; inter-module CommonJS
resolution remains deferred. Constructor promotion is also deferred.

Overload signatures select their one implementation body. Constructors and
tagged templates remain in the invocation census; JSX rendering and callback
registration do not manufacture call edges. UTF-16 compiler offsets are mapped
back to valid original UTF-8 bytes, including astral characters and Unicode
identifiers. Invalid UTF-8 fails explicitly.

Configuration, global, syntax and module-resolution errors invalidate the
program. Other semantic diagnostics invalidate affected call or target ranges;
the independent oracle includes unrelated errors and error-recovery decoys.
Fixtures use distinct filenames for `.js`, `.jsx`, `.mjs`, `.cjs`, `.ts`, `.tsx`,
`.mts` and `.cts`, because compiler extension precedence can otherwise omit a
same-basename implementation.

The frozen public application oracle, source anchors, input pins and
preparation command ship under `benchmarks/js_ts`. New measurements and
reports stay local. The U1 decision
accepts this compiler approach and tool versions for production implementation.
Production isolation, snapshot admission, application and consumer acceptance
passed within the documented supported regime.
No thresholds or known-target denominators are relaxed.

## Portable source structure

`stele code extract --language js-ts` inventories the eight source-family extensions
and parses them without Node or application dependencies. JS and JSX use
JavaScript grammar 0.25.0; TS and TSX use TypeScript/TSX grammar 0.23.2, with
Tree-sitter runtime 0.25. Syntax emits definitions, lexical containment, and
spelled static import/re-export references; it emits no canonical calls.
Reference names retain the entire original statement so type-only imports,
individual type specifiers, aliases and exports remain distinguishable.
CommonJS `require` is an observation until its binding is proven.

The `spelled/js_ts/1` inventory requires regular source files; linked source
files and linked working-tree ancestors are refused so external bytes cannot
escape the tree identity. It excludes dependency directories named
`node_modules` or `.yarn`. Compiler output is excluded only by explicit
repository-relative directory declarations in a tracked `.codemap-js-ts.json`:

```json
{"exclude_directories": ["output", "packages/widget/generated"]}
```

Paths must be normalized relative directories without `.` or `..`; unknown
configuration fields are rejected. Source directories called `dist` or `build`
remain eligible unless declared. Commit/history reads use that revision's
policy; dirty extraction uses a policy that participates in the working-tree
identity. An ignored, untracked policy cannot change facts. Policy changes
participate in source-history selection.

Overload and declaration-only signatures are observations, while their types
remain structural nodes. Executable bodies get one definition; anonymous
functions use lexical containment and source-byte identities. JSX rendering,
constructor calls, dynamic imports and calls without a body caller are counted
without asserting framework execution. Required semantics uses the explicit
prepared project below. Semantic history and measures remain unavailable.

The pinned JavaScript grammar reports a parse error for a newline between
`export` and `default` on an anonymous function, even though Node accepts that
formatting. Its recovered function has an anonymous source identity; the
adapter does not reconstruct a default binding from text. The regression
retains the explicit parse-error signal. Comments between those tokens and
transparent callable expression wrappers preserve AST-proven binding names.

## Guarded runtime prerequisite

The operator Node runtime uses the same shared pin and Darwin image checks as
C++, with a distinct `node` executable role and `js-ts-runtime-pins/1` identity
domain. Initial guarded semantics targets Darwin arm64. The explicitly executed
live control loads the pinned compiler, parses source, verifies equal image
proofs before/after processing, and checks denied network, unapproved execution
and outside writes while permitting private scratch writes.

This verified prerequisite
establishes the runtime controls reused by the production helper and
canonical admission described below.

The probe records configuration/compiler host exceptions independently of
TypeScript diagnostics. A swallowed read, alias, budget or emit failure refuses
the entire result, including failures in unused source. Ordinary
unsupported-call observations retain their existing policy.

The captured input prerequisite
uses expected file digests and complete consulted-directory listings. It exposes
immutable text and logical paths for the virtual compiler filesystem;
metadata for an unpinned file grants no content read. Source and compiler-library
roots are disjoint, and original content, aliases and directory membership are
verified again after processing, including a full-scan metadata check. The
production helper and required semantic CLI reuse this contract.

## Required project extraction

Guarded semantics supports macOS arm64 and native Linux x86_64/aarch64 with
the pinned Node/compiler bundle above. Extraction reads a prepared environment; it never installs tools
or learns missing input permissions.

```sh
stele code extract --path /absolute/repository --language js-ts --calls required \
  --js-ts-project packages/app/tsconfig.json \
  --js-ts-environment /absolute/operator/environment.json \
  --out /absolute/output/snapshot.json
```

The environment is a strict `JsTsExtractConfig` JSON object with `roots`, `inputs`,
`runtime` and `bundle`. `roots.source` is the canonical selected repository;
`roots.typescript` names its disjoint operator compiler-library directory.
`inputs` supplies logical file SHA-256 pins and complete consulted directory
inventories, including explicit in-root aliases. Every inventoried application
source must be captured, including sources outside the selected compiler project.
`runtime` supplies the full Node executable, tooling and Darwin cache-family
manifest used by the shared runtime verifier. `bundle` is the canonical operator
directory containing the exact helpers, compiler and canonical lock.

The selected config is a normalized repository-relative path and belongs to the
committed tree in clean mode. `--dirty` uses captured working-tree content and
marks a changed tree dirty. Compiler source bytes must equal snapshot source
bytes, and inputs and repository state are rechecked before publication.

Canonical calls require exact parser and compiler call/callee/caller/target-body
ranges, one immutable lexical or checked static import binding, and a valid
implementation or overload signature. Rust independently checks binding scopes,
writes, aliases, dynamic evaluation, diagnostics and declaration-only targets.
Parsed final tokens establish executable ranges when Tree-sitter attaches a
trailing extra comment. Constructors, receiver/computed dispatch, cross-file
CommonJS, top-level execution, framework dispatch and unproved bindings remain
explicit observations. Out-of-project sites are `Unprofiled`.

The optional `js_ts_semantic` identity binds project, input closure, runtime,
compiler, helper bundle, complete evidence and policy. Validated call edges use
`TypeScript` provenance; the validator requires one exact resolved census site
for each edge and callable endpoints. Positive-path projection accepts exact
reviewer-selected nodes. Absence claims remain refused, as do semantic history
and measures. The independent application and consumer checks passed; Python
implementation follows the accepted adapter under its own plan.

## Independently labelled benchmark

`bin/js-ts-benchmark` extracts and validates a required-call snapshot, then scores
separately pinned original labels and source-only parser anchors. Freeze both
inputs before querying canonical calls. Keep application source, labels, pins,
snapshots and reports outside versioned files.

```sh
bin/js-ts-benchmark --repo /absolute/repository --project tsconfig.json \
  --environment /absolute/operator/environment.json --stele /absolute/stele \
  --oracle /absolute/local/oracle.json --oracle-sha256 ORIGINAL_LABEL_DIGEST \
  --anchors /absolute/local/anchors.json --anchors-sha256 SOURCE_ANCHOR_DIGEST \
  --out-dir /absolute/local/new-benchmark-directory
```

The command refuses output inside the analyzed Git root and verifies the
extractor executable, prepared environment and validated snapshot through the
run. `--snapshot` can score an existing snapshot instead of extracting one.
The scorer requires distinct exact caller/target bodies, keeps known unsupported
targets in recall denominators, and rejects wrong endpoints, non-call decoys,
duplicate edges and unanchored call spans. Unknown and external labels remain
separate. Its report records gate status and source pins; it makes no application
deployment or graph-completeness claim.

## Completion limits and progress

The Linux helper session has a hard 400-second limit; Darwin retains 180 seconds.
The Linux runtime identity includes its deadline, launch flags, kernel policy
and output bounds. Timeout still refuses extraction and terminates the owned
helper group. Changing the time budget does not relax source admission or the
proven-call contract.

`extract --progress` prints a start message, ten-second elapsed-time updates and
successful completion on stderr, leaving snapshot JSON unchanged. It reports
activity rather than an estimated percentage or remaining time. Terminal output
uses a separate nonblocking handle, so a stalled terminal can drop updates
without holding up extraction. Regular log files retain updates; pipes and
unknown sinks suppress them.

## Native Linux runtime profile

The environment's `runtime` accepts an explicit Linux object with `platform`
set to `linux`, a pinned ELF `runtime` manifest and independent `kernel` pins.
It requires one selected `node`, its authenticated ELF `loader`, the complete
runtime-library closure, and a data pin named `openssl_empty` containing zero
bytes. Anonymous execution must be denied. Runtime files must remain outside
the analyzed source. Darwin's existing manifest shape is unchanged.

Missing kernel capabilities,
wrong architecture, missing/changed files and unknown images fail closed;
a successfully installed binary does not imply an accepted runtime profile.
