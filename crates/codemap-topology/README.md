# codemap-topology

Render a validated Codemap snapshot as deterministic topology JSON and an offline
interactive HTML map. Exact calls, inheritance and implementation become
edges; nothing else does. Unresolved sites remain annotations, and a map
without liveness input has no absence markers.

```sh
stele topology --snapshot snapshot.json --out topology.json --html TOPOLOGY.html
```

Two options shape what the map covers:

```sh
stele topology --snapshot snapshot.json --selection selection.json \
  --exclude test --exclude vendor/cache \
  --out topology.json --html TOPOLOGY.html
```

`--selection` alone marks the selected entry points. It is checked against
the snapshot only, so it supports no absence claim and the map makes none.
`--exclude PREFIX` (repeatable) leaves every file under a repository-relative
path prefix off the map, with the nodes in those files and every edge that
touches them. Prefixes match whole path segments, and one that names no file
is refused. The map states what it left out; the snapshot and its extraction
report are unchanged.

For admitted, qualified absence markers, use the `symbols` checker and provide
an independently expected selection manifest:

```sh
stele topology --snapshot snapshot.json \
  --liveness symbols-check.json --selection selection.json \
  --out topology.json --html TOPOLOGY.html
```

The check input is the JSON output of
`stele spec check symbols.stele evidence.stele --json`. Exit 1 with
`status: incomplete` can mean legitimate findings; the consumer validates the
positive coverage admission separately. Rejected admission, mismatched
identity, policy, census, selections or witness fails before writing outputs.
Only symbols-check liveness is supported; file-level architecture findings do
not become callable markers.

The selection manifest has exactly these fields:

```json
{
  "entry_policy": "policy:entry-point/rust",
  "coverage_policy": "observed-call-coverage/v1",
  "min_call_coverage_per_mille": 500,
  "entry_points": ["rust:function:src/main.rs#entry"],
  "public_api": []
}
```

The lists contain sorted, duplicate-free canonical **node IDs**, matching the
actual `entry_point` and `public_api` evidence facts after the projector has
expanded its language policy and explicit choices. They are not the symbol
keys accepted by the evidence CLI. Obtain the manifest from the same projection
invocation or independently review those facts against your intended entry/API
choices; do not obtain expected selections from the check report itself.
The normal coverage default is 1000; the 500 above belongs to the synthetic
fixture with one resolved and one candidate site. It is not a completeness
claim. Even admitted markers only say “no path in the observed graph under
entry-point policy …”.

The viewer's `codemap.topology.v2` extension carries:

- top level: full snapshot `identity`, expected `selection`, qualified `absence`
  with checker/admission witnesses, `extraction_report`, what `--exclude` left
  out, unowned unresolved sites with their counts by category and file,
  policy relations with a summary per policy, and explicitly unknown
  unsupported-construct census. The HTML carries the counts only; the JSON
  lists every unowned site;
- each edge: its kind (`call`, `inherits`, `implements` or `extends`), original
  `edge_id`, full canonical `provenance`, safe `source_link`;
- each callable/container: original `node_id`, `symbol_key`, canonical kind and
  provenance, source link, and unresolved call-site annotations.

Directory and structural containment form the circle groups. Call endpoints
are callable leaves; inheritance and implementation endpoints are the circles
of their types. `flows` and `observations` are omitted; the consumer
produces no `dispatch`, `read` or `write` edges.

The viewer lifts each edge to the circles on screen: an end that is too small
to draw at the current zoom becomes its nearest drawn ancestor, and edges
that share both lifted ends are one arc whose width and number count them.
An arc is therefore a count of exact edges between the members of two
circles, never a relation of its own. Selecting an arc lists those edges
with their evidence. Selecting a circle lists every edge entering or leaving
it. Search shows each node's canonical kind and file.

For a Ruby snapshot the map also draws associations, dashed, as derived by
`policy:association/ruby` from the literal arguments of `belongs_to`,
`has_one`, `has_many`, `has_and_belongs_to_many` and `delegated_type` calls
in class bodies. They are policy relations, kept apart from `edges` in
`codemap.policy_relations` with the policy, the rule that found the class
(`class_name`, `inferred`, `through`, `polymorphic_as` or `delegated_type`)
and the declaration they rest on. A declaration relates two classes only
when it resolves to exactly one class in the snapshot;
`codemap.policies` counts the relations derived and what stayed unresolved
by reason. Custom inflections are not read.

This crate derives none of that. It reads the canonical graph only, and
`stele topology` hands it the relations each policy derived; `project`
takes them in `Scope::policies`, checks that they name nodes and files of the
snapshot, and places them.

Relative source links resolve
relative to the HTML file by default. Pass `--source-root /path/to/source`
with `--html` when storing the report elsewhere. The renderer computes escaped
relative destinations from that explicit directory; JSON source citations remain
unchanged. No source directory is inferred from snapshot identity.

Synthetic snapshots can be regenerated with
`cargo run -p codemap-topology --example synthetic-fixtures`. The integration
fixture reports are actual output from the evidence projector and checker,
and `cargo test -p stele --test topology` checks that complete workflow
through `stele code evidence` and `stele spec check`.

See `VENDORED.md`, `NOTICE`, and the two license files for viewer attribution
and the pinned upstream sources. Generated HTML retains the licenses.
