<!--
SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
SPDX-License-Identifier: GPL-3.0-only
-->

## pressboxctl

Runs as root on the server and talks to pressboxd over
`/run/pressbox/control.sock`, which only root and the service can open.

| Command | What it does |
| --------------------------------------- | ---------------------------------------------------------- |
| `status` | Feeds (failing, removed), items, users, devices, pairings |
| `users` | Users, admins, and their feed and device counts |
| `pairings` | Pending pairing requests |
| `approve CODE [--user U]` / `deny CODE` | Answer one |
| `devices [--user U]` | Paired devices, their users, when each was last seen |
| `revoke NAME` | Revoke a device's token |
| `add-device NAME [--user U]` | Create a device and print its token |
| `backup [--keep N]` | Back up now (the timer runs this daily) |
| `refresh` | Fetch every feed now (except ones fetched a moment ago) |
| `opml-export [--user U]` | Print a user's feeds as OPML |
| `opml-to-nix FILE` | Convert OPML to a `feeds` attrset (offline) |
| `discover URL` | Find a site's feeds and print them as Nix (no root needed) |

`--user` is required when there is more than one user.
