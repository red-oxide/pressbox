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

## For pressbox-darwin

Server work behind items on
[pressbox-darwin's TODO](https://github.com/red-oxide/pressbox-darwin/blob/master/TODO.md).
pressbox-android will want the same. watchOS, CarPlay and Boosts need nothing
new here; tvOS uses the artwork cache above.

- [ ] Feed categories (category view): parse `itunes:category` (and Atom and
  JSON Feed categories) and return them on `Feed` as `categories`, next to
  the Nix-declared `tags`.
- [ ] Feed requests (request new feeds): feeds stay declared in Nix, so the app
  can only ask. `POST /api/v1/feed-requests` with a URL (discovered as
  `pressboxctl discover` does), listed to admins over the API and by
  `pressboxctl requests`, which prints the Nix to paste into
  `services.pressbox.feeds`. Dropped once the feed appears in the config.
- [ ] `podcast:guid` on feeds: the key that podroll, podping and value
  recipients refer to shows by.
- [ ] Podroll: parse `podcast:podroll` into `Feed.podroll`
  (`{guid, url?, title?}`), marking shows you already follow.
- [ ] Podping: listen for podpings (the Hive stream, or a podping.cloud relay)
  for followed feeds' URLs and refresh those feeds at once. Off by default
  in the NixOS module.
- [ ] Person: parse channel and item `podcast:person`
  (`{name, role, group, img?, href?}`) into `Feed.persons` and
  `media.persons`. Linking to a person's other shows works across the feeds
  the server knows.
- [ ] Funding: parse `podcast:funding` into `Feed.funding` (`{url, title}`).
- [ ] Live items: parse `podcast:liveItem` (status, start, end, enclosures) as
  items with a `live` field, poll feeds with a pending or live item more
  often, and let `GET /api/v1/items` filter on `live`.
- [ ] Value4Value: parse channel and item `podcast:value` and
  `podcast:valueTimeSplit` (recipients, splits, time ranges) into `value`,
  so the app can stream sats and send boosts without parsing the feed
  itself.

## Project

- [ ] Cache Nix builds in CI (magic-nix-cache or Cachix). `nix flake check`
  rebuilds everything, the NixOS VM test and coverage included, on each
  push.
- [ ] Keep Cargo dependencies current: a scheduled job that runs `cargo update`
  and opens a pull request, as `update-flake-lock.yml` does for flake
  inputs. `cargo deny` only reports advisories; it doesn't update.
- [ ] A guide for running pressboxd outside NixOS (`docs/manual.md`): building
  with Cargo or `nix build`, writing `pressbox.toml` by hand (every key the
  module sets), a systemd unit and backup timer equivalent to the module's,
  a reverse proxy with `behind_proxy`, and pairing with `pressboxctl`.
