<!--
SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
SPDX-License-Identifier: GPL-3.0-only
-->

# Security

pressboxd faces the network, holds a token for every paired device, and fetches
whatever URLs its feeds point at, so security reports are welcome.

## Reporting a vulnerability

Please report privately, not in a public issue, discussion or pull request:

- [Report a vulnerability](https://github.com/red-oxide/pressbox/security/advisories/new)
  on GitHub (preferred), or
- email chris@red-oxide.org.

Say which part is affected (pressboxd, `pressboxctl`, a client, or a Nix
module) and at what version or commit, what an attacker needs to start with
(nothing, network access, a paired device, an admin device, or control of a
feed's content), how to reproduce it, and what it lets them do.

## What happens next

pressbox has one maintainer, so these are aims rather than promises: a reply
within a week, and an assessment within two. A fix ships as a patch release with
a GitHub security advisory that credits you, unless you'd rather it didn't.
Please allow 90 days before disclosing; if a fix needs longer, I'll say why and
agree a date with you.

## Supported versions

Only the latest release gets fixes. Versions are `YY.MINOR.PATCH` (see
[CHANGELOG.md](CHANGELOG.md)); please check the latest release or `main` before
reporting.

## Scope

Everything in this repository, for example:

- pairing and approval: getting a token without approval, approving for another
  user without being an admin, getting around the per-address limits
- tokens and data: reading another device's token, or another user's feeds,
  items, read and starred state, positions or queue
- content from feeds: HTML that gets through sanitizing, or a feed that gets
  pressboxd to fetch a private address despite the public-only check (through
  article, chapter or transcript URLs, redirects, or DNS)
- the control socket: `pressboxctl` access for a local user who shouldn't
  have it
- the NixOS and Home Manager modules: insecure defaults, or files and sockets
  with loose permissions

Not in scope:

- feeds an admin declares in the NixOS config, which are trusted to be fetched
- private addresses fetched with `fetchPrivateAddresses = true`, which asks for
  them
- plain HTTP on an untrusted network; serve pressbox over HTTPS (see
  [Server](./docs/server.md))
- `behindProxy = true` without a proxy in front, which lets clients pick their
  own address (see [Pairing](./docs/pairing.md))
- advisories in dependencies pressbox doesn't reach; report those upstream
  (`cargo deny` already fails CI on RustSec advisories)
- the Apple and Android apps, which take reports in their own repos:
  [pressbox-darwin](https://github.com/red-oxide/pressbox-darwin) and
  [pressbox-android](https://github.com/red-oxide/pressbox-android)
