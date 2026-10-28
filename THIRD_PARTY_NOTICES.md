# Third-party notices

The project license in `LICENSE` does not replace the grants below or the
component notices. Earlier MIT-licensed copies of Stele and LemmaSpec retain
their permissions. The earlier project notice is preserved at the end of
this file; its grant applies to those earlier copies and their covered portions.

## External benchmark corpora

The public corpus descriptors in `benchmarks/` identify separately obtained
upstream source and its license at the pinned commit. No Monica or lazygit
source code is included in this repository. The synthetic labelled fixtures
are separate from those application checkouts. Keep the upstream notices
with any fetched checkout; the descriptors link to the pinned license files.

## Black benchmark metadata

The public Black benchmark under `benchmarks/python/` records independently
frozen source locations, names, labels and hashes from
[psf/black](https://github.com/psf/black/tree/994ef7eeb18172178552daa1f36f001d156ad583).
Black is MIT licensed; its notice is preserved in
`benchmarks/python/LICENSE.black`. The anchors include verbatim decorator and
call-expression excerpts from Black under that MIT license, including
multi-line expressions, comments and string literals. Complete corpus source files are fetched
and materialized only in local prepared inputs, with upstream notices intact.

The separate Black operator recipe references pinned public CPython, mypy and
dependency archives. Their code is prepared only in local outputs. Python's
license remains in the runtime, mypy's license is copied beside the compiler,
and dependency notices remain with the extracted wheel contents.

## Excalidraw benchmark metadata

The public benchmark under `benchmarks/js_ts/` preserves independently frozen
names, source spellings and fragments, labels and hashes from
[excalidraw/excalidraw](https://github.com/excalidraw/excalidraw/tree/f39ac4a653335efaaaf9834bf28e9ffc1452cb59).
These source fragments are included under Excalidraw's MIT license, preserved
in `benchmarks/js_ts/LICENSE.excalidraw`. The logical dependency inventory
records names, hashes and aliases; dependency source is prepared locally from
the upstream lockfile and is not copied into this repository.

## Public Rust, Ruby and C++ benchmark labels

The public ripgrep, Mastodon and double-conversion descriptors and oracles under
`benchmarks/rust`, `benchmarks/ruby` and `benchmarks/cpp` identify exact upstream
commits. These independently authored labels record names and source locations;
complete application sources are obtained separately. Upstream notices are
retained as `LICENSE.ripgrep`, `LICENSE.mastodon` and
`LICENSE.double-conversion` beside the descriptors.

## Parser crate notice supplements

The published `tree-sitter-cpp` 0.23.4, `tree-sitter-typescript` 0.23.2 and
`tree-sitter` 0.25.10 crates omit their root MIT notice files. The ICU notice
in tree-sitter's Unicode source is a separate grant that also belongs in release
payloads. The full notices from the upstream commits recorded
in those crates are retained under `publication/third-party/`.
`publication/dependency-notices.json` binds each supplement to its exact crate
checksum, upstream commit, source URL and notice content hash. Preserve these
notices when distributing binaries that include those parsers.

## Gitleaks policy metadata

`toolchain/security/gitleaks-default-paths.json` records the global path
allowlist from Gitleaks v8.30.1's default configuration. Its upstream MIT
notice is retained as `toolchain/security/LICENSE.gitleaks`. The scanner binary
and its bundled notice are prepared only in local review outputs.

## Lemmalog

Portions of LemmaSpec are modified from Lemmalog 0.2.0 at commit
`74d428a2497066795f6328946457f22d713fcbd5`.

Source: <https://github.com/JordyZomer/lemmalog/tree/74d428a2497066795f6328946457f22d713fcbd5>

```text
MIT License

Copyright (c) 2026 Jordy

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Earlier project license

Earlier copies supplied under the following notice retain this grant. This
is not the grant for newly supplied project material covered by `LICENSE`.

```text
MIT License

Copyright (c) 2026 Morten Lochmann
Copyright (c) 2026 Jordy (portions derived from Lemmalog)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
