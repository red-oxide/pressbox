<!--
SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
SPDX-License-Identifier: GPL-3.0-only
-->

# Changelog

All notable changes to pressbox. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions are
[calendar versions](https://calver.org/), `YY.MINOR.PATCH`: the year of the
release, then a count of that year's releases and a count of fixes to each, both
starting at 0 (26.0.0, then 26.0.1 for a fix, 26.1.0, and 27.0.0 in January).
They say when, not how much changed; read the entries for that. The server,
`pressboxctl` and the clients in this repository share one version; the HTTP API
has its own (the `v1` in `/api/v1`), which only changes on an incompatible
change. The Apple and Android apps,
[pressbox-darwin](https://github.com/red-oxide/pressbox-darwin) and
[pressbox-android](https://github.com/red-oxide/pressbox-android), have their
own releases.

## [Unreleased]

### Server

- An OpenAPI 3.1 description of the HTTP API, built from the routes and the
  `pressbox-api` types, served at `GET /api/v1/openapi.json` (no token needed)
  and checked in as `docs/openapi.json`, so the Swift and Kotlin clients can be
  generated or tested against it. A test fails when the checked-in copy falls
  behind the API; `just openapi` rewrites it.

- Error responses and the pending answer from `GET /api/v1/pair/{poll_id}` have
  types in `pressbox-api` (`ErrorResponse`, `PairPending`). Their JSON is
  unchanged.

### Desktop client

- `PRESSBOX_SERVER` and `PRESSBOX_TOKEN_FILE` override `client.json`, as they
  already did for the terminal client.

### Project

- Both clients are
  [nix-wrapper-modules](https://github.com/nix-community/nix-wrapper-modules)
  wrappers (`wrappers.pressbox`, `wrappers.pressbox-iced`, and
  `wrapperModules.*`), with the Home Manager module's settings baked in, for
  using them without Home Manager.

- Nix inputs are pinned with [tack](https://github.com/manic-systems/tack) in
  `.tack/` instead of flake inputs, and `flake.lock` is gone. Flakes using
  pressbox should drop any `pressbox.inputs.*.follows`; pressbox builds with its
  own nixpkgs, which the red-oxide Cachix cache has builds for. A daily
  `update-pins` workflow (`tack update`) replaces `update-flake-lock`.

### Security

- Pairing: the per-client limit on waiting requests counts an IPv6 /64 as one
  client, as a single machine usually has a whole /64 and could otherwise take
  every slot and keep anyone else from pairing.

- Pairing: only admins can deny a waiting request, so one user can't turn away
  another's new devices.

- `DELETE /api/v1/me` revokes the token it is sent with, so a client can sign
  itself out without `pressboxctl revoke`.

- Article pages, chapters and transcripts are no longer fetched from the
  server's own addresses (a public one included), nor from 6to4 addresses that
  carry a private IPv4 address, Teredo, site-local or local-use NAT64 ones.

- A chapters or transcript file that can't be fetched or read isn't fetched
  again for 15 minutes, however often it is asked for.

- Searches are limited to 16 words and 500 characters.

- The control socket is bound in a private staging directory and moved into
  place, so it never exists at its path with permissions looser than `0600`,
  whatever the umask.

- CI pins its GitHub Actions to commits, and Dependabot keeps the pins current.

## [26.0.0] - 2026-11-17

The first release.

- The logo's home plate is filled rather than outlined, matching the
  pressbox-darwin and pressbox-android app icons. It is also pressbox-iced's
  icon on Linux.

### Server

- pressboxd: feeds and users declared in NixOS (`services.pressbox`), with
  shared feeds plus each user's own, and fully separate read and starred state
  per user. RSS 0.9x/1.0/2.0, Atom and JSON Feed, with conditional requests,
  backoff and sanitized HTML.

- Full articles: `fullText = true` on a feed fetches each new item's page and
  keeps its main content, for feeds that only carry a summary.

- Retention by count (`maxItemsPerFeed`) and age (`maxAgeDays`); starred items
  are always kept.

- Full-text search (SQLite FTS5).

- Devices pair with a short code that a signed-in device or `pressboxctl`
  approves; admins can approve for anyone. Pairing requests are limited per
  client address, with `behindProxy` for reverse proxies.

- HTTP API under `/api/v1`, including `GET /api/v1/version` without a token.

- systemd integration: notify and watchdog, socket activation, credentials,
  journald logging, and a backup timer.

- `pressboxctl`: status, users, devices, pairing, refresh, backups, OPML export,
  `opml-to-nix`, and `discover` to find a site's feeds.

- A feed keeps its artwork and medium when it answers 304 Not Modified. They
  were cleared, so feeds that honour their ETag or Last-Modified lost their
  artwork after the first fetch with nothing new; the upgrade fetches feeds with
  no artwork in full once to get it back. An RSS feed whose items the two
  parsers count differently keeps its channel artwork and medium too.

- Podcasts (Podcasting 2.0). Items carry their audio and video as `media`: the
  RSS `<enclosure>` (or Atom enclosure link, or JSON Feed attachment), any
  `podcast:alternateEnclosure`s (video, other bitrates, HLS), and the episode's
  duration, artwork, season and episode, `podcast:chapters` and
  `podcast:transcript`s. Feeds carry their artwork and `podcast:medium`.

- Each user's playback position is kept per item:
  `PUT /api/v1/items/{id}/position`, returned as `position`. Item lists take
  `media` and `in_progress` filters.

- `GET /api/v1/items/{id}/chapters` and `/transcript` read an episode's chapters
  file and best transcript (JSON, WebVTT, SRT, HTML or text) into one JSON shape
  each, so clients don't parse them; files are kept for a week.

- An Up Next queue per user: `GET /api/v1/queue` lists the episodes to play
  next, and `PUT /api/v1/queue` replaces it with a list of ids, which is how
  clients add, remove and reorder. Marking an item read takes it out, and queued
  episodes are kept from pruning. Database migration 9 adds the table.

- Database migration 7 adds the columns and a table for those files, and 8
  fetches every feed in full once, so episodes stored before the upgrade get
  their media.

- Item lists page by cursor: `GET /api/v1/items` takes `cursor` and returns
  `next_cursor`, which stays fast deep into a list (a page after 4,900 items:
  7.6 ms to 0.3 ms). `offset`/`next_offset` still work for older clients.

- API responses are compressed (gzip, brotli) when the client accepts it.

- Feed parsing, sanitizing and database work run off the async runtime, so a
  large feed or a long write no longer holds up API requests.

- Full articles are fetched in the background, at most four feeds at once,
  instead of holding a feed-polling slot until every page has loaded.

- Feed and article bodies are cut off at their size limit as they download, even
  without a `Content-Length`.

- Entries unchanged since the last fetch are no longer re-sanitized (a new
  database column records each one's digest as sent), and per-feed pruning only
  runs when something was added or `maxAgeDays` is set.

- `POST /api/v1/items/read-all` marks a whole list read in one request (any
  filters, search included), or only from a given item down.

- A socket from systemd that isn't a TCP stream (`ListenDatagram=`, say) is
  refused at startup with a clear message, instead of failing on the first
  connection.

- Feeds report `episodes` and `unread_episodes`: how many of their items, in all
  and unread, are podcast episodes (have media). Clients use them to show
  podcasts, news or both.

- Titles keep text that only looks like a tag. Every `<...>` was removed, so
  "Why Vec<T> beats LinkedList<T>" was stored as "Why Vec beats LinkedList" and
  "a < b and c > d" as "a d". Atom titles now follow their `type`, JSON Feed
  titles are kept as written, and RSS titles (and chapter and enclosure titles)
  lose only lowercase HTML tags such as `<b>` or `<em>`. Items stored before
  this release are corrected on the first fetch after upgrading.

- An item's `url` and a feed's `site_url` are absolute `http(s)` URLs or left
  out, as media URLs already were. A feed could put `javascript:` or `file:`
  links there, which clients pass to the browser or `xdg-open`.

- A feed that lists more entries than `maxItemsPerFeed` (a long-running podcast,
  say) no longer has the older ones stored, counted as new and pruned again on
  every fetch. They also showed as unread for a moment each time.

- Giving a feed a new id in the configuration (same URL) renames it. It used to
  add the new id and keep the old one as a removed feed while it had starred
  items, which were then listed twice.

- The URLs feeds supply (article pages for `fullText`, podcast chapters and
  transcripts) are only fetched from public addresses, by name, by address or
  after a redirect, so a feed cannot make pressboxd send requests to the machine
  it runs on or to the local network. `fetchPrivateAddresses = true`
  (`fetch_private_addresses` in `pressbox.toml`) lifts this for feeds on your
  own network. Feeds themselves are still fetched from wherever their `url`
  says.

- A feed whose entries cannot be stored (a full disk, say) backs off like any
  failing feed, with the reason as its error. It used to stay due, so its server
  was asked again every second for as long as storing failed. If not even that
  can be recorded, the scheduler waits a minute before trying again.

- `POST /api/v1/refresh` and `pressboxctl refresh` leave alone feeds fetched
  within the last minute, and failing feeds fetched within the last ten, so a
  client that refreshes often (or a failing feed that asked for a pause with
  `Retry-After`) does not have every feed fetched again each time.

- The transcript served for an episode is one in the show's language when there
  is one, rather than the best format in any language. A `podcast:transcript`
  without a `language` gets the channel's as its `lang`, as the specification
  says, and those are listed first.

### Desktop client

- Up Next: the server's queue in the sidebar, Add to Up Next and Play next on
  episodes (`a`, `n`), ↑ and ↓ (`J`, `K`) to reorder, and the next episode
  starts when one ends.

- Plays podcasts with GStreamer: audio in the background, video above the
  article, with a player bar (progress, chapters, skip, speed without pitch
  change), clickable chapters and a transcript that follows playback. Positions
  sync with the server and episodes played to the end are marked read. Appears
  in the system's media controls (MPRIS, macOS Now Playing). The Nix package
  carries the GStreamer plugins it needs.

- `pressbox-iced`, a desktop reader built with iced 0.14 (`clients/iced`):
  sources with unread counts (unread, all, starred, tags, feeds), the item list
  with search and paging, and the article with star, read/unread and open in
  browser. Pairs itself, or uses the TUI's `client.json` and token; can approve
  other devices. Birdland colors, following the system's light or dark mode.
  Packaged with a desktop entry; Home Manager installs it with
  `programs.pressbox.desktop.enable`.

- A server address that isn't one (`http://bad host`) is reported when you press
  Pair.

- Saving a token over an existing token file closes it to other users (mode
  0600); the mode was only set when the file was created. The same goes for
  `pressbox pair` in the terminal client.

### Terminal client

- `pressbox`, a terminal reader (ratatui).

- Up Next, a row under Starred that lists the server's queue: `a` adds an
  episode or takes it out, `n` plays it next, `J`/`K` reorder, and the next
  episode starts when one ends.

- Plays podcasts in mpv: `p` plays or pauses, `V` plays the video, `x` stops,
  `,`/`.` seek, `[`/`]` move between chapters and `-`/`+` change the speed. The
  position syncs with the server, so playback resumes on any client, and an
  episode played to the end is marked read. The reader lists chapters and `t`
  shows the transcript. `client.json` (and Home Manager) can set the `player`.

- Pages by cursor, falling back to offsets with an older server; asks for gzip
  responses. Loading more items no longer repeats the first page.

- `M` marks the selected item and every item below it read, including ones not
  loaded yet.

- `u` cycles unread, all and read items, so read posts can be listed on their
  own.

- mpv's control socket is made in `$XDG_RUNTIME_DIR`, which only you can reach,
  instead of the shared temporary directory (still the fallback when it is
  unset).

### Project

- Licenses: server AGPL-3.0-only, clients GPL-3.0-only, logo CC-BY-SA-4.0; REUSE
  compliant.

- Checks: NixOS and Home Manager module tests, a NixOS VM test, treefmt and
  reuse lint; `cargo deny` in CI.

- `SECURITY.md`: how to report a vulnerability privately, and what's in scope.

- `just release` makes a release from a clean, pushed master: it checks that the
  changelog entry for `version` in `Cargo.toml` is dated, runs `just verify`,
  tags and pushes `vX.Y.Z`, and publishes the GitHub release with the entry as
  its notes.

[26.0.0]: https://github.com/red-oxide/pressbox/releases/tag/v26.0.0
[unreleased]: https://github.com/red-oxide/pressbox/compare/v26.0.0...HEAD
