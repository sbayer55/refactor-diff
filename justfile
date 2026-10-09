# Common tasks for refactor-diff. Run `just` to list them.

set shell := ["bash", "-euo", "pipefail", "-c"]

desktop := justfile_directory() / "desktop"
vendor  := justfile_directory() / "crates/refactor-diff-server/python/vendor"
bundle  := justfile_directory() / "target/release/bundle"

# Pinned pure-Python dependencies bundled into the binary for Python code navigation.
jedi_version  := "0.20.0"
parso_version := "0.8.7"

# List recipes
default:
    @just --list --unsorted

# ------------------------------------------------------------------ Rust ---

# Install the toolchain components and the desktop app's npm dependencies
setup:
    rustup component add rustfmt clippy
    cd "{{ desktop }}" && npm install

# Run the test suite (extra args go to cargo test, e.g. `just test -p refactor-diff-core seqmatch`)
test *args:
    cargo test {{ args }}

# Lint and check formatting
lint:
    cargo fmt --all --check
    cargo clippy --all-targets -- -D warnings

# Format code
fmt:
    cargo fmt --all

# Lint, format check and tests: what should pass before a PR
check: lint test

# Run the CLI from the checkout (e.g. `just run main..HEAD --no-browser`)
run *args:
    cargo run -p refactor-diff -- {{ args }}

# Install the CLI into ~/.cargo/bin from this checkout
install:
    cargo install --path crates/refactor-diff --locked

# Build the release binary (target/release/refactor-diff)
build:
    cargo build --release -p refactor-diff

# Remove build output
clean:
    cargo clean
    rm -rf "{{ desktop }}/src-tauri/gen"

# Refresh the vendored jedi/parso to the pinned versions (needs network; commit the result)
vendor-python:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    python3 -m pip download --no-deps --only-binary=:all: --dest "$tmp" \
        "jedi=={{ jedi_version }}" "parso=={{ parso_version }}"
    rm -rf "{{ vendor }}"
    mkdir -p "{{ vendor }}/LICENSES"
    for whl in "$tmp"/*.whl; do unzip -q "$whl" -d "{{ vendor }}"; done
    find "{{ vendor }}" -name '__pycache__' -type d -prune -exec rm -rf {} +
    for info in "{{ vendor }}"/*.dist-info; do
        name="$(basename "$info" | cut -d- -f1)"
        for f in LICENSE.txt AUTHORS.txt; do [ -f "$info/$f" ] && cp "$info/$f" "{{ vendor }}/LICENSES/$name-$f"; done
        rm -rf "$info"
    done
    { echo "jedi=={{ jedi_version }}"; echo "parso=={{ parso_version }}"; } > "{{ vendor }}/VERSIONS"
    du -sh "{{ vendor }}"

# ---------------------------------------------------------- Desktop app ---

# Install the desktop app's npm dependencies (the Tauri CLI)
[group('desktop')]
desktop-setup:
    cd "{{ desktop }}" && npm install

# Generate src-tauri/icons from app-icon.svg (needed once before the first build)
[group('desktop')]
desktop-icons:
    cd "{{ desktop }}" && npm run icons

# Run the desktop app with the in-process server (RUST_LOG=debug for more output)
[group('desktop')]
desktop-dev:
    cd "{{ desktop }}" && npm run dev

# Lint the desktop crate (not part of `rs-lint`: it needs Tauri's system libraries)
[group('desktop')]
desktop-lint:
    cargo clippy -p refactor-diff-desktop --all-targets -- -D warnings

# Build the macOS app and dmg for this machine's architecture
[group('desktop')]
desktop-build: desktop-clean-bundle
    cd "{{ desktop }}" && npm run build

# Build a universal (Apple Silicon + Intel) app and dmg
[group('desktop')]
desktop-build-universal: desktop-clean-bundle
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    cd "{{ desktop }}" && npm run tauri -- build --target universal-apple-darwin

# Open the built app
[group('desktop')]
desktop-open-app:
    open "{{ bundle }}/macos/Refactor Diff.app"

# Run the desktop shell's Rust unit tests
[group('desktop')]
desktop-test:
    cargo test -p refactor-diff-desktop

# Remove scratch disk images an interrupted dmg build left behind, and detach their volumes
[group('desktop')]
desktop-clean-bundle:
    hdiutil info | awk -v t="{{ justfile_directory() }}/target/" \
      '/^image-path/ { sub(/^image-path *: /, ""); keep = index($0, t) == 1 } keep && /^\/dev\/disk[0-9]+\t/ { print $1; keep = 0 }' \
      | while read -r disk; do diskutil eject "$disk" || true; done
    find "{{ justfile_directory() }}/target" -path '*/bundle/macos/rw.*.dmg' -delete 2>/dev/null || true

# Remove the desktop app's build output
[group('desktop')]
desktop-clean:
    cargo clean -p refactor-diff-desktop
    rm -rf "{{ desktop }}/src-tauri/gen"
