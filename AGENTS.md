<!--
SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
SPDX-License-Identifier: GPL-3.0-only
-->

# AGENTS.md

Instructions for coding agents working in this repository. The README and
`docs/` describe what pressbox is; this file covers how to work on it.

## Layout

- `crates/pressboxd`: the server (polling, SQLite storage, search, HTTP API),
  with `pressboxctl` in `src/bin`
- `crates/pressbox-api`: JSON types shared by the server and the Rust clients
- `clients/tui`: terminal client (ratatui), package `pressbox`
- `clients/iced`: desktop client (iced), package `pressbox-iced`
- `modules/`: the NixOS module (`services.pressbox`) and Home Manager module
  (`programs.pressbox`)
- `nix/`: flake parts (packages, checks, dev shell), loaded through devfmt's
  `mkFlake` and `importTree`
- The Apple and Android apps live in their own repos (red-oxide/pressbox-darwin,
  red-oxide/pressbox-android). Don't add mobile code here.

## Commands

Use the `just` recipes; they run Cargo through `nix develop` when you're not in
the dev shell.

- `just build`, `just test`, `just lint` (Clippy, warnings are errors)
- `just fmt` to format; `just fmt-check` to check without writing
- `just coverage`: fails below 95% line coverage per file
- `just deny`: advisories, licenses, duplicate crates
- `just openapi`: regenerates `docs/openapi.json` (a test fails when it's stale)
- `just check`: `nix flake check` (builds, module checks, NixOS VM test, REUSE
  lint, coverage); this is what CI runs
- `just verify`: everything CI checks. Run it before calling a change done.

## Rules

- **Licensing (REUSE).** Every new file needs an SPDX header with
  `SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>`. The server
  (`crates/pressboxd`, `modules/nixos.nix`) is `AGPL-3.0-only`; everything else
  is `GPL-3.0-only`; the logo is `CC-BY-SA-4.0`. Copy the header style from a
  neighboring file. Files that can't carry a header go in `REUSE.toml`.
- **Formatting** comes from red-oxide/devfmt. Don't add per-repo formatter
  overrides; if treefmt needs a fix, it belongs in devfmt.
- **Coverage.** New code comes with tests that keep each file at or above 95%.
  Only mark code `#[cfg(not(tarpaulin_include))]` when it genuinely can't be
  tested, with a comment saying why.
- **The HTTP API is a contract.** pressbox-darwin, pressbox-android, the clients
  here and the Birdland bar all depend on `/api/v1`. Additive changes are fine;
  anything incompatible needs a new API version and a discussion first.
- **OpenAPI.** `docs/openapi.json` is generated from the server's routes
  (utoipa, `crates/pressboxd/src/openapi.rs`) and the `pressbox-api` types.
  Never edit it by hand: change the code, annotate new routes and types for
  utoipa, then run `just openapi` and commit the result. Update `docs/api.md`
  too when a route is added or its behavior changes.
- **Database schema.** Change it only by appending a migration to the list in
  `crates/pressboxd/src/db.rs`. Never edit an existing migration.
- **Module options.** If you add or change an option in `modules/`, update
  `docs/server.md` or `docs/clients.md` to match.
- **Dependencies.** Don't add crates or pins, or run `just update` (`tack
  update`) / `cargo update`, unless asked. Nix inputs are tack pins in
  `.tack/pins.toml`, not flake inputs. A new crate must pass `just deny`; don't loosen `deny.toml` to
  make it pass.
- **Changelog.** Record user-visible changes in `CHANGELOG.md` (Keep a
  Changelog, CalVer `YY.MINOR.PATCH`). Leave version bumps, tags and
  `just release` to the maintainer.
