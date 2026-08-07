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
        isLinux = pkgs.stdenv.hostPlatform.system == "x86_64-linux";
        k8sImageRegistry = "us-central1-docker.pkg.dev/it-ops-nixstation/lavender";

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
        # protobuf is needed for graph-api's / graph-compute's prost/tonic builds.
        depsNative = craneLib.buildDepsOnly (commonArgs // {
          nativeBuildInputs = [ pkgs.protobuf ];
        });
        depsWasm   = craneLibWasm.buildDepsOnly (commonArgs // {
          CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
          cargoExtraArgs = "--package graph-layouts --package tvix-wasm";
        });

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

        graph-compute-k8s-binary = craneLib.buildPackage (commonArgs // {
          cargoArtifacts = depsNative;
          cargoExtraArgs = "--package graph-compute";
          nativeBuildInputs = [ pkgs.protobuf ];
          doCheck = false;
        });

        # Perf benches (REPORT-ONLY, never gates a merge). Runs the criterion
        # bench_pagerank example (size sweep) **and** bench_scaling (the
        # degree × structure / sparse↔dense matrix), capturing all criterion
        # JSON/HTML to $out as a Hydra build product — both write into the same
        # target/criterion tree, so one copy archives every group per merge for
        # over-time tracking. `__noChroot` so the build reaches the real GPU —
        # only meaningful on the aarch64-darwin Metal builders (perf under Linux
        # lavapipe is software and meaningless), so hydraJobs wires this on
        # darwin only. Timing output varies run-to-run ⇒ it never caches; that's
        # intended for a per-merge perf signal. Requires the darwin Hydra
        # builders to permit __noChroot (nix.settings extra-sandbox / trusted).
        bench-pagerank = craneLib.mkCargoDerivation (commonArgs // {
          cargoArtifacts = depsNative;
          pname = "graph-compute-bench-pagerank";
          version = "0.1.0";
          __noChroot = true;
          nativeBuildInputs = [ pkgs.protobuf ];
          buildPhaseCargoCommand = ''
            cargo run --release -p graph-compute --example bench_pagerank -- \
              --bench --noplot --save-baseline hydra
            cargo run --release -p graph-compute --example bench_scaling -- \
              --bench --noplot --save-baseline hydra
          '';
          doInstallCargoArtifacts = false;
          doCheck = false;
          installPhaseCommand = ''
            mkdir -p $out/nix-support

            # Criterion's native report is the immutable benchmark artifact.
            # Hydra owns publication and retention; the derivation does not
            # mutate a separate dashboard store or manage upload credentials.
            if [ -d target/criterion ]; then
              cp -r target/criterion $out/criterion
              echo "report criterion $out/criterion" >> $out/nix-support/hydra-build-products
            fi
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

        # nixpkgs Chromium is Linux-only. On Darwin the browser wrapper below
        # accepts CHROME_BIN / CHROMIUM_BIN or discovers an installed app.
        nixChromiumBin =
          if pkgs.stdenv.hostPlatform.isLinux
          then "${pkgs.chromium}/bin/chromium"
          else "";
        browserVulkanIcdDir =
          if pkgs.stdenv.hostPlatform.isLinux
          then "${pkgs.mesa}/share/vulkan/icd.d"
          else "";
        # Chromium aborts in Skia when a minimal container has no usable font
        # manager. Keep the browser smoke image self-contained and deterministic.
        browserFontsConf =
          if pkgs.stdenv.hostPlatform.isLinux
          then pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; }
          else null;

        # `nix run .#test-browser-rust` — bring up graph-api + open the
        # page in chromium with WebGPU enabled + run the Rust smoke test.
        #
        # NOTE: this wrapper depends on:
        #   1. A trunk dist. Defaults to the nix-built app-web (the Dioxus
        #      frontend) store path; override with ASSETS_DIR for fast
        #      iteration against a local `trunk watch` build in app/.
        #   2. An isolated test vault (the wrapper creates and seeds one unless
        #      VAULT_ROOT is explicitly provided).
        # The wrapper bails with a clear error if (1) is missing.
        test-browser-rust = pkgs.writeShellApplication {
          name = "test-browser-rust";
          runtimeInputs = [
            graph-api
            test-browser
            pkgs.curl
            pkgs.coreutils
          ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.chromium ];
          text = ''
            set -euo pipefail

            REPO_ROOT="''${REPO_ROOT:-$PWD}"
            ASSETS_DIR="''${ASSETS_DIR:-${app-web}}"
            # PID-derived defaults keep simultaneous local/agent runs from
            # racing on the same listener. TEST_PORT remains an explicit
            # deterministic override for callers that need one.
            PORT="''${TEST_PORT:-$((40000 + $$ % 20000))}"
            OUT_DIR="''${OUT_DIR:-$REPO_ROOT/target/test-browser-rust}"
            RUN_ROOT=$(mktemp -d "''${TMPDIR:-/tmp}/jump-cannon-browser.XXXXXX")
            RUN_OUT="$RUN_ROOT/out"
            BROWSER_BIN="''${CHROME_BIN:-''${CHROMIUM_BIN:-}}"
            if [ -n "''${VAULT_ROOT:-}" ]; then
              VAULT="$VAULT_ROOT"
              SEED_NODE_EDITOR_FIXTURES=0
              FIXTURE_REQUIRE_ARGS=()
            else
              VAULT="$RUN_ROOT/vault"
              SEED_NODE_EDITOR_FIXTURES=1
              FIXTURE_REQUIRE_ARGS=(--fixtures-required)
            fi

            SERVER_PID=""
            cleanup() {
              if [ -n "$SERVER_PID" ]; then
                kill "$SERVER_PID" 2>/dev/null || true
              fi
              rm -rf "$RUN_ROOT"
            }
            trap cleanup EXIT

            if [ -z "$BROWSER_BIN" ] && [ "$(uname -s)" = "Darwin" ]; then
              for candidate in \
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
                "/Applications/Chromium.app/Contents/MacOS/Chromium" \
                "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary"
              do
                if [ -x "$candidate" ]; then
                  BROWSER_BIN="$candidate"
                  break
                fi
              done
            fi
            if [ -z "$BROWSER_BIN" ]; then
              BROWSER_BIN="${nixChromiumBin}"
            fi
            if [ -z "$BROWSER_BIN" ] || [ ! -x "$BROWSER_BIN" ]; then
              echo "error: no Chrome or Chromium executable found" >&2
              echo "hint: install Google Chrome or set CHROME_BIN=/path/to/browser" >&2
              exit 2
            fi

            if [ ! -f "$ASSETS_DIR/index.html" ]; then
              echo "error: no trunk dist at $ASSETS_DIR" >&2
              echo "hint: unset ASSETS_DIR to use the nix-built app-web, or run" >&2
              echo "  'cd app && trunk build --release' and point ASSETS_DIR at app/ui/dist." >&2
              exit 2
            fi

            mkdir -p "$VAULT" "$VAULT/Jump Cannon" "$OUT_DIR" "$RUN_OUT"
            for source in ${./charts/jump-cannon/knowledge}/*.md; do
              cp -f "$source" "$VAULT/Jump Cannon/$(basename "$source")"
            done
            if [ ! -f "$VAULT/Welcome.md" ]; then
              printf 'Begin with [[Start Here]]. See [[Performance]].\n' > "$VAULT/Welcome.md"
            fi
            if [ ! -f "$VAULT/Performance.md" ]; then
              printf 'See [[Performance Engineering]], [[Scheduled Tests]], and [[Observability]].\n' > "$VAULT/Performance.md"
            fi
            # Stable fixtures for the Nodes workbench browser contract. Keep
            # these outside the managed knowledge folder so their ids and tag
            # shapes are independent of living-document edits.
            if [ "$SEED_NODE_EDITOR_FIXTURES" -eq 1 ] && [ ! -f "$VAULT/Node Editor Fixture.md" ]; then
              printf '%s\n' \
                '---' \
                'title: Node Editor Fixture' \
                'tags: [browser-editor, browser-shared]' \
                '---' \
                "" \
                '# Node Editor Fixture' \
                "" \
                'BROWSER_NODE_EDITOR_SENTINEL' \
                > "$VAULT/Node Editor Fixture.md"
            fi
            if [ "$SEED_NODE_EDITOR_FIXTURES" -eq 1 ] && [ ! -f "$VAULT/Node Shared Fixture.md" ]; then
              printf '%s\n' \
                '---' \
                'title: Node Shared Fixture' \
                'tags: [browser-shared]' \
                '---' \
                "" \
                'Shared tag sibling.' \
                > "$VAULT/Node Shared Fixture.md"
            fi
            if [ "$SEED_NODE_EDITOR_FIXTURES" -eq 1 ] && [ ! -f "$VAULT/Node Untagged Fixture.md" ]; then
              printf '%s\n' \
                '---' \
                'title: Node Untagged Fixture' \
                '---' \
                "" \
                'Synthetic untagged-group fixture.' \
                > "$VAULT/Node Untagged Fixture.md"
            fi

            # Software vulkan ICD for WebGPU on headless linux — mirrors the
            # devshell's VK_ICD_FILENAMES setting.
            if [ -n "${browserVulkanIcdDir}" ] && [ -z "''${VK_ICD_FILENAMES:-}" ] && [ -d "${browserVulkanIcdDir}" ]; then
              export VK_ICD_FILENAMES="${browserVulkanIcdDir}/lvp_icd.x86_64.json"
            fi

            echo "→ starting graph-api on port ''${PORT}…"
            graph-api \
              --vault-root "$VAULT" \
              --port "$PORT" \
              --no-browser \
              --assets-dir "$ASSETS_DIR" &
            SERVER_PID=$!

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
              --chromium "$BROWSER_BIN" \
              --out-dir "$RUN_OUT" \
              --timeout-secs 60 \
              "''${FIXTURE_REQUIRE_ARGS[@]}"

            install -m 0644 "$RUN_OUT/boot.png" "$OUT_DIR/boot.png"
            install -m 0644 "$RUN_OUT/nodes-editor.png" "$OUT_DIR/nodes-editor.png"
            install -m 0644 "$RUN_OUT/report.json" "$OUT_DIR/report.json"
          '';
        };

        # ----- Local distributed-compute development backend -----
        # This service spec drives the local docker-compose stack. Production
        # compute is declared by charts/jump-cannon and scheduled by the
        # consuming Kubernetes environment. Edit this attrset, then run
        # `nix run .#render-compose` to regenerate docker-compose.yml.
        graphComputeService = {
          name        = "graph-compute";
          port        = 50051;
          tickHz      = 30;
          rustLog     = "info";
          # Bind all interfaces inside the container/pod so external clients
          # (broker, probe) can reach the gRPC port. The native default is
          # `[::1]:50051` which works only for in-host loopback.
          bindAddr    = "[::]:50051";
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
        # for changes via inotify; progress is surfaced to the frontend's
        # Progress panel via `GET /progress`. The compose service below
        # bind-mounts the host's $VAULT_ROOT into /vault and the trunk dist
        # into /assets. $ASSETS_DIR defaults to the nix-built app-web (the
        # Dioxus frontend) derivation, so `just dev-up` works without a
        # prior trunk build; set it explicitly when iterating on the
        # frontend with `trunk watch` in app/.
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
              "GRAPH_API_NO_BROWSER=true"
              "JUMP_CANNON_ASSETS_DIR=/assets"
              "VAULT_ROOT=/vault"
              "RUST_LOG=${graphApiService.rustLog}"
            ];
          };
        };

        unsupportedK8sImage = name:
          pkgs.runCommand "${name}-unsupported"
            {
              meta = {
                description = "${name} OCI image (only built on x86_64-linux)";
                platforms = [ "x86_64-linux" ];
              };
            }
            ''
              mkdir -p "$out"
              echo "${name} is only available on x86_64-linux" > "$out/README"
            '';

        graph-api-k8s-image =
          if isLinux then
            pkgs.dockerTools.streamLayeredImage {
              name = "${k8sImageRegistry}/jump-cannon-graph-api";
              tag = "latest";
              contents = [
                graph-api
                vault-search
                pkgs.cacert
              ];
              extraCommands = ''
                mkdir -p assets etc/ssl/certs
                cp -r ${app-web}/. assets/
                ln -sf ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt etc/ssl/certs/ca-bundle.crt
              '';
              fakeRootCommands = ''
                printf 'root:x:0:0::/root:/noshell\njump:x:10001:10001:Jump Cannon:/tmp:/noshell\n' > etc/passwd
                printf 'root:x:0:\njump:x:10001:\n' > etc/group
                chmod 0644 etc/passwd etc/group
              '';
              config = {
                Entrypoint = [ "/bin/graph-api" ];
                ExposedPorts."${toString graphApiService.port}/tcp" = {};
                Env = [
                  "GRAPH_API_HOST=0.0.0.0"
                  "GRAPH_API_PORT=${toString graphApiService.port}"
                  "GRAPH_API_NO_BROWSER=true"
                  "JUMP_CANNON_ASSETS_DIR=/assets"
                  "VAULT_ROOT=/vault"
                  "RUST_LOG=${graphApiService.rustLog}"
                  "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
                  "NIX_SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
                ];
                Labels = {
                  "org.opencontainers.image.source" = "https://github.com/ocasazza/jump-cannon";
                  "org.opencontainers.image.description" = "jump-cannon graph-api with Dioxus frontend assets";
                };
              };
            }
          else
            unsupportedK8sImage "graph-api-k8s-image";

        graph-compute-k8s-image =
          if isLinux then
            pkgs.dockerTools.streamLayeredImage {
              name = "${k8sImageRegistry}/jump-cannon-graph-compute";
              tag = "latest";
              contents = [
                graph-compute-k8s-binary
                pkgs.cacert
                pkgs.vulkan-loader
              ];
              extraCommands = ''
                mkdir -p etc/ssl/certs
                ln -sf ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt etc/ssl/certs/ca-bundle.crt
              '';
              fakeRootCommands = ''
                printf 'root:x:0:0::/root:/noshell\njump:x:10001:10001:Jump Cannon:/tmp:/noshell\n' > etc/passwd
                printf 'root:x:0:\njump:x:10001:\n' > etc/group
                chmod 0644 etc/passwd etc/group
              '';
              config = {
                Entrypoint = [ "/bin/graph-compute" ];
                ExposedPorts."${toString graphComputeService.port}/tcp" = {};
                Env = [
                  "GRAPH_COMPUTE_TICK_HZ=${toString graphComputeService.tickHz}"
                  "GRAPH_COMPUTE_ADDR=${graphComputeService.bindAddr}"
                  "RUST_LOG=${graphComputeService.rustLog}"
                  "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
                  "NIX_SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
                ];
                Labels = {
                  "org.opencontainers.image.source" = "https://github.com/ocasazza/jump-cannon";
                  "org.opencontainers.image.description" = "jump-cannon graph-compute gRPC backend";
                };
              };
            }
          else
            unsupportedK8sImage "graph-compute-k8s-image";

        test-workload-bins =
          if isLinux then
            craneLib.mkCargoDerivation (commonArgs // {
              cargoArtifacts = depsNative;
              pname = "jump-cannon-test-workload-bins";
              version = "0.1.0";
              nativeBuildInputs = [ pkgs.protobuf ];
              buildPhaseCargoCommand = ''
                CARGO_TARGET_DIR=target/layout-fuzz \
                  cargo test -p graph-layouts --test fuzz --release --no-run
                CARGO_TARGET_DIR=target/compute-fuzz \
                  cargo test -p graph-compute --test fuzz --release --no-run
                CARGO_TARGET_DIR=target/benches \
                  cargo build --release -p graph-layouts --example bench_static_layouts
                CARGO_TARGET_DIR=target/benches \
                  cargo build --release -p graph-compute \
                    --example bench_pagerank \
                    --example bench_scaling
              '';
              doInstallCargoArtifacts = false;
              doCheck = false;
              installPhaseCommand = ''
                mkdir -p "$out/bin"

                layout_fuzz=$(find target/layout-fuzz/release/deps -maxdepth 1 -type f -perm -111 -name 'fuzz-*' | sort | head -n1)
                compute_fuzz=$(find target/compute-fuzz/release/deps -maxdepth 1 -type f -perm -111 -name 'fuzz-*' | sort | head -n1)
                install -m 0755 "$layout_fuzz" "$out/bin/graph-layouts-fuzz"
                install -m 0755 "$compute_fuzz" "$out/bin/graph-compute-fuzz"

                install -m 0755 target/benches/release/examples/bench_static_layouts "$out/bin/graph-layouts-bench-static"
                install -m 0755 target/benches/release/examples/bench_pagerank "$out/bin/graph-compute-bench-pagerank"
                install -m 0755 target/benches/release/examples/bench_scaling "$out/bin/graph-compute-bench-scaling"
              '';
            })
          else
            pkgs.runCommand "jump-cannon-test-workload-bins-unsupported" { } ''
              mkdir -p "$out/bin"
            '';

        testMetricsPushShell = ''
          push_metrics() {
            test_name="$1"
            passed="$2"
            failed="$3"
            duration="$4"
            now="$(date +%s)"
            metrics_file="$(mktemp)"
            {
              printf 'jump_cannon_test_last_run_timestamp_seconds{app="jump-cannon",test="%s"} %s\n' "$test_name" "$now"
              printf 'test_run_total{app="jump-cannon",test="%s"} 1\n' "$test_name"
              printf 'test_run_passed{app="jump-cannon",test="%s"} %s\n' "$test_name" "$passed"
              printf 'test_run_failed{app="jump-cannon",test="%s"} %s\n' "$test_name" "$failed"
              printf 'test_duration_seconds{app="jump-cannon",test="%s"} %s\n' "$test_name" "$duration"
            } > "$metrics_file"
            curl -fsS --data-binary @"$metrics_file" \
              "$PUSHGATEWAY_URL/metrics/job/jump-cannon-$test_name/app/jump-cannon/test/$test_name"
            rm -f "$metrics_file"
          }
        '';

        testMetricsShell = testMetricsPushShell + ''
          run_and_report() {
            test_name="$1"
            shift
            started="$(date +%s)"
            set +e
            "$@"
            status="$?"
            set -e
            ended="$(date +%s)"
            duration="$((ended - started))"
            if [ "$status" -eq 0 ]; then
              push_metrics "$test_name" 1 0 "$duration"
            else
              push_metrics "$test_name" 0 1 "$duration" || true
            fi
            return "$status"
          }
        '';

        jump-cannon-fuzz = pkgs.writeShellApplication {
          name = "jump-cannon-fuzz";
          runtimeInputs = [
            test-workload-bins
            pkgs.coreutils
            pkgs.curl
          ];
          text = ''
            set -euo pipefail
            : "''${PUSHGATEWAY_URL:=http://pushgateway.monitoring.svc.cluster.local:9091}"
            : "''${PROPTEST_CASES:=10000}"
            export PROPTEST_CASES
            ${testMetricsPushShell}

            started="$(date +%s)"
            set +e
            graph-layouts-fuzz
            layout_status="$?"
            graph-compute-fuzz
            compute_status="$?"
            set -e
            ended="$(date +%s)"
            duration="$((ended - started))"
            if [ "$layout_status" -eq 0 ] && [ "$compute_status" -eq 0 ]; then
              push_metrics fuzz 1 0 "$duration"
              exit 0
            fi
            push_metrics fuzz 0 1 "$duration" || true
            exit 1
          '';
        };

        jump-cannon-perf = pkgs.writeShellApplication {
          name = "jump-cannon-perf";
          runtimeInputs = [
            test-workload-bins
            pkgs.coreutils
            pkgs.curl
          ];
          text = ''
            set -euo pipefail
            : "''${PUSHGATEWAY_URL:=http://pushgateway.monitoring.svc.cluster.local:9091}"
            ${testMetricsShell}

            if [ "''${BENCH_INCLUDE_1M:-0}" != "1" ]; then
              unset BENCH_INCLUDE_1M
            fi

            run_and_report performance sh -c '
              graph-layouts-bench-static --bench &&
              graph-compute-bench-pagerank --bench --noplot &&
              graph-compute-bench-scaling --bench --noplot
            '
          '';
        };

        jump-cannon-browser-smoke = pkgs.writeShellApplication {
          name = "jump-cannon-browser-smoke";
          runtimeInputs = [
            test-browser
            pkgs.chromium
            pkgs.coreutils
            pkgs.curl
          ];
          text = ''
            set -euo pipefail
            : "''${PUSHGATEWAY_URL:=http://pushgateway.monitoring.svc.cluster.local:9091}"
            : "''${JUMP_CANNON_BASE_URL:=http://jump-cannon:80}"
            : "''${JUMP_CANNON_BROWSER_TIMEOUT_SECONDS:=90}"
            OUT_DIR="''${OUT_DIR:-/tmp/jump-cannon-browser-smoke}"
            mkdir -p "$OUT_DIR"
            ${testMetricsShell}

            run_and_report browser test-browser \
              --base-url "$JUMP_CANNON_BASE_URL" \
              --chromium ${pkgs.chromium}/bin/chromium \
              --out-dir "$OUT_DIR" \
              --timeout-secs "$JUMP_CANNON_BROWSER_TIMEOUT_SECONDS"
          '';
        };

        test-runner-image =
          if isLinux then
            pkgs.dockerTools.streamLayeredImage {
              name = "${k8sImageRegistry}/jump-cannon-test-runner";
              tag = "latest";
              contents = [
                jump-cannon-fuzz
                jump-cannon-perf
                jump-cannon-browser-smoke
                test-workload-bins
                test-browser
                pkgs.bashInteractive
                pkgs.chromium
                pkgs.coreutils
                pkgs.curl
                pkgs.cacert
                pkgs.dejavu_fonts
                pkgs.fontconfig
                pkgs.jq
                pkgs.mesa
                pkgs.vulkan-loader
              ];
              extraCommands = ''
                mkdir -p tmp etc/ssl/certs
                chmod 1777 tmp
                ln -sf ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt etc/ssl/certs/ca-bundle.crt
              '';
              fakeRootCommands = ''
                printf 'root:x:0:0::/root:/noshell\njump:x:10001:10001:Jump Cannon:/tmp:/noshell\n' > etc/passwd
                printf 'root:x:0:\njump:x:10001:\n' > etc/group
                chmod 0644 etc/passwd etc/group
              '';
              config = {
                Cmd = [ "/bin/jump-cannon-browser-smoke" ];
                WorkingDir = "/tmp";
                User = "10001:10001";
                Env = [
                  "PATH=/bin"
                  "HOME=/tmp"
                  "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
                  "NIX_SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
                  "FONTCONFIG_FILE=${browserFontsConf}"
                  "VK_ICD_FILENAMES=/share/vulkan/icd.d/lvp_icd.x86_64.json"
                ];
                Labels = {
                  "org.opencontainers.image.source" = "https://github.com/ocasazza/jump-cannon";
                  "org.opencontainers.image.description" = "jump-cannon cluster test runner";
                };
              };
            }
          else
            unsupportedK8sImage "test-runner-image";

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
          # surfaces progress to the frontend via GET /progress.
          services."${graphApiService.name}" = {
            image       = "${graphApiService.name}:latest";
            ports       = [ "${toString graphApiService.port}:${toString graphApiService.port}" ];
            # Bind-mount the host vault (rw — see below). Bind-mount the
            # pre-built trunk dist so the in-container graph-api can serve
            # / and /assets. `VAULT_ROOT` and `ASSETS_DIR` are host-side
            # env vars; default both to the canonical in-repo paths.
            volumes = [
              # rw, not ro: the frontend's PUT /vault/page editor surface
              # needs to write back to the vault.
              "\${VAULT_ROOT:-./vault}:/vault:rw"
              # ASSETS_DIR is set by `nix run .#dev-up` to the nix-built
              # app-web store path. Direct `podman-compose up` users can
              # either export ASSETS_DIR=$(nix build --no-link
              # --print-out-paths .#app-web) or point it at their local
              # `trunk watch` dist (app/ui/dist) for fast iteration.
              "\${ASSETS_DIR:-./app/ui/dist}:/assets:ro"
            ];
            environment = {
              GRAPH_API_HOST              = "0.0.0.0";
              GRAPH_API_PORT              = toString graphApiService.port;
              GRAPH_API_NO_BROWSER        = "true";
              JUMP_CANNON_ASSETS_DIR      = "/assets";
              VAULT_ROOT                  = "/vault";
              JUMP_CANNON_COMPUTE_URL     = "http://${graphComputeService.name}:${toString graphComputeService.port}";
              RUST_LOG                    = graphApiService.rustLog;
            };
            depends_on = [ graphComputeService.name ];
            restart    = "unless-stopped";
          };
        };

        # `nix run .#render-compose` — regenerates the local development
        # compose file from graphComputeService and graphApiService above.
        render-compose = pkgs.writeShellApplication {
          name = "render-compose";
          runtimeInputs = [ pkgs.coreutils ];
          text = ''
            set -euo pipefail
            install -m 0644 ${docker-compose-yaml} docker-compose.yml
            echo "rendered: docker-compose.yml"
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
            # The Dioxus frontend (app-web) is the frontend; set ASSETS_DIR
            # explicitly to point at a local trunk-watch dist (app/ui/dist)
            # when iterating on it.
            ASSETS_DIR_DEFAULT="${app-web}"
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

        # ----- app-web: the Dioxus frontend (app/ workspace), trunk-built -----
        #
        # The app/ workspace is deliberately separate from this one (it owns
        # the Tauri/Dioxus dependency tree), but its WASM frontend builds
        # through the same crane + trunk machinery. The prost output is
        # checked in (app/ui/src/proto/), so no protoc — but the workspace is
        # no longer fully self-contained: jump-cannon-ui's wgpu renderer
        # drives the GPU force layout via a path dependency on
        # crates/graph-layouts (which has no path deps of its own), so the
        # source root is the repo root with a fileset union of app/ + that
        # crate. `sourceRoot` drops the build into app/ where Trunk.toml and
        # the workspace manifest live; the relative ../../crates/graph-layouts
        # path then resolves inside the union. wasm-bindgen is pinned to
        # =0.2.118 in app/Cargo.toml to match the nixpkgs CLI exactly (no
        # CLI/crate version-skew caveats).
        #
        # The Tauri shell itself stays a devshell build (`just app-dev` /
        # `just app-build`): bundling needs platform signing toolchains that
        # nix can't usefully sandbox on macOS.
        appSrc = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            # wgsl: the ported renderer embeds its node/edge shaders via
            # include_str! (app/ui/src/shaders/).
            (pkgs.lib.fileset.fileFilter
              (file: builtins.any file.hasExt [ "rs" "toml" "lock" "html" "css" "wgsl" ])
              ./app)
            # graph-layouts embeds its compute WGSL via include_str!.
            (pkgs.lib.fileset.fileFilter
              (file: builtins.any file.hasExt [ "rs" "toml" "lock" "wgsl" ])
              ./crates/graph-layouts)
            # tvix-wasm: client-side Nix eval for Layout seeds + Generate
            # Inline executor (phase 4) — second path dep outside app/.
            # "nix": the crate embeds its demo catalog via include_str!
            # (src/nix/*.nix).
            (pkgs.lib.fileset.fileFilter
              (file: builtins.any file.hasExt [ "rs" "toml" "lock" "nix" ])
              ./crates/tvix-wasm)
          ];
        };

        depsAppWasm = craneLibWasm.buildDepsOnly {
          pname = "app-web-deps";
          version = "0.1.0";
          src = appSrc;
          sourceRoot = "source/app";
          # No Cargo.lock at the union root: point crane's vendoring at the
          # app workspace's lockfile, and drop a copy where `cargo --locked`
          # (running with cwd source/app in the dummy source) expects it.
          cargoLock = ./app/Cargo.lock;
          extraDummyScript = ''
            mkdir -p $out/app
            cp ${./app/Cargo.lock} $out/app/Cargo.lock
          '';
          strictDeps = true;
          CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
          cargoExtraArgs = "--package jump-cannon-ui";
          doCheck = false;
        };

        app-web = craneLib.buildTrunkPackage {
          pname = "app-web";
          version = "0.1.0";
          src = appSrc;
          sourceRoot = "source/app";
          cargoLock = ./app/Cargo.lock;
          strictDeps = true;
          cargoArtifacts = depsAppWasm;
          CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
          trunkIndexPath = "ui/index.html";
          cargoExtraArgs = "--package jump-cannon-ui";
          wasm-bindgen-cli = pkgs.wasm-bindgen-cli_0_2_118;
        };

      in {
        packages = {
          default          = graph-api;
          inherit vault-search graph-api graph-compute graph-layouts-wasm tvix-wasm app-web;
          inherit bench-pagerank;
          chart-tarball = pkgs.callPackage ./packages/chart-tarball { };
          inherit graph-compute-image graph-api-image docker-compose-yaml;
          inherit graph-api-k8s-image graph-compute-k8s-image test-runner-image;
          inherit test-browser test-workload-bins;
        };

        apps = {
          render-compose = { type = "app"; program = "${render-compose}/bin/render-compose"; };
          dev-up   = { type = "app"; program = "${dev-up}/bin/dev-up"; };
          dev-down = { type = "app"; program = "${dev-down}/bin/dev-down"; };
          test-browser-rust = { type = "app"; program = "${test-browser-rust}/bin/test-browser-rust"; };
        };

        checks = {
          # The Dioxus frontend (app/ workspace) builds reproducibly — this
          # also type-checks panel-kit and jump-cannon-ui for wasm32.
          inherit app-web;

          # Native: clippy + tests + fmt
          clippy = craneLib.cargoClippy (commonArgs // {
            cargoArtifacts = depsNative;
            # graph-compute's build.rs runs tonic-build → needs protoc.
            nativeBuildInputs = [ pkgs.protobuf ];
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
          tests-unit = craneLib.cargoNextest (commonArgs // {
            cargoArtifacts = depsNative;
            cargoNextestExtraArgs = "--profile unit";
            nativeBuildInputs = [ pkgs.protobuf ];
          });

          tests-integration = craneLib.cargoNextest (commonArgs // {
            cargoArtifacts = depsNative;
            cargoNextestExtraArgs = "--profile integration";
            nativeBuildInputs = [ pkgs.protobuf ];
          });

          tests-e2e = craneLib.cargoNextest (commonArgs // {
            cargoArtifacts = depsNative;
            cargoNextestExtraArgs = "--profile e2e";
            nativeBuildInputs = [ pkgs.protobuf ];
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
            nativeBuildInputs = [ pkgs.protobuf ];
            GPU_PAGERANK_SCALE_N = "200000";
          } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
            # Force the Vulkan backend so wgpu uses lavapipe and never touches the
            # GL/EGL backend — on a headless builder (no display) wgpu-hal's GLES
            # EGL init panics (`unwrap()` on None in gles/egl.rs), which fails
            # every GPU test before lavapipe is even tried.
            WGPU_BACKEND = "vulkan";
            VK_ICD_FILENAMES =
              "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.x86_64.json";
            LD_LIBRARY_PATH =
              pkgs.lib.makeLibraryPath [ pkgs.vulkan-loader ];
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

            # Dioxus + Tauri app (app/ workspace): `just app-dev` / `just app-build`
            cargo-tauri

            # Build tools
            pkg-config
            protobuf

            # Dev workflow
            just
            # Declarative process manager for `just dev-up`'s hot-reload stack
            # (graph-compute + trunk watch + cargo-watch graph-api). Replaces
            # the old hand-rolled PID/trap/pkill orchestration; see just/dev-stack.yaml.
            process-compose

            # Local dev cluster (`just dev-up` / `just dev-down`). podman runs
            # rootless on NixOS without enabling system docker; podman-compose
            # parses the same docker-compose.yml.
            podman
            podman-compose
          ];

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

    # Hydra jobs — what the nixstation Hydra builds per merge to main, as a
    # flake-type jobset. Scoped DELIBERATELY to the GPU-analytics
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
      x86_64-linux.chart-tarball = inputs.self.packages.x86_64-linux.chart-tarball;
      x86_64-linux.graph-api-k8s-image = inputs.self.packages.x86_64-linux.graph-api-k8s-image;
      x86_64-linux.graph-compute-k8s-image = inputs.self.packages.x86_64-linux.graph-compute-k8s-image;
      x86_64-linux.test-runner-image = inputs.self.packages.x86_64-linux.test-runner-image;
      aarch64-darwin.graph-compute = inputs.self.packages.aarch64-darwin.graph-compute;
      aarch64-darwin.bench-pagerank = inputs.self.packages.aarch64-darwin.bench-pagerank;
    };
  };
}
