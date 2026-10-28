# Reproducible public C++ gate

`double-conversion.json` pins Google's BSD-licensed double-conversion release,
its source inventory, the original synthetic oracle and the independently
reviewed application labels. `LICENSE.double-conversion` retains its notice.
The labels and parser-anchor digest were frozen from source before querying
clangd. The command never generates labels from its output.

This is a selected-source gate. The five translation units in the descriptor
were selected because they parse cleanly with the pinned Tree-sitter grammar.
Other library units contain unsupported macro/preprocessor shapes. This profile
does not cover the string-to-double half of the library or establish whole-library
precision or recall.

The labelled families cover every RawBigit call in bignum.cc, every spelled
Bignum member call in bignum-dtoa.cc (including local aliases and static calls),
the FastDtoa/FastFixedDtoa/BignumDtoa calls in double-to-string.cc, the cache-power
calls in fast-dtoa.cc and the UNREACHABLE macro calls in the selected units.
Const and mutable overload labels follow the receiver type and enclosing
method's const qualification. Header-body targets, unprofiled cache-power
bodies and source calls missing parser anchors remain known unsupported sites
in the overall denominator. External macro targets are separate. A missing
anchor never earns recall credit; an incorrect edge at that offset fails
precision.

The unchanged gates require zero false positives, at least 90% supported-shape
recall and 50% overall known-target recall, at least 60 labelled sites and ten
cross-module queries. The complete snapshots must be byte-identical across two
independent checkouts and query lanes 1 and 4. The oracle is a declared sample,
not a census of every call in the project.

## Preparation

This recipe has been exercised on macOS arm64 with clangd 23.1.4. The descriptor
fixes the C++11 target profile. Linux extraction exists, but this public-corpus
preparation recipe makes no Linux acceptance claim. Install Ruby with the TTY
gems (`tty-option`, `tty-prompt`, `tty-command`), the repository's Rust toolchain,
LemmaSpec, LLVM, and an explicit Apple SDK before running. Do not execute the
application's CMake, build commands, tests or install scripts.

Clone the public source and detach at `commit` from the descriptor. Set
`remote.origin.url` to its exact `repository` URL. Keep the checkout clean.
Use the Homebrew LLVM 23.1.4 `arm64_golden_gate` bottle at the standard
`/opt/homebrew` prefix, with z3 5.3.2 and zstd 1.5.7_1. The descriptor records
the immutable public formula URL, bottle URL/digest and installed image digests
for clangd, LLVM, z3 and zstd. The bottle digest identifies the public source
artifact; Homebrew relocates and re-signs the LLVM libraries when pouring it.
The image pins match the installed bytes at `/opt/homebrew`; another prefix or
manual archive extraction fails those pins by design. Preparation refuses a different
compiler distribution. Select
the Apple macOS 27.0 SDK (`SDKSettings.json`: `Version` 27.0, `CanonicalName`
macosx27.0); another SDK requires a separately reviewed profile. The consumer
is the public LemmaSpec v0.4.2 arm64 release, whose commit, archive URL/digest,
binary digest and version output are pinned and checked. Install those exact
artifacts before running; the helpers download nothing.

Choose their local paths and complete host dyld cache family explicitly:

```sh
bin/prepare-cpp-benchmark \
  --repo /local/double-conversion \
  --clangd /local/llvm/bin/clangd \
  --sdk /local/MacOSX.sdk \
  --resource /local/llvm/lib/clang/23 \
  --cxx /local/llvm/include/c++/v1 \
  --cache-directory /System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld \
  --cache-prefix dyld_shared_cache_arm64e \
  --out /local/new-cpp-profile
```

The helper inspects executable metadata using system `otool`, pins the actual
clangd and recursive non-system library closure, the standard guard/inspectors
and every member of the chosen cache family. It hashes tracked source files,
SDK, resource and C++ header inputs; aliases must remain within declared roots.
It writes an inert compilation database, runtime manifest and source manifest.
It runs no inspected executable and no application command. These generated
files contain host paths and are local preparation data, not native evidence.
The guarded adapter verifies the pins and loaded images before admitting facts.
An unbound runtime or unsupported host fails without a fallback.

## Acceptance

From the Stele checkout, run one command using that prepared profile. Select the
Cargo executable explicitly (preserve rustup's `cargo` symlink name); prepare its
cache beforehand if using `CARGO_NET_OFFLINE=true`.

```sh
bin/cpp-public-benchmark \
  --repo /local/double-conversion \
  --profile /local/new-cpp-profile/profile.json \
  --cargo /local/bin/cargo \
  --lemmaspec /local/bin/lemmaspec \
  --out /local/new-cpp-results
```

The command builds the release producer, regenerates parser anchors and checks
the frozen digest before compiler queries. It requires exact execution witnesses
for the original eight-call fixture, original-input drift refusal, owned-process
exit refusal, network/child-execution/outside-write guard, canonical extraction
and positive LemmaSpec consumer (including dropped-fact refusal). It clones the
pinned application twice without local object sharing, prepares equivalent
source-root bindings, extracts and scores both lane settings in both clones,
validates every snapshot, checks source cleanliness and compares complete bytes.
Any failure stops before a passing result is written. New output directories
must be outside Git; existing files and aliases are refused.

Logs, snapshots, generated manifests, measurements and `result.json` remain
local. Preparation success alone is not acceptance. The full command and
independent reviewer reproduction are the evidence gates; the original fixture
and application oracle serve different scopes.
