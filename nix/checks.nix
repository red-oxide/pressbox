# SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
# SPDX-License-Identifier: GPL-3.0-only

{ self, inputs, ... }:
{
  perSystem =
    {
      pkgs,
      lib,
      config,
      system,
      ...
    }:
    let
      token = "/run/secrets/pressbox-token";

      feeds = {
        lwn = "https://lwn.net/headlines/rss";
        nixos = {
          url = "https://nixos.org/blog/announcements-rss.xml";
          title = "NixOS";
          tags = [ "nix" ];
          refreshInterval = 3600;
          fullText = true;
        };
      };

      # The NixOS module, evaluated in a throwaway system.
      nixos = inputs.nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          self.nixosModules.default
          {
            boot.isContainer = true;
            system.stateVersion = "26.05";
            services.pressbox = {
              enable = true;
              address = "::";
              tokenFile = token;
              openFirewall = true;
              behindProxy = true;
              fetchPrivateAddresses = true;
              maxAgeDays = 90;
              inherit feeds;
              users = {
                chris = {
                  admin = true;
                  feeds.hn = "https://news.ycombinator.com/rss";
                };
                anna = { };
              };
            };
          }
        ];
      };
      service = nixos.config.systemd.services.pressbox;
      socketActivated = nixos.extendModules {
        modules = [ { services.pressbox.socketActivation = true; } ];
      };
      noBackups = nixos.extendModules {
        modules = [ { services.pressbox.backup.enable = false; } ];
      };

      # The Home Manager module, evaluated against stand-ins for the few
      # Home Manager options it sets, so the flake needs no home-manager input.
      evalHomeManager =
        settings:
        lib.evalModules {
          specialArgs = { inherit pkgs; };
          modules = [
            self.homeManagerModules.default
            {
              options = {
                assertions = lib.mkOption { type = lib.types.listOf lib.types.unspecified; };
                home.packages = lib.mkOption { type = lib.types.listOf lib.types.package; };
                xdg.configFile = lib.mkOption {
                  type = lib.types.attrsOf (lib.types.submodule { options.text = lib.mkOption { }; });
                };
              };
            }
            { config.programs.pressbox = settings; }
          ];
        };
      homeManager = evalHomeManager {
        enable = true;
        server = "https://rss.example.org";
        tokenFile = token;
      };
      clientJson = builtins.fromJSON homeManager.config.xdg.configFile."pressbox/client.json".text;
      paired = evalHomeManager {
        enable = true;
        tui.enable = false;
      };
      desktop = evalHomeManager {
        enable = true;
        desktop.enable = true;
      };
      pairedJson = builtins.fromJSON paired.config.xdg.configFile."pressbox/client.json".text;

      expect =
        name: actual: expected:
        lib.assertMsg (
          actual == expected
        ) "${name}: expected ${builtins.toJSON expected}, got ${builtins.toJSON actual}";
    in
    {
      checks = {
        inherit (config.packages) pressboxd pressbox pressbox-iced;

        # Every file has an SPDX license and copyright (https://reuse.software).
        reuse = pkgs.runCommand "pressbox-reuse-lint" { nativeBuildInputs = [ pkgs.reuse ]; } ''
          cd ${self}
          reuse lint
          touch $out
        '';

        nixos-module =
          assert expect "LoadCredential" service.serviceConfig.LoadCredential [ "token:${token}" ];
          assert expect "DynamicUser" service.serviceConfig.DynamicUser true;
          assert expect "Type" service.serviceConfig.Type "notify";
          assert expect "WatchdogSec" service.serviceConfig.WatchdogSec 60;
          assert expect "no socket by default" (nixos.config.systemd.sockets ? pressbox) false;
          assert expect "socket ListenStream" socketActivated.config.systemd.sockets.pressbox.listenStreams [
            "[::]:8470"
          ];
          assert expect "socket-activated wantedBy" socketActivated.config.systemd.services.pressbox.wantedBy
            [ ];
          assert expect "firewall" nixos.config.networking.firewall.allowedTCPPorts [ 8470 ];
          assert expect "RuntimeDirectory" service.serviceConfig.RuntimeDirectory "pressbox";
          assert expect "pressboxctl on PATH"
            (builtins.elem config.packages.pressboxd nixos.config.environment.systemPackages)
            true;
          assert expect "backup timer" nixos.config.systemd.timers.pressbox-backup.timerConfig.OnCalendar
            "daily";
          assert expect "backup command" nixos.config.systemd.services.pressbox-backup.serviceConfig.ExecStart
            "${config.packages.pressboxd}/bin/pressboxctl backup --keep 14";
          assert expect "no backups" (noBackups.config.systemd.timers ? pressbox-backup) false;
          pkgs.runCommand "pressbox-nixos-module" { nativeBuildInputs = [ pkgs.remarshal ]; } ''
            config=${nixos.config.services.pressbox.configFile}
            cat $config
            toml2json $config > config.json
            ${lib.getExe pkgs.jq} -e '
              .listen == "[::]:8470"
              and (has("token_file") | not)
              and .feeds.lwn == { url: "https://lwn.net/headlines/rss", tags: [], full_text: false }
              and .feeds.nixos.full_text == true
              and .behind_proxy == true
              and .fetch_private_addresses == true
              and .max_age_days == 90
              and .feeds.nixos.title == "NixOS"
              and .feeds.nixos.tags == ["nix"]
              and .feeds.nixos.refresh_interval == 3600
              and (.feeds.lwn | has("refresh_interval") | not)
              and .control_socket == "/run/pressbox/control.sock"
              and .users.chris.admin == true
              and .users.chris.feeds.hn == { url: "https://news.ycombinator.com/rss", tags: [], full_text: false }
              and .users.anna == { admin: false, feeds: {} }
              and .backup_dir == "/var/lib/pressbox/backups"
            ' config.json
            touch $out
          '';

        home-manager-module =
          assert expect "client.json" clientJson {
            server = "https://rss.example.org";
            tokenFile = token;
          };
          assert expect "packages" (map lib.getName homeManager.config.home.packages) [ "pressbox" ];
          assert expect "paired client.json" pairedJson { server = "http://127.0.0.1:8470"; };
          assert expect "no TUI" paired.config.home.packages [ ];
          assert expect "desktop" (map lib.getName desktop.config.home.packages) [
            "pressbox"
            "pressbox-iced"
          ];
          pkgs.emptyFile;
      }
      // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
        # A real server polling a feed served by nginx, driven over HTTP.
        nixos-vm = pkgs.testers.runNixOSTest {
          name = "pressbox";
          nodes.machine = {
            imports = [ self.nixosModules.default ];
            environment.etc."pressbox-token" = {
              text = "test-token";
              mode = "0400";
            };
            environment.systemPackages = [
              pkgs.curl
              pkgs.jq
            ];
            services.nginx = {
              enable = true;
              virtualHosts.localhost.root = pkgs.linkFarm "feeds" {
                "feed.xml" = pkgs.writeText "feed.xml" ''
                  <?xml version="1.0"?>
                  <rss version="2.0"><channel><title>Test feed</title>
                    <item><title>Hello</title><guid>t-1</guid><link>http://localhost/1</link>
                      <description>&lt;p&gt;kept&lt;script&gt;dropped()&lt;/script&gt;&lt;/p&gt;</description></item>
                    <item><title>World</title><guid>t-2</guid><link>http://localhost/2</link></item>
                  </channel></rss>
                '';
                "chris.xml" = pkgs.writeText "chris.xml" ''
                  <?xml version="1.0"?>
                  <rss version="2.0"><channel><title>Chris only</title>
                    <item><title>Private</title><guid>c-1</guid><link>http://localhost/c1.html</link>
                      <description>Just the first line.</description></item>
                  </channel></rss>
                '';
                "c1.html" = pkgs.writeText "c1.html" ''
                  <html><head><title>Private</title></head><body>
                    <nav><a href="/">Home</a></nav>
                    <article><h1>Private</h1>
                      ${lib.concatStrings (
                        lib.replicate 8 "<p>The whole article, with every paragraph the feed left out.</p>"
                      )}
                    </article>
                  </body></html>
                '';
              };
            };
            services.pressbox = {
              enable = true;
              tokenFile = "/etc/pressbox-token";
              # The article fullText fetches is served from this machine, which
              # is off limits by default (the unit tests check that refusal).
              fetchPrivateAddresses = true;
              feeds.test = {
                url = "http://localhost/feed.xml";
                refreshInterval = 600;
              };
              users = {
                chris = {
                  admin = true;
                  feeds.mine = {
                    url = "http://localhost/chris.xml";
                    fullText = true;
                  };
                };
                anna = { };
              };
            };
            specialisation.socket.configuration.services.pressbox.socketActivation = true;
          };
          testScript = ''
            import json

            auth = "-H 'Authorization: Bearer test-token'"
            machine.wait_for_unit("nginx.service")
            machine.wait_for_unit("pressbox.service")
            machine.wait_for_open_port(8470)
            machine.succeed("curl -sf localhost:8470/health")
            # Type=notify: systemd shows pressboxd's own status line.
            machine.wait_until_succeeds(
                "systemctl show -P StatusText pressbox.service | grep -q 'new at'"
            )
            # Logs go to the journal natively, tagged pressboxd.
            machine.succeed("journalctl -t pressboxd | grep -qE 'fetched [0-9]+ feeds'")
            machine.fail("curl -sf localhost:8470/api/v1/feeds")
            test_unread = "jq -e '.[] | select(.id == \"test\") | .unread'"
            machine.wait_until_succeeds(
                f"curl -sf {auth} localhost:8470/api/v1/feeds | {test_unread} | grep -qx 2"
            )
            item = machine.succeed(
                f"curl -sf {auth} 'localhost:8470/api/v1/items?feed=test' | jq '.items[0].id'"
            ).strip()
            machine.succeed(
                f"curl -sf {auth} -X PUT -H 'Content-Type: application/json'"
                f" -d '{{\"value\":true}}' localhost:8470/api/v1/items/{item}/read"
            )
            machine.succeed(f"curl -sf {auth} localhost:8470/api/v1/feeds | {test_unread} | grep -qx 1")

            # Feed HTML is sanitized; search finds items.
            summary = machine.succeed(
                f"curl -sf {auth} 'localhost:8470/api/v1/items?q=hello' | jq -r '.items[0].summary'"
            )
            assert "kept" in summary and "script" not in summary, summary
            machine.succeed(f"curl -sf {auth} 'localhost:8470/api/v1/items?q=nothing' | jq -e '.items == []'")

            # Pairing: a client asks, pressboxctl approves, the client gets a token.
            start = json.loads(machine.succeed(
                "curl -sf -X POST -H 'Content-Type: application/json'"
                " -d '{\"name\":\"vm client\"}' localhost:8470/api/v1/pair"
            ))
            machine.succeed(f"pressboxctl pairings | grep -q {start['code']}")
            # With two users, pressboxctl must be told whose device it is.
            machine.fail(f"pressboxctl approve {start['code']}")
            machine.succeed(f"pressboxctl approve {start['code']} --user anna")
            device = json.loads(machine.succeed(f"curl -sf localhost:8470/api/v1/pair/{start['poll_id']}"))
            device_auth = f"-H 'Authorization: Bearer {device['token']}'"
            machine.succeed(f"curl -sf {device_auth} localhost:8470/api/v1/me | jq -e '.user == \"anna\"'")

            # anna sees the shared feed with her own read state, not chris's feed.
            machine.succeed(f"curl -sf {device_auth} localhost:8470/api/v1/feeds | jq -e 'map(.id) == [\"test\"]'")
            machine.succeed(f"curl -sf {device_auth} localhost:8470/api/v1/feeds | {test_unread} | grep -qx 2")
            machine.succeed(f"curl -sf {auth} localhost:8470/api/v1/feeds | jq -e 'map(.id) | sort == [\"mine\", \"test\"]'")
            private = machine.succeed(
                f"curl -sf {auth} 'localhost:8470/api/v1/items?feed=mine' | jq '.items[0].id'"
            ).strip()
            machine.fail(f"curl -sf {device_auth} localhost:8470/api/v1/items/{private}")
            # fullText: the item's page was fetched and its article kept.
            machine.wait_until_succeeds(
                f"curl -sf {auth} localhost:8470/api/v1/items/{private} | jq -r .content"
                " | grep -q 'every paragraph the feed left out'"
            )

            machine.succeed("pressboxctl users | grep -q '^anna .* 1 devices'")
            machine.succeed("pressboxctl devices --user anna | grep -q vm-client")
            machine.succeed("pressboxctl revoke vm-client")
            machine.fail(f"curl -sf {device_auth} localhost:8470/api/v1/feeds")
            machine.succeed("pressboxctl status | grep -q 'feeds: *2'")
            machine.succeed("pressboxctl status | grep -q 'users: *2'")

            # The backup timer's service writes a backup as the service user.
            machine.succeed("systemctl start pressbox-backup.service")
            machine.succeed("ls /var/lib/private/pressbox/backups/pressbox-*.db")

            # Read state survives a restart.
            machine.systemctl("restart pressbox.service")
            machine.wait_for_open_port(8470)
            machine.succeed(f"curl -sf {auth} localhost:8470/api/v1/feeds | {test_unread} | grep -qx 1")

            # Socket activation: nothing runs until the first request.
            machine.succeed(
                "/run/current-system/specialisation/socket/bin/switch-to-configuration test"
            )
            machine.wait_for_unit("pressbox.socket")
            machine.succeed("systemctl stop pressbox.service")
            machine.succeed("! systemctl is-active pressbox.service")
            machine.succeed(f"curl -sf {auth} localhost:8470/api/v1/feeds | {test_unread} | grep -qx 1")
            machine.succeed("systemctl is-active pressbox.service")
          '';
        };
      };
    };
}
