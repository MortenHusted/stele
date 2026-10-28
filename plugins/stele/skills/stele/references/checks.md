# Checking code against a shipped checker

`stele check <checker>` checks code against one of the four checkers Stele
ships. In one process it extracts the repository, writes the checker's
evidence from the facts and checks it. It prints the report as
`stele spec check` does and exits the same way: `0` clean, `1` an
expectation is open, `2` extraction or projection failed (`stele check: `
and the reason).

```sh
stele check symbols --path . --public-api "rust:src/lib.rs#extract"
stele check architecture --path . --core src/eval.rs --presentation src/html.rs
stele check hotspots --path path/to/ruby-app --language ruby
stele check symbols --snapshot snapshot.json --json
```

`--path` extracts Rust, Ruby, PHP or Go (`--language`, default `rust`). It
gets verified calls for architecture and symbols and measures for hotspots,
which is what each checker's evidence needs. `--snapshot FILE` reuses a
snapshot from `stele code extract` instead. That is also the way to check
C++, JS/TS or Python, and a working tree with uncommitted changes
(`stele code extract --dirty`, then `--snapshot` with `--allow-dirty`).

## The checkers

- **architecture**: does the evaluation core stay free of presentation, and
  which production files does nothing reach? Declare the layout with
  repeatable `--core FILE` and `--presentation FILE`. Each is one
  repository-relative file that holds shipped definitions, such as
  `crates/engine/src/eval.rs` in a workspace; directories and globs are
  refused, so repeat the flag per file. It finds core symbols that reach
  presentation code, production files with no observed path from an entry
  point, and production files only tests reach. Needs verified calls.
- **symbols**: which shipped symbols have no path in the observed call graph
  from an entry point? `--public-api KEY` declares symbols exported to
  callers outside the repository, which count as reached; `--entry KEY`
  declares further entry points. Needs verified calls.
- **hotspots**: does any entry point sit above cyclomatic complexity 10? A
  complex helper deep in a model may be the right shape; branching where a
  request, job or command lands is the finding a reviewer acts on first.
  Needs measures. A clean result covers only the entry points the check
  saw, so name them in the report (`--json` lists them). In a Rust
  workspace those are the binaries' `main`s, test-fixture binaries
  included, and not the subcommand handlers: declare those with `--entry`
  to hold them to the cutoff too.
- **citations**: do the locations cited in rule cards (`--rule-cards FILE`)
  fit a recorded declaration? A valid location is structural evidence only.
  Every claim stays reviewer-declared, so this checker always reports
  incomplete; its value is the invalid locations. The rule cards are
  Markdown in this narrow form:

  ```markdown
  ### RULE-001: Discount cap
  **Source:** `src/discount.rs:3-5`
  **Plain English:** The discount is capped at 20 percent.
  ```

  A heading with `RULE-` and three digits starts a card and the next
  heading ends it. `Source` is one backtick-quoted, repository-relative
  `path:line[-line]`; `Plain English` states the claim. Other fields and
  prose are ignored, and a file with no card is refused.

Symbol keys look like `rust:src/lib.rs#extract`: language, repository path,
then the qualified name.

## Entry points and coverage

Entry points come from convention: a Rust binary's `main`, Rails controller
actions, job and worker `perform`, PHP controller actions and job and
command `handle`. Declare anything else with `--entry`, for any checker.

Architecture and symbols only conclude something when the call graph is
admitted. The default admission policy needs every recognized call site
resolved to an exact in-repository target (1000 per mille), a semantic
extraction, and no parse errors or skipped files. Below that the check
reports incomplete and exits nonzero, even with no findings.
`--min-call-coverage-per-mille N` lowers the threshold and keeps the other
guards; say that you lowered it. Even 1000 is an admission policy, not
proof that every call was captured: unresolved dynamic dispatch can hide a
real caller. Report an unreached symbol as having no path in the observed
graph, not as dead code.

## Keeping a finding

`stele check` prints and keeps nothing. To keep the evidence, render it or
hand it on, run the steps one at a time:

```sh
stele code extract --path . --calls=required --out snapshot.json
stele code evidence --file snapshot.json --checker symbols \
  --public-api "rust:src/lib.rs#extract" --readable --out evidence.stele
stele code checker symbols --out symbols.stele
stele spec check symbols.stele evidence.stele
stele spec bind symbols.stele evidence.stele          # evidence.bound.stele
stele spec render evidence.bound.stele --source-root .
```

`--readable` adds evidence bases, labels and maintainer notes, which make
the rendered report readable. Every evidence fact names where it came
from: a source location for what the snapshot extracted,
`policy:<name>/<language>` for what a named rule classified, and no
provenance for a decision passed on the command line, which stays
reviewer-declared.
