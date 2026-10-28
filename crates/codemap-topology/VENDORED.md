# Vendored topology viewer

Upstream: [anthropics/claude-plugins-official](https://github.com/anthropics/claude-plugins-official).
Pinned commit: `ad30d62cd52ad0d0af1beda1eab7f61c601045d0`.

| Original file | SHA-256 before modification |
| --- | --- |
| `plugins/code-modernization/assets/topology-viewer.html` | `2075246e9479df1f305d0190f8790f7542bd6d5ae38eabb9a3a761afa03a3ab0` |
| `plugins/code-modernization/LICENSE` | `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30` |

The [original template](https://github.com/anthropics/claude-plugins-official/blob/ad30d62cd52ad0d0af1beda1eab7f61c601045d0/plugins/code-modernization/assets/topology-viewer.html)
and [map schema](https://github.com/anthropics/claude-plugins-official/blob/ad30d62cd52ad0d0af1beda1eab7f61c601045d0/plugins/code-modernization/commands/modernize-map.md)
are Apache-2.0. The full license is retained as `LICENSE.apache-2.0`.

The template identifies its inline D3 subset as v7. The full
[D3 license](https://github.com/d3/d3/blob/v7.9.0/LICENSE) is retained as
`LICENSE.d3-ISC` (SHA-256 `3e6849627f74ff73c257a3ae1efb574015d94fc1035c05ec3c15805165efcbc4`).
Both licenses and the attribution notice are also embedded in the template
so each standalone generated HTML file includes them.

Modifications:

- retain `codemap` attributes on normalized nodes and edges;
- display complete snapshot identity, extraction scope and expected selections;
- show edge/node provenance and safe repository-relative, fragment or HTTPS links;
- optionally resolve source links against an explicit source directory;
- show unresolved calls as source annotations without target guesses;
- replace dead-end wording with qualified observed-graph absence and its witnesses;
- include attribution and license notices;
- fit the graph around controls on initial load/untouched resize and provide
  a mobile detail sheet; make admitted markers visibly dashed;
- cancel animated navigation when the user pans, zooms or resets the viewport.

The original inline D3 subset and interaction design are unchanged. JSON
injection retains upstream's `<`, `>` and `&` escaping and the offline CSP.
Upstream schema changes require deliberate re-vendoring and another license review.
