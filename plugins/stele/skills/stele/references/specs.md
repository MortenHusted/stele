# Specifications

A `.stele` file is an executable specification: typed relations, facts
asserted with evidence, rules that derive conclusions, expectations that
state exact-count claims, and mutation policies that prove the rules are
load-bearing. You own the model and the evidence; the engine owns
deterministic derivation and expectation checking. Files written before
Stele, with the `.lemmaspec` extension, still read.

## Author an artifact

1. Decide the question the artifact must answer. Keep one coherent
   implementation, feature or analysis boundary per file.
2. Translate verified or explicitly supplied observations into `fact`
   blocks. Never promote a guess to a base fact to make an expectation pass.
3. Declare every predicate as a typed `relation`. Prefer relations that stay
   meaningful outside one sentence, such as `requires`, `changes`, `calls`,
   `blocked_by`, `implemented` or `satisfies`.
4. Express consequences and invariants as `rule` blocks. Give facts, rules
   and expectations stable, descriptive IDs.
5. Express acceptance criteria as `expect` blocks with exact counts. An
   expectation is a claim to test, not a value to force.
6. Keep the evidence's standing explicit. Facts may declare `basis:
   snapshot`, `policy`, `observed` or `reviewer_declared`. Each basis needs a
   `source` string; `snapshot` and `observed` also need an `identity`
   string. This metadata states the producer's claim; it does not
   authenticate authorship or prove truth. Untyped facts stay
   reviewer-declared, including facts with provenance or full confidence.
   Generated observations must come from the named deterministic tool. If
   you typed a fact, it is `reviewer_declared`, even when you copied it from
   a tool's output at a pinned commit and however well you cite it.
7. Write for the reader, not only the engine. Comments before `spec` state
   the question; keep maintainer guidance in `notes { text: "..." }`. A
   comment directly above a relation, fact, rule, expectation or mutation
   explains it in the rendered guide. Give relations `roles` naming each
   argument and a `reads` template such as
   `"{item} depends on {dependency}"` so facts and rule conditions render as
   sentences. Give symbols human labels and source links; unlabelled symbols
   stay visibly marked.

Run `stele spec syntax` rather than guessing the grammar. The boundaries
that matter most:

- Variables begin with an uppercase letter; lowercase or quoted values are
  symbols.
- Every relation a fact, rule or expectation uses must be declared.
- One relation cannot be both asserted directly and derived.
- `count`, `min` and `max` are available in rule heads, as in
  `derive: "open_total(count(Item))"`. `sum` and `now` are not available in
  self-contained artifacts.
- Symbols are opaque: there are no string operations, so a rule cannot
  match a prefix or a part of a name. State any relation between names you
  need as a fact.
- Time or revisions must be ordinary explicit values if the model needs
  them.

## Evaluate and repair

```sh
stele spec walk path/to/spec.stele --json
```

Read the exit status before changing the file:

- `0`: the artifact is valid and every expectation holds.
- `1`: the artifact is valid, but at least one expectation does not hold.
  That is evidence about the specification or the implementation. Do not
  change facts or counts to get green output.
- `2`: invocation, reading, parsing or validation failed. Repair the syntax
  or the model from the diagnostic on stderr.

Inspect the JSON facts and their `why` trees before trusting a derived
conclusion, and keep asserted and derived facts apart in any report. The
JSON gives each fact's provenance but not its basis or source; render the
artifact to read standings. A `why` tree lists the positive premises only:
a conclusion drawn from something being absent (`not covered(X)`) rests on
every fact the rule could have matched, so name that set when you report
it.

## Mutation-test the specification

When an artifact declares `mutation` policies:

```sh
stele spec mutate path/to/spec.stele --json
```

Mutation analysis evaluates every mutant from a fresh copy of the parsed
artifact, and needs a clean baseline: every expectation must hold. When the
honest answer to the question is an open expectation, `mutate` reports
`baseline_incomplete`, exits `1` and runs no policy. Test the rules the way
Stele's own checkers do: put them in a checker whose fixture is a small
made-up case where the expectations hold, mutate the checker, and check the
real facts against it with `stele spec check` (next section). The operators
are:

- `drop_rule`, optionally excluding rule IDs with `except`;
- `drop_condition`, optionally excluding whole rule IDs or named conditions
  with `except`;
- `drop_fact`, scoped by a required `relation` and optionally excluding fact
  IDs.

Name a condition when it needs a durable identity or its own exclusion: use
a keyed `when` map and refer to the condition as a quoted
`rule_id.condition_id` value.

```text
rule select_release {
  derive: "selected(Item)"
  when: {
    item_exists: "release(Item)"
    item_is_ready: "ready(Item)"
  }
}

mutation condition_coverage {
  operator: drop_condition
  except: ["select_release.item_is_ready"]
}
```

Leave out `must_fail` when any failed expectation may kill a mutant. Set
`must_fail: EXPECTATION_ID` when that exact oracle must fail; unrelated
failures then do not count. Treat `survived` as a specification gap,
`rejected` as a validation boundary and `excluded` as explicit policy. They
are not interchangeable successes.

Rejected and excluded targets are not valid executions. A policy that
executes no valid mutant is `vacuous`, and a report with a vacuous policy
exits `1`. An evaluation failure is an analysis error and exits `2`. Two
policies with identical configuration are invalid, because counting the same
mutants twice would mislead.

## Check evidence against a checker

A checker is an artifact whose facts are a fixture and whose expectations
and mutation policies prove its rules bite. To apply one to something real,
write an evidence file: a `spec` holding facts over the checker's relations,
with source citations and declared bases from their producers, plus the
expectations that should hold there. Labels and notes may accompany the
evidence. Facts you author stay reviewer-declared even when they cite code.

```sh
stele spec check path/to/checker.stele path/to/evidence.stele --json
```

The evidence file's facts and expectations replace the checker's fixture and
expectations; the report and exit status read as for `walk`. Assert only
facts you established; never invent one to satisfy an expectation. A failed
expectation with its `why` witness is the finding: report the derived fact,
the rule, and the sources and standings of the facts beneath it. An
exemption is a reviewer-declared fact citing the review that granted it, not
a deleted piece of evidence.

For code, Stele writes the evidence itself and ships the checkers; see
[checks](checks.md).

To keep, render or project the result, bind it into one file:

```sh
stele spec bind path/to/checker.stele path/to/evidence.stele
```

This writes `path/to/evidence.bound.stele` (or the `-o` path), a
self-contained artifact that `walk`, `project` and `render` accept
unchanged. Binding drops the checker's mutation policies, so mutate the
checker, not the bound file.

## Project the graph

```sh
stele spec project path/to/spec.stele --json
```

The projection is the deterministic, closed graph of the artifact:

- IDs are namespaced by spec, and every edge endpoint is a node in the same
  projection.
- Edge relations have fixed endpoint types.
- Symbols are local anchor nodes. Do not replace them with external
  repository or datastore IDs inside the projection.
- Proof edges keep one deterministic body witness for each producing rule.
  They are not an exhaustive retraction-impact graph.
- Recursive rules may produce cyclic `depends_on` edges; traversals need a
  visited set.
- The output has no ingest envelope, revision or clock-derived timestamp.

When identity stability matters, run `project --json` twice in separate
processes and compare the bytes.

## Render the human view

```sh
stele spec render path/to/spec.stele
stele spec render path/to/spec.stele --source-root . --output path/to/report.html
stele spec render path/to/spec.stele --format md
```

The default output replaces `.stele` with `.html`, or `.md` with
`--format md`. Both open on the answers: failed expectations first, numbered
witness steps, and the declared standings and sources of the premises.
Maintainer notes stay out of this brief. The offline HTML graph starts on
the selected answer's witness; the full graph and walkthrough are opt-in.
Tell a reader to press `?` for the guide, and point them at the failed
answer or reviewer-declared premise that needs attention.

When source citations are repository-relative, pass `--source-root` with
that repository's directory so links resolve from where the report is
written. `--source-root .` means the invocation directory. Without it,
relative links resolve beside the report.

Observed status records the identity it was observed against. Pass
`--target-identity ID` to compare with an explicit target; a mismatch puts a
not-current banner first and exits `1`. A static report does not watch
sources. Exit `1` still writes the report: keep it and report its open
claims or stale observations.

## What to add to the report

- the artifact path and the question it models;
- asserted facts versus derived conclusions;
- each expectation and whether it held;
- node and edge counts when projected;
- any modelling choice that could change the answer.

## What the format leaves out

The artifact is a static, replayable subset of the Lemmalog engine:
assertions are `fact`s, installed rules are `rule`s, a query is an `expect`
with an exact count, and `walk --json` carries the dump and the `why`
explanations. Mutation policies copy and re-evaluate the static artifact and
expose no mutable engine state. Do not invent syntax for runtime actions:
retraction, ad hoc queries, authored hypotheticals, batch install and
uninstall, change feeds, clocks and persistence are not part of the format.
