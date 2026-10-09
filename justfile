# Common tasks for refactor-diff. Run `just` to list them.

set shell := ["bash", "-euo", "pipefail", "-c"]

desktop := justfile_directory() / "desktop"
bundle := desktop / "src-tauri/target/aarch64-apple-darwin/release/bundle"
# tauri-build checks bundled resources exist; unit tests don't need the frozen sidecar.
no_sidecar := '{"bundle":{"resources":[]}}'

# List recipes
default:
    @just --list --unsorted

# ---------------------------------------------------------------- Python ---

# Install the Python environment with dev dependencies
setup:
    uv sync

# Run the test suite (extra args go to pytest, e.g. `just test -k typescript`)
test *args:
    uv run pytest {{ args }}

# Lint and check formatting
lint:
    uv run ruff check .
    uv run ruff format --check .

# Format code and apply safe lint fixes
fmt:
    uv run ruff format .
    uv run ruff check --fix .

# Lint, format check and tests: what should pass before a PR
check: lint test

# Run the CLI from the checkout (e.g. `just run main..HEAD --no-browser`)
run *args:
    uv run refactor-diff {{ args }}

# Install the CLI as a uv tool from this checkout
install:
    uv tool install --force .

# Build the sdist and wheel into dist/
build:
    uv build

# Remove caches and build output
clean:
    rm -rf build dist .pytest_cache .ruff_cache .coverage htmlcov
    find . -path ./.venv -prune -o -path ./desktop/node_modules -prune -o -name __pycache__ -type d -print0 | xargs -0 rm -rf

# ---------------------------------------------------------- Desktop app ---

# Install the desktop app's npm dependencies
[group('desktop')]
desktop-setup:
    rustup target add aarch64-apple-darwin
    cd "{{ desktop }}" && npm install

# Generate src-tauri/icons from app-icon.svg (needed once before the first build)
[group('desktop')]
desktop-icons:
    cd "{{ desktop }}" && npm run icons

# Freeze the Python backend with PyInstaller (e.g. `just desktop-sidecar --python 3.13`)
[group('desktop')]
desktop-sidecar *args:
    "{{ desktop }}/scripts/build-sidecar.sh" {{ args }}

# Run the desktop app against the frozen sidecar
[group('desktop')]
desktop-dev:
    cd "{{ desktop }}" && npm run dev

# Run the desktop app against the checkout's Python code, without re-freezing
[group('desktop')]
desktop-dev-live:
    cd "{{ desktop }}" && REFACTOR_DIFF_SIDECAR="{{ desktop }}/scripts/sidecar-dev.sh" npm run dev

# Build the Apple Silicon app and dmg (freezes the sidecar first via beforeBuildCommand)
[group('desktop')]
desktop-build: desktop-clean-bundle
    cd "{{ desktop }}" && npm run build

# Open the built app
[group('desktop')]
desktop-open-app:
    open "{{ bundle }}/macos/Refactor Diff.app"

# Run the desktop shell's Rust unit tests
[group('desktop')]
desktop-test:
    cd "{{ desktop }}/src-tauri" && TAURI_CONFIG='{{ no_sidecar }}' cargo test

# Check the desktop shell's formatting and clippy lints
[group('desktop')]
desktop-lint:
    cd "{{ desktop }}/src-tauri" && cargo fmt --check
    cd "{{ desktop }}/src-tauri" && TAURI_CONFIG='{{ no_sidecar }}' cargo clippy --all-targets -- -D warnings

# Remove scratch disk images an interrupted dmg build left behind, and detach their volumes
[group('desktop')]
desktop-clean-bundle:
    hdiutil info | awk -v t="{{ desktop }}/src-tauri/target/" \
      '/^image-path/ { sub(/^image-path *: /, ""); keep = index($0, t) == 1 } keep && /^\/dev\/disk[0-9]+\t/ { print $1; keep = 0 }' \
      | while read -r disk; do diskutil eject "$disk" || true; done
    find "{{ desktop }}/src-tauri/target" -path '*/bundle/macos/rw.*.dmg' -delete 2>/dev/null || true

# Remove the desktop app's build output and frozen sidecar
[group('desktop')]
desktop-clean:
    rm -rf "{{ desktop }}/build" "{{ desktop }}/dist" "{{ desktop }}/src-tauri/sidecar" "{{ desktop }}/src-tauri/target"
