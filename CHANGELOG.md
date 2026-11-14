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

## [26.0.0] - Unreleased

- The logo's home plate is filled rather than outlined, matching the
  pressbox-darwin and pressbox-android app icons. It is also pressbox-iced's
  icon on Linux.

### Server

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

### Terminal client

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

## [0.1.0] - 2026-11-07

The first release.

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

### Clients

- `pressbox`, a terminal reader (ratatui).

### Project

- Licenses: server AGPL-3.0-only, terminal client GPL-3.0-only, logo
  CC-BY-SA-4.0; REUSE compliant.
- Checks: NixOS and Home Manager module tests, a NixOS VM test, treefmt and
  reuse lint; `cargo deny` in CI.

[0.1.0]: https://github.com/red-oxide/pressbox/releases/tag/v0.1.0
[26.0.0]: https://github.com/red-oxide/pressbox/compare/v0.1.0...v26.0.0
