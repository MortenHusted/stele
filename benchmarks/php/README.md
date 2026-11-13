# Laravel source-policy gate

`laravel.json` is an additional PHP corpus contract. It pins the MIT-licensed
Laravel.io application at `b9d33e281c25c8f764ae144d95eb736e01c8fa0e`, its
Composer manifests, the independent source oracle, the static source profile,
the Rust toolchain and the Blade parser identity. `LICENSE.laravelio` retains
the application's license. Fetch its source separately and retain its notices.
The existing Monica PHP semantic-call contract and its oracle are unchanged.

Laravel.io's lockfile records Laravel `v13.42.0` at
`f5c01f6b63803f16f38ebb31622c4fea412bd022`. This is the tested framework source
pin. `laravel-profile.json` records the reviewed external class roots and finite
trait surfaces the policy assumes. These are named static source conventions;
they do not prove the identity of installed Composer packages, provider boot
behavior, runtime autoloading or runtime method dispatch.

## Frozen labels

`laravel-oracle.json` was labelled directly from public source before querying
the extractor. Source paths, hashes and declaration spans are retained; full
application source files are not copied into Stele. It requires:

- Eight complete selected Eloquent declarations, covering direct returned
  `belongsTo`, `hasOne`, `hasMany` and `belongsToMany` calls with literal class
  targets. Every selected declaration must produce exactly its labelled
  owner-to-model relationship, resting on the labelled call and method.
- Five literal Blade references in four complete selected templates: layout
  extension, an include, and anonymous component tags. Missing, duplicate,
  additional or wrong-target relations within those selected templates fail.
- Six selected omission controls, including polymorphic targets, a chained
  relation, an interpolated `includeIf` and namespaced mail views. No policy
  relation may rest on those excluded declarations or constructs.

The scorer also checks every PHP source hash on which these labels depend,
exact class bindings, clean source identity, and topology/snapshot identity.
PHP source parser errors refuse this framework gate. The Blade reader has a
separate source-reviewed boundary in `laravel-blade-scope.json`: nineteen
specific templates contain constructs the pinned grammar cannot read, including
custom one-shot directives, an application override of `@error`, literal `@`
text, dynamic/conditional HTML and extension syntax. Their paths, source hashes,
construct spans and compiler/grammar evidence were reviewed separately from the
relationship labels. Such a template contributes only one whole-file refusal
observation and no usable reference or relationship. The gate requires exactly
that reviewed boundary and matching refusal counts; any new failure, changed
source, partial relationship from a refused template, or failure in a required
selected template is rejected. The eight association and five template labels
are unchanged.
Blade contributes no canonical definitions, edges or measures, and no resolver
may report a resolved Blade call site. An unresolved summary cannot substitute
for a missing required relationship.

This is selected-declaration and selected-template coverage. It makes no
application-wide precision, recall, complete call-graph or dead-code claim.
The result retains the policy summaries and categorized unresolved counts for
inspection; these counts are observations, not an independently labelled
application-wide census.

## Run

Prepare Stele's pinned Rust toolchain, public crate cache and the Ruby TTY
runtime first. Do not install Laravel dependencies or execute the application
to prepare this gate. With a clean checkout at the exact public application
commit, run from the Stele checkout:

```sh
cargo fetch --locked
bin/public-corpus-benchmark --language php --corpus laravel \
  --repo /local/laravel.io --out /local/new-laravel-results
```

The output directory must be new and outside Git. This syntax-only framework
gate accepts neither `--bundle` nor `--cargo-home`; PHPactor and an application
PHP runtime are not needed. Cargo is offline during the gate. The command
builds the owned Stele binary, runs the declared synthetic extraction control,
then creates two independent source clones with the public origin restored.
Both PHP snapshots are validated and scored through the actual
`stele topology` policy output. Snapshot bytes and topology bytes must agree
across the two checkouts, and extraction must leave both sources clean.

Snapshots, topology JSON, logs, binary/input digests and the passing result stay
in the output directory. `result.json` is written only after all checks pass.
Changing the frozen oracle, profile assumptions or pin requires review before
accepting a new result; expected labels must not be rewritten from extractor
output.
