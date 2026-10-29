# Public Ruby corpus gate

`mastodon.json` pins mastodon/mastodon v4.7.3 at
`8ff11c2bb7b9f0714e947b10fa3629c6542c7121`. Mastodon is AGPL-3.0-or-later;
`LICENSE.mastodon` preserves its upstream license. The oracle contains public
symbol, span and hash metadata, without copied method bodies. Fetch the source
separately and retain its notices. File hashes occupy their own JSON lines:
a same-line `signature_authentication.rb` hash triggered a verified Gitleaks
false positive; this layout scans clean without suppressions.

Discourse remains a possible additional
corpus; this gate accepts only the Mastodon pin.

`mastodon-oracle.json` was labelled from public source and syntax-only anchors,
then independently reviewed before the first Ruby-LSP application query. Its
18 complete production callers cover controllers, models, concerns, services,
helpers and library code. All 30 required source-span edges must match exactly,
with no extra selected edges. Nine explicit forbidden targets include seven
same-spelling decoys outside the verified receiver ancestry or method kind.
Seven noncanonical sites must remain excluded. Every labelled site must appear
exactly once as an observation, whose category agrees with edge admission.
This is selected-caller coverage, without application-wide precision or recall.

R2 admits implicit-self instance calls within the receiver's instance ancestry,
constant-receiver singleton calls within its singleton ancestry, and verified
direct construction as described below. A host
method called from an included concern (`Account#deleted?`) and a method reached
through an untyped response (`Request::ClientLimit#body_with_limit`) are known
runtime targets that R2 cannot verify. Their explicit exclusions describe
coverage limits. Rails-generated predicates and instance-variable receivers
also remain outside the selected canonical calls. The constant-receiver
`SelfDestructHelper.self_destruct?` must reach the singleton method, excluding
the same-spelling instance method and a false self-edge.

Prepare Ruby 4.0.7 and Bundler 4.0.18. Use the absolute installation
path for that Ruby version, including for subprocesses; a moving `latest`
alias can select an incompatible interpreter for native gems. Put the real
Cargo installation directory before version-manager shims too: a Cargo shim
can reset the Ruby environment when launching tests. The examples use Rust
1.99.0, matching the prepared acceptance operator. Create a separate operator directory
outside the application and copy Stele's `toolchain/ruby/Gemfile` and
`Gemfile.lock` there. Install exactly that locked gem set; never use the
application's Gemfile or install its dependencies:

```sh
export PATH="/local/ruby-4.0.7/bin:/local/rust-1.99.0/bin:$PATH"
ruby --version
cargo --version
gem install bundler:4.0.18
bundle _4.0.18_ --version
mkdir -p /local/ruby-operator
cp toolchain/ruby/Gemfile toolchain/ruby/Gemfile.lock /local/ruby-operator/
BUNDLE_GEMFILE=/local/ruby-operator/Gemfile bundle _4.0.18_ config set --local path ./gems
BUNDLE_GEMFILE=/local/ruby-operator/Gemfile BUNDLE_FROZEN=true bundle _4.0.18_ install
```

The lock pins ruby-lsp 0.26.10, RBS 4.1.2 and their dependencies and checksums.
The runtime switches to the lockfile's Bundler version; that version must be
installed even when the default Bundler is newer. Preparation is not semantic
evidence. With a clean checkout at the public pin,
run from Stele:

```sh
PATH="/local/ruby-4.0.7/bin:/local/rust-1.99.0/bin:$PATH" bin/public-corpus-benchmark --language ruby --repo /local/mastodon --bundle /local/ruby-operator --out /local/new-ruby-results
```

`--bundle` takes the operator directory; the runner passes its `Gemfile` to
`CODEMAP_RUBY_TOOL_BUNDLE`. It builds Stele, requires an exact named passing
witness for the eight-edge R2 fixture (the original seven plus the reviewed
`Sub.new.receiver_method` edge), and executes both CLI refusal
tests (absent bundle and drifted lockfile), plus the qualified-superclass R2
regression, the constructor-chain positive/refusal oracle and the pinned
absolute-body provider witness. It then creates two independent
source clones, forbids syntax calls, validates semantic snapshots, requires
more than 1,000 Ruby files and nonempty canonical calls, checks source pins and
selected labels, and requires byte-identical output with one and three
Ruby-LSP definition workers. The gate checks the bundle digest and tool
versions in each snapshot's identity. It does not boot Rails or execute the
application.

Snapshots, counts, digests and logs stay in the new output directory outside
Git. `result.json` is written only after every gate passes. Hosted CI tests the
shared scorer and command refusals; this native corpus gate requires its
separately prepared operator.

The first application run against the reviewed oracle refused two inherited
calls. Ruby-LSP supplied the correct targets, but extractor 0.5.2 discarded the
superclass reference's namespace when establishing ancestry. The same resolver
also mapped qualified superclass and mixin references to unrelated same-name
classes or modules, including false self-edges. Extractor 0.5.3 preserves lexical
scope and written qualification, admits superclass targets only as classes and
mixin targets only as modules, and omits unresolved or ambiguous references. A class reopened across files
has multiple per-file nodes and remains ambiguous as a heritage target; that
omission is a coverage limit. Compact class/module names stay one lexical
scope element and do not introduce the written namespace as outer nesting.
Its separate synthetic controls retain positive qualified, lexical and absolute
lookups alongside namespace decoys, kind mismatches and ambiguity. Ruby
snapshots accepted before this repair may contain wrong heritage and canonical
call edges; regenerate them. The frozen application labels are unchanged.

Receiver-qualified mixin calls such as `Other.include Feature` remain
observations: they establish no edge from the enclosing class. The real
`Other` receiver may include that module at run time; omitting its edge is
a coverage limit. Implicit and explicit-self mixins retain the enclosing
lexical receiver. `extend self` creates no heritage self-edge.

The pinned Ruby-LSP can index a root-only repository file twice, under `/`
and `//` URI spellings. Admission coalesces owned answers only when both
paths canonicalize to the same file and their full selection ranges agree.
Different ranges, different owned files and mixed owned/external answers
stay noncanonical `Candidate` observations.

Extractor 0.5.4 gives absolute class/module declarations their global identity
while preserving the enclosing lexical nesting. Methods and nested definitions
use that semantic owner; superclass and mixin references still use Ruby's
lexical lookup. Regenerate older Ruby snapshots containing absolute declarations.

Ruby-LSP 0.26.10 has a separate coverage limit: implicit-self calls inside an
absolute-path class or module nested in another namespace are `Unmapped`.
Its `lib/ruby_lsp/node_context.rb:33–34,60–63` joins raw nesting entries into
`Outer::::Top`; `lib/ruby_lsp/type_inferrer.rb:137` uses that invalid self type.
The source oracle retains all seven intended call pairs. A deterministic test
passes their definition answers through production admission, while the native
witness explicitly pins this provider's one answered call and six empty answers
at exact sites. A provider update must revisit that witness. No missing answer
is replaced with a guessed edge. This provider omission does not affect the
selected public application's call labels; its nested absolute body has no
implicit call sites.

Extractor 0.5.6 admits a direct `Widget.new(...).perform` only when Ruby-LSP
supplies a unique in-repository instance-method target within the class's
verified instance ancestry, and the inner `new` separately resolves to the
default `Class#new` declaration. That provider is pinned to public RBS 4.1.2
`core/class.rbs`, SHA-256
`36c7659b5c6a936881a49ca4d1aade1c47198bf93ac33ded7118fb45e6e80ef6`,
at its exact declaration range. Custom/inherited/extended constructors,
unresolved or external heritage providers, all class extensions, module receivers and
ambiguous reopened classes remain observations. Source parse errors or a
constructor/mixin receiver whose class cannot be established disable new
constructor admission; existing call regimes are unaffected. `initialize` may
return another value, but the default `Class#new` still returns its allocated
instance. Local variables, arbitrary factories, and further chained return
types are not inferred; runtime metaprogramming is outside this static profile.
The separately prepared native provider test also rejects missing/multiple
answers and wrong provider files, ranges or bytes. The frozen Mastodon
application oracle is unchanged.

Class extensions remain excluded even when their immediate module is known:
that module can contribute `new` through its own include/prepend ancestry.
This profile does not infer that full constructor lookup chain.
Constructor uncertainty also propagates through included/prepended modules;
explicit `included`, `prepended`, `extended` or `inherited` hooks stay unverified.
That includes source declarations nested in a concern-style inclusion block.
Reflective definitions of constructor or callback methods and evaluated code
also make their receivers uncertain. A dynamic method name or receiver cannot
establish that the constructor stayed unchanged. Literal non-constructor method
names do not add that uncertainty. Native controls retain reflective factories,
evaluated factories and constructor aliases as noncanonical cases.
The profile also refuses instance singleton replacements, including those
installed during initialization, and literal reflective operations forwarded
through `send`, `public_send` or `__send__`. A known method name mutated on an
untyped instance receiver prevents constructor edges to that method; it does
not prevent edges to unaffected method names.

```sh
cargo test --locked -p codemap --lib ruby_lsp::tests::constructor_provider_requires_the_pinned_class_new_declaration -- --ignored --exact
```

Source-side heritage references bind to the containing declaration in their own
file and span, so a separate reopening does not discard its superclass or mixins.
Targets still require an unambiguous language-aware identity; this does not merge
reopenings into a single runtime class.

Methods declared inside arbitrary call blocks retain lexical declarations and
metrics, but their runtime owners are unverified. They cannot be canonical call
targets, and implicit or explicit `self` calls inside those blocks remain
observations. An explicit class or module body establishes a fresh context.
The gate includes a synthetic redirected-definition and rebound-self oracle.
This does not infer ActiveSupport::Concern class methods.

A configured Ruby tool bundle that fails probing reports the attempted Gemfile
and the subprocess failure, including stderr or timeout. Check that the
Ruby executable used by Stele matches the one that installed the native gems;
setting the bundle path alone cannot repair an incompatible Ruby runtime.

The built-in architecture checker selects physical files containing shipped
callable definitions. A Rails host file containing only a class declaration,
includes, associations and scopes is not a logical selector for methods in its
concerns. Select the files that define those methods, or write an explicit
hierarchy specification; a declaration-only host is refused rather than
reported clean from an empty call set. Call-coverage admission still applies.
