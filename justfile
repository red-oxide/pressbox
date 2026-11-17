# SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
# SPDX-License-Identifier: GPL-3.0-only

set dotenv-load := false

# Cargo comes from the dev shell: use it directly inside `nix develop` (or
# direnv), and run it through `nix develop` otherwise.
cargo := if env("IN_NIX_SHELL", "") == "" { "nix develop --command cargo" } else { "cargo" }
dev := if env("IN_NIX_SHELL", "") == "" { "nix develop --command " } else { "" }

# List available recipes
default:
    @just --list

# Build everything with Cargo
build:
    {{ cargo }} build --workspace

# Run the tests
test:
    {{ cargo }} test --workspace

# Lint with Clippy, warnings as errors (as CI does)
lint:
    {{ cargo }} clippy --workspace --all-targets -- -D warnings

# Line coverage per file, from LLVM's instrumentation (no extra toolchain
# component); fails below 95%, as the flake's coverage check does. Built in
# target/tarpaulin so normal builds stay cached; `just coverage --out Html`
# also writes target/coverage/tarpaulin-report.html. Code marked
# `#[cfg(not(tarpaulin_include))]` isn't counted; each says why.
coverage *args:
    {{ cargo }} tarpaulin --workspace --engine llvm --fail-under 95 --target-dir target/tarpaulin --output-dir target/coverage {{ args }}

# Check advisories, licenses, duplicate crates and sources
deny:
    {{ cargo }} deny check

# Check only for security advisories (RustSec): vulnerable, unsound,
# unmaintained or yanked crates, as `cargo audit` would
audit:
    {{ cargo }} deny check advisories

# Time the database on 100k items (not a test)
bench:
    {{ cargo }} test --release -p pressboxd bench -- --ignored --nocapture

# Run the server with a local config (see "Development" in the README)
server config="pressbox.toml":
    {{ cargo }} run -p pressboxd -- -c {{ config }}

# Run pressboxctl, e.g. `just ctl -s ./pressbox.sock status`
ctl *args:
    {{ cargo }} run -q -p pressboxd --bin pressboxctl -- {{ args }}

# Run the terminal client, e.g. `just tui whoami`
tui *args:
    {{ cargo }} run -q -p pressbox -- {{ args }}

# Run the desktop client (reads client.json and the token like the TUI)
gui:
    {{ cargo }} run -q -p pressbox-iced

# Build a package with Nix: pressboxd (default), pressbox or pressbox-iced
package name="pressboxd":
    nix build .#{{ name }}

# Run the flake checks: builds, module checks, the NixOS VM test, reuse lint
check:
    nix flake check

# Format Rust, Nix and the other configured files
fmt:
    nix fmt

# Check formatting without modifying files (the flake's treefmt check, run on a copy)
fmt-check:
    nix build --no-link ".#checks.$(nix eval --impure --raw --expr builtins.currentSystem).treefmt"

# Update all flake inputs
update:
    nix flake update

# Update a specific flake input
update-input input:
    nix flake update {{ input }}

# Everything CI checks
verify: fmt-check lint test deny check

# From a clean, pushed master: check that the changelog entry for the version
# in Cargo.toml is dated, run `just verify`, then tag `vX.Y.Z`, push the tag and
# publish the GitHub release with that entry as its notes (`gh auth login`
# once first).
# Tag and publish a release of the version in Cargo.toml
release:
    #!/usr/bin/env bash
    set -euo pipefail
    fail() { echo "error: $*" >&2; exit 1; }
    version=$(awk -F'"' '/^\[workspace\.package\]/ { p = 1 } p && /^version/ { print $2; exit }' Cargo.toml)
    tag="v$version"
    heading=$(grep -F -m1 "## [$version] - " CHANGELOG.md) || fail "CHANGELOG.md has no entry for $version"
    date=${heading#"## [$version] - "}
    [[ $date =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || fail "CHANGELOG.md's entry for $version is dated '$date', not YYYY-MM-DD"
    [ -z "$(git status --porcelain)" ] || fail "the working tree has changes; commit or stash them first"
    [ "$(git branch --show-current)" = master ] || fail "release from master"
    git fetch --quiet origin master
    [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/master)" ] || fail "master isn't the same as origin/master; push or pull first"
    if git rev-parse -q --verify "refs/tags/$tag" > /dev/null || [ -n "$(git ls-remote --tags origin "refs/tags/$tag")" ]; then
        fail "$tag already exists"
    fi
    # The entry, up to the next one or the link references, with each paragraph
    # and list item on one line: release notes keep the changelog's line breaks.
    notes=$(mktemp)
    trap 'rm -f "$notes"' EXIT
    awk -v heading="$heading" '
        function flush() { if (cur != "") { emit(cur); cur = "" } }
        function emit(s) { if (n++) print ""; print s }
        !found { found = ($0 == heading); next }
        /^## \[/ || /^\[[^]]+\]: / { exit }
        /^[[:space:]]*$/ { flush(); next }
        /^#/ { flush(); emit($0); next }
        /^- / { flush(); cur = $0; next }
        { sub(/^[[:space:]]+/, ""); cur = cur == "" ? $0 : cur " " $0 }
        END { flush() }
    ' CHANGELOG.md > "$notes"
    [ -s "$notes" ] || fail "CHANGELOG.md's entry for $version is empty"
    just verify
    git tag -a "$tag" -m "pressbox $version"
    git push origin "$tag"
    {{ dev }}gh release create "$tag" --verify-tag --title "pressbox $version" --notes-file "$notes"

# Remove Cargo and Nix build output
clean:
    {{ cargo }} clean
    rm -f result result-*
