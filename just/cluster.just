# graph-compute cluster subcommands — `just cluster <action> [where]`.
# WHERE picks the backend: `local` (podman, default) or `sky` (SkyPilot cloud).
#
#   just cluster up [local|sky]    bring up the cluster (local also renders configs first)
#   just cluster down [local|sky]  tear it down
#   just cluster endpoint sky      print JUMP_CANNON_COMPUTE_URL=http://<host>:50051
#   just cluster render            regenerate docker-compose.yml + infra/sky/*.yaml from flake.nix
#
# `just cluster --list` shows these as real recipes.

# Submodule recipes default to the module-file dir; pin to the repo root so
# `nix run` / `sky` resolve the flake and infra/ paths.
set working-directory := '..'

# Bring up the cluster. local: render configs + boot podman stack. sky: launch.
# (Default recipe, so bare `just cluster` == `just cluster up local`.)
[group('cluster')]
[doc('Bring up the cluster: local (podman, default) or sky (SkyPilot)')]
up where='local':
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{where}}" in
      local) nix run .#render-stack-configs && nix run .#dev-up ;;
      sky)   sky launch -c graph-compute infra/sky/graph-compute.yaml --yes ;;
      *)     echo "cluster up: unknown target '{{where}}' (expected 'local' or 'sky')" >&2; exit 2 ;;
    esac

# Tear down the cluster.
[group('cluster')]
down where='local':
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{where}}" in
      local) nix run .#dev-down ;;
      sky)   sky down graph-compute --yes ;;
      *)     echo "cluster down: unknown target '{{where}}' (expected 'local' or 'sky')" >&2; exit 2 ;;
    esac

# Print the SkyPilot cluster endpoint as a JUMP_CANNON_COMPUTE_URL= line.
[group('cluster')]
endpoint:
    sky status --endpoint 50051 graph-compute | awk '{print "JUMP_CANNON_COMPUTE_URL=http://" $0}'

# Regenerate docker-compose.yml + infra/sky/*.yaml from flake.nix.
[group('cluster')]
render:
    nix run .#render-stack-configs
