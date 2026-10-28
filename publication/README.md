# Export review

The publication plan requires separate automated checks and human provenance
review. `bin/export-review` produces the concrete private inventory to review;
it never marks a file approved. Publication proposals stay planned until the
owner names and accepts them.

## Prepare the scanner

`toolchain/security/gitleaks.json` pins the public Gitleaks v8.30.1 release
archives and their executable digests for macOS arm64 and Linux x64/arm64.
Download the asset matching the host from its exact URL. The command verifies
the archive and binary digests before executing the scanner; it downloads nothing and runs no
application code. The selected archive is unpacked only into the new private
report directory, with its pinned bundled MIT notice beside the executable.

The configuration extends that release's built-in rules without custom
allowlists. Its built-in global path allowlist and per-rule allowlists still
apply. The global path patterns are pinned in `gitleaks-default-paths.json`;
matching inventory paths are flagged for manual secret review. Such paths
include lockfiles, vendored dependencies, images, documents and `.bin`/`.exe`
payloads. A clean scan does not establish that those contents were checked. Its digest is pinned. Each invocation supplies the configuration
explicitly, clears caller configuration variables, uses no baseline or ignore
file, disables inline `gitleaks:allow`, redacts findings and requires the exit
status to agree with the JSON report. The binary and configuration are checked
again afterward. A scanner error cannot produce a clean result.

## Review a concrete export

Use a clean, complete Git export with the intended refs. Pass the existing
private identifier deny-list and every unpacked artifact intended for release.
Both the list and output directory must be outside Git and scanned inputs.
Compressed archives must be unpacked separately before identifier scanning;
review their exact contents and bind archive digests to the payload inventory.
Omitting artifact arguments is explicitly a source-only review.

```sh
bin/export-review \
  --repo /local/curated-export \
  --deny-list /private/identifier-deny-list.txt \
  --scanner-archive /local/gitleaks_8.30.1_darwin_arm64.tar.gz \
  --out /private/new-export-review \
  /local/unpacked-release
```

`inventory.json` lists every committed path with its Git mode and SHA-256,
the source/disclosure group and required notice paths. Every supplied artifact
file has its own mode and content hash. Source links must resolve to tracked
files; artifact links must remain inside their payload. Submodules, special
files, missing notices, dirty/shallow inputs and changed content are refused.
Captured scanner inputs are private regular data files: link target spellings
are scanned as blob bytes, and their target files are independently included.
No application or artifact executable is launched.

The pinned scanner checks the captured tree and artifact payloads beneath a
neutral `content/` parent, preventing root ignore-file auto-loading. History is
scanned from a private, independent bare mirror with the exact captured refs.
The Git environment is scrubbed; hooks, fsmonitor, global/system configuration,
text conversion and external diff are disabled, and attributes are bound to an
empty tree in the mirror's object format. The identifier check additionally inspects all local Git
objects, including unreachable objects. Input inventories, including every ref name and object ID, must still match
when scanning ends. `result.json` binds the inventory, scanner and configuration
digests. Findings and locations remain in private reports; stdout contains only
the clean/findings status and scope. The report root and captured directories are mode 0700; reports and captured
files are mode 0600, enforced after scanner writes. The private Git mirror stays
inside that root.

Exit 0 means these bounded scans found no matches; 1 means findings; 2 means
incomplete inputs or execution. Neither 0 nor an inventory hash establishes
that provenance, licenses or the publication destination have been approved.
The identifier result covers the supplied tokens, and Gitleaks covers its
pinned rules; neither establishes absence of all possible private material.

## Human provenance and artifact notices

`publication/provenance.json` identifies disclosed source families and notices,
including Lemmalog portions in LemmaSpec, the topology viewer/D3, the Rust tags
query and benchmark metadata. The three dependency notice supplements are bound to exact crate checksums and
upstream commits in `dependency-notices.json`; their content hashes are checked.
The default project-maintained group is **not**
a claim of independent authorship. Each file remains pending owner review;
family membership is not proof that every file is copied from that upstream.
Review ancestry, modifications, applicable grants and notices for each entry.
The inventory itself stays local until reviewed for publication.

Artifact entries remain pending build and notice provenance: a filename, version
string or clean scan cannot establish which source produced an executable.
Bind each payload to its reviewed build inputs and retained dependency notices.
The release packager includes project and dependency notices as described
below; its exact payload inventory still requires provenance review.
Operative terms, inherited rights, approvals and repository cutover remain
separate owner decisions.

Run the synthetic boundary and native canary controls with a prepared archive:

```sh
STELE_GITLEAKS_ARCHIVE=/local/pinned-host-archive.tar.gz \
  ruby bin/export-review.d/test/review_test.rb
```

Without the environment variable, the native scanner tests are explicitly
skipped. Hosted CI prepares the pinned Linux archive and runs them. The canaries
are generated synthetic strings, not observations from any private repository.

## Release payloads

`bin/package-release` retains the legacy `scripts/release-package.sh` arguments:

```sh
bin/package-release --version 0.5.5 --target aarch64-apple-darwin \
  --dist-dir /local/releases
```

Prepare the declared Rust toolchain, cached public crate archives and Ruby TTY
runtime first. Linux retains the existing Cross/container preflight and glibc
floor; the remapping flags are forwarded through `Cross.toml`. The owned source
must be clean and committed. Existing archives/checksums are refused.

The command builds the binary with stable remapping of builder-home, source,
Cargo-cache, compiler-root and host/Cross build paths, then checks the binary
and decompressed archive for the measured lexical and physical prefixes. Registry
and Rust library source paths must also sit under `/stele/` logical roots.
It refuses an archive if either check fails. This is a
bounded check of those prefixes, not a general privacy or secret proof. Native
binaries must report the expected version; cross-host version execution remains
explicitly unavailable except the existing macOS Rosetta case.

`cargo metadata --filter-platform` supplies the normal dependency closure from
Stele for the selected target. This includes proc-macro packages and is not a
minimal linker map. Each cached public `.crate` must match its Cargo.lock SHA-256.
The collector retains legal files recursively, including nested Unicode notices.
A nested third-party notice cannot substitute for a missing primary grant: a
primary root/declared notice or a version/checksum/VCS-bound supplement is required.
The Rust standard-library copyright inventory and license texts are retained
from the configured toolchain too.
The inventory's compiler entry identifies that notice toolchain. Cross can use
a different host build of the same Rust version; its target-toolchain source
paths may use the `/stele/builder/` home map.

Each archive contains a versioned `stele-X-T/` directory holding `stele`, project `LICENSE`, `NOTICE`,
`THIRD_PARTY_NOTICES.md`, `licenses/`, `THIRD_PARTY_INVENTORY.json` and `BUILD.json`.
Headers are sorted and use uid/gid 0, empty user/group names, normalized modes,
the commit timestamp and gzip mtime 0, with no xattrs or `.` entry.
The receipt records the source commit, target and hashes without embedding
builder paths. It keeps provenance review pending. Notices describe their own
components and preserve inherited grants; their presence does not approve the
operative terms or the publication destination. Review and scan the final
unpacked payload, including paths excluded by the secret scanner's defaults.

## Curated root tooling

`bin/prepare-public-export` builds a local candidate from committed blob bytes.
It never clones source history, uploads files or changes repository visibility.
The source must be pinned by its full commit ID. The include list and the
license/notice inputs are repository-relative paths tracked at that commit;
dirty and untracked working files are never substituted for them.

The tracked include list is JSON with one key, `files`, containing an explicit,
nonempty array of file paths. There are no globs or implicit directory copies:

```json
{"files":["README.md","THIRD_PARTY_NOTICES.md","src/example.rs"]}
```

`--license` and `--notice` map their tracked regular-file inputs to `LICENSE`
and `NOTICE` in the candidate. A conflicting include-list entry is refused.
Include all other required component notices explicitly; export-review checks
their presence. Git metadata, traversal, submodules, untracked inputs and links
to anything except a selected regular file are refused. Executable file modes
and safe source links are retained. Git validates all selected names with its
HFS/NTFS protections before payload files are written. No exported program or
hook is executed. The resulting tree must exactly match the selected paths,
modes and blob IDs before its root commit is created. Valid UTF-8 source names
are retained in inventories and identifier findings; unrepresentable source
names refuse review.

```sh
bin/prepare-public-export --repo /local/source --source-commit FULL_COMMIT_ID \
  --include-list publication/files.json --license terms/LICENSE \
  --notice terms/NOTICE --deny-list /private/identifiers.txt \
  --scanner-archive /local/gitleaks.tar.gz --out /private/new-export \
  /local/unpacked-release
```

The output must be new and outside Git and scanned inputs. Its private directory
contains `repo/`, a repository with just `refs/heads/main` and one root commit;
`mapping.json`, the source/list/file hashes and resulting tree/commit IDs;
`review/`, the export-review evidence; and `result.json`. The mapping and scan
reports stay outside the candidate Git repository. The commit has a fixed
export identity/message and the pinned source commit's timestamp, so the same
selection and blob bytes repeat independently of checkout paths and Git login.
Source replacement refs, global configuration, hooks, templates, commit signing
and inherited Git environment do not supply the export's identity or content.

The command always runs export-review over the new repository and the explicitly
supplied unpacked artifacts. Exit 0 means its scans are clean, 1 means findings,
and 2 means incomplete. A failed scan leaves the local candidate and evidence
for inspection. Every result keeps publication/provenance approval pending;
manual review of scanner-filtered paths is still required.

Synthetic terms in tests establish the mechanism only. The actual export still
needs review of the operative terms and rights, an intentionally reviewed include list,
public-facing AGENTS/README/plan text, regenerated deliberate-break and passing
evidence at the new source identity, and Morten's approval of the concrete
destination and ref inventory. Do not publish a candidate merely because its
mechanical scan passes.

## Evidence at the new root

The private run ledger must be omitted from the candidate's tracked include
list. Copying it leaves runs whose bases cannot be resolved in the new history.
Approvals bind statement/example content digests and can carry over unchanged;
changing an approved statement still requires the owner's named approval.

### Single-commit evidence

For a publication history containing exactly one commit, add `--single-commit`
to the replay invocation below. This mode records every actual run with an
explicit source baseline using `stele intent record --source-baseline SOURCE`,
where `SOURCE` is the clean root's `sha256:` source identity. The recorder
verifies that identity against HEAD before running, captures HEAD for the
observed patch, and writes `base: "source:sha256:..."` during recording.
Previously recorded commit bases are never converted.

The controller checks every observed failure patch against the baseline and
requires applying it to reproduce the recorded failing source identity. It
then creates a new parentless commit containing the source and ledger. Source
identity excludes the ledgers, so the final commit independently resolves the
recorded source baseline without referring to its own commit hash. The
temporary recording commit is removed from refs, reflogs and object storage;
the controller refuses remaining unreachable objects.

The frozen recorder and its build receipt remain private preparation evidence:
that receipt names the temporary recording commit. A second native release
package is built after the final public commit exists, in `public-release/`
with unpacked contents in `public-payload/`. Only that package names the final
public commit as its build source. Both payloads are scanned, but the temporary
recorder package is not a public release. The private v2 replay receipt
distinguishes recorder and release provenance. An independent replay must
reproduce the ledger and final commit before accepting the candidate.

Without `--single-commit`, the existing commit-baseline procedure below
retains its two-commit result and v1 receipt.

The tracked `publication/intent-mutations.json` declares intended test cases,
not observations. Each case names an existing example/channel and a text patch
against owned Rust implementation code. It contains no inherited run bases,
source identities, digests or old diff-index metadata. A patch applying is not
proof that the example detects it: a real test failure is required.
The current fixture covers 18 examples with 16 distinct breaks: the three impact
examples share one mutation. Each example still needs its own recorded failure
and clean-source pass; shared source changes do not substitute for a test run.

With a clean single-root candidate `R`, no inherited `intent/runs.stele`, the
declared Rust toolchain, public crate archives and Ruby runtime prepared:

```sh
bin/replay-intent-evidence --repo /local/fresh-candidate \
  --out /private/new-replay --deny-list /private/identifiers.txt \
  --scanner-archive /local/gitleaks.tar.gz /local/additional-unpacked-release
```

This command executes owned Cargo tests and temporarily mutates their declared
implementation files. Use a fresh candidate, not a development checkout. It
requires every patch to apply before building, refuses test-file or metadata
edits, and restores each selected source file after its recording attempt.
An incomplete attempt retains its local evidence; recreate the candidate rather
than overwriting or silently extending that attempt.

The controller invokes `R`'s `bin/package-release` to build a remapped native
recorder in the external receipt directory. Linux uses the existing Cross
prerequisites and glibc floor. Its bundle pins the public source commit, binary,
notices and compiler. The controller freezes those binary bytes before any
mutations, so Cargo cannot replace the running recorder with a broken build.
All builds and tests use the declared Rust toolchain and prepared offline cache;
the mutation-test target directory is separate from the recorder build.

Every intended case must record `fail`, not `error`, a compile failure or `skip`.
After source restoration, all examples must record `pass` at `R`'s clean source
identity. The controller checks each observed diff with `git apply --check`,
checks fresh diff-index metadata against `R`'s public blobs, and requires every
recorded base to be exactly `R`. Observed runs are not rewritten to resemble the
intended fixtures.

The resulting `E` has only `R` as parent and changes only `intent/runs.stele`;
every path in `R` remains at `E`. Both commits use the fixed export identity and
the root's timestamp. The ledger is outside the source identity, so that identity
must stay equal. Do not amend the ledger into `R`: that would change its commit
ID and orphan the recorded bases. The actual curated evidence history therefore
has two commits and one root.

Acceptance requires `intent report` to match `E` with every statement verified,
`plan check` to exit 0, and a fresh export-review over both commits and the
rebuilt recorder payload plus supplied artifacts. The root-to-child path check
keeps history paths covered by the manual scanner-path inventory too. The private
receipt records the recorder SHA-256/toolchain, mutation and ledger hashes,
root/child IDs, source identity and outcomes. Exit 0 requires all those gates
and clean scans; findings exit 1, incomplete execution exits 2. Publication and
provenance approval remain pending.
