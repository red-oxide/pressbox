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

  # Inputs are pinned with tack in ./.tack (pins.toml, locked by
  # `tack update`), not here, so there is no flake.lock. A flake taking this
  # one as an input needs no `inputs.pressbox.inputs.*.follows`.
  outputs =
    { self, ... }@args:
    let
      pins = (import ./.tack) { overrides = args.tackOverrides or { }; };
      # Drop tack's own attrs so flake-parts only sees real inputs.
      inputs =
        removeAttrs pins [
          "_meta"
          "_patched"
          "__functor"
        ]
        // {
          inherit self;
        };
    in
    inputs.devfmt.lib.mkFlake { inherit inputs; } {
      imports = [
        (inputs.devfmt.lib.importTree ./nix)
      ];
    };
}
