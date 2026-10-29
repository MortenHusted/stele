# Code facts

`stele code` extracts a snapshot of a repository at a commit (files,
symbols, modules, imports, implementations and, with a resolver, verified
calls), every fact with its source location. The queries read that
snapshot. Nothing runs the project's code, and the same commit always gives
the same bytes.

## Extract

```sh
stele code extract --path . --out snapshot.json
stele code extract --path . --calls=required --out snapshot.json   # verified calls
stele code extract --path . --measures --out measured.json         # complexity, lines, clones
stele code extract --path . --dirty --out dirty.json               # uncommitted changes
stele code validate --file snapshot.json
stele code report --file snapshot.json
```

- One language per snapshot: `--language rust` (default), `ruby`, `php`,
  `go`, `cpp`, `js-ts` or `python`. Run `stele code extract --help` for the
  inputs C++, JS/TS and Python need.
- Without `--calls=required` a snapshot is structural: it has no `calls`
  edges, so call questions have no answer from it. Verified calls need
  rust-analyzer for Rust, or the language's tool bundle for Ruby, PHP and
  Go (Stele's README, section Development). Without the resolver,
  extraction exits 1. Rust needs the rust-analyzer component of the
  toolchain the repository pins; a rustup proxy on `PATH` is not enough.
- Extraction refuses a dirty worktree so a snapshot always names its commit.
  `--dirty` reads the working tree as it is; the snapshot is marked dirty
  and must not be reported as the commit's.
- `--measures` adds per-callable cyclomatic complexity and line counts,
  per-file line classes and clone groups; `--twins` (with `--measures`)
  adds near-copy pairs. Measures change the snapshot ID.

Extract once and pass `--file snapshot.json` to every query. Without
`--file`, a query extracts `--path` again on each call.

## Find a symbol

A query names one symbol by, in this order: its exact node ID, its exact
symbol key (`rust:src/lib.rs#extract`), its declared name, or a qualified
suffix such as `UsersController#index` or `Admin::UsersController::index`.
When several symbols match, the command lists them; pick one and ask again
with its symbol key. When none matches, the command exits 1; that is a
lookup miss, not a finding, so look the name up with `search`.

```sh
stele code search extract --file snapshot.json        # name or key contains the text; ends with the key
stele code structure src/cli.rs --file snapshot.json  # containment tree under a file, module or symbol
stele code locate src/lib.rs:40-52 --file snapshot.json   # declarations containing those lines
```

## Follow the graph

```sh
stele code context extract --file snapshot.json                         # container, edges, fan-in and fan-out
stele code impact extract --direction upstream --file snapshot.json     # everything that reaches it
stele code impact extract --direction downstream --file snapshot.json   # everything it reaches
stele code trace main extract --kinds calls --file snapshot.json        # shortest path between two
stele code cycles --kinds calls --file snapshot.json                    # strongly connected components
```

`--kinds` takes a comma list of `calls`, `contains`, `imports`,
`implements`, `inherits` and `extends`; empty means all. Impact and trace
over `calls` answer only from a snapshot with verified calls; on a
structural snapshot say the call graph was not extracted.

## Compare commits

```sh
stele code diff main HEAD                       # exact node and edge delta, plus churn
stele code diff main HEAD --calls=required      # include the call-edge delta (slower)
stele code history --out history --limit 40     # the structural change stream, oldest first
```

## Heuristics

`stele code hotspots` (coupling by churn), `refactor` (cycles, hubs,
god-symbols, orphans), `clusters` and `processes` are derived signals, not
facts. Present them as places to look, never as findings.

## Slop: what a change made harder to maintain

`stele slop` reads snapshots extracted with `--measures`, from the same
binary and language for both sides.

```sh
stele slop diff --base base.json --head head.json        # per-callable deltas, cutoff crossings, new clones
stele slop report --file head.json                       # erosion with its counts, hotspots, per-file figures
stele slop gate --base base.json --head head.json --budget slop.toml   # exit 1 on a breach
stele slop series --history history --window 20          # per-commit figures and their slope
```

`extract` reads the checked-out tree, so extract the base from a clean
checkout of the base commit, such as a git worktree. `--exclude
db/migrate/` (repeatable) or `exclude = "db/migrate/"` lines in `slop.toml`
keep run-once code out of the figures; the all-files line still prints. A
ratio always comes with its counts: report the count of callables over the
cutoff and the worst hotspot, not the ratio alone. The gate is advisory: a
breach is a reason to look, and the user decides.

## Topology

`stele topology --snapshot snapshot.json --out topology.json --html
topology.html` renders a validated snapshot as an offline map.
