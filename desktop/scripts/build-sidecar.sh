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

die() { echo "build-sidecar: $*" >&2; exit 1; }

# The app is Apple Silicon only: refuse to freeze an Intel backend by accident (a Rosetta
# shell, or an x86_64 Python that uv found in /usr/local).
[[ "$(uname -s)" == Darwin ]] || die "macOS only"
[[ "$(sysctl -in sysctl.proc_translated)" != 1 ]] || die "this shell runs under Rosetta; use a native arm64 terminal"
[[ "$(uname -m)" == arm64 ]] || die "Apple Silicon only (this Mac is $(uname -m))"

cd "$ROOT"
uv sync --group desktop "${UV_ARGS[@]}"
py_arch="$(uv run --group desktop "${UV_ARGS[@]}" python -c 'import platform; print(platform.machine())')"
[[ "$py_arch" == arm64 ]] || die "uv picked an $py_arch Python; pass --python, or set UV_PYTHON_PREFERENCE=only-managed"
uv run --group desktop "${UV_ARGS[@]}" pyinstaller --noconfirm --clean \
  --distpath "$DESKTOP/dist" --workpath "$DESKTOP/build" "$DESKTOP/sidecar.spec"

rm -rf "$OUT/$NAME"
mkdir -p "$OUT"
cp -R "$DESKTOP/dist/$NAME" "$OUT/"

# Every native file must run on arm64 (universal binaries are fine).
bad=()
while IFS= read -r -d '' f; do
  archs="$(lipo -archs "$f" 2>/dev/null)" || continue  # not Mach-O
  [[ " $archs " == *" arm64 "* ]] || bad+=("$f ($archs)")
done < <(find "$OUT/$NAME" -type f \( -perm -u+x -o -name '*.so' -o -name '*.dylib' \) -print0)
((${#bad[@]} == 0)) || die "not built for arm64:$(printf '\n  %s' "${bad[@]}")"

# Smoke test: proves the frozen interpreter boots and imports uvicorn, starlette, tree-sitter.
"$OUT/$NAME/$NAME" --help >/dev/null
echo "sidecar: $OUT/$NAME ($(du -sh "$OUT/$NAME" | cut -f1))"
