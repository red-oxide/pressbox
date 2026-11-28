# SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
# SPDX-License-Identifier: GPL-3.0-only

{
  description = "pressbox: a self-hosted RSS/Atom aggregator with feeds declared in Nix";

  # Builds from this flake's CI. Used by `nix build`/`nix run` on this flake
  # (after accepting the prompt); a flake that takes this one as an input
  # must add the cache to its own nix.settings instead.
  nixConfig = {
    extra-substituters = [ "https://red-oxide.cachix.org" ];
    extra-trusted-public-keys = [
      "red-oxide.cachix.org-1:kX7CnXH4XIGlSiCojVBNK3zcka8p2po2PnfNToDYauI="
    ];
  };

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    devfmt = {
      url = "github:red-oxide/devfmt";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # For the clients' wrapper modules (wrapperModules.*, wrappers.*).
    nix-wrapper-modules = {
      url = "github:nix-community/nix-wrapper-modules";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs:
    inputs.devfmt.lib.mkFlake { inherit inputs; } {
      imports = [
        (inputs.devfmt.lib.importTree ./nix)
      ];
    };
}
