# LemmaSpec

LemmaSpec is the specification engine in Stele: typed, self-contained
software specifications evaluated by a deterministic deductive engine. Its
command line is `stele spec`.

```text
.stele artifact -> typed validation -> logic engine -> executable report
```

An artifact keeps relation schemas, asserted facts, derivation rules,
expectations, and optional mutation policies in one reviewable file. The same
artifact can produce a human report or a deterministic graph for another tool
to consume.

## Readable evidence

HTML reports open on the answers, with failed expectations first, numbered
reasoning and source citations. Select an answer to see its witness graph;
open the full graph or walkthrough when you need the whole model.

```sh
stele spec render tests/fixtures/readability/labelled_dependency.lemmaspec --source-root . -o /tmp/report.html
stele spec render tests/fixtures/readability/labelled_dependency.lemmaspec --format md --source-root . -o /tmp/report.md
```

These examples intentionally fail an acceptance expectation, so the reports
are written and the command exits 1. Display labels, notes and evidence bases
never change evaluation output or proof identities.

When citations are repository-relative and the report is written elsewhere,
supply `--source-root PATH`. Relative roots are resolved from the invocation
directory; no Git root is guessed. Links are rebased from the report directory
to this explicit source directory. Without the flag, authored destinations stay
unchanged and resolve relative to the report, preserving sibling-file links.
HTTPS URLs and report-local fragments stay unchanged in either mode. The source
root must exist; rendering creates the output directory if needed.

Library callers can construct `SourceContext::new(source_root, output_directory)`
from existing directories and pass it to `render_projection_html_with_context`
or `render_projection_markdown_with_context`. Existing render functions preserve
their link behavior. Context changes link destinations only, never citations,
source artifacts, projection metadata or proof identities.

A fact may declare its producing boundary with `basis: snapshot`, `policy`,
`observed`, or `reviewer_declared`, plus a `source` string. Snapshot and observed
facts also require an `identity` string. Untyped facts, including those with
free-form provenance, render as reviewer-declared. This metadata describes the
producer's claim; it does not authenticate authorship or prove truth. Generated
observations must come from the named deterministic tool. LLM-authored claims
remain reviewer-declared.

Observed status reads “as of” its identity. Supply `--target-identity ID` to
compare it with an explicit target: mismatches put a not-current banner first
and return exit 1. A static report does not promise to monitor source changes.

## Install

LemmaSpec ships inside the `stele` binary. From the Stele repository:

```sh
cargo install --locked --path crates/stele
stele spec --version
```

Run the examples below from `crates/lemmaspec`.

## Quick start

Inspect the language and evaluate an example:

```sh
stele spec syntax
stele spec walk examples/release_readiness.stele
stele spec walk examples/release_readiness.stele --json
```

The example contains the complete model:

```text
spec release_readiness {
  relation depends_on { args: [symbol, symbol] }
  relation incomplete { args: [symbol] }
  relation blocked { args: [symbol] }

  fact release_needs_tests {
    relation: depends_on
    args: [release, tests]
  }

  fact tests_are_incomplete {
    relation: incomplete
    args: [tests]
    provenance: ["plan:test-gate"]
  }

  rule blocked_by_incomplete_dependency {
    derive: "blocked(Item)"
    when: [
      "depends_on(Item, Dependency)",
      "incomplete(Dependency)",
    ]
  }

  expect release_is_blocked {
    query: "blocked(\"release\")"
    count: 1
  }
}
```

`walk` rejects unknown relations, wrong arity, value-type mismatches, and rule
variables used with incompatible types. Its report separates asserted and
derived facts and carries a deterministic `why` witness for every fact.
Repeated walks over the same artifact produce byte-stable JSON.

Artifacts can attach display metadata to their existing symbol values:

```text
symbol release { label: "Publish the release" source: "plan.md#publish" }
symbol "module::verify" { label: "Verify calls" }
notes { text: "Refresh the evidence when the plan changes." }
```

These declarations belong inside `spec`. A symbol must occur in a fact, rule,
or expectation; forward references and repeated display labels are allowed.
Labels, optional sources, and maintainer notes never change evaluation or proof
identities. Notes are separate from the reader's question before `spec`.
Source links allow repository-relative paths, fragments, and HTTPS URLs;
unsafe destinations remain text. Binding keeps labels for surviving checker
symbols, lets evidence labels override them, and combines both sets of notes.

## Commands

```text
stele spec walk <path.stele> [--json]
stele spec mutate <path.stele> [--json]
stele spec check <checker.stele> <evidence.stele> [--json]
stele spec bind <checker.stele> <evidence.stele> [-o <bound.stele>]
stele spec project <path.stele> [--json]
stele spec render <path.stele> [--format html|md] [--source-root <path>] [-o <path>]
stele spec syntax
```

- `walk` validates and evaluates the artifact.
- `mutate` tests whether its expectations notice deliberate omissions.
- `check` evaluates one artifact's rules over another file's facts.
- `bind` writes that combination as one self-contained artifact.
- `project` emits its deterministic, internally closed graph.
- `render` writes a dependency-free HTML report beside the artifact.
- `syntax` prints the supported artifact and rule language.

Exit status `0` means the requested check passed, `1` means the artifact is
valid but an expectation, mutation policy, or baseline remains incomplete, and
`2` means the command or artifact is invalid.

## Mutation analysis

Mutation policies can remove rules, named rule conditions, or facts:

```text
mutation semantic_rules_are_observable {
  operator: drop_rule
  except: [known_equivalent_rule]
}

mutation violations_are_guarded {
  operator: drop_fact
  relation: violation
  must_fail: nothing_unexpected
}
```

Each mutant starts from a fresh parsed artifact. Results distinguish
expectation-`killed`, valid `survived`, validation-`rejected`, and
policy-`excluded` mutants. Rejected and excluded targets are not counted as
executions; a policy without a valid execution is `vacuous` and exits `1`.
Evaluation failures are analysis errors and exit `2`.

Without `must_fail`, any previously passing expectation can kill a mutant.
With `must_fail`, that exact expectation must fail, so an unrelated failure
cannot mask a surviving oracle. Duplicate policies are rejected rather than
counting the same mutants twice.

See [examples/mutation_analysis.stele](examples/mutation_analysis.stele)
for a complete artifact.

## Checking evidence

A checker is an ordinary artifact whose facts are a fixture and whose
expectations and mutation policies are its self-test. `check` keeps the
checker's relations and rules, replaces its fixture with the facts of an
evidence file, and evaluates the evidence file's expectations instead:

```sh
stele spec check examples/state_as_records.stele .stele/state_as_records.stele
```

The evidence file is a `spec` with facts and expectations only; a relation or
rule in it is rejected, and its facts are validated against the checker's
relations. The result is one closed artifact evaluated exactly like a walk,
with a `why` witness for every derived fact, so a finding traces back to the
evidence and its provenance. Evidence is written by whoever can establish it,
an agent reading a schema today or an extractor later; LemmaSpec does not
inspect source code.

`bind` writes the same combination to disk as an ordinary artifact, by
default beside the evidence as `<evidence>.bound.stele`:

```sh
stele spec bind examples/state_as_records.stele .stele/state_as_records.stele
stele spec render .stele/state_as_records.bound.stele
```

The bound file is self-contained, so walk, mutate, project, and render accept
it unchanged. Its opening comment names the checker and the evidence it was
bound from. It is the proof record for one checker over one codebase, fit to
review and commit.

[examples/state_as_records.stele](examples/state_as_records.stele)
judges a Rails schema fixture against the convention that togglable state is
a record rather than a boolean column.

## Graph and HTML projections

`project` emits nodes for the spec, relations, facts, rules, mutations,
expectations, and symbols, plus typed edges such as `asserts`, `derives`,
`depends_on`, `proves`, `matches`, `expects`, `targets`, and `references_symbol`.
A satisfied expectation is `proves`-linked from the facts that match its query;
an open one is `matches`-linked from the facts found instead.

The graph is internally closed: every ID is spec-namespaced and every edge
endpoint is emitted in the same projection. Symbols that may later resolve to
external definitions remain local anchor nodes. Concrete proof edges retain
one deterministic witness for each producing rule; recursive rules may create
cycles, so consumers should traverse with a visited set.

The projection intentionally has no persistence envelope, revision identity,
or clock-derived timestamp. Those are responsibilities of a separate adapter,
which can translate the stable projection into a datastore-specific contract.
[examples/persistence_adapter_readiness.stele](examples/persistence_adapter_readiness.stele)
models that boundary as a deliberately incomplete executable specification.

`render` writes one offline HTML file with the graph on stage and a journey
beside it: the question the artifact answers, then observations, assumptions,
reasoning, conclusions with proof trees, claims, stress tests, and reference.
Each step lights its part of the graph; selecting a card or a node follows one
thread through the argument. The page carries its own guide: press `?` or the
guide button for how to read it. Comments in the source become the prose. Relations can declare `roles` for their arguments and a
`reads` sentence template so facts and rule conditions render as sentences:

```text
// Can the release ship today?
spec release_readiness {
  relation depends_on {
    args: [symbol, symbol]
    roles: [item, dependency]
    reads: "{item} depends on {dependency}"
  }
  ...
}
```

Facts with `provenance` are observations; facts without it, or below full
confidence, are assumptions, listed with everything that would fall if they
turned out to be wrong. Render with:

```sh
stele spec render examples/release_readiness.stele
```

## Agent use

Agents reach the engine through the `stele` command: `stele intro` orients
an agent, and `stele agent install` installs Stele's skill.

## Scope

LemmaSpec currently packages a static, replayable subset of its engine:
asserted facts, stratified rules, incremental fixpoint evaluation, confidence
and provenance annotations, explanations, hypotheticals, and change feeds.
The `.stele` artifact adds typed schemas, executable expectations, graph
projection, and isolated omission-based mutation analysis.

Mutable sessions, persistence, retrieval, clocks, and application-specific
adapters are deliberately outside the artifact format. The direct engine API
also retains one upstream boundary: uninstalling a rule batch containing inline
fact clauses cannot distinguish that batch's support from an independently
declared base fact. Artifacts do not install facts through rule batches.

## Development

The engine is a Stele workspace member. Its tests run with the workspace gates:

```sh
cargo test -p lemmaspec --locked
cargo test -p stele --locked   # the `stele spec` command line
```

## Lineage and license

LemmaSpec is an independent project inspired by
[Lemmalog](https://github.com/JordyZomer/lemmalog); it is not affiliated with or
endorsed by the Lemmalog project or its author.

Several engine and engine-test files are modified from Lemmalog 0.2.0 at commit
[`74d428a`](https://github.com/JordyZomer/lemmalog/commit/74d428a2497066795f6328946457f22d713fcbd5).
The exact files, upstream copyright, and MIT terms are preserved in
[NOTICE](../../NOTICE) and [THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md).

Project material is covered by the [Stele Source-Available License 1.0](../../LICENSE).
Personal, educational and internal business use, including paid consulting and
reports, is free. Selling, externally embedding or hosting the tools needs
separate written permission. Earlier MIT grants and the inherited Lemmalog
MIT portions retain their permissions; see [third-party notices](../../THIRD_PARTY_NOTICES.md).
