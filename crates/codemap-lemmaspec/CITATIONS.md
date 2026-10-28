# Rule-card citation evidence

`stele code evidence --checker citations --file snapshot.json
--rule-cards BUSINESS_RULES.md` accepts this narrow Markdown format:

```markdown
### RULE-001: Discount cap
**Source:** `src/discount.rs:3-5`
**Plain English:** The discount is capped at 20 percent.
```

The heading must contain `RULE-` followed by exactly three decimal digits,
`: `, and a nonempty title. Each card needs one nonempty `**Plain English:**`
line. Its source is one backtick-delimited `path:line[-line]`. Additional
card fields, summary tables, category headings, and prose are not interpreted.
Fenced Markdown examples are ignored. Duplicate rule IDs, duplicate Source or
Plain English fields, and malformed rule headings fail the input. Missing or
malformed Source fields remain cards with invalid locations, so they cannot
quietly disappear. A new Markdown heading ends the preceding card.

Paths must already be canonical repository-relative paths; line numbers are
positive and ranges inclusive. The checker evaluates source containment in a
validated pinned snapshot, using `Graph::locate`. File containers and import
placeholders do not qualify. The whole range must fit a declaration. An
unrelated range spanning two functions cannot select either function.

Every card produces separate `location_valid(rule, 0|1)` and
`semantic_support_unverified(rule)` results. A valid location means only that
recorded declaration spans contain the cited lines. Snapshot spans cannot
distinguish comments, blank lines, and executable code. Executable
classification is unsupported; claims about business behavior are always
reviewer-declared and semantically unverified, including a claim of 20 when
the source actually uses 5. No source is opened, parsed, or reproduced.

The `located` facts cite actual matched snapshot nodes and the snapshot ID.
Location status is classified under `declaration-containment/v1`; it cannot
upgrade the rule claim to a snapshot fact. Every semantic-unverified result
violates the checker's zero-unverified expectation: this tool never certifies
business rules. An empty catalog is rejected.

Citations require LemmaSpec 0.4 typed fact metadata. The other checkers retain
legacy output by default; `--readable` opts into 0.4 explicit evidence bases,
symbol labels, source links, and maintainer notes without changing lexical
IDs, relations, or expected counts.
