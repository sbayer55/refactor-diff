#!/bin/sh
# Stand-in for the frozen sidecar during development: runs the checkout's Python code so
# backend changes show up without re-freezing.
#
#   REFACTOR_DIFF_SIDECAR="$PWD/scripts/sidecar-dev.sh" npm run dev
exec uv run --project "$(dirname "$0")/../.." refactor-diff "$@"
