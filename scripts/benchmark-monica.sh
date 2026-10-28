#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <monica-repository> [new-output-directory-outside-git]" >&2
  exit 2
fi
readonly script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$script_directory/../bin/public-corpus-benchmark" \
  --language php --repo "$1" \
  --bundle "${CODEMAP_PHP_TOOL_BUNDLE:-$script_directory/../toolchain/php}" \
  --out "${2:-${TMPDIR:-/tmp}/stele-monica-benchmark-$$}"
