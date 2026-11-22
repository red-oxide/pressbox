<!--
SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
SPDX-License-Identifier: GPL-3.0-only
-->

## Pairing devices

Every client gets its own token, and only its SHA-256 is stored. To pair:

1. On the new client, start pairing: `pressbox pair`, or "Pair this device" in
   the Apple or Android app. It shows a code like `K7W-4QX`.
1. Approve it on the server with `sudo pressboxctl approve K7W-4QX --user anna`
   (`--user` is needed once there is more than one user), or from a client that
   is already paired: `pressbox approve K7W-4QX`, or Settings in the apps. A
   device approved from a client goes to that client's user; admins can name
   another (`pressbox approve CODE --user anna`, or the user field in the apps'
   Settings).
1. The client picks up its token on its own. Codes expire after ten minutes.

`pressboxctl devices` lists devices with their user and when each was last seen;
`pressboxctl revoke <name>` locks one out, and a client can sign itself out with
`DELETE /api/v1/me`. For a client that cannot pair (a script, say),
`pressboxctl add-device <name> --user <user>` prints a token. `pressbox whoami`
shows which user and device a machine is signed in as, and the server's version.

Anyone who can reach the server can ask to pair, and the device name in a
request is whatever the requester typed, so approve by the code the new device
shows, never by its name. Only admins can deny a request. Requests are limited:
3 waiting per client and 50 in all, where a client is an IPv4 address or an IPv6
/64 (the block a single machine is usually given). Behind a reverse proxy or
`tailscale serve`, every request comes from the proxy; set `behindProxy = true`
so the address the proxy puts in `X-Forwarded-For` counts instead. Leave it off
otherwise, since clients could then send any address they like.
