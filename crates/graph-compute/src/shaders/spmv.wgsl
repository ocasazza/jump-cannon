// Weighted sparse matrix–vector product y = A·x over a CSR matrix.
//
// One thread per row v:
//     y[v] = Σ_{e ∈ row v} weights[e] * x[neighbors[e]]
//
// The unifying primitive the graph analytics reduce to (PageRank is this with
// weights = inv_deg and x = ranks; the semiring variants — min for CC, min/+1
// for BFS — swap the * and + for their respective operators). Each thread writes
// only its own row → race-free, no atomics. Accumulation is f32 (the f16 storage
// variant — half-precision weights/x with f32 accumulate — is gated behind the
// SHADER_F16 device feature and lives in a separate enable-f16 shader).

struct Params {
  n:   u32,
  _p0: u32,
  _p1: u32,
  _p2: u32,
};

@group(0) @binding(0) var<storage, read>       offsets:   array<u32>;   // n + 1
@group(0) @binding(1) var<storage, read>       neighbors: array<u32>;   // m (columns)
@group(0) @binding(2) var<storage, read>       weights:   array<f32>;   // m (values)
@group(0) @binding(3) var<storage, read>       x:         array<f32>;   // n
@group(0) @binding(4) var<storage, read_write> y:         array<f32>;   // n
@group(0) @binding(5) var<uniform>             params:    Params;

@compute @workgroup_size(64)
fn spmv(@builtin(global_invocation_id) gid: vec3<u32>,
          @builtin(num_workgroups) nwg: vec3<u32>) {
  // 2-D-tiled dispatch: linear index across rows of nwg.x·64 invocations.
  let v = gid.y * nwg.x * 64u + gid.x;
  if (v >= params.n) { return; }

  var acc: f32 = 0.0;
  let start = offsets[v];
  let end   = offsets[v + 1u];
  for (var e: u32 = start; e < end; e = e + 1u) {
    acc = acc + weights[e] * x[neighbors[e]];
  }
  y[v] = acc;
}
