# Common tasks for refactor-diff. Run `just` to list them.

set shell := ["bash", "-euo", "pipefail", "-c"]

desktop := justfile_directory() / "desktop"

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

# Build the macOS app and dmg (freezes the sidecar first via beforeBuildCommand)
[group('desktop')]
desktop-build:
    cd "{{ desktop }}" && npm run build

# Remove the desktop app's build output and frozen sidecar
[group('desktop')]
desktop-clean:
    rm -rf "{{ desktop }}/build" "{{ desktop }}/dist" "{{ desktop }}/src-tauri/sidecar" "{{ desktop }}/src-tauri/target"
