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
    # Explicit cross-platform system list. `nix-systems/default` would expose
    # only the host's system, which breaks evaluating darwin outputs from a
    # linux dev box (and vice versa). We need all four so CI on linux and
    # devs on nix-darwin (M-series + Intel) can both build the workspace.
    systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];

    perSystem = { pkgs, system, self', ... }:
      let
        pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [ inputs.rust-overlay.overlays.default ];
        };

        # Native toolchain — full default + wasm32 target so a single toolchain
        # can build both native and WASM (wasm-pack picks up rustc from PATH;
        # this avoids "wasm32-unknown-unknown target not found" when the
        # native toolchain wins in PATH ordering).
        rustToolchainNative = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "clippy" "rustfmt" ];
          targets = [ "wasm32-unknown-unknown" ];
        };

        # Kept for crane wasm-only check derivations (no need for full default
        # tooling there).
        rustToolchainWasm = pkgs.rust-bin.stable.latest.minimal.override {
          targets = [ "wasm32-unknown-unknown" ];
        };

        craneLib     = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchainNative;
        craneLibWasm = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchainWasm;

        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            (pkgs.lib.fileset.fileFilter (file: builtins.any file.hasExt [ "rs" "toml" "lock" "md" "html" "scss" "js" "ts" "json" "png" "ico" "sh" "csv" "proto" "wgsl" ]) ./.)
          ];
        };

        # Shared build args
        commonArgs = { inherit src; strictDeps = true; };

        # Dependency caches — built once, reused per target
        # protobuf is needed for graph-api's prost-build; pkg-config + bevyLibs
        # remain for graph-layouts/graph-renderer (Bevy is still in graph-renderer's tree as historical/example bin? — keep until we drop it).
        depsNative = craneLib.buildDepsOnly (commonArgs // {
          nativeBuildInputs = [ pkgs.pkg-config pkgs.protobuf ];
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

        graph-api = craneLib.buildPackage (commonArgs // {
          cargoArtifacts = depsNative;
          cargoExtraArgs = "--package graph-api";
          # graph-api is pure Rust (axum + prost) — no system libs, just protoc
          nativeBuildInputs = [ pkgs.protobuf ];
        });

        graph-compute = craneLib.buildPackage (commonArgs // {
          cargoArtifacts = depsNative;
          cargoExtraArgs = "--package graph-compute";
          nativeBuildInputs = [ pkgs.protobuf ];
        });

        # ----- Distributed compute backend: single source of truth -----
        # The same service spec drives both the local docker-compose stack
        # (`just dev-up`) and the SkyPilot cloud task (`just sky-up`). Edit
        # this attrset, then `nix run .#render-stack-configs` to regenerate
        # both YAMLs.
        graphComputeService = {
          name        = "graph-compute";
          port        = 50051;
          tickHz      = 30;
          rustLog     = "info";
          # Bind all interfaces inside the container/pod so external clients
          # (broker, probe) can reach the gRPC port. The native default is
          # `[::1]:50051` which works only for in-host loopback.
          bindAddr    = "[::]:50051";
          # Cloud-only: SkyPilot accelerator request. Ignored locally.
          accelerator = "L4:1";
        };

        # OCI image built from the Crane derivation — no Dockerfile needed.
        graph-compute-image = pkgs.dockerTools.buildLayeredImage {
          name     = graphComputeService.name;
          tag      = "latest";
          contents = [ graph-compute pkgs.cacert ];
          config   = {
            Cmd = [ "/bin/graph-compute" ];
            ExposedPorts."${toString graphComputeService.port}/tcp" = {};
            Env = [
              "GRAPH_COMPUTE_TICK_HZ=${toString graphComputeService.tickHz}"
              "GRAPH_COMPUTE_ADDR=${graphComputeService.bindAddr}"
              "RUST_LOG=${graphComputeService.rustLog}"
            ];
          };
        };

        yamlFmt = pkgs.formats.yaml {};

        docker-compose-yaml = yamlFmt.generate "docker-compose.yml" {
          services."${graphComputeService.name}" = {
            image       = "${graphComputeService.name}:latest";
            ports       = [ "${toString graphComputeService.port}:${toString graphComputeService.port}" ];
            environment = {
              GRAPH_COMPUTE_TICK_HZ = toString graphComputeService.tickHz;
              GRAPH_COMPUTE_ADDR    = graphComputeService.bindAddr;
              RUST_LOG              = graphComputeService.rustLog;
            };
            restart = "unless-stopped";
          };
        };

        sky-task-yaml = yamlFmt.generate "graph-compute.sky.yaml" {
          resources = {
            accelerators = graphComputeService.accelerator;
            ports        = [ graphComputeService.port ];
          };
          file_mounts."/opt/jump-cannon" = ".";
          setup = ''
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
            source $HOME/.cargo/env
            sudo apt-get update -y
            sudo apt-get install -y libvulkan1 vulkan-tools mesa-vulkan-drivers
            vulkaninfo --summary || echo "WARN: no vulkan; wgpu falls back to CPU"
            cd /opt/jump-cannon
            cargo build --release -p graph-compute
          '';
          run = ''
            source $HOME/.cargo/env
            cd /opt/jump-cannon
            GRAPH_COMPUTE_TICK_HZ=${toString graphComputeService.tickHz} \
            GRAPH_COMPUTE_ADDR=${graphComputeService.bindAddr} \
            RUST_LOG=${graphComputeService.rustLog} \
            ./target/release/graph-compute
          '';
        };

        # `nix run .#render-stack-configs` — regenerates both YAML files
        # from the shared graphComputeService spec above.
        render-stack-configs = pkgs.writeShellApplication {
          name = "render-stack-configs";
          runtimeInputs = [ pkgs.coreutils ];
          text = ''
            set -euo pipefail
            install -m 0644 ${docker-compose-yaml} docker-compose.yml
            install -d infra/sky
            install -m 0644 ${sky-task-yaml} infra/sky/graph-compute.yaml
            echo "rendered: docker-compose.yml + infra/sky/graph-compute.yaml"
          '';
        };

        # `nix run .#dev-up` — load the Nix-built image into podman, start compose.
        # On darwin, podman needs a Linux VM (`podman machine`) — preflight it
        # so the script gives a clear actionable error instead of a confusing
        # "Cannot connect to Podman" from `podman load`.
        dev-up = pkgs.writeShellApplication {
          name = "dev-up";
          runtimeInputs = [ pkgs.podman pkgs.podman-compose ];
          text = ''
            set -euo pipefail
            if [ "$(uname -s)" = "Darwin" ]; then
              if ! podman machine list --format '{{.Running}}' 2>/dev/null | grep -q true; then
                echo "error: podman machine is not running on darwin." >&2
                echo "  start it with: podman machine init && podman machine start" >&2
                echo "  (one-time init; subsequent boots only need 'podman machine start')" >&2
                exit 1
              fi
            fi
            echo "loading ${graphComputeService.name}:latest into podman..."
            podman load < ${graph-compute-image}
            echo "starting compose stack..."
            podman-compose up -d
          '';
        };

        dev-down = pkgs.writeShellApplication {
          name = "dev-down";
          runtimeInputs = [ pkgs.podman-compose ];
          text = ''
            set -euo pipefail
            podman-compose down
          '';
        };

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
          default          = graph-api;
          inherit vault-search graph-api graph-compute graph-layouts-wasm tvix-wasm;
          inherit graph-compute-image docker-compose-yaml sky-task-yaml;
        };

        apps = {
          render-stack-configs = { type = "app"; program = "${render-stack-configs}/bin/render-stack-configs"; };
          dev-up   = { type = "app"; program = "${dev-up}/bin/dev-up"; };
          dev-down = { type = "app"; program = "${dev-down}/bin/dev-down"; };
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

            # Build tools
            pkg-config
            protobuf

            # Dev workflow
            just

            # Headless browser test (`just test-browser`).
            # nodejs runs the Playwright script; playwright-driver.browsers
            # provides a Chromium that's already wired up for both Linux and
            # macOS — no `npx playwright install` needed at runtime. The
            # PLAYWRIGHT_BROWSERS_PATH env var below points playwright at it.
            nodejs_22
            playwright-driver.browsers

            # Local dev cluster (`just dev-up` / `just dev-down`). podman runs
            # rootless on NixOS without enabling system docker; podman-compose
            # parses the same docker-compose.yml.
            podman
            podman-compose
          ] ++ bevyLibs;

          # Point Playwright at the nix-provided browser bundle and skip its
          # post-install download step (which fails in the pure devshell).
          PLAYWRIGHT_BROWSERS_PATH = "${pkgs.playwright-driver.browsers}";
          PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS = "true";
          PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD = "true";

          # Linux: make Bevy's dynamic libs findable at runtime
          LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.isLinux
            (pkgs.lib.makeLibraryPath bevyLibsLinux);

          # Linux: Vulkan software renderer fallback (headless CI / no GPU)
          VK_ICD_FILENAMES = pkgs.lib.optionalString pkgs.stdenv.isLinux
            "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.x86_64.json";

          # Source .env on shell entry. graph-api also reads it directly via
          # dotenvy at startup; sourcing it in the shell is convenience for
          # ad-hoc commands. Future: add a per-machine .env per host name.
          shellHook = ''
            if [ -f .env ]; then
              set -a
              # shellcheck disable=SC1091
              . ./.env
              set +a
            fi
            # Make cargo-built binaries findable for cross-process spawning
            # (graph-api spawns vault-search as a subprocess).
            export PATH="$PWD/target/release:$PWD/target/debug:$PATH"
          '';
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
