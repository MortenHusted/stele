#!/usr/bin/env bash
set -euo pipefail

# Hermetic R2 gate: run every Ruby semantic-call test that `cargo test`
# skips by default. The oracle and fail-closed suites require a conformant
# ruby-lsp tool bundle; without this script nothing in the default suite
# exercises them, so resolver/validator regressions would leave CI green.
#
# The bundle Gemfile is supplied by the operator via
# CODEMAP_RUBY_TOOL_BUNDLE, defaulting to the repository's own pinned
# toolchain bundle. Its lockfile must be byte-identical to the canonical
# pin set (toolchain/ruby/Gemfile.lock) or extraction fails closed.

readonly script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly codemap_repository="$(git -C "$script_directory/.." rev-parse --show-toplevel)"
readonly default_bundle="$codemap_repository/toolchain/ruby/Gemfile"

if [[ $# -gt 1 ]]; then
  echo "usage: $0 [bundle-gemfile]" >&2
  exit 2
fi

readonly bundle="${1:-${CODEMAP_RUBY_TOOL_BUNDLE:-$default_bundle}}"
if [[ ! -f "$bundle" ]]; then
  echo "gate failed: tool bundle not found at $bundle" >&2
  echo "  install a conformant bundle or export CODEMAP_RUBY_TOOL_BUNDLE" >&2
  exit 1
fi

export CODEMAP_RUBY_TOOL_BUNDLE="$bundle"

# The lockfile must be byte-identical to the canonical pin set. The
# extractor enforces this too; fail here for a faster, clearer error.
readonly expected_lock_sha="$(sha256sum "$codemap_repository/toolchain/ruby/Gemfile.lock" | cut -d' ' -f1)"
readonly actual_lock_sha="$(sha256sum "${bundle%.Gemfile}.lock" 2>/dev/null | cut -d' ' -f1 || true)"
if [[ "$actual_lock_sha" != "$expected_lock_sha" ]]; then
  echo "gate failed: bundle lockfile drifted from the canonical pin" >&2
  echo "  expected: $expected_lock_sha" >&2
  echo "  actual:   $actual_lock_sha" >&2
  exit 1
fi

echo "gate: using tool bundle $bundle"

cargo test --manifest-path "$codemap_repository/Cargo.toml" -p codemap \
  --test ruby_semantic_calls -- --ignored
cargo test --manifest-path "$codemap_repository/Cargo.toml" -p stele \
  --test code_ruby_bundle_unavailable -- --ignored
