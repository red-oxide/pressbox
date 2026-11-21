<!--
SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
SPDX-License-Identifier: GPL-3.0-only
-->

## Server (NixOS)

Add the flake to your system configuration:

```nix
{
  inputs.pressbox = {
    url = "github:red-oxide/pressbox";
    inputs.nixpkgs.follows = "nixpkgs";
  };
}
```

Then import `inputs.pressbox.nixosModules.default` and declare your feeds:

```nix
{
  services.pressbox = {
    enable = true;
    refreshInterval = 1800; # seconds, per feed

    feeds = {
      lwn = "https://lwn.net/headlines/rss"; # shorthand for { url = ...; }
      nixos = {
        url = "https://nixos.org/blog/announcements-rss.xml";
        title = "NixOS";
        tags = [ "nix" ];
        refreshInterval = 3600; # this feed only
      };
    };
  };
}
```

### Users

For a household or friends, declare users. Everyone gets the shared `feeds` plus
their own, with their own read and starred state, devices and pairings. Nobody
sees another user's feeds or state, and a URL several people follow is fetched
once.

```nix
{
  services.pressbox.users = {
    chris = {
      admin = true; # can approve devices for anyone
      feeds.hn = "https://news.ycombinator.com/rss";
    };
    anna.feeds.recipes = "https://example.com/recipes.xml";
  };
}
```

Without `users` there is a single admin user named `default`. When you first
declare users, the existing data (read state, stars, devices) goes to the first
admin. Removing a user deletes their devices and state. Users come only from the
config; nobody signs up.

Moving from another reader? Export OPML from it and run
`pressboxctl opml-to-nix subscriptions.opml > feeds.nix`, then
`services.pressbox.feeds = import ./feeds.nix;`. To add a site whose feed URL
you don't know, `pressboxctl discover https://example.org` finds its feeds and
prints them as Nix.

Some feeds carry only a summary or the first paragraph. For those, set
`fullText = true` on the feed: pressbox fetches each new item's page and keeps
its main content (the article, without navigation, ads or comments), sanitized
like feed HTML. Clients show it in place of the feed's text, and search covers
it. Pages without enough text to be an article keep the feed's own text.

Article pages, and the chapters and transcripts podcasts link to, are URLs the
feed supplies, so pressbox only fetches them from public addresses, and never
from one of the server's own (a public address included): a feed can't point it
at your router or another service on your network, or at a service on the server
that the firewall keeps from the internet. Behind NAT, the router's public
address isn't one of the server's; if the router forwards ports back to it,
those services are as reachable as from anywhere else. If a feed on your own
network links to pages there, set `fetchPrivateAddresses = true`. Feeds
themselves are fetched from wherever their `url` says.

Feed ids (the attribute names) may contain letters, digits, `-` and `_`. The
generated config is checked by `pressboxd --check` at build time, so a bad feed
fails `nixos-rebuild`, not the service.

Removing a feed (for everyone, or from one user's `feeds`) deletes its items for
those users, except starred ones: the feed stays under "removed" in their
clients until they unstar them. Adding it back resumes it.

| Option | Default | Notes |
| ----------------------- | ----------- | ------------------------------------------------------------------------ |
| `address` | `127.0.0.1` | `0.0.0.0` or `::` to listen on all interfaces |
| `port` | `8470` | |
| `openFirewall` | `false` | |
| `refreshInterval` | `1800` | Seconds, 60–86400; feeds can set their own |
| `maxItemsPerFeed` | `500` | Starred items are never pruned |
| `maxAgeDays` | `null` | Remove items older than this (starred ones stay) |
| `behindProxy` | `false` | Behind a reverse proxy or `tailscale serve`; see [Pairing](./pairing.md) |
| `fetchPrivateAddresses` | `false` | Also fetch what feeds link to from private addresses |
| `backup.enable` | `true` | Daily copy to `/var/lib/private/pressbox/backups` |
| `backup.schedule` | `daily` | systemd OnCalendar |
| `backup.keep` | `14` | |
| `watchdogSec` | `60` | 0 turns the watchdog off |
| `socketActivation` | `false` | Feeds refresh only once the first client connects |
| `users` | `{}` | `<name>.admin`, `<name>.feeds`; see [Users](#users) |
| `tokenFile` | `null` | Optional static token; acts as the first admin |
| `configFile` | read-only | The generated `pressbox.toml`, for inspection |

For the Apple app, serve pressbox over HTTPS. iOS only allows plain HTTP to
local network addresses; the Android app allows it but warns outside the local
network or a tailnet. `tailscale serve --bg 8470` is the least work; a reverse
proxy with ACME works too.

### How feeds are fetched

Each feed has its own schedule. Fetches are conditional (ETag and
Last-Modified), and a server's `Cache-Control: max-age` can stretch the wait but
never shorten it below the interval. A failing feed backs off: the delay doubles
with each failure, up to a day, and `Retry-After` on a 429 or 503 is honored.
`pressboxctl status` and the clients show failing feeds.

Feed HTML is cleaned when it is stored: scripts, styles, iframes, forms and
event handlers are dropped, links get `rel="noopener noreferrer"`, relative URLs
are made absolute, and tracking pixels (1×1 images and known tracker hosts) are
removed. Every item's text is indexed for full-text search.

Each feed keeps its newest `maxItemsPerFeed` items. With `maxAgeDays` set, items
published longer ago are removed too, and not stored when a feed still lists
them. Starred items are kept either way.

### systemd

pressboxd integrates with systemd directly:

- **Readiness and status.** It is a `Type=notify` service, and
  `systemctl status pressbox` shows its own status line, e.g.
  `12 feeds, 2 users, 3 new at 14:30, 1 failing`.
- **Watchdog.** It pings systemd's watchdog only while its database answers, so
  a wedged server is restarted automatically.
- **Credentials.** `tokenFile`, if set, reaches it through `LoadCredential`, so
  the secret can stay readable by root only.
- **Journal.** Logs go straight to the journal with proper priorities:
  `journalctl -t pressboxd -p warning`.
- **Backups.** `pressbox-backup.timer` runs `pressboxctl backup` on schedule.
- **Socket activation (optional).** With `socketActivation = true`, systemd owns
  the port, so it stays open across restarts.

Outside systemd all of this is skipped and pressboxd runs the same by hand.
