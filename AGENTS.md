# AGENTS.md — Stele

Stele combines `codemap`, the extractor in this workspace, with LemmaSpec.
Spec files take the `.stele` extension, and `stele` is the one command for
both.

## Purpose

`codemap` is a standalone, deterministic, source-addressed extractor for code
architecture. Its job is to answer structural questions about a pinned source
tree: what files, symbols, modules, imports, implementations, and **proven
call edges** exist at a given commit.

It is the canonical producer for this architecture data. It has no runtime
dependency on Graphiti, Rails, authentication, remote graph storage, or an LLM.
Those systems may consume validated output; they do not define this project's
canonical facts and must not maintain a divergent copy of Codemap source.

## Initial scope

- **One adapter at a time.** Rust, Ruby, PHP, Go, C++, JS/TS and Python are
  accepted within their documented profiles. Another language requires its
  own plan, public corpus, labelled oracle and acceptance decision.
- Read clean, committed worktrees by default. `stele code extract --dirty` is
  the one intentional exception: it reads the working tree as it is, names
  the tree it read by its own content hash, marks the snapshot `dirty` when
  that tree differs from `HEAD`, and stays byte-deterministic for the same
  content. Consumers keyed by commit must refuse dirty snapshots.
- Produce a versioned neutral snapshot with repository identity, commit/tree
  identity, extractor version, source span, fact source, and confidence on
  every emitted fact.
- Snapshots retain one source-language namespace. The accepted JS/TS adapter
  deliberately treats JavaScript, TypeScript, JSX, and TSX as one `js_ts`
  family so mixed-source applications can share a snapshot; this does not
  authorize arbitrary polyglot snapshots or cross-language edges. Mixed
  repositories use separate per-language snapshots; consumers may group
  them only when repository and commit/tree identities and clean/dirty
  status agree, retaining each snapshot's language and semantic-profile
  identity.

## Non-negotiable truth boundary

- Deterministic parsers and language tooling establish structural facts.
  An LLM must never create or upgrade a `contains`, `imports`, `calls`, or
  `implements` edge.
- Every emitted edge must have source provenance and must reference valid
  source-addressed nodes in the same snapshot.
- A call edge is exact only when its target is resolved by a language-aware
  mechanism. Ambiguous/name-only candidates must be omitted or emitted only as
  explicitly non-authoritative candidates, never as canonical call facts.
- Repeat extraction of the same clean commit with the same extractor version
  must be byte-deterministic.
- Never upload, query, or mutate a remote graph as part of extraction.

## Architecture direction

Keep four concerns separate:

1. **Core schema** — snapshot identity, nodes, edges, provenance, validation.
2. **Language adapters** — parsing and language-specific extraction.
3. **Resolution/enrichment** — optional tooling (rust-analyzer, ruby-lsp,
   phpactor) that raises precision without weakening the base truth
   boundary. One LSP transport (`lsp.rs`) serves every client; each
   language module owns only its launch policy, indexing wait, and
   admission model.
4. **Consumers** — the CLI, the LemmaSpec evidence projection, or any other
   reader of the snapshot JSON. They own transport and presentation on their
   own side of the boundary, never fact derivation; `codemap` and
   `codemap-graph` ship no consumer-specific writer or export.

`codemap` and `codemap-graph` are the only canonical implementation crates.
`codemap-lemmaspec` is a consumer crate hosted in this workspace: it depends
only on `codemap-graph`, the canonical crates never depend on it, and its
classifications are named policies with tests, never inline judgment.

Graphiti-style temporal narrative is a later, optional consumer concern. If
introduced, it may contain only source-backed architecture episodes and may
never become the authority for structural relations.

## Quality gates

The Rust adapter must demonstrate, on a pinned representative corpus:

- clean-worktree identity and snapshot reproducibility;
- stable node IDs and valid path/span provenance;
- zero dangling graph references;
- language purity (Rust-only means no other-language facts);
- measured precision and recall for a curated call-impact fixture set,
  including receiver-qualified methods and cross-module calls;
- explicit coverage/unsupported/ambiguous-resolution reporting;
- no hidden network, LLM, or remote-storage dependency.

## Evidence before merge

Stele plans itself (`plans/stele-plans.stele`) against its own catalog
(`intent/`). A done change must serve statements verified at HEAD, and
green runs count only at the source they were recorded at, so before
merging a change that touches tracked files:

1. `stele intent record` on the clean tree; every outcome must be `pass`.
2. Commit `intent/runs.stele` (the ledger is outside the source identity).
3. Paste `stele plan check plans/stele-plans.stele` (exit 0) and
   `stele intent report` (exit 0) into the pull request.

`cargo test` checks the plan's links, order and anchors but not whether
its done changes are verified, so it does not depend on how fresh the run
ledger is. Approvals are recorded only for items the owner names, with
`--by MortenHusted`.

## Working conventions

- Prefer deletion and a smaller schema over generic abstractions.
- Add one language adapter at a time, each with its own test corpus and
  acceptance threshold. Rust, Ruby, PHP, Go, C++, JS/TS, and Python are
  accepted and change only under their own labelled oracle and a re-pinned
  benchmark. Another adapter requires a separate plan and decision.
- Separate observations from hypotheses in design documents.
- Keep private-source material and new, unreviewed benchmark measurements
  local. Reviewed public-corpus pins, hashes, paths, symbols and oracle labels
  may ship when required to reproduce the declared gates; publish only
  reviewed outcomes and never material taken from private codebases.
- Do not copy Graphiti implementation. Graphiti's upstream license is Apache
  License 2.0; ideas still require original design and a license review before
  any code reuse.

## Public source and evidence

Initial public distribution uses reviewed curated source and self-contained
run evidence. Never import the private build-up history or use private commits
as required run bases. The operative LICENSE and component notices govern their
respective code; a source snapshot or a clean scan grants no additional rights.

- Add no material from private codebases: identifiers, paths, snippets or
  measurements. Use public corpora and synthetic fixtures.
- Preserve the pinned public corpus, labelled oracle, declared scope and
  positive/forbidden thresholds for every accepted language. Changing an
  oracle or excluding a case requires review before accepting new results.
- Published run bases must resolve in the public history. Retain matching
  approval digests, deliberate-break evidence and passing runs at the current
  source identity; an exit code alone does not establish verified standings.
