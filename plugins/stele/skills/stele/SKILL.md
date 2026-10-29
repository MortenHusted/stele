---
name: stele
description: Answer questions about code from deterministic facts, and check code and typed specifications, with the `stele` command. Use when checking a codebase against architecture, unreached-symbol, entry-point complexity or rule-card citation rules; when asking what calls what, what a change would reach, how two symbols connect, or what changed structurally between commits; when measuring whether a change made code harder to maintain; when planning a change as decisions and ordered changes tied to what the system is for, and checking that plan against the code; when backing a catalog of intents with approved, tested examples and a map to the code; and when authoring, checking, mutation-testing or rendering a `.stele` specification from requirements, observed facts and acceptance criteria. Not for mutable runtime state or persistence.
---

# Stele

Stele extracts deterministic facts from source code and evaluates typed
specifications against them. You choose the question and supply only
evidence you have verified. Stele derives the answer. A claim that does
not hold is a finding, not an error to work around.

## Start from the installed binary

```sh
stele --version
stele intro
```

`stele intro` is the orientation for the installed version; read it once
before the first command of a task. Each group lists its commands with
`--help` (`stele code --help`, `stele spec --help`), and `stele spec syntax`
prints the specification grammar. Never guess a flag or the grammar.

If `stele` is not on `PATH`, tell the user it needs installing (Stele's
README, section Install) and stop. Do not build it inside the user's
project. Only when working on Stele's own source, run the checkout with
`cargo run -q -p stele -- <args>` from its root.

## Pick the command

| Question | Start with | Details |
|---|---|---|
| Does the evaluation core stay free of presentation code? | `stele check architecture --path . --core <file> --presentation <file>` | [checks](references/checks.md) |
| Which shipped symbols does nothing reach? | `stele check symbols --path .` | [checks](references/checks.md) |
| Does any entry point exceed the complexity cutoff? | `stele check hotspots --path .` | [checks](references/checks.md) |
| Do rule-card citations point at real declarations? | `stele check citations --path . --rule-cards <file>` | [checks](references/checks.md) |
| What is in this repository, and where is a symbol? | `stele code extract`, `stele code search`, `stele code structure`, `stele code locate` | [code](references/code.md) |
| What calls a symbol, what does it reach, how do two connect? | `stele code context`, `stele code impact`, `stele code trace` | [code](references/code.md) |
| What changed structurally between two commits? | `stele code diff`, `stele code history` | [code](references/code.md) |
| Did a change make the code harder to maintain? | `stele slop diff`, `stele slop gate`, `stele slop report` | [code](references/code.md) |
| Plan a change: decisions and ordered changes, tied to intents and the code; what is next? | `stele plan check plans/<name>.stele` | [plans](references/plans.md) |
| Is what the system is for still true: which statements are verified, what awaits approval? | `stele intent report` | [catalog](references/catalog.md) |
| Which statements does a code change touch, and which examples need recording again? | `stele intent impact main` | [catalog](references/catalog.md) |
| Onboard a repository that has no catalog yet | the `stele-onboard` skill | |
| Can a question be settled by facts and rules? | write a `.stele` file, then `stele spec walk`, `stele spec mutate`, `stele spec render` | [specs](references/specs.md) |

## Ground rules

- **Exit status.** For `stele check`, `stele plan check`, `stele intent
  report`, `stele spec` and `stele slop gate`:
  `0` clean; `1` the input is valid but a claim is open (an expectation, a
  mutation policy, a slop budget); `2` the invocation or input is wrong, so
  read stderr and fix the cause. Never report `1` from these as a tool
  failure. The `stele code` queries differ: they exit `1` when no symbol or
  path matches, which is a lookup miss to fix (try `stele code search`), not
  a finding.
- **An open expectation is a finding.** Report it with its witness. Never
  change a count, drop a fact or loosen a rule to turn it green.
- **Know who said what.** Only facts a tool writes itself carry a
  `snapshot`, `policy` or `observed` basis. If you typed a fact, it is
  `reviewer_declared`, whatever tool printed what you copied and however
  well you cite it. Never promote a guess to a fact.
- **Verified calls need the language's resolver.** Call-graph questions and
  the architecture and symbols checks use rust-analyzer for Rust, and the
  ruby-lsp, phpactor or gopls bundle for Ruby, PHP or Go. Without it Stele
  refuses to guess. Resolver-required `stele code extract` fails with exit 1;
  `stele check` extraction or setup failures exit 2. Read stderr to distinguish
  these errors from a check finding. For Rust, a `rust-analyzer` on `PATH` that is
  only a rustup proxy is not enough: the component must be installed for
  the toolchain the repository pins (`rustup component add rust-analyzer`);
  suggest that to the user rather than running it. `stele check` has no
  syntax-only mode: a structural snapshot passed with `--snapshot` reports
  incomplete, never clean. Say the resolver is missing; never present a
  syntax-only answer as complete.
- **A snapshot names its commit.** Extraction refuses a dirty worktree.
  `stele code extract --dirty` reads uncommitted changes; say the result
  describes the working tree, not a commit.
- **Heuristics are signals.** `stele code hotspots`, `refactor`,
  `clusters` and `processes` are non-authoritative; label them so.
- **Private evidence stays local.** Keep snapshots, evidence, reports and
  source references from private code out of anything published unless the
  user says otherwise.

## Report

- each command run and its exit status;
- the question and the answer Stele derived, kept apart from anything you
  inferred;
- every open expectation or breach, with its witness, the facts beneath it,
  and their sources and bases. The `--json` reports carry each fact's
  provenance but not its basis or source; `stele spec render` shows them;
- for a finding that rests on something being absent, the facts the absence
  was checked against: the witness lists only the positive premises;
- any resolver, coverage or scope limit that bounds the answer, such as
  which entry points a clean check covered;
- the path of any rendered report or kept artifact.
