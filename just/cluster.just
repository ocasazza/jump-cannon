# Local graph-compute development subcommands — `just cluster <action>`.
#
#   just cluster up       render docker-compose.yml and start the local stack
#   just cluster down     stop the local stack
#   just cluster render   regenerate docker-compose.yml from flake.nix
#
# `just cluster --list` shows these as real recipes.
# Production compute is deployed through charts/jump-cannon and admitted by
# the consuming Kubernetes environment's Kueue policy.

# Submodule recipes default to the module-file dir; pin to the repo root so
# `nix run` resolves the flake from the repository root.
set working-directory := '..'

# Render the compose file and boot the local development stack.
# (Default recipe, so bare `just cluster` == `just cluster up`.)
[group('cluster')]
[doc('Bring up the local graph-compute development stack')]
up:
    nix run .#render-compose
    nix run .#dev-up

# Tear down the local development stack.
[group('cluster')]
down:
    nix run .#dev-down

# Regenerate docker-compose.yml from flake.nix.
[group('cluster')]
render:
    nix run .#render-compose
