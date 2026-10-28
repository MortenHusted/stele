#!/usr/bin/env bash
# Compatibility entrypoint for the Ruby release command.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec "${repo_root}/bin/package-release" "$@"
