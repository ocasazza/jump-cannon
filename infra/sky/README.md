# SkyPilot: graph-compute worker

One-command bring-up of the `graph-compute` GPU worker on a cloud node.

## Prereqs

- `pip install skypilot` (or `pip install "skypilot[aws,gcp,lambda]"` for the
  clouds you care about).
- Cloud credentials configured (`aws configure`, `gcloud auth …`, etc.).
- `sky check` shows at least one enabled cloud.

## Launch

```
just sky-up
```

This wraps `sky launch -c graph-compute infra/sky/graph-compute.yaml --yes`.
First boot installs rustup + the Vulkan loader and builds graph-compute in
release; subsequent `sky launch` reuses the cluster.

## Pointing graph-api at the worker

Once the cluster is up, fetch the public endpoint:

```
just sky-endpoint   # prints: JUMP_CANNON_COMPUTE_URL=http://<host>:50051
```

Export that line in the shell where you run `just dev` / `cargo run -p
graph-api`. The broker reads `JUMP_CANNON_COMPUTE_URL` at startup
(`crates/graph-api/src/compute_broker.rs`) and dials it. If the worker pod
restarts, the broker now reconnects with exponential backoff (1s → 30s).

## Teardown

```
just sky-down
```

## Notes

- The cloud image must ship a Vulkan ICD for wgpu to find a GPU adapter. The
  setup step installs `libvulkan1` + `mesa-vulkan-drivers` and runs
  `vulkaninfo --summary`; if that fails, wgpu falls back to a CPU adapter
  (slow but functional).
- TODO: bearer-token auth on the gRPC `Subscribe` stream and TLS are not yet
  shipped. SkyPilot's per-cluster firewall (`ports: [50051]`) is the only
  defense — keep this on internal-network use only until follow-up auth lands.
