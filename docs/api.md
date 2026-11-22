<!--
SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
SPDX-License-Identifier: GPL-3.0-only
-->

## API

The API is versioned: everything lives under `/api/v1/`, except `/health`.
Pairing routes, `version` and `openapi.json` need no token (pairing is how a
client gets one); every other route needs `Authorization: Bearer <token>`, a
paired device's token or the static `tokenFile` token. A token belongs to one
user, and every answer is about that user's feeds and state. Timestamps are Unix
seconds.

| Method | Path | Body / query | Response |
| ------ | --------------------------------- | ---------------------------------------------------------------------------------- | -------------------------------------------------------- |
| GET | `/health` | | `ok` |
| GET | `/api/v1/version` | | `{version, api}`, e.g. `{"version": "26.0.0", "api": 1}` |
| GET | `/api/v1/openapi.json` | | the OpenAPI description |
| POST | `/api/v1/pair` | `{"name": str}` | `{code, poll_id, expires_in}`; 429 when too many wait |
| GET | `/api/v1/pair/{poll_id}` | | 202 pending; `{token, name}` once; 404 |
| GET | `/api/v1/me` | | `{user, admin, device}` |
| GET | `/api/v1/feeds` | | `[Feed]` |
| GET | `/api/v1/items` | `feed`, `tag`, `unread`, `starred`, `media`, `in_progress`, `q`, `limit`, `cursor` | `{items, next_cursor}` |
| GET | `/api/v1/items/{id}` | | `Item` with `content` |
| PUT | `/api/v1/items/{id}/read` | `{"value": bool}` | 204 |
| PUT | `/api/v1/items/{id}/starred` | `{"value": bool}` | 204 |
| PUT | `/api/v1/items/{id}/position` | `{"position": seconds}` | 204 |
| GET | `/api/v1/items/{id}/chapters` | | `{chapters: [Chapter]}`; 404 if none; 502 if unreadable |
| GET | `/api/v1/items/{id}/transcript` | | `{segments: [Segment]}`; 404 if none; 502 if unreadable |
| GET | `/api/v1/queue` | | `{items}` |
| PUT | `/api/v1/queue` | `{"ids": [id]}` | `{items}`; 400 over 500 |
| POST | `/api/v1/feeds/{id}/read-all` | | `{"updated": n}` |
| POST | `/api/v1/items/read-all` | the list's filters, optional `from` item id | `{"updated": n}` |
| POST | `/api/v1/refresh` | | 202 |
| GET | `/api/v1/pairings` | | `[{code, name, requested}]` |
| POST | `/api/v1/pairings/{code}/approve` | optional `{"user": str}` (admins only) | `{name, user}` |
| DELETE | `/api/v1/pairings/{code}` | | 204 |

[`openapi.json`](./openapi.json) describes every route, parameter and JSON shape
as OpenAPI 3.1, for generating clients or checking them against the server. The
server serves the same document at `GET /api/v1/openapi.json`. It is built from
the server's routes and the `pressbox-api` types, and a test fails when the
checked-in copy falls behind them; `just openapi` rewrites it. Errors are
`{"error": "message"}`, with a message meant for showing to the user.

Item lists come newest first. To get the next page, pass the previous page's
`next_cursor` as `cursor` with the same filters; it is absent on the last page.
A cursor is opaque, and a malformed one is a 400. The older `offset` parameter
(answered with `next_offset`) still works, but gets slower the further into a
list it goes.

`POST /api/v1/refresh` fetches every feed now, except those fetched within the
last minute (ten minutes for a feed whose last fetch failed), so asking often
doesn't hammer the feeds' servers.

`unread=false` lists only read items. `POST /api/v1/items/read-all` marks read
every item a list shows, however many pages it has: its body takes the same
filters as `GET /api/v1/items` (`feed`, `tag`, `unread`, `starred`, `q`), and
with `"from": id` only that item and the older ones after it. `updated` counts
the items that were unread.

Responses are compressed (gzip or brotli) when the request's `Accept-Encoding`
allows it.

`q` matches every word as a prefix, in titles and text. With `q`, `unread` is
usually left out so read items are found too. Breaking changes will go to a new
version (`/api/v2/`), served alongside `v1` for a while.

### Podcasts

An item with audio or video has `media`: its `enclosures` (the feed's own
`<enclosure>` first, then any Podcasting 2.0 `podcast:alternateEnclosure`s,
which is where video and other bitrates come from), and, when the feed says,
`duration` (seconds), `image`, `season`, `episode`, `chapters` and
`transcripts`. RSS `<enclosure>`, Atom `rel="enclosure"` links and JSON Feed
attachments all count. Every URL is absolute `http(s)`. A feed has `image` (its
artwork) and `medium` (`podcast:medium`) when it says, and always `episodes` and
`unread_episodes`: how many of its `total` and `unread` items have media, so
clients can tell podcasts from news.

`position` is where this user stopped playing, in seconds; it is left out
when 0. Clients save it with `PUT /api/v1/items/{id}/position` every so often
while playing and when pausing or stopping, and the last write wins. When an
episode is finished, clients mark it read and set its position to 0.
`in_progress=true` lists unread items with a saved position (started, not
finished); `media=true` lists only items with media, and `media=false` only
items without.

Each user has an Up Next queue: `GET /api/v1/queue` lists the episodes to play
next, in order, as list items (without `content`). `PUT /api/v1/queue` replaces
it with `ids` in the order given, so clients add, remove and reorder by sending
the list they want, and answers with the new queue. Items the user can't see,
items without media and repeats are left out, and it holds at most 500. Marking
an item read takes it out, so finishing an episode on any client leaves Up Next
everywhere. When an episode ends, clients start the first episode in the queue.
Queued episodes are kept when a feed's old items are pruned.

`chapters` and `transcript` read the episode's chapters file and its best
transcript (one in the show's language before a translation, then JSON, WebVTT,
SRT, HTML and plain text) so clients don't have to: chapters come as
`{start, end?, title?, image?, url?, hidden?}` sorted by `start` (`hidden`
chapters only change the artwork), and transcripts as
`{start, end, speaker?, text}` segments, with runs of very short cues joined
into sentences. Files are fetched once and kept for a week.
