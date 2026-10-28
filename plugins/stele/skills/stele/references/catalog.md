# The catalog: standings, examples, runs and the map

The catalog in `intent/` says what the system is for (see
[plans](plans.md) for its format). `stele intent` backs each statement
with evidence and gives it one standing:

1. `unapproved`: its text, links or channel tags differ from its latest
   approval, or it was removed without an approved removal;
2. `stale`: a statement it links to changed since it was approved;
3. `unverified`: approved, but its examples have not been seen failing
   and then passing on every channel it applies to;
4. `verified`.

An intent is verified when every definition refining it, and every
exception narrowing those, is verified.

## Examples

An example is a `#[test]` in a package's `tests/*.rs`, tagged with the
statement it exemplifies and the channels it runs on:

```rust
/// intent: example greets_ada exemplifies greeting_names_the_person on library
#[test]
fn greets_ada_by_name() {
    assert_eq!(greeter::greet("Ada"), "Hello, Ada!");
}
```

A test tagged for several channels picks its driver from
`STELE_CHANNEL` and panics on any other. The report lists tags that do
not parse under "Ignored examples". An example's digest covers the test
and also the file's inner attributes and `//!` module docs, so editing
those unapproves every example in the file.

## Runs

```sh
stele intent record --example greets_ada    # after a deliberate break: expect `fail`
stele intent record                         # on a clean tree: expect `pass`
```

`record` runs each example with `cargo test --locked -p <package>`, once
per channel, and appends to `intent/runs.stele`. A passing run counts
only at the source identity it was recorded at: any change to a tracked
file needs green recorded again. A failing run keeps the diff of its
break for review.

By default `base` names the HEAD commit used for the observed diff. For a
curated single-commit publication, `record --source-baseline sha256:HEX`
instead records an explicit `source:sha256:HEX` baseline. Supply the exact
source identity of HEAD, with 64 lowercase hex digits; a mismatch is refused
before tests run. The final published commit must have that same source
identity, and its observed failure patches must reproduce their recorded
source identities when applied to that baseline. Do not rewrite existing
commit-based observations into source-based ones. The two ledgers are outside
source identity, so adding their evidence does not create a self-reference.

## Approvals

```sh
stele intent approve <ids...> --by "<name>"
stele intent reconfirm <ids...> --by "<name>"    # clears stale when the item is unchanged
stele intent remove <ids...> --by "<name>"
```

Approvals append to `intent/approvals.stele`. Anyone may draft;
approving is the project's decision. An agent records an approval only
for items a human names, with that human's name. Never edit either
ledger by hand; on a rebase conflict keep the base's entries first.

## The map

Definitions and exceptions name the code that realizes them:

```text
definition greeting_names_the_person {
  refines: greets_people
  realized_by: ["rust:src/lib.rs#greet"]
  text: "A greeting is Hello, the name and an exclamation mark."
}
```

`realized_by` takes the anchors a plan does: symbol keys in any language
the code is in, and paths of any file, documentation and manifests
included. The map is not approved, so moving code needs no re-approval;
the report checks it against the working tree, like plan anchors, and
takes `--language` and `--snapshot` as `stele plan check` does.

## Impact

`stele intent impact` reads the map backwards: which statements does a
change touch?

```sh
stele intent impact main         # main to the working tree, uncommitted changes included
stele intent impact main HEAD    # between two commits; --json for the same data
```

A symbol anchor is touched when its declaration overlaps a changed line
before or after the change, or when the change adds or removes it; a path
anchor when its file changed. A Rust type does not span its `impl`
methods, so map a method by its own key. Each touched statement comes with
its standing at HEAD's commit, as `stele plan check` takes it, and its
examples: re-read the statement and record the examples again. Changed code no statement maps is a gap in the map or a
statement not yet written. Untracked files are left out; `git add -N`
them first. It reads each language the map's symbol anchors name, or
those `--language` lists. It exits 0.

## Plans

Once `approvals.stele` exists, `stele plan check` reports `unverified_done`
for a done change that serves a statement not verified at HEAD's commit,
so record and commit green before marking a change done.

## Report

```sh
stele intent report                       # Markdown; --format html, -o PATH
stele intent report --base main           # approvals and failing runs since main
```

It exits 1 while anything awaits approval or a mapped anchor is missing,
and 0 otherwise, even with statements unverified or unmapped. Report the
standings with their reasons, what awaits approval, and the Map section.
To onboard a repository that has no catalog yet, use the
`stele-onboard` skill.
