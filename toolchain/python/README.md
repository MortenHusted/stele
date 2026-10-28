# Python U1 source probe

This is an isolated parser and compiler-scope experiment, not a Python adapter
or a producer of canonical calls. It requires an explicitly selected CPython
3.14.7 interpreter outside the source checkout.

Run the tests with that interpreter's absolute path:

```text
<interpreter> -I -S -B toolchain/python/test_probe.py -v
```

The operator clears the child environment and runs the helper in a private
directory. Source files arrive as base64 bytes with SHA-256 pins. The helper
parses and compiles them for validation and obtains compiler symbol tables;
it never executes the resulting code or imports application modules. It does
not discover files, resolve dependencies, or install packages.

Requests and responses are limited to 32 MiB, with an 8 MiB per-file limit.
The parent must independently bound runtime and output, validate the original
regular-file inventory, and verify interpreter/helper content before and after
the probe. These tests use a timeout and file-backed output. Production runtime
attestation and admission remain U1/U3 work; this experiment does not establish
them.

Spans address original source bytes, including encoding cookies, UTF-8 BOMs,
and newline conventions. Sources whose encoding cannot round-trip exactly are
refused. Spellings preserve original identifiers even when the compiler
normalizes their names. Compiler-scope flags are observations about scope;
they do not establish an immutable callable identity.

Calls in lambdas and comprehensions have their own evaluation context and no
outer caller claim. Defaults and decorators are separately marked from
annotations, type aliases, and type parameters. Stub
definitions are declaration-only observations and cannot establish a source
implementation body.

The initial private lexical-binding experiment did not satisfy the strict
usefulness gate. The selected native policy uses an explicit compiled execution
profile; ordinary CPython bindings remain unproven. New corpus measurements
and snapshots stay local.

The public Black benchmark's operator archives, compiler commit and wheel
digests ship in `benchmarks/python/black-operator.json`.
`bin/prepare-black-operator` builds closed source-only runtime and compiler
inventories and pins an explicitly selected host cache family before
extraction. See `benchmarks/python/README.md` for that preparation flow.
The guarded runtime below establishes execution witnesses independently.

## Captured native binding

The owned parser suppresses `SyntaxWarning` diagnostics before the closed query.
Formatting those diagnostics would otherwise load warning support lazily or read
source filenames. Parsing, AST compilation and scope validation still run;
invalid syntax still fails. The warning policy and helper content are bound into
parser identity, and query imports, execution and filesystem access stay refused.

`python_native::NativeSession` owns interpreter/runtime attestation, the closed
mypy/mypyc 2.3.1 compiler bundle, and guarded compiler input reads. It supplies
source bytes through the captured filesystem, with application plugins and
site-package discovery disabled. Application modules are never imported or
executed. `native_bootstrap.py` is the owned entrypoint. Transport evidence
continues to carry `canonical_admission: false`; only `python_semantic` may
promote it after independent complete original-source matching. The superseded
standalone native probe has been removed.

Native inputs require UTF-8 without a BOM, including any encoding cookie.
Other declared encodings refuse even when their bytes also decode as UTF-8:
the parser and native compiler must interpret exactly the same source text.
The source-only parser retains its broader encoding support.

Native queries require `package_profile` with ordered repository-relative
`source_roots`, a boolean `namespace_packages` policy, and `external_stub_roots`.
The current native query refuses external stub roots; `.pyi` files supplied
under a declared source root remain declaration inputs. Roots cannot overlap,
and competing module names or module/package collisions refuse. Regular
packages require captured initializer inputs. Module names derive from the
declared roots while evidence retains original repository-relative paths.
Compiler search uses only those roots and the captured typeshed policy;
the working directory does not add an implicit package root. The owned
source-only workload control retains its fixed `.` namespace profile unless
the pinned request supplies an explicit `package_profile`.

`python_profile::NativeProfile` describes the captured native query. It binds
logical operator/runtime fingerprints, bootstrap and query-helper bytes,
required versions, execution policies, ordered package roots, source hashes,
and the supplied native unit/declaration selection. The file list determines
the query's connected directory inventory. Fixed compiler options are bound
by the helper and compiler content pins. Physical paths, PIDs, and image
addresses stay outside identity. Its fingerprint hashes compact JSON with
sorted object keys.

The guarded control attaches this descriptor only after its query postchecks
and verifies equal identities from original and copied operator inputs. The
descriptor itself does not prove execution, loaded images, or source commit/tree
identity, and cannot admit canonical facts.

Native responses retain every original parser call site in `observations`,
including declaration inputs and calls outside ordinary function bodies.
Each observation keeps the call/callee byte spans, spelling, caller, and
evaluation context. `eligible_candidate` sites correspond to native body
evidence; the response still carries `canonical_admission: false`.
`withheld` sites report `declaration_only_source`,
`unsupported_evaluation_context`, or `no_unique_supported_native_body`.
The last reason covers all remaining admission failures, including dynamic
callbacks, receivers, decorated/deferred bodies, external targets, and
unresolved source identity; it does not guess a target. Complete call spans
distinguish nested calls sharing a starting byte. Duplicate native evidence
for one source site refuses the query.

`native_binding.py` owns the shared IR classifier and captured compiler query.
BuildSource text comes from the captured source inventory, without writing source
files. Queries use the three explicit pinned typeshed roots and a supplied source
root; caller working directories and ambient library fallbacks do not participate.
Mypy's SQLite/incremental stores are disabled. The captured read scope covers
source parsing, typechecking and IR generation; its ledger includes source reads.

The initial profile is one mypyc compilation unit. Only actual direct native
`Call` IR operations with an implementation declaration object and exact
original source ownership qualify. Calling async/generator targets is withheld;
original async/generator callers may have direct calls to synchronous bodies.
Virtual dispatch, decorated targets, external stubs, setup expressions,
lambdas, and comprehensions remain outside this policy. Ordinary CPython
execution has no proven-call promotion from this evidence. Mypyc documents
[early binding](https://mypyc.readthedocs.io/en/stable/differences_from_python.html#early-binding)
and its [compilation unit boundary](https://mypyc.readthedocs.io/en/stable/compilation_units.html).

`native_oracle.json`, `native_observation_oracle.json`, and the later original-body
oracle retain the independent synthetic expectations frozen before admission.
The ignored real-tool suites in `python_native.rs` and `python_semantic.rs` use
explicit captured operator/runtime inputs and the owned APIs. They verify native
response contracts, original call/callee/caller/target bodies, unsupported
observations, repeat/relocation identity and the guarded execution boundary.
Ignored tests must be run explicitly before acceptance. The original standalone
launcher and its duplicate test are superseded by these guarded suites.

## Interpreter runtime guard control

The ignored Darwin arm64 test `isolated_python_images_and_os_guard` takes an
explicit `CODEMAP_PYTHON_RUNTIME_MANIFEST`. It captures the selected interpreter,
shared-cache family, inspector, and sandbox content pins, and binds loaded-image
reports to the owned process before and after parsing. Its owned fixture checks
that the OS guard denies network access, unapproved execution, and writes outside
the private state directory. Application source is parsed without execution.

This control covers executable and loaded-image identity. CPython standard
library source and bytecode imports, compiler input read closure, and canonical
profile integration remain pending. `-B` prevents bytecode writes; it does not
prevent reading existing bytecode. The control does not establish complete
runtime closure or enable Python snapshot admission.

## Captured operator inventory

The Unix `python_inputs::CapturedOperator` control captures binary bytes for an
explicit interpreter and an optional compiler bundle. Runtime-only captures
omit `OperatorRoots.compiler` and reject every `{compiler}` file or directory.
Native profiles require that compiler root. Logical paths, content pins, and
complete directory listings form a relocation-stable identity. Every listed
file needs a content pin; every listed directory needs its own inventory. Symlinks, special entries, malformed paths, incomplete
inventories, and roots overlapping application source are refused. Individual
files are limited to 128 MiB and the complete captured bundle to 512 MiB.

The initial layout requires `bin/python3.14` and `lib/python3.14`, with
`lib/python314.zip` and `lib/python3.14/site-packages` absent. When selected,
the compiler root contains only `mypy/`, `mypyc/`, and `dependencies/`. Any
runtime bytecode present is pinned along with source; the public recipe omits
generated caches. Physical installation paths do not enter the logical
fingerprint. The operator fingerprint hashes compact JSON of
`["python-operator-inputs/1", {"files": ..., "directories": ...}]` with that
struct-field order and sorted keys inside each inventory map. The control
retains immutable bytes and checks the original tree for content and inventory
drift.

The ignored test `captures_explicit_operator_bundle` takes
`CODEMAP_PYTHON_OPERATOR_MANIFEST` and its `..._SHA256` pin,
`CODEMAP_PYTHON_OPERATOR_RUNTIME`, `CODEMAP_PYTHON_OPERATOR_COMPILER`, and an
independently prepared `CODEMAP_PYTHON_OPERATOR_FINGERPRINT`. It verifies the
prepared bundle without importing application code. Production bootstrap,
compiler read-ledger enforcement, and canonical profile integration remain
pending; capturing an inventory alone does not establish execution closure.

The ignored `captures_explicit_parser_runtime` control uses the same manifest,
runtime and fingerprint pins with `CODEMAP_PYTHON_OPERATOR_COMPILER` absent.
Its private copy contains no compiler directory. This proves runtime-only
capture and materialization; an owned syntax parser launcher remains separate.

`CapturedOperator::materialize` creates an exclusive private copy from captured
bytes, with read-only files and file/directory modification times fixed to one
second after the Unix epoch. The interpreter remains executable. Original and
copied content identities are checked, and existing or overlapping destinations
refuse. Execution profiles must include `MATERIALIZATION_POLICY` as well as
content identity: CPython's bytecode selection consults source modification
times ([import loader metadata](https://docs.python.org/3.14/library/importlib.html#importlib.abc.SourceLoader.path_stats)).
The launcher must keep these operator copies outside its writable state directory.

The Darwin control `isolated_captured_compiler_bootstrap` uses the same explicit
operator configuration plus `CODEMAP_PYTHON_RUNTIME_MANIFEST`. It launches the
copied interpreter with `-I -S -B` under the shared OS guard, checks the selected
prefix/import paths and pinned module origins, and binds interpreter and compiler
extension images to the owned process before and after the control query. It
also checks that the guard denies new files under the copied runtime. The parent
delivers an owned native query after the ready/image-attestation handshake. The
control uses the shared engine, checks call evidence and the source-read ledger,
then rechecks helper/config/policy bytes and operator inventories. The complete
frozen/real-source guarded oracles and canonical profile integration are still
required for acceptance.

The control runs the frozen synthetic cases in fresh attested processes and
checks their independent original-byte evidence coordinates. Ready and result
files are published by atomic rename after their complete write.

`isolated_captured_native_workload` uses the same guarded transport for an
explicit source-only JSON request with `files` entries containing `path`,
`source_base64`, and `sha256`. It additionally requires
`CODEMAP_PYTHON_NATIVE_QUERY`, its `..._SHA256` pin, and an exclusive local
`CODEMAP_PYTHON_NATIVE_QUERY_OUTPUT` path. The test preserves source pins and
rechecks the request before writing its fully verified result. Independent
oracle scoring remains required; a successful transport is not acceptance.

`CapturedOperator::guard_policy` adds a Python read boundary: captured operator
and runtime files, their literal directories, and private state are readable;
undeclared files and process forks are denied. Ancestor directory handles are
allowed for the OS loader, without granting reads of unlisted child files.
State must be canonical and disjoint from operator/application roots and every
pinned runtime file, and
the selected runtime must pin the captured interpreter. Both captures are
rechecked before policy generation. Execution profiles must include
`GUARD_POLICY`. The bootstrap control also checks denied external reads, stat
queries, and directory scans. This boundary needs the captured compiler view
and import freeze to establish query closure.

## Captured compiler filesystem view

`captured_fs.py` supplies compiler observations from immutable captured bytes
and complete connected directory inventories under disjoint runtime, compiler,
and source roots. File reads retain binary content; stat results use fixed
metadata; directory listings and negative lookups use the inventory. Queries
outside the declared roots refuse. Refusals and observation-budget failures
are sticky, so a caught exception cannot produce a successful ledger. Logical
addresses and content/inventory digests keep observations stable across physical
relocation. Source files retain the 8 MiB limit; operator files allow 128 MiB,
with a 512 MiB aggregate and 100,000 input/observation limits.

`captured_fscache.py` binds this view to Mypy's existing filesystem cache,
including its case checks and content hashing. Fabricated package roots refuse.
The owned input-view controls run through
`isolated_captured_filesystem_matches_owned_expectations` with the same explicit
interpreter path and content pin as the parser test.

`compiler_read_scope.py` supplies captured stat, directory, and read observations
during a single compiler phase. An audit hook refuses raw reads, late imports,
code execution, dynamic loading, and process/network operations; exit verifies
the module graph and sticky input ledger, then restores the patched interfaces.
Mypy's exact disabled-cache sentinel is an explicit policy observation and
does not query the host device. The owned controls include descriptor queries,
saved-open bypasses,
module mutations, caught refusals, and restoration after compiler failure.
The OS boundary is still required: CPython does not audit every raw metadata
operation. These controls run through the same ignored input-view test.

Guarded native query integration, closed search-path policy, and complete profile
identity remain pending before canonical admission. These component controls
do not establish complete compiler read closure.

## Owned source-only parser

`python_parser::parse_sources` accepts a `ParserConfig` containing runtime-only
operator roots, a complete input manifest, and a pinned runtime manifest. It
requires macOS arm64 and exact CPython 3.14.7. It supplies normalized `.py`
source paths and original bytes as data; `.pyi` inputs and compiler roots are
refused. The API returns parser witnesses and a `python-parser-profile/1`
identity, without creating a snapshot or admitting call targets.

The source-only census uses `codemap-python-source-census/v2`. Definitions carry
explicit lexical parent indices, source-local occurrence ordinals, and ambiguity
when a name is defined repeatedly under the same parent, including across
definition kinds. Decorators and class bases retain original expression spans,
spellings, and byte hashes. Call and import witnesses carry lexical owners;
call evaluation contexts and callers remain separate observations. Rust checks
parent containment/order, qualified ownership, repeated-name accounting,
expression spans/hashes, and observation owners before exposing records. The
native probe retains its existing default census and admission policy.

The launcher materializes the captured runtime outside application source,
clears the child environment, disables site initialization and bytecode writes,
and applies the captured OS guard. `parser_bootstrap.py` warms captured standard
codec modules before accepting source. Windows-only codecs are excluded from
this Darwin profile. Query-time imports, executable code, filesystem access,
network, processes, and tracing are refused; the owned result marker is the
single permitted query write. AST compilation validates source without executing
it or evaluating application annotations.

The parent holds the process alive for before/after loaded-image checks, validates
source hashes and original byte/line spans, and rechecks runtime, operator,
helper, configuration and guard bytes before exposing output. Exit observation
retains the process-group leader until cleanup; every error path terminates the
owned group. Stdin writes are nonblocking and deadline-bound. Requests/results
are limited to 32 MiB, each file to 8 MiB, stderr/readiness to 1 MiB, and the
owned-process phase to 180 seconds. Result JSON retains the decoder's depth
limit of 128. Physical paths and process/image addresses do not enter identity.

The actual `python_parser` integration controls require the runtime-only operator
manifest, runtime root, manifest/fingerprint pins, and
`CODEMAP_PYTHON_RUNTIME_MANIFEST`; `CODEMAP_PYTHON_OPERATOR_COMPILER` must be
absent. They exercise original encodings/newlines, modern syntax, no application
execution, two fresh runtime copies, malformed-source refusal, and independently
frozen lexical/repeated-name records across unrelated line insertion. They must be
run explicitly; ignored tests do not count as passing verification.
