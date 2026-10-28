# Plans

A plan is a typed spec of a change: the decisions behind it and the
ordered changes that carry it out, each tied to a statement of what the
system is for. `stele plan check` checks that every link holds, that the
code the plan names exists, and says which changes are ready.

## The catalog and the plan

The catalog says what the system is for and lasts as long as the
project. It is a directory, by convention `intent/`, of `.stele` files
under one root file that imports every other:

```text
// intent/catalog.stele
catalog greeter {
  use "./channels.stele" as channels
  use "./greeting.stele" as greeting
}

// intent/greeting.stele
catalog greeting {
  use "./channels.stele" as ch

  intent greets_people { text: "The greeter greets people by name." }
  definition greeting_names_the_person {
    refines: greets_people
    text: "A greeting is Hello, the name of the person, and an exclamation mark."
  }
  exception library_returns_text {
    narrows: greeting_names_the_person
    channels: [ch.library]
    kind: holds_instead
    text: "The library returns the greeting instead of printing it."
  }
}
```

- An **intent** says what the system is for. A **definition** refines one
  intent and says what a term means. An **exception** narrows one
  definition on named **channels** (the ways the system is used): either
  the definition `does_not_apply` there, or the exception's text
  `holds_instead`.
- A bare ID names a block of the same file; `alias.id` names one in the
  file the alias imports.

A plan, by convention in `plans/`, imports the catalog root once and
reaches every catalog statement through its alias:

```text
plan farewell {
  use "../intent/catalog.stele" as intent

  intent says_goodbye { text: "The greeter says goodbye by name." }

  decision reuse_the_greeting_format {
    governs: [says_goodbye]
    text: "A farewell uses the same format as a greeting."
    rejected: "A separate format: two shapes for one kind of message."
  }
  change write_greet {
    serves: [intent.greets_people]
    touches: ["rust:src/lib.rs#greet"]
    state: done
    text: "Write greet."
  }
  change add_farewell {
    after: [write_greet]
    follows: [reuse_the_greeting_format]
    serves: [says_goodbye]
    creates: ["rust:src/lib.rs#farewell"]
    text: "Add farewell, formatted like greet."
  }
}
```

- **Proposals.** A statement block in a plan proposes it for the catalog;
  `replaces: intent.old_id` proposes a new version of a catalog statement
  of the same kind. The catalog itself changes only when the user accepts
  the proposal.
- **`decision`**: `governs` (the statements it is about), `text`, and
  `rejected` (the alternatives and why they lost).
- **`change`**: `text`, `serves` (statements), `follows` (decisions),
  `after` (changes that come first), `touches` (code it edits),
  `creates` (code it adds), and `state`: `planned` (the default) or
  `done`.
- **Anchors** in `touches` and `creates` are symbol keys,
  `language:path#name` as `stele code search` prints them (`rust:`,
  `ruby:`, `php:`, `go:`, `js_ts:`, …), in as many languages as the
  change spans, or repository-relative paths of any file: source,
  documentation, manifests. A path anchor is present when the working
  tree holds the file, untracked included, unless git ignores it.

## Check it

```sh
stele plan check plans/farewell.stele
stele plan check intent/catalog.stele          # the catalog on its own
```

It reads the files from the git repository that holds the plan, and the
code from that repository's working tree, uncommitted changes included:
a snapshot of each language the symbol anchors name, Rust when they name
none, and the working tree's files for path anchors. `--language`
replaces the derived languages and repeats. Rust, Ruby, PHP and Go
extract without setup; C++, JS/TS and Python need a prepared profile,
so extract them with `stele code extract` and pass `--snapshot FILE`,
which holds one language. A plan without anchors reads no code. The
report reads as `stele spec check`, then:

```text
ready: add_farewell
blocked: none
```

Exit `2` is an invalid file: an unknown reference, a link to the wrong
kind, a duplicate ID, a malformed anchor; the message names `path:line`.
Exit `1` is a finding:

| Finding | Meaning | Fix |
|---|---|---|
| `change_serves_nothing` | A change ties to no statement | Name what it serves, or propose the statement it serves |
| `unserved_proposal` | A proposed statement no change serves | Add the change, or drop the proposal |
| `uncited_decision` | A decision no change carries out | Make a change follow it, or drop it |
| `cyclic_order` | Changes come after each other | Fix the order |
| `missing_anchor` | Code a change edits is not in the code | Point at where the code is now, or ask whether the change is still right |
| `unbuilt` | A done change adds code that is not there | The change is not done, or the anchor is wrong |
| `unverified_done` | A done change serves a statement that is not verified at HEAD (only once the catalog has `approvals.stele`) | Back it with approved examples and their runs (see [catalog](catalog.md)), or the change is not done |

Unlike a spec's expectation, a plan finding is fixed in the plan, because
the plan is the thing being checked. Fixing never means marking a change
done that is not, or deleting an anchor only to silence a finding.
`state: done` is reviewer-declared: set it when the work has landed. Once
the catalog has approvals, a done change also needs every statement it
serves verified, with the standings taken at HEAD's commit, so committed
green runs count while the next change is edited.

## Work through a plan

1. Read the catalog first and cite its statements. Propose a statement
   only when none says what the change is for, and tell the user.
2. Record each decision with what was rejected and why.
3. Order the changes with `after`, anchor them in the code they edit or
   add, and run `stele plan check` until it exits 0. Once the code is
   edited, `stele intent impact main` lists the statements it touches:
   check them against the change's `serves`.
4. Take the next change from the `ready:` line. When it lands, set
   `state: done` and check again: its `creates` anchors must now exist.

Ask before committing a catalog or plan to a repository that has none:
some projects keep them out of version control.

## Keep or render the result

```sh
stele plan evidence plans/farewell.stele --out evidence.stele
stele plan checker --out plan.stele
stele spec bind plan.stele evidence.stele        # evidence.bound.stele
stele spec render evidence.bound.stele --source-root .
```

Report the findings with their witnesses, the `ready:` and `blocked:`
lines, and any proposal the user has to accept into the catalog.
