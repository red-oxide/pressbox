<!--
SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
SPDX-License-Identifier: GPL-3.0-only
-->

<p align="center"><img src="assets/logo.svg" width="128" alt=""></p>

# pressbox

A self-hosted RSS/Atom/JSON Feed aggregator. You declare feeds (and the people
using them) in your NixOS configuration, a small Rust server polls them, and
terminal and desktop clients on NixOS, an
[iPhone, iPad and Mac app](https://github.com/red-oxide/pressbox-darwin) and an
[Android app](https://github.com/red-oxide/pressbox-android) share read and
starred state through its API. Each client is paired with a short code; there is
no website and no shared secret.
[Birdland](https://github.com/red-oxide/birdland)'s bar can show the unread
count.

> [!NOTE]
> pressbox is in its early days. Expect rough edges, and configuration and
> behavior that may change between versions.

```
NixOS config ──▶ pressbox.toml ──▶ pressboxd ──▶ SQLite (items, search index, devices)
(services.pressbox.feeds)            │ HTTP API, one token per device
                   ┌───────────────┼────────────────┐
             TUI, desktop    Birdland bar    Apple, Android apps
                           pressboxctl (control socket, on the server)
```

| Part | Path | What it is |
| ------------------- | ----------------------------------------------------------------- | --------------------------------------------------- |
| `pressboxd` | `crates/pressboxd` | Server: polling, storage, search, API |
| `pressboxctl` | `crates/pressboxd/src/bin` | Pairing, devices, backups, OPML |
| `pressbox-api` | `crates/pressbox-api` | JSON types shared by the Rust crates |
| `pressbox` | `clients/tui` | Terminal client (ratatui) |
| `pressbox-iced` | `clients/iced` | Desktop client (iced) |
| Apple app | [pressbox-darwin](https://github.com/red-oxide/pressbox-darwin) | SwiftUI client (iPhone, iPad, Mac), in its own repo |
| Android app | [pressbox-android](https://github.com/red-oxide/pressbox-android) | Jetpack Compose client, in its own repo |
| NixOS module | `modules/nixos.nix` | `services.pressbox` |
| Home Manager module | `modules/home-manager.nix` | `programs.pressbox` |

## Documentation

- [Server (NixOS)](./docs/server.md): installing, feeds, users, options, how
  feeds are fetched, systemd
- [Pairing devices](./docs/pairing.md): giving each client its own token
- [pressboxctl](./docs/pressboxctl.md): the admin CLI on the server
- [Clients (Home Manager)](./docs/clients.md): the terminal and desktop clients
  and the Birdland bar
- [API](./docs/api.md): the HTTP API clients use

## Apple and Android apps

The mobile and Mac apps are in their own repos, with their own releases, and
talk to the server through API v1; see their READMEs for building them and what
they can do.

- [pressbox-darwin](https://github.com/red-oxide/pressbox-darwin) (formerly
  pressbox-ios): iPhone and iPad on iOS 18, and Mac on macOS 15.
- [pressbox-android](https://github.com/red-oxide/pressbox-android): Android 12
  or later.

## Project layout

```
pressbox/
├── crates/                        the server side
│   ├── pressboxd/                 server and pressboxctl (AGPL-3.0-only)
│   │   └── src/
│   │       ├── main.rs            startup, scheduler, status line
│   │       ├── config.rs          pressbox.toml: feeds, users, paths
│   │       ├── db.rs              SQLite schema, migrations, queries
│   │       ├── fetch.rs           polling, backoff, parsing, full articles
│   │       ├── sanitize.rs        HTML cleaning, search text
│   │       ├── api.rs             HTTP API for clients (/api/v1)
│   │       ├── admin.rs           control-socket API for pressboxctl
│   │       ├── pairing.rs         device tokens and pairing codes
│   │       ├── systemd.rs         notify, watchdog, sockets, journal
│   │       └── bin/pressboxctl.rs admin CLI, feed discovery, OPML
│   └── pressbox-api/              JSON types shared with the clients
├── clients/
│   ├── tui/                       pressbox, terminal client
│   │   └── src/
│   │       ├── main.rs            CLI: run, pair, approve, whoami
│   │       ├── client.rs          HTTP client, client.json, token
│   │       ├── app.rs             state and keys (tested without a terminal)
│   │       └── ui.rs              drawing
│   ├── iced/                      pressbox-iced, desktop client
│   │   └── src/
│   │       ├── main.rs            window settings
│   │       ├── app.rs             state, messages and updates
│   │       ├── ui.rs              pairing screen and the three panes
│   │       ├── api.rs             async HTTP client
│   │       ├── config.rs          client.json and token, shared with the TUI
│   │       └── theme.rs           Birdland and Birdland Light
├── modules/
│   ├── nixos.nix                  services.pressbox (AGPL-3.0-only)
│   └── home-manager.nix           programs.pressbox
├── nix/                           flake-parts modules, loaded by devfmt
│   ├── rust.nix                   Rust toolchain (fenix) and crane
│   ├── packages.nix               pressboxd, pressbox, pressbox-iced (crane), Clippy
│   ├── modules.nix                exports the NixOS and Home Manager modules
│   ├── checks.nix                 module checks, NixOS VM test, reuse lint
│   └── devshell.nix               nix develop
├── assets/                        logo and app icon (CC-BY-SA-4.0)
├── LICENSES/                      license texts (REUSE)
├── .github/workflows/             flake check, cargo-deny, lock updates
├── flake.nix, Cargo.toml, REUSE.toml, justfile
├── docs/                          user documentation: server, pairing, clients, API
├── deny.toml                      allowed licenses and sources (cargo-deny)
├── CHANGELOG.md
└── README.md
```

Files in `nix/` must be flake-parts modules, because devfmt imports every file
there; that is why the NixOS and Home Manager modules live in `modules/`.

## Development

```sh
nix develop          # Rust toolchain, treefmt, just, cargo-deny, cargo-tarpaulin
                     # (optional: outside it, the recipes run Cargo through
                     # nix develop)
just                 # list the recipes
just test            # cargo test
just lint            # clippy, warnings as errors
just fmt             # devfmt's treefmt setup
just deny            # advisories, licenses, duplicate crates, sources
just audit           # security advisories only
just check           # nix flake check: builds, module checks, NixOS VM test,
                     # coverage
just verify          # all of the above, as CI does
just coverage        # line coverage per file; fails below 95%, as CI does
```

`cargo deny` also runs in CI on every push and weekly, so a new advisory against
a dependency shows up without a code change. Its allowed licenses are in
`deny.toml`; anything else needs a deliberate edit there.

Changes are recorded in [CHANGELOG.md](CHANGELOG.md). A release bumps `version`
in `Cargo.toml`, moves the changelog's entries under the new version, and is
tagged `vX.Y.Z`.

To run the server locally, write a `pressbox.toml` (same keys as the module
generates: `listen`, `database`, `refresh_interval`, `max_items_per_feed`,
`max_age_days`, `behind_proxy`, `control_socket`, `backup_dir`, `token_file`,
`[feeds.<id>]` with `url`, `title`, `tags`, `refresh_interval`, `full_text`, and
`[users.<name>]` with `admin` and `[users.<name>.feeds.<id>]`) and run
`just server` (or `cargo run -p pressboxd -- -c pressbox.toml`). The control
socket defaults to `pressbox.sock` next to the database:
`just ctl -s ./pressbox.sock status`. `just tui` and `just gui` run the terminal
and desktop clients the same way.

## License

pressbox is [REUSE](https://reuse.software) compliant; every file states its
license in an SPDX header (`reuse lint` checks this, also in `nix flake check`).

- The server, `pressboxd` and `pressboxctl` (`crates/pressboxd`), and the NixOS
  module are licensed under the
  [GNU Affero General Public License v3.0 only](LICENSES/AGPL-3.0-only.txt). If
  you let others use a modified server over a network, you must offer them its
  source.
- The terminal and desktop clients (`pressbox`, `pressbox-iced`), the shared
  `pressbox-api` types, the Home Manager module, and the build files and docs
  are licensed under the
  [GNU General Public License v3.0 only](LICENSES/GPL-3.0-only.txt).
- The logo (`assets/logo.svg`) is licensed under
  [Creative Commons Attribution-ShareAlike 4.0](LICENSES/CC-BY-SA-4.0.txt).

The apps are licensed separately:
[pressbox-darwin](https://github.com/red-oxide/pressbox-darwin) under the
Mozilla Public License 2.0, and
[pressbox-android](https://github.com/red-oxide/pressbox-android) under the GPL
v3.0 only, like pressbox.

### Name and logo

The pressbox name and logo identify this project. The licenses above cover
copyright, not trademarks: you're welcome to use the logo to refer to pressbox,
but forks and derived projects must use their own name and logo.
