---
name: stele-onboard
description: Onboard an existing repository onto Stele by backfilling its catalog of what the system is for, the map from each statement to the code that realizes it and the tests that exemplify it, and the red and green runs that verify it, for a human to approve. Use when asked to onboard, adopt or set up Stele in a repository, to backfill intents, definitions or channels from existing code, or to tag existing tests as examples. Drafting is the agent's work; approving is the human's.
---

# Onboard a repository onto Stele

You draft; the human approves. Everything you write is a proposal until
the human approves it by name, and you record only the approvals they
give. Work from the code, its tests and its documentation, never from
what the system ought to do.

The Stele skill's `references/catalog.md` describes the catalog format,
examples, runs, approvals and the map; read it first.

## 1. Orient

```sh
stele --version
stele intent --help
stele intro
stele intent check
```

If `stele` is not on `PATH`, stop and say it needs installing. If
`stele intent --help` fails, the installed binary predates `stele intent`
(its version number may not show it): stop and say it needs reinstalling.
If `intent/catalog.stele` exists, read the catalog and
`stele intent report` first, and extend what is there: never rewrite an
approved statement, propose a replacement instead.

## 2. Map the code

```sh
stele code extract --path . --out /tmp/stele-onboard.json
stele code structure src --file /tmp/stele-onboard.json    # give it a root: a whole tree is long
stele code search <name> --file /tmp/stele-onboard.json    # each hit ends with its symbol key
stele code clusters --file /tmp/stele-onboard.json
```

`stele code processes` traces from entry points, so it means something
only on a snapshot with verified calls, extracted with `--calls=required`
(which needs the language's resolver). On the default syntax-only
snapshot every function is an entry point; skip it there.

Read the README, the command-line help, the public API and the tests.
From them, list:

- the **channels**: each way the system is used, such as a command line,
  an HTTP API, a library, a job queue or an agent tool. Every channel
  needs an example for every definition that applies on it, or an
  exception saying the definition does not apply there, so list only the
  ways people use it: a library nothing outside the repository uses is
  not a channel;
- the **capabilities**: what users rely on it for, in their words;
- for each capability, the code that realizes it (entry points, modules)
  and the tests that check it.

`processes` and `clusters` are signals; confirm each with the code before
you use it.

## 3. Draft the catalog

Write `intent/catalog.stele` and one file per area, all imported from the
root. For each statement:

- an **intent** says what the system is for, in terms that would survive
  a rewrite of the code;
- a **definition** refines one intent into a claim a test can check;
- an **exception** narrows a definition on named channels;
- a comment above it cites where you found it, such as
  `// from README.md, Usage` or `// from tests/cli.rs`, and says so when
  you inferred it;
- `realized_by: [...]` on each definition and exception names the code
  that realizes it, as source paths or symbol keys copied from the last
  field `stele code search` prints. Copy them rather than composing them:
  a method's key carries its type and generics, such as
  `rust:src/graph.rs#Graph<'a>::search`.

Keep it small and true: a dozen statements the code demonstrably meets
beat fifty that read well. Leave out what you cannot cite. Then:

```sh
stele intent check
stele intent report        # confirm every anchor in the Map section
```

## 4. Tag examples

For each definition, find existing tests whose assertion is the
definition's claim, in a package's `tests/*.rs`, and tag them:

```rust
/// intent: example greets_ada exemplifies greeting_names_the_person on library
#[test]
fn greets_ada_by_name() { ... }
```

Standalone Ruby projects can instead tag tracked `test/**/*_test.rb` methods
in direct `Minitest::Test` subclasses with Ruby `# intent: example ...`
comments. Use `--runner minitest` on every recording command below and follow
the [catalog guide](../stele/references/catalog.md#standalone-minitest) for
supported runtimes and forms. The runner is explicit and Unix-only; it refuses
Rails roots. Do not boot Rails or prepare a database through this workflow.

A test that merely calls the code is not an example, and an `#[ignore]`
test cannot be one: it records as `skip`, which never counts. A tagged
test should drive the channel it names, not only the code behind it; a
test tagged for more than one channel must pick its driver from
`STELE_CHANNEL`. Where a definition has no such test, propose one to the
human instead of writing it unasked.

Under clippy's pedantic lints, `doc_markdown` reads the tag's IDs as code
missing backticks; add `#![allow(clippy::doc_markdown)]` to each tagged
file. Do it before anything is approved: a test file's inner attributes
and `//!` module docs are part of every example's digest in that file,
so editing them later unapproves every example there.

## 5. Record red, then green

For each example, break the behaviour it checks on purpose in a tracked
file, so the test still builds and fails, then:

```sh
git diff --stat                          # the break is applied, and is the only change
stele intent record --example <id>      # every printed outcome must be `fail`
git checkout -- <the file you broke>
```

Confirm the break before recording: if it did not apply, `record` runs
the unbroken code and appends a passing run instead.

Record red from a tree whose only uncommitted change is the break: a
failing run stores the whole diff. Then, on a clean tree:

```sh
stele intent record                      # every printed outcome must be `pass`
```

An `error` or `skip` does not count. If breaking the behaviour cannot
make a test fail, it does not exemplify the definition: untag it and say
so.

## 6. Ask for approval

```sh
stele intent report
```

The report groups what awaits approval by kind, channels first, gives
each statement's `file:line`, and shows each example's latest red and
green runs, with the source the green one ran at.
`stele intent report --base <the branch point>` adds the failing runs
recorded since then, each with the diff of its break.

Show the human what awaits approval in that order. For each, give the
text, the citation, the map, and the red and green runs. Ask which to
approve.

- Record an approval only for the items the human names, with their own
  name: `stele intent approve <ids...> --by "<their name>"`.
- Never approve your own drafts unasked, and never take approval of one
  item as approval of another.
- Revise what they reject and ask again, or drop it.
- `stele intent report` exits 1 while anything awaits approval or a
  mapped anchor is missing. Leave that state in front of the human; do
  not work around it.

## 7. Hand over

Ask before committing `intent/`, the tagged tests and the ledgers; some
projects keep them out of version control. Report:

- the statements drafted, and which the human approved;
- each statement's standing, with the reasons for any that is not
  verified;
- definitions with no example, and the tests you proposed;
- statements under "Unmapped" in the report;
- anything inferred rather than cited.

Never edit `intent/approvals.stele` or `intent/runs.stele` by hand; only
`stele intent` writes them.
