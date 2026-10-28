# Public Excalidraw preparation

`excalidraw.json` pins the public commit, project, tool versions, upstream MIT
license and frozen inputs. The oracle and source anchors are the original
independently reviewed files, without relabelling against new compiler output.
They include source names, spellings and fragments from Excalidraw under its
MIT license; `LICENSE.excalidraw` preserves that notice. Complete source files
and encoded source payloads are not included.

`excalidraw-inputs.json` preserves logical file hashes, consulted directory
membership and explicit in-root symlink targets from the accepted preparation.
Listed names grant no content read unless a file hash is separately pinned.
The file contains no physical operator paths, runtime pins or execution
witnesses. Preparation never expands its input permissions to make a run pass.

Gitleaks 8.30.1 flagged 16 declared file SHA-256 values beside names such as
`api.d.ts` and `sentry.ts`; each match was verified as a frozen file hash.
The layout separates filenames and literal hashes to avoid that false
credential context while preserving every value, with unchanged scan rules
and no ignore entries.

Use Node 22.22.2 and a clean public checkout at the commit in `excalidraw.json`,
with origin `https://github.com/excalidraw/excalidraw.git`. Inside that checkout,
prepare dependencies using the repository's `packageManager` pin:

```sh
corepack yarn --version  # must report 1.22.22
corepack yarn install --frozen-lockfile --ignore-scripts --non-interactive
```

Keep dependency symlinks intact. Install scripts are disabled; application
modules are not executed. The pinned lockfile must remain unchanged. The
frozen input verifier rejects different dependency bytes, directory membership
or aliases instead of silently capturing a replacement inventory.

Prepare a separate operator bundle from this Stele checkout:

```sh
mkdir /local/compiler
cp toolchain/js_ts/package.json toolchain/js_ts/package-lock.json toolchain/js_ts/manifest.json toolchain/js_ts/probe.mjs toolchain/js_ts/collect_candidates.mjs toolchain/js_ts/resolve.mjs /local/compiler/
(cd /local/compiler && npm ci --ignore-scripts --no-audit --no-fund)
```

Use the same pinned Node for that installation. Supply an explicit operator
runtime profile as described in `toolchain/js_ts/README.md`, then run:

```sh
bin/prepare-js-ts --repo /local/excalidraw --bundle /local/compiler --runtime /local/runtime.json --out /local/new-inputs
```

The command verifies the clean source pin, anchor hashes, canonical tool
files, all frozen file hashes and directory/alias metadata. It writes
`environment.json` and `result.json` to a new directory outside Git. The
receipt records input, bundle, supplied runtime-profile and environment
digests. Runtime pins are passed through unchanged: this command does not
validate them as execution witnesses or establish semantic acceptance.

Use that environment with `bin/js-ts-benchmark`, the shipped oracle and
anchors, and their SHA-256 pins from `excalidraw.json`. The existing scorer
keeps its supported and overall known-target denominators, recall thresholds,
zero false positives, JSX decoys and source-anchored caller/target checks.
Run again from an independent prepared checkout to check snapshot equality.
Keep snapshots, logs and measurements outside Git.

Fresh public checkout and dependency preparation have passed on macOS arm64.
The guarded benchmark passed the frozen oracle from two independently
prepared checkouts and produced byte-identical validated snapshots.
Other dependency trees and runtime profiles require their own actual
verification; this preparation does not assert that a platform passed.
