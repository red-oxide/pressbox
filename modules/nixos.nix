# SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
# SPDX-License-Identifier: AGPL-3.0-only

{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;
  cfg = config.services.pressbox;

  feedType = types.submodule {
    options = {
      url = mkOption {
        type = types.strMatching "https?://.+";
        description = "URL of the RSS, Atom or JSON feed.";
        example = "https://lwn.net/headlines/rss";
      };
      title = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Title to show instead of the one the feed reports.";
      };
      tags = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "Tags clients can filter by.";
        example = [ "linux" ];
      };
      refreshInterval = mkOption {
        type = types.nullOr (types.ints.between 60 86400);
        default = null;
        description = "Seconds between fetches of this feed, overriding `refreshInterval`.";
      };
      fullText = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Fetch each new item's web page and keep its main content, for feeds
          that only carry a summary or the first paragraph. Pages without
          enough text to be an article keep the feed's own text.
        '';
      };
    };
  };

  feedsType = types.attrsOf (types.coercedTo types.str (url: { inherit url; }) feedType);

  userType = types.submodule {
    options = {
      admin = mkOption {
        type = types.bool;
        default = false;
        description = "Admins can approve new devices for any user from their clients.";
      };
      feeds = mkOption {
        type = feedsType;
        default = { };
        description = "Feeds only this user gets, on top of the shared `feeds`.";
      };
    };
  };

  toFeedSettings = lib.mapAttrs (
    _: feed:
    lib.filterAttrs (_: v: v != null) {
      inherit (feed) url title tags;
      refresh_interval = feed.refreshInterval;
      full_text = feed.fullText;
    }
  );

  listenAddress =
    if lib.hasInfix ":" cfg.address then
      "[${cfg.address}]:${toString cfg.port}"
    else
      "${cfg.address}:${toString cfg.port}";

  # pressboxd finds the token itself in $CREDENTIALS_DIRECTORY/token, so the
  # config never names a path.
  settings = {
    listen = listenAddress;
    database = "/var/lib/pressbox/pressbox.db";
    control_socket = "/run/pressbox/control.sock";
    backup_dir = "/var/lib/pressbox/backups";
    refresh_interval = cfg.refreshInterval;
    max_items_per_feed = cfg.maxItemsPerFeed;
    behind_proxy = cfg.behindProxy;
    fetch_private_addresses = cfg.fetchPrivateAddresses;
    feeds = toFeedSettings cfg.feeds;
    users = lib.mapAttrs (_: user: {
      inherit (user) admin;
      feeds = toFeedSettings user.feeds;
    }) cfg.users;
  }
  // lib.optionalAttrs (cfg.maxAgeDays != null) { max_age_days = cfg.maxAgeDays; };

  rawConfig = (pkgs.formats.toml { }).generate "pressbox-unchecked.toml" settings;

  # Fail the build, not the service, on a configuration pressboxd rejects.
  configFile = pkgs.runCommand "pressbox.toml" { } ''
    ${lib.getExe cfg.package} --check --config ${rawConfig}
    cp ${rawConfig} $out
  '';
in
{
  options.services.pressbox = {
    enable = lib.mkEnableOption "the pressbox feed aggregator";

    package = mkOption {
      type = types.package;
      description = "The pressboxd package to run.";
    };

    address = mkOption {
      type = types.str;
      default = "127.0.0.1";
      description = ''
        Address to listen on. Use `0.0.0.0` or `::` to reach the server
        from other machines, or keep the default and put it behind a
        reverse proxy or `tailscale serve`. Every client needs a paired
        token either way.
      '';
    };

    port = mkOption {
      type = types.port;
      default = 8470;
      description = "Port to listen on.";
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Open `port` in the firewall.";
    };

    behindProxy = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Set when a reverse proxy or `tailscale serve` forwards requests to
        pressboxd. Pairing requests are rate-limited per client address, and
        behind a proxy every request seems to come from the proxy, so this
        makes pressboxd use the address the proxy appends to
        `X-Forwarded-For` instead. Leave it off when clients connect
        directly, or anyone could pick their own address.
      '';
    };

    fetchPrivateAddresses = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Let the URLs feeds supply (article pages for `fullText`, podcast
        chapters and transcripts) be fetched from this machine and private
        networks too. By default only public addresses that aren't this
        machine's own are, so a feed can't make pressboxd send requests to
        your router or other local services.
        Turn it on if a feed on your own network links to pages or files
        there. The feeds themselves are fetched from wherever their `url`
        says either way.
      '';
    };

    refreshInterval = mkOption {
      type = types.ints.between 60 86400;
      default = 1800;
      description = ''
        Seconds between fetches of each feed, unless the feed sets its own.
        Failing feeds back off (doubling, up to a day), and a server's
        `Cache-Control: max-age` or `Retry-After` can stretch the wait.
      '';
    };

    maxItemsPerFeed = mkOption {
      type = types.ints.positive;
      default = 500;
      description = "Items kept per feed. Starred items are never removed.";
    };

    maxAgeDays = mkOption {
      type = types.nullOr types.ints.positive;
      default = null;
      example = 90;
      description = ''
        Remove items published more than this many days ago; items that old
        are not stored in the first place. Starred items are always kept.
        `null` keeps items until `maxItemsPerFeed` pushes them out.
      '';
    };

    tokenFile = mkOption {
      type = types.nullOr (types.pathWith { inStore = false; });
      default = null;
      example = "/run/secrets/pressbox-token";
      description = ''
        Optional static token accepted alongside paired devices' tokens, for
        example a sops-nix or agenix secret. It is passed to the service with
        systemd's LoadCredential, so it can stay readable by root only.
        Normally you pair each client instead (`pressboxctl approve`), and
        leave this unset.
      '';
    };

    backup = {
      enable = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Take a consistent copy of the database on a schedule, into
          `/var/lib/pressbox/backups` (on disk: `/var/lib/private/pressbox/backups`),
          for restic, Borg or similar to pick up.
        '';
      };
      schedule = mkOption {
        type = types.str;
        default = "daily";
        example = "*-*-* 03:00:00";
        description = "When to back up, as a systemd OnCalendar expression.";
      };
      keep = mkOption {
        type = types.ints.positive;
        default = 14;
        description = "Backups to keep; older ones are deleted.";
      };
    };

    feeds = mkOption {
      type = feedsType;
      default = { };
      description = ''
        Feeds to aggregate, keyed by an id made of letters, digits, `-` and
        `_`. A plain string is shorthand for `{ url = ...; }`. Removing a
        feed here deletes its unread and read items; starred ones are kept
        under the feed, marked as removed, until you unstar them.
        `pressboxctl opml-to-nix` converts an OPML export into this attrset.
      '';
      example = lib.literalExpression ''
        {
          lwn = "https://lwn.net/headlines/rss";
          nixos = {
            url = "https://nixos.org/blog/announcements-rss.xml";
            title = "NixOS";
            tags = [ "nix" ];
          };
        }
      '';
    };

    socketActivation = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Let systemd own the listening socket (`pressbox.socket`) and start
        pressboxd on the first connection. The port stays open across
        restarts, so clients never see a refused connection during
        `nixos-rebuild switch`. Feeds are only refreshed while pressboxd runs,
        which is from the first connection on.
      '';
    };

    watchdogSec = mkOption {
      type = types.ints.unsigned;
      default = 60;
      description = ''
        Restart pressboxd if it stops answering for this many seconds
        (systemd's WatchdogSec). pressboxd only pings the watchdog while its
        database responds. 0 turns the watchdog off.
      '';
    };

    users = mkOption {
      type = types.attrsOf userType;
      default = { };
      description = ''
        People who use this server. Each gets the shared `feeds` plus their
        own, and their own read and starred state, devices and pairings;
        nobody sees another user's feeds or state. A feed URL several users
        follow is fetched once. With no users, there is one admin user named
        `default`.

        The first time users are declared, the existing (single-user) data
        goes to the first admin. Removing a user deletes their devices and
        state.
      '';
      example = lib.literalExpression ''
        {
          chris = {
            admin = true;
            feeds.hn = "https://news.ycombinator.com/rss";
          };
          anna.feeds.recipes = "https://example.com/recipes.xml";
        }
      '';
    };

    configFile = mkOption {
      type = types.path;
      readOnly = true;
      description = "The generated configuration, for inspection.";
    };
  };

  config = lib.mkIf cfg.enable {
    services.pressbox.configFile = configFile;

    # pressboxctl (in the same package) for pairing, devices and backups.
    environment.systemPackages = [ cfg.package ];

    networking.firewall.allowedTCPPorts = lib.optional cfg.openFirewall cfg.port;

    systemd.sockets.pressbox = lib.mkIf cfg.socketActivation {
      description = "pressbox feed aggregator socket";
      wantedBy = [ "sockets.target" ];
      listenStreams = [ listenAddress ];
      socketConfig.BindIPv6Only = lib.mkIf (cfg.address == "::") "both";
    };

    systemd.services.pressbox = {
      description = "pressbox feed aggregator";
      wantedBy = lib.mkIf (!cfg.socketActivation) [ "multi-user.target" ];
      requires = lib.optional cfg.socketActivation "pressbox.socket";
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ] ++ lib.optional cfg.socketActivation "pressbox.socket";
      environment.RUST_LOG = lib.mkDefault "pressboxd=info";

      serviceConfig = {
        Type = "notify";
        NotifyAccess = "main";
        WatchdogSec = cfg.watchdogSec;
        ExecStart = "${lib.getExe cfg.package} --config ${configFile}";
        LoadCredential = lib.optional (cfg.tokenFile != null) "token:${cfg.tokenFile}";
        Restart = "on-failure";
        RestartSec = 5;

        DynamicUser = true;
        StateDirectory = "pressbox";
        StateDirectoryMode = "0700";
        # Holds control.sock for pressboxctl; only root and the service get in.
        RuntimeDirectory = "pressbox";
        RuntimeDirectoryMode = "0700";
        UMask = "0077";

        CapabilityBoundingSet = "";
        DevicePolicy = "closed";
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        NoNewPrivileges = true;
        PrivateDevices = true;
        PrivateTmp = true;
        PrivateUsers = true;
        ProcSubset = "pid";
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHome = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProtectSystem = "strict";
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];
      };
    };

    systemd.services.pressbox-backup = lib.mkIf cfg.backup.enable {
      description = "Back up the pressbox database";
      wants = [ "pressbox.service" ];
      after = [ "pressbox.service" ];
      # Runs as root to reach pressboxd's control socket; pressboxd itself
      # writes the backup, as its own user.
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/pressboxctl backup --keep ${toString cfg.backup.keep}";
        PrivateNetwork = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        NoNewPrivileges = true;
      };
    };

    systemd.timers.pressbox-backup = lib.mkIf cfg.backup.enable {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = cfg.backup.schedule;
        Persistent = true;
        RandomizedDelaySec = "15m";
      };
    };
  };
}
