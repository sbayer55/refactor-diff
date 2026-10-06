#!/usr/bin/env bash
# Freeze the refactor-diff backend with PyInstaller into desktop/src-tauri/sidecar/, where
# tauri.conf.json picks it up as a bundled resource.
#
# Usage: scripts/build-sidecar.sh [--python VERSION]
#   --python  Python to build with (passed to uv), e.g. 3.13 if PyInstaller has trouble with
#             the interpreter uv picked for the project.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DESKTOP="$ROOT/desktop"
OUT="$DESKTOP/src-tauri/sidecar"
NAME="refactor-diff-sidecar"

UV_ARGS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --python) UV_ARGS+=(--python "$2"); shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

cd "$ROOT"
uv sync --group desktop "${UV_ARGS[@]}"
uv run --group desktop "${UV_ARGS[@]}" pyinstaller --noconfirm --clean \
  --distpath "$DESKTOP/dist" --workpath "$DESKTOP/build" "$DESKTOP/sidecar.spec"

rm -rf "$OUT/$NAME"
mkdir -p "$OUT"
cp -R "$DESKTOP/dist/$NAME" "$OUT/"

# Smoke test: proves the frozen interpreter boots and imports uvicorn, starlette, tree-sitter.
"$OUT/$NAME/$NAME" --help >/dev/null
echo "sidecar: $OUT/$NAME ($(du -sh "$OUT/$NAME" | cut -f1))"
