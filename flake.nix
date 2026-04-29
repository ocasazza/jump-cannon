{
  description = "jump-cannon — Rust monorepo: graph visualization, vault search, combinator query language";

  nixConfig = {
    extra-substituters = [
      "https://nix-community.cachix.org"
      "https://ocasazza.cachix.org"
      "https://crane.cachix.org"
    ];
    extra-trusted-public-keys = [
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
      "ocasazza.cachix.org-1:4J9/Csix7SSPiUIyaSeISIT475va14uZPwJVipSDY+Y="
      "crane.cachix.org-1:8Scfpmn9w+hGdXH/Q9tTLiYAE/2dnJYRJP7kl80GuRk="
    ];
  };

  inputs = {
    nixpkgs.url     = "github:nixos/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    systems.url     = "github:nix-systems/default";

    crane.url = "github:ipetkov/crane";
    crane.inputs.nixpkgs.follows = "nixpkgs";

    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";

    omnix.url = "github:juspay/omnix";
    omnix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = inputs: inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    systems = import inputs.systems;

    perSystem = { pkgs, system, self', ... }:
      let
        pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [ inputs.rust-overlay.overlays.default ];
        };

        # Native toolchain (stable, includes rust-src for IDE)
        rustToolchainNative = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "clippy" "rustfmt" ];
        };

        # WASM toolchain (minimal + wasm32 target)
        rustToolchainWasm = pkgs.rust-bin.stable.latest.minimal.override {
          targets = [ "wasm32-unknown-unknown" ];
        };

        craneLib     = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchainNative;
        craneLibWasm = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchainWasm;

        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            (pkgs.lib.fileset.fileFilter (file: builtins.any file.hasExt [ "rs" "toml" "lock" "md" "html" "scss" "js" "ts" "json" "png" "ico" "sh" "csv" ]) ./.)
          ];
        };

        # Shared build args
        commonArgs = { inherit src; strictDeps = true; };

        # Dependency caches — built once, reused per target
        depsNative = craneLib.buildDepsOnly (commonArgs // {
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = bevyLibs;
        });
        depsWasm   = craneLibWasm.buildDepsOnly (commonArgs // {
          CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
          cargoExtraArgs = "--package graph-layouts --package tvix-wasm";
        });

        # System deps for Bevy — platform-split so the flake works on both Linux and macOS
        bevyLibsLinux = with pkgs; lib.optionals stdenv.isLinux [
          libGL
          vulkan-loader
          alsa-lib
          udev
          wayland
          libxkbcommon
          libx11
          libxcursor
          libxrandr
          libxi
        ];
        # macOS: Bevy uses Metal via wgpu. Frameworks (Metal, CoreAudio, AppKit, etc.)
        # are provided by the Xcode CLT and don't need to be listed here — adding them
        # via darwin.apple_sdk breaks on nixpkgs-unstable (apple_sdk_11_0 removed).
        bevyLibs = bevyLibsLinux;

        # ----- Native packages -----

        vault-search = craneLib.buildPackage (commonArgs // {
          cargoArtifacts = depsNative;
          cargoExtraArgs = "--package vault-search";
        });

        graph-ui = craneLib.buildPackage (commonArgs // {
          cargoArtifacts = depsNative;
          cargoExtraArgs = "--package graph-ui";
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = bevyLibs;
        });

        # ----- WASM packages -----

        graph-layouts-wasm = craneLibWasm.buildPackage (commonArgs // {
          cargoArtifacts = depsWasm;
          cargoExtraArgs = "--package graph-layouts";
          CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
          nativeBuildInputs = [ pkgs.wasm-bindgen-cli ];
        });

        tvix-wasm = craneLibWasm.buildPackage (commonArgs // {
          cargoArtifacts = depsWasm;
          cargoExtraArgs = "--package tvix-wasm --features wasm";
          CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
          nativeBuildInputs = [ pkgs.wasm-bindgen-cli ];
        });

      in {
        packages = {
          default          = graph-ui;
          inherit vault-search graph-ui graph-layouts-wasm tvix-wasm;
        };

        checks = {
          # Native: clippy + tests + fmt
          clippy = craneLib.cargoClippy (commonArgs // {
            cargoArtifacts = depsNative;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
          tests-unit = craneLib.cargoNextest (commonArgs // {
            cargoArtifacts = depsNative;
            cargoNextestExtraArgs = "--profile unit";
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = bevyLibs;
          });

          tests-integration = craneLib.cargoNextest (commonArgs // {
            cargoArtifacts = depsNative;
            cargoNextestExtraArgs = "--profile integration";
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = bevyLibs;
          });

          tests-e2e = craneLib.cargoNextest (commonArgs // {
            cargoArtifacts = depsNative;
            cargoNextestExtraArgs = "--profile e2e";
            # E2E needs display server libs for headless Bevy
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = bevyLibs;
          });
          fmt = craneLib.cargoFmt { inherit src; };

          # WASM: clippy only (no test runner for wasm32 in CI)
          clippy-wasm = craneLibWasm.cargoClippy (commonArgs // {
            cargoArtifacts = depsWasm;
            CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
            cargoExtraArgs = "--package graph-layouts --package tvix-wasm";
            cargoClippyExtraArgs = "-- -D warnings";
          });
        };

        devShells.default = craneLib.devShell {
          # Inherit all checks so they can be run inside the shell
          checks = self'.checks;

          packages = with pkgs; [
            # Rust tooling
            rustToolchainNative
            cargo-nextest
            cargo-watch
            cargo-expand
            rust-analyzer

            # WASM tooling
            rustToolchainWasm
            wasm-pack
            wasm-bindgen-cli
            trunk

            # Bevy runtime/build deps
            pkg-config
          ] ++ bevyLibs;

          # Linux: make Bevy's dynamic libs findable at runtime
          LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.isLinux
            (pkgs.lib.makeLibraryPath bevyLibsLinux);

          # Linux: Vulkan software renderer fallback (headless CI / no GPU)
          VK_ICD_FILENAMES = pkgs.lib.optionalString pkgs.stdenv.isLinux
            "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.x86_64.json";
        };
      };

    # omnix CI config — om ci runs build + checks
    flake.om.ci.default = {
      root = {
        dir = ".";
        steps = {
          build.enable  = true;
          checks.enable = true;
        };
      };
    };
  };
}
