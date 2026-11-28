# SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
# SPDX-License-Identifier: GPL-3.0-only

# Wrapper modules (nix-wrapper-modules) for the clients: the Home Manager
# module's settings baked into the package instead of ~/.config.
{
  self,
  inputs,
  lib,
  ...
}:
let
  # Settings both clients take, as in programs.pressbox.
  common =
    { lib, ... }:
    {
      options = {
        server = lib.mkOption {
          type = lib.types.str;
          default = "http://127.0.0.1:8470";
          example = "https://rss.example.org";
          description = "URL of the pressboxd server.";
        };
        tokenFile = lib.mkOption {
          type = lib.types.nullOr (lib.types.pathWith { inStore = false; });
          default = null;
          example = "/run/secrets/pressbox-token";
          description = ''
            File holding the API token. Leave unset to pair instead: pairing
            saves the token to `$XDG_STATE_HOME/pressbox/token`.
          '';
        };
      };
    };

  client =
    name:
    {
      config,
      lib,
      pkgs,
      wlib,
      ...
    }:
    {
      imports = [
        wlib.modules.default
        common
      ];
      config = {
        package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.${name};
        # Both clients (and Birdland's bar) read these before client.json.
        env = {
          PRESSBOX_SERVER = config.server;
        }
        // lib.optionalAttrs (config.tokenFile != null) {
          PRESSBOX_TOKEN_FILE = config.tokenFile;
        };
      };
    };
in
{
  flake = {
    wrapperModules = {
      pressbox =
        { config, lib, ... }:
        {
          imports = [ (client "pressbox") ];
          options.player = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            example = lib.literalExpression "lib.getExe pkgs.mpv";
            description = "The mpv to play podcasts with. Leave unset to use `mpv` from the `PATH`.";
          };
          # The player has no flag or variable, so it goes in a config file.
          config.flags = lib.mkIf (config.player != null) {
            "--config" = builtins.toFile "client.json" (builtins.toJSON { inherit (config) player; });
          };
        };
      pressbox-iced = client "pressbox-iced";
    };
    wrappers = lib.mapAttrs (
      _: module: (inputs.nix-wrapper-modules.lib.evalModule module).config
    ) self.wrapperModules;
  };

  perSystem =
    { pkgs, ... }:
    {
      checks = lib.mapAttrs' (
        name: wrapper:
        lib.nameValuePair "wrapper-${name}" (
          wrapper.wrap {
            inherit pkgs;
            server = "https://rss.example.org";
            tokenFile = "/run/secrets/pressbox-token";
          }
        )
      ) self.wrappers;
    };
}
