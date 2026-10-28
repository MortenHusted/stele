# codemap-topology

Render a validated Codemap snapshot as deterministic topology JSON and an offline
interactive HTML map. Only exact calls become edges. Unresolved sites remain
annotations, and a map without liveness input has no absence markers.

```sh
stele topology --snapshot snapshot.json --out topology.json --html TOPOLOGY.html
```

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

The viewer's `codemap.topology.v1` extension carries:

- top level: full snapshot `identity`, expected `selection`, qualified `absence`
  with checker/admission witnesses, `extraction_report`, unowned unresolved
  sites, and explicitly unknown unsupported-construct census;
- each edge: original `edge_id`, full canonical `provenance`, safe `source_link`;
- each callable/container: original `node_id`, `symbol_key`, canonical kind and
  provenance, source link, and unresolved call-site annotations.

Directory and structural containment form the circle groups. Edge endpoints
are callable leaves. `flows` and `observations` are omitted; the consumer
produces no `dispatch`, `read` or `write` edges. Relative source links resolve
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
