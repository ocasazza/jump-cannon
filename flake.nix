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
            # "nix" is required: tvix-wasm embeds crates/tvix-wasm/src/nix/*.nix
            # via include_str!, so those files must be in the crane source or any
            # build that compiles tvix-wasm natively (e.g. the graph-compute
            # gpu tests' Nix-fixture corpus) fails to compile.
            (pkgs.lib.fileset.fileFilter (file: builtins.any file.hasExt [ "rs" "toml" "lock" "md" "html" "scss" "js" "ts" "json" "png" "ico" "sh" "csv" "proto" "wgsl" "nix" ]) ./.)
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

        # Perf benches (REPORT-ONLY, never gates a merge). Runs the criterion
        # bench_pagerank example and captures its JSON/HTML to $out as a Hydra
        # build product. `__noChroot` so the build reaches the real GPU — only
        # meaningful on the aarch64-darwin Metal builders (perf under Linux
        # lavapipe is software and meaningless), so hydraJobs wires this on
        # darwin only. Timing output varies run-to-run ⇒ it never caches; that's
        # intended for a per-merge perf signal. Requires the darwin Hydra
        # builders to permit __noChroot (nix.settings extra-sandbox / trusted).
        bench-pagerank = craneLib.mkCargoDerivation (commonArgs // {
          cargoArtifacts = depsNative;
          pname = "graph-compute-bench-pagerank";
          version = "0.1.0";
          __noChroot = true;
          nativeBuildInputs = [ pkgs.pkg-config pkgs.protobuf ];
          buildInputs = bevyLibs;
          buildPhaseCargoCommand = ''
            cargo run --release -p graph-compute --example bench_pagerank -- \
              --bench --noplot --save-baseline hydra
          '';
          doInstallCargoArtifacts = false;
          doCheck = false;
          installPhaseCommand = ''
            mkdir -p $out/nix-support
            if [ -d target/criterion ]; then cp -r target/criterion $out/criterion; fi
            # Surface the criterion dir as a Hydra build product for the report.
            echo "report criterion $out/criterion" > $out/nix-support/hydra-build-products
          '';
        });

        # Foundation of the Rust-driven browser regression suite. The
        # `test-browser` binary speaks CDP directly via chromiumoxide —
        # no chromedriver, no playwright, no JS. It expects an already-
        # running graph-api server and a chromium executable on the CLI.
        # The `test-browser-rust` app below wires the full stack.
        test-browser = craneLib.buildPackage (commonArgs // {
          cargoArtifacts = depsNative;
          cargoExtraArgs = "--package test-browser";
        });

        # `nix run .#test-browser-rust` — bring up graph-api + open the
        # page in chromium with WebGPU enabled + run the Rust smoke test.
        #
        # NOTE: this wrapper depends on:
        #   1. A trunk dist. Defaults to the nix-built graph-renderer-web
        #      store path; override with ASSETS_DIR for fast iteration
        #      against a local `trunk watch` build.
        #   2. A test vault at /tmp/test-vault (auto-seeded by `just`
        #      recipes; this wrapper also seeds a minimal one).
        # The wrapper bails with a clear error if (1) is missing.
        test-browser-rust = pkgs.writeShellApplication {
          name = "test-browser-rust";
          runtimeInputs = [
            graph-api
            test-browser
            pkgs.chromium
            pkgs.curl
            pkgs.coreutils
          ];
          text = ''
            set -euo pipefail

            REPO_ROOT="''${REPO_ROOT:-$PWD}"
            ASSETS_DIR="''${ASSETS_DIR:-${graph-renderer-web}}"
            VAULT="''${VAULT_ROOT:-/tmp/test-vault}"
            PORT="''${TEST_PORT:-47896}"
            OUT_DIR="''${OUT_DIR:-$REPO_ROOT/target/test-browser-rust}"

            if [ ! -f "$ASSETS_DIR/index.html" ]; then
              echo "error: no trunk dist at $ASSETS_DIR" >&2
              echo "hint: unset ASSETS_DIR to use the nix-built graph-renderer-web," >&2
              echo "  or run 'just wasm' and point ASSETS_DIR at the trunk-watch dist." >&2
              exit 2
            fi

            mkdir -p "$VAULT" "$OUT_DIR"
            if [ ! -f "$VAULT/Alpha.md" ]; then
              printf 'See [[Beta]] and [[Gamma]].\n' > "$VAULT/Alpha.md"
              printf '[[Alpha]]\n'                   > "$VAULT/Beta.md"
              printf '[[Alpha]] [[Beta]]\n'          > "$VAULT/Gamma.md"
            fi

            # Software vulkan ICD for WebGPU on headless linux — mirrors the
            # devshell's VK_ICD_FILENAMES setting.
            if [ -z "''${VK_ICD_FILENAMES:-}" ] && [ -d ${pkgs.mesa}/share/vulkan/icd.d ]; then
              export VK_ICD_FILENAMES=${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.x86_64.json
            fi

            echo "→ starting graph-api on port $PORT…"
            graph-api \
              --vault-root "$VAULT" \
              --port "$PORT" \
              --no-browser \
              --assets-dir "$ASSETS_DIR" &
            SERVER_PID=$!
            trap 'kill "$SERVER_PID" 2>/dev/null || true' EXIT

            # Wait for /
            for _ in $(seq 1 30); do
              if curl -sf "http://127.0.0.1:$PORT/" > /dev/null; then
                break
              fi
              sleep 1
            done

            echo "→ running test-browser…"
            test-browser \
              --base-url "http://127.0.0.1:$PORT" \
              --chromium ${pkgs.chromium}/bin/chromium \
              --out-dir "$OUT_DIR" \
              --timeout-secs 60
          '';
        };

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

        # ----- graph-api service -----
        #
        # The graph-api container ingests $VAULT_ROOT at startup and watches
        # for changes via inotify; progress is surfaced to the frontend
        # footer via `GET /progress`. The compose service below bind-mounts
        # the host's $VAULT_ROOT into /vault and the trunk dist into
        # /assets. $ASSETS_DIR defaults to the nix-built graph-renderer-web
        # derivation, so `just dev-up` works without a prior `just wasm`;
        # set it explicitly when iterating on the frontend with
        # `trunk watch`.
        graphApiService = {
          name = "graph-api";
          port = 8765;
          rustLog = "info";
        };

        graph-api-image = pkgs.dockerTools.buildLayeredImage {
          name     = graphApiService.name;
          tag      = "latest";
          # vault-search is a sibling binary that graph-api spawns as a
          # subprocess at startup. Bake it into the image's PATH alongside
          # graph-api itself so the in-container spawn works without
          # needing a separate sidecar.
          contents = [ graph-api vault-search pkgs.cacert ];
          config   = {
            Cmd = [ "/bin/graph-api" ];
            ExposedPorts."${toString graphApiService.port}/tcp" = {};
            Env = [
              "GRAPH_API_HOST=0.0.0.0"
              "GRAPH_API_PORT=${toString graphApiService.port}"
              "GRAPH_API_NO_BROWSER=1"
              "GRAPH_RENDERER_ASSETS_DIR=/assets"
              "VAULT_ROOT=/vault"
              "RUST_LOG=${graphApiService.rustLog}"
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
          # graph-api: ingests $VAULT_ROOT on boot, watches for changes,
          # surfaces progress to the renderer via GET /progress.
          services."${graphApiService.name}" = {
            image       = "${graphApiService.name}:latest";
            ports       = [ "${toString graphApiService.port}:${toString graphApiService.port}" ];
            # Bind-mount the host vault read-only (the renderer's PUT
            # /vault/page editor surface would need rw — flip to `:rw` if
            # you're using that). Bind-mount the pre-built trunk dist
            # so the in-container graph-api can serve / and /assets.
            # `VAULT_ROOT` and `ASSETS_DIR` are host-side env vars; default
            # both to the canonical in-repo paths.
            volumes = [
              # rw, not ro: the renderer's PUT /vault/page editor surface
              # (commit c629cd7f) needs to write back to the vault.
              "\${VAULT_ROOT:-./vault}:/vault:rw"
              # ASSETS_DIR is set by `nix run .#dev-up` to the nix-built
              # graph-renderer-web store path. Direct `podman-compose up`
              # users can either export ASSETS_DIR=$(nix build --no-link
              # --print-out-paths .#graph-renderer-web) or point it at
              # their local `trunk watch` dist for fast iteration.
              "\${ASSETS_DIR:-./crates/graph-renderer/assets/dist}:/assets:ro"
            ];
            environment = {
              GRAPH_API_HOST              = "0.0.0.0";
              GRAPH_API_PORT              = toString graphApiService.port;
              GRAPH_API_NO_BROWSER        = "1";
              GRAPH_RENDERER_ASSETS_DIR   = "/assets";
              VAULT_ROOT                  = "/vault";
              JUMP_CANNON_COMPUTE_URL     = "http://${graphComputeService.name}:${toString graphComputeService.port}";
              RUST_LOG                    = graphApiService.rustLog;
            };
            depends_on = [ graphComputeService.name ];
            restart    = "unless-stopped";
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

        # `nix run .#dev-up` — bring up the graph-compute backend.
        #
        # Linux: load the Nix-built OCI image into podman + start compose.
        # The image's binary is Linux ELF for the host arch, so it runs.
        #
        # Darwin: the `graph-compute` derivation is a darwin Mach-O binary
        # (rust-toolchain targets the host system). Packaging it into an
        # OCI image and `podman exec`-ing it inside the Linux VM fails
        # with "Exec format error" — Mach-O can't run in a Linux VM.
        # Cross-compiling the Rust crate to Linux from a darwin host is
        # non-trivial (wgpu + protobuf + C cross-toolchain), so darwin
        # devs run the native binary directly instead. Same `[::]:50051`
        # bind, same env vars — `graph-api`'s broker dials the same URL
        # either way.
        dev-up = pkgs.writeShellApplication {
          name = "dev-up";
          runtimeInputs = [ pkgs.podman pkgs.podman-compose ];
          text = ''
            set -euo pipefail
            if [ "$(uname -s)" = "Darwin" ]; then
              echo "darwin: running graph-compute natively (no podman container)."
              echo "  the OCI image build target is the host system, so a darwin"
              echo "  binary can't exec inside the Linux VM podman drives. The"
              echo "  native binary is functionally equivalent for the broker."
              echo "  → ${graph-compute}/bin/graph-compute"
              export GRAPH_COMPUTE_TICK_HZ='${toString graphComputeService.tickHz}'
              export GRAPH_COMPUTE_ADDR='${graphComputeService.bindAddr}'
              export RUST_LOG='${graphComputeService.rustLog}'
              exec ${graph-compute}/bin/graph-compute
            fi
            if ! podman machine list --format '{{.Running}}' 2>/dev/null | grep -q true; then
              # On linux hosts podman machine isn't usually used, but if it is
              # configured the same gate applies.
              :
            fi
            echo "loading ${graphComputeService.name}:latest into podman..."
            podman load < ${graph-compute-image}
            echo "loading ${graphApiService.name}:latest into podman..."
            podman load < ${graph-api-image}
            # graph-api in-container serves the trunk dist from /assets.
            # The default is the nix-built graph-renderer-web store path,
            # so a fresh `nix run .#dev-up` works without a prior
            # `just wasm`. Set ASSETS_DIR to point at a local trunk-watch
            # dist when iterating on the frontend.
            ASSETS_DIR_DEFAULT="${graph-renderer-web}"
            ASSETS_DIR="''${ASSETS_DIR:-$ASSETS_DIR_DEFAULT}"
            if [ ! -f "$ASSETS_DIR/index.html" ]; then
              echo "warn: no trunk dist at $ASSETS_DIR — graph-api will serve 404 for /" >&2
            fi
            if [ -z "''${VAULT_ROOT:-}" ]; then
              echo "warn: VAULT_ROOT not set; the compose mount will resolve to ./vault" >&2
              echo "  export VAULT_ROOT=/abs/path/to/vault before 'just dev-up' for a real ingest" >&2
            fi
            export ASSETS_DIR
            echo "starting compose stack..."
            podman-compose up -d
          '';
        };

        # `nix run .#dev-down` — tear down whatever `dev-up` brought up.
        # Linux: stop the compose stack. Darwin: kill the native process by
        # name (the foreground `exec` in `dev-up` makes Ctrl-C the normal
        # shutdown, but a stale background process can be cleaned up here).
        dev-down = pkgs.writeShellApplication {
          name = "dev-down";
          runtimeInputs = [ pkgs.podman-compose ];
          text = ''
            set -euo pipefail
            if [ "$(uname -s)" = "Darwin" ]; then
              # `pkill` returns 1 when no process matches — that's the
              # idempotent "nothing to tear down" case, not an error.
              pkill -x graph-compute || true
              echo "darwin: killed any running native graph-compute."
              exit 0
            fi
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

        # ----- graph-renderer-web: trunk-built WASM frontend -----
        #
        # Produces the `dist/` directory (index.html, hashed wasm/js, assets)
        # as a Nix store path. Lets `dev-up` and the compose stack consume the
        # built bundle without requiring a prior `just wasm` on the host.
        #
        # Trunk needs wasm-bindgen-cli whose version matches the wasm-bindgen
        # crate in Cargo.lock. The lockfile pins 0.2.120; nixpkgs ships up to
        # 0.2.118. The two-patch gap normally works because .cargo/config.toml
        # disables the wasm `reference-types` feature that was the load-bearing
        # incompatibility (see wasm-bindgen #4211 / #4654). If a future bump
        # breaks this, override `wasm-bindgen-cli` with a custom build of
        # 0.2.120 — the source hash is already known via `nix-prefetch-url`.
        # Renderer-only WASM deps cache. The workspace's broader `depsWasm`
        # is scoped to `graph-layouts` + `tvix-wasm` because the wasm32
        # target choke-points (e.g. getrandom-0.3 requiring the wasm_js
        # cfg) only affect packages the renderer pulls in. Restricting
        # this build to `--package graph-renderer` matches what trunk
        # itself drives.
        depsWasmRenderer = craneLibWasm.buildDepsOnly (commonArgs // {
          CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
          cargoExtraArgs = "--package graph-renderer";
          # graph-renderer's build.rs reuses the graph-api .proto definitions
          # via prost-build, which shells out to protoc.
          nativeBuildInputs = [ pkgs.protobuf ];
          # getrandom 0.3 on wasm32-unknown-unknown needs the wasm_js cfg
          # to pick the JS-backed entropy source. Without it the build
          # fails with the same compile_error the renderer would hit at
          # `trunk build` time outside the sandbox.
          RUSTFLAGS = "--cfg getrandom_backend=\"wasm_js\" -C target-feature=+reference-types";
        });

        graph-renderer-web = craneLib.buildTrunkPackage (commonArgs // {
          pname = "graph-renderer-web";
          cargoArtifacts = depsWasmRenderer;
          CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
          RUSTFLAGS = "--cfg getrandom_backend=\"wasm_js\" -C target-feature=+reference-types";
          # Trunk.toml at the repo root drives target/dist; the index lives
          # under crates/graph-renderer/assets/. buildTrunkPackage copies
          # `$(dirname trunkIndexPath)/dist` to $out, so the on-disk dist
          # path the justfile expects matches what the derivation produces.
          trunkIndexPath = "crates/graph-renderer/assets/index.html";
          cargoExtraArgs = "--package graph-renderer";
          nativeBuildInputs = [ pkgs.protobuf ];
          wasm-bindgen-cli = pkgs.wasm-bindgen-cli_0_2_118;
        });

      in {
        packages = {
          default          = graph-api;
          inherit vault-search graph-api graph-compute graph-layouts-wasm tvix-wasm graph-renderer-web;
          inherit bench-pagerank;
          inherit graph-compute-image graph-api-image docker-compose-yaml sky-task-yaml;
          inherit test-browser;
        };

        apps = {
          render-stack-configs = { type = "app"; program = "${render-stack-configs}/bin/render-stack-configs"; };
          dev-up   = { type = "app"; program = "${dev-up}/bin/dev-up"; };
          dev-down = { type = "app"; program = "${dev-down}/bin/dev-down"; };
          test-browser-rust = { type = "app"; program = "${test-browser-rust}/bin/test-browser-rust"; };
        };

        checks = {
          # Native: clippy + tests + fmt
          clippy = craneLib.cargoClippy (commonArgs // {
            cargoArtifacts = depsNative;
            # graph-compute's build.rs runs tonic-build → needs protoc.
            nativeBuildInputs = [ pkgs.pkg-config pkgs.protobuf ];
            buildInputs = bevyLibs;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
          tests-unit = craneLib.cargoNextest (commonArgs // {
            cargoArtifacts = depsNative;
            cargoNextestExtraArgs = "--profile unit";
            nativeBuildInputs = [ pkgs.pkg-config pkgs.protobuf ];
            buildInputs = bevyLibs;
          });

          tests-integration = craneLib.cargoNextest (commonArgs // {
            cargoArtifacts = depsNative;
            cargoNextestExtraArgs = "--profile integration";
            nativeBuildInputs = [ pkgs.pkg-config pkgs.protobuf ];
            buildInputs = bevyLibs;
          });

          tests-e2e = craneLib.cargoNextest (commonArgs // {
            cargoArtifacts = depsNative;
            cargoNextestExtraArgs = "--profile e2e";
            # E2E needs display server libs for headless Bevy
            nativeBuildInputs = [ pkgs.pkg-config pkgs.protobuf ];
            buildInputs = bevyLibs;
          });
          # GPU analytics correctness (gpu_pagerank_* + gpu_engines). The kernels
          # run on a real wgpu adapter: Metal on the aarch64-darwin builders, and
          # lavapipe software-Vulkan in the Linux sandbox so the WGSL actually
          # executes (not just compiles). Linux sets GPU_PAGERANK_REQUIRE_ADAPTER
          # so a missing/misconfigured adapter is a hard failure rather than a
          # silent skip. Scale test runs a small N here; the millions-scale
          # timing is a report-only bench on the Metal builders.
          tests-gpu = craneLib.cargoNextest (commonArgs // {
            cargoArtifacts = depsNative;
            cargoNextestExtraArgs = "--profile gpu -p graph-compute";
            nativeBuildInputs = [ pkgs.pkg-config pkgs.protobuf ];
            buildInputs = bevyLibs;
            GPU_PAGERANK_SCALE_N = "200000";
          } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
            VK_ICD_FILENAMES =
              "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.x86_64.json";
            LD_LIBRARY_PATH =
              pkgs.lib.makeLibraryPath (bevyLibsLinux ++ [ pkgs.vulkan-loader ]);
            GPU_PAGERANK_REQUIRE_ADAPTER = "1";
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

    # Hydra jobs — what the nixstation Hydra (pdx-nxst-001) builds per merge to
    # main, as a flake-type jobset. Scoped DELIBERATELY to the GPU-analytics
    # deliverable this CI exists to guard (correctness + perf regression):
    #
    #   x86_64-linux.tests-gpu      — the GPU correctness gate. Runs the WGSL
    #     analytics kernels under lavapipe software-Vulkan in the Nix sandbox,
    #     so PageRank/CC/BFS/SpMV(+f16,+hybrid)/distributed correctness gates
    #     every merge.
    #   aarch64-darwin.graph-compute — the native Metal build (verifies the
    #     darwin binary compiles + links wgpu/Metal).
    #   aarch64-darwin.bench-pagerank — report-only perf bench on real Metal.
    #
    # The workspace-wide `clippy`/`clippy-wasm`/`fmt`/`tests-unit`/
    # `tests-integration`/`tests-e2e` checks stay in `checks` (for `nix flake
    # check` + local dev) but are intentionally NOT gated here: jump-cannon had
    # no CI before this jobset, so they surface PRE-EXISTING workspace lint/fmt
    # debt + env-dependent tests unrelated to the GPU work. Linting the full tree
    # is a separate cleanup (tracked in todo.md); gating it would keep CI red on
    # debt that isn't this deliverable's.
    flake.hydraJobs = {
      x86_64-linux.tests-gpu = inputs.self.checks.x86_64-linux.tests-gpu;
      aarch64-darwin.graph-compute = inputs.self.packages.aarch64-darwin.graph-compute;
      aarch64-darwin.bench-pagerank = inputs.self.packages.aarch64-darwin.bench-pagerank;
    };
  };
}
