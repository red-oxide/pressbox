# SPDX-FileCopyrightText: 2026 Chris Palmer <chris@red-oxide.org>
# SPDX-License-Identifier: GPL-3.0-only

# pressboxd, pressbox and pressbox-iced, built with crane. There are two
# dependency builds: cargoArtifacts, shared by pressboxd, the TUI and their
# Clippy check, and guiArtifacts for the desktop client. So pressboxd and the
# TUI never build iced and its graphics stack. (buildDepsOnly can't start
# from other artifacts, so crates both use, such as tokio, build in each.)
_: {
  perSystem =
    {
      pkgs,
      lib,
      craneLib,
      ...
    }:
    let
      cargoToml = lib.importTOML ../Cargo.toml;

      src = lib.fileset.toSource {
        root = ../.;
        fileset = lib.fileset.unions [
          ../Cargo.toml
          ../Cargo.lock
          (craneLib.fileset.commonCargoSources ../crates)
          (craneLib.fileset.commonCargoSources ../clients/tui)
          (craneLib.fileset.commonCargoSources ../clients/iced)
          ../assets/logo.svg
          ../assets/pressbox-iced.desktop
        ];
      };

      commonArgs = {
        inherit src;
        inherit (cargoToml.workspace.package) version;
        pname = "pressbox-workspace";
        strictDeps = true;

        # reqwest (rustls) loads the system CA certificates when a client is
        # built, and the build sandbox has none, so tests that build a client
        # fail with "No CA certificates were loaded from the system".
        # cacert's setup hook points SSL_CERT_FILE at its bundle.
        nativeCheckInputs = [ pkgs.cacert ];
      };

      # The desktop client plays podcasts with GStreamer: it links the core
      # and base libraries, and finds the plugins (HTTP sources, demuxers,
      # decoders, scaletempo for faster playback) through
      # GST_PLUGIN_SYSTEM_PATH_1_0 at run time.
      gstLibs = [
        pkgs.gst_all_1.gstreamer
        pkgs.gst_all_1.gst-plugins-base
      ];
      gstPlugins =
        gstLibs
        ++ [
          pkgs.gst_all_1.gst-plugins-good
          pkgs.gst_all_1.gst-plugins-bad
          pkgs.gst_all_1.gst-libav
        ]
        # PipeWire's own sink, so autoaudiosink can play straight to PipeWire
        # rather than through its PulseAudio shim.
        ++ lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.pipewire;
      gstPluginPath = lib.makeSearchPathOutput "lib" "lib/gstreamer-1.0" gstPlugins;
      # GStreamer's HTTP source does TLS through GIO, whose TLS backend is a
      # module in glib-networking: without it every https:// episode fails
      # to start ("Element failed to change its state").
      gioModules = "${pkgs.glib-networking}/lib/gio/modules";
      # What building (and testing) pressbox-iced needs.
      gstBuild = {
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = gstLibs;
      };

      # Everything but the desktop client.
      serverArgs = commonArgs // {
        cargoExtraArgs = "--locked --workspace --exclude pressbox-iced";
      };
      guiArgs =
        commonArgs
        // gstBuild
        // {
          cargoExtraArgs = "--locked --package pressbox-iced";
        };

      cargoArtifacts = craneLib.buildDepsOnly serverArgs;
      guiArtifacts = craneLib.buildDepsOnly guiArgs;

      # winit and wgpu open these with dlopen at run time instead of linking
      # them, so they go on pressbox-iced's RUNPATH (and in the dev shell).
      guiRuntimeLibs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [
        pkgs.libGL
        pkgs.libxkbcommon
        pkgs.vulkan-loader
        pkgs.wayland
        pkgs.libx11
        pkgs.libxcursor
        pkgs.libxi
        pkgs.libxrandr
      ];

      mkCrate =
        {
          pname,
          description,
          license,
          mainProgram ? pname,
        }:
        craneLib.buildPackage (
          commonArgs
          // {
            inherit pname cargoArtifacts;
            # Applies to both the build and the tests.
            cargoExtraArgs = "--locked --package ${pname}";

            meta = {
              inherit description license mainProgram;
              homepage = "https://github.com/red-oxide/pressbox";
              platforms = lib.platforms.unix;
            };
          }
        );

      pressboxd = mkCrate {
        pname = "pressboxd";
        description = "pressbox RSS/Atom aggregation server";
        license = lib.licenses.agpl3Only;
      };

      pressbox = mkCrate {
        pname = "pressbox";
        description = "Terminal client for pressbox";
        license = lib.licenses.gpl3Only;
      };

      pressbox-iced = craneLib.buildPackage (
        guiArgs
        // {
          pname = "pressbox-iced";
          cargoArtifacts = guiArtifacts;

          nativeBuildInputs = gstBuild.nativeBuildInputs ++ [ pkgs.makeWrapper ];

          postInstall = ''
            install -Dm644 assets/pressbox-iced.desktop -t $out/share/applications
            install -Dm644 assets/logo.svg $out/share/icons/hicolor/scalable/apps/pressbox-iced.svg
          '';

          postFixup =
            lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
              patchelf --add-rpath ${lib.makeLibraryPath guiRuntimeLibs} $out/bin/pressbox-iced
            ''
            + ''
              wrapProgram $out/bin/pressbox-iced \
                --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${gstPluginPath} \
                --prefix GIO_EXTRA_MODULES : ${gioModules}
            '';

          meta = {
            description = "Desktop client for pressbox, built with iced";
            homepage = "https://github.com/red-oxide/pressbox";
            license = lib.licenses.gpl3Only;
            mainProgram = "pressbox-iced";
            platforms = lib.platforms.linux ++ lib.platforms.darwin;
          };
        }
      );
    in
    {
      _module.args = {
        inherit
          guiRuntimeLibs
          gstLibs
          gstPluginPath
          gioModules
          ;
      };

      packages = {
        inherit pressboxd pressbox pressbox-iced;
        default = pressboxd;
      };

      checks = {
        # Together the same as `just lint`.
        clippy = craneLib.cargoClippy (
          serverArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          }
        );
        clippy-iced = craneLib.cargoClippy (
          guiArgs
          // {
            cargoArtifacts = guiArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          }
        );
      }
      // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
        # Line coverage of the whole workspace, as `just coverage` measures
        # it, which must stay at 95% or more: new code comes with tests, or is
        # marked `#[cfg(not(tarpaulin_include))]` with the reason. Not 100%:
        # newer rustc counts error-path closures, `?` returns and logging
        # arguments as lines of their own, and many can't run in a test.
        # Linux only, where CI runs it. The instrumented build shares nothing
        # with the others, so it inherits no artifacts.
        coverage = craneLib.cargoTarpaulin (
          commonArgs
          // gstBuild
          // {
            cargoArtifacts = null;
            doInstallCargoArtifacts = false;
            # The code as written, not as optimized away.
            CARGO_PROFILE = "dev";
            cargoExtraArgs = "--locked --workspace";
            cargoTarpaulinExtraArgs = "--engine llvm --fail-under 95 --skip-clean --out xml --output-dir $out";
            # Tarpaulin runs the tests in the build phase, so the CA bundle
            # (see commonArgs) is needed there.
            nativeBuildInputs = gstBuild.nativeBuildInputs ++ [ pkgs.cacert ];
          }
        );
      };
    };
}
