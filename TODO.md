<!--
SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
SPDX-License-Identifier: GPL-3.0-only
-->

# TODO

What's planned or worth doing next, roughly in order within each section. Done
items move to [CHANGELOG.md](CHANGELOG.md) when they land. The Apple and Android
apps keep their own lists in
[pressbox-darwin](https://github.com/red-oxide/pressbox-darwin) and
[pressbox-android](https://github.com/red-oxide/pressbox-android); this one only
holds what they need from the server.

## Release

- [ ] Release 26.0.0: date the changelog entry, tag `v26.0.0`, and publish a
  GitHub release with that entry as its notes.
- [ ] Decide the server's license: keep AGPL-3.0-only or move `pressboxd`,
  `pressboxctl` and the NixOS module to GPL-3.0-only. Do it before 26.0.0 if
  it's changing, and update the SPDX headers, `REUSE.toml`, `LICENSES/` and
  the README together.
- [ ] A release workflow: on a `v*` tag, check that the tag matches `version` in
  `Cargo.toml`, run `just verify`, and create the release from the
  changelog.

## Server and API

- [ ] Sync changes since a point in time. Read, starred, position and queue
  changes only show up by refetching lists, which is costly for the apps,
  especially pressbox-darwin with offline downloads. Record when each user's
  state for an item last changed (a migration) and add
  `GET /api/v1/changes?since=` (or `updated_since` on `/items`) returning
  what changed, including items that were pruned.
- [ ] Devices over the API: `GET /api/v1/devices` and
  `DELETE /api/v1/devices/{name}` (your own; admins for anyone), so a client
  can list where you're signed in and sign itself or a lost device out.
  Today only `pressboxctl revoke` does this.
- [ ] Artwork cache: fetch feed and episode artwork once, store it resized (e.g.
  200 and 600 px), and serve it from `/api/v1/images/{id}`. The tile layouts
  in pressbox-darwin and the Apple TV app lean on artwork, and this keeps
  every client from hitting third-party hosts for it.
- [ ] Push changes to clients: `GET /api/v1/events` (server-sent events) for new
  items, unread counts and queue changes, so the desktop client and the
  Birdland bar update without polling.
- [ ] An API description generated from `pressbox-api` (OpenAPI via `utoipa` or
  JSON Schema via `schemars`), checked in CI, so the Swift and Kotlin
  clients can be generated or tested against it instead of following
  `docs/api.md` by hand.
- [ ] WebSub: subscribe to a feed's hub when it advertises one, for new items in
  seconds and fewer polls.

## Project

- [ ] Cache Nix builds in CI (magic-nix-cache or Cachix). `nix flake check`
  rebuilds everything, the NixOS VM test and coverage included, on each
  push.
- [ ] Keep Cargo dependencies current: a scheduled job that runs `cargo update`
  and opens a pull request, as `update-flake-lock.yml` does for flake
  inputs. `cargo deny` only reports advisories; it doesn't update.
- [ ] `SECURITY.md`: how to report a vulnerability privately. pressboxd faces
  the network and holds device tokens.
- [ ] A guide for running pressboxd outside NixOS (`docs/manual.md`): building
  with Cargo or `nix build`, writing `pressbox.toml` by hand (every key the
  module sets), a systemd unit and backup timer equivalent to the module's,
  a reverse proxy with `behind_proxy`, and pairing with `pressboxctl`.
