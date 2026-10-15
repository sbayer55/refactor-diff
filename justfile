# Common tasks for refactor-diff. Run `just` to list them.

set shell := ["bash", "-euo", "pipefail", "-c"]

desktop := justfile_directory() / "desktop"
wheels_lock := justfile_directory() / "crates/refactor-diff-server/python/wheels.lock"
bundle  := justfile_directory() / "target/release/bundle"
extension_out := justfile_directory() / "target/browser-extension"

# Pure-Python dependencies bundled into the binary for Python code navigation (`just pin-python`).
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

# Pin jedi/parso in python/wheels.lock to the versions above (needs network; commit the result)
pin-python:
    #!/usr/bin/env bash
    set -euo pipefail
    {
        echo "# Pure-Python wheels bundled into the binary for Python code navigation, fetched by build.rs."
        echo "# One wheel per line: <url> <sha256>. Regenerate with \`just pin-python\`."
        for spec in "jedi/{{ jedi_version }}" "parso/{{ parso_version }}"; do
            curl -fsSL "https://pypi.org/pypi/$spec/json" | python3 -c 'import json, sys
    whl = [u for u in json.load(sys.stdin)["urls"] if u["filename"].endswith("-py2.py3-none-any.whl") or u["filename"].endswith("-py3-none-any.whl")]
    print(whl[0]["url"], whl[0]["digests"]["sha256"])'
        done
    } > "{{ wheels_lock }}.tmp"
    mv "{{ wheels_lock }}.tmp" "{{ wheels_lock }}"
    cat "{{ wheels_lock }}"

# ---------------------------------------------------------- Desktop app ---

# Install the desktop app's npm dependencies (the Tauri CLI)
[group('desktop')]
desktop-setup:
    cd "{{ desktop }}" && npm install

# Regenerate src-tauri/icons (checked in) after changing app-icon.svg
[group('desktop')]
desktop-icons:
    cd "{{ desktop }}" && npm run icons

# Install the Tauri CLI when node_modules is missing or older than package-lock.json
[private]
desktop-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v npm >/dev/null || { echo "npm not found: install Node.js (e.g. brew install node)" >&2; exit 1; }
    cd "{{ desktop }}"
    if [ ! -x node_modules/.bin/tauri ] || [ package-lock.json -nt node_modules/.package-lock.json ]; then
        npm install
    fi

# Run the desktop app with the in-process server (RUST_LOG=debug for more output)
[group('desktop')]
desktop-dev: desktop-deps
    cd "{{ desktop }}" && npm run dev

# Lint the desktop crate (not part of `rs-lint`: it needs Tauri's system libraries)
[group('desktop')]
desktop-lint:
    cargo clippy -p refactor-diff-desktop --all-targets -- -D warnings

# Build the macOS app and dmg for this machine's architecture
[group('desktop')]
desktop-build: desktop-deps desktop-clean-bundle
    cd "{{ desktop }}" && npm run build

# Build a universal (Apple Silicon + Intel) app and dmg
[group('desktop')]
desktop-build-universal: desktop-deps desktop-clean-bundle
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

# ------------------------------------------------------ Browser extension ---

# Run the browser extension's tests
[group('extension')]
extension-test:
    node --test browser-extension/*.test.js

# Build the loadable extension into target/browser-extension (Load unpacked in chrome://extensions)
[group('extension')]
extension-build:
    rm -rf "{{ extension_out }}"
    mkdir -p "{{ extension_out }}"
    rsync -a --exclude '*.test.js' --exclude 'README.md' --exclude '.*' \
      "{{ justfile_directory() }}/browser-extension/" "{{ extension_out }}/"
    @echo "Built {{ extension_out }}"
    @echo "Load it in chrome://extensions → Developer mode → Load unpacked (or reload its card)."

# Zip the built extension into target/ (for sharing)
[group('extension')]
extension-zip: extension-build
    rm -f "{{ justfile_directory() }}/target/refactor-diff-extension.zip"
    cd "{{ extension_out }}" && zip -r -X "{{ justfile_directory() }}/target/refactor-diff-extension.zip" .
