// GPU PageRank — pull-style power iteration over the symmetrized CSR.
//
// One thread per node v:
//     rank_out[v] = teleport + damping * Σ_{u ∈ N(v)} rank_in[u] * inv_deg[u]
//
// Each thread writes only its OWN row → race-free, NO atomics (WebGPU/WGSL has
// no f32 atomics anyway). On the undirected/symmetrized CSR this PULL form is
// numerically identical to the CPU oracle's PUSH form (`geometric::pagerank`),
// because u ∈ N(v) ⟺ v ∈ N(u). Dangling nodes are excluded by the host
// (inv_deg = 0) and the host rejects graphs with degree-0 nodes for now — the
// global dangling-mass redistribution is a later milestone (Part A P1).
//
// Accumulation is f32 (the rank-underflow rule keeps ranks f32 at scale; f16
// storage is for the weighted-SpMV primitive, not these ranks).

struct Params {
  n:        u32,
  damping:  f32,
  teleport: f32,   // (1 - damping) / n
  _pad:     f32,
};

@group(0) @binding(0) var<storage, read>       offsets:   array<u32>;   // n + 1, CSR row offsets
@group(0) @binding(1) var<storage, read>       neighbors: array<u32>;   // m, flat adjacency
@group(0) @binding(2) var<storage, read>       inv_deg:   array<f32>;   // n, 1/deg(u) (0 if dangling)
@group(0) @binding(3) var<storage, read>       rank_in:   array<f32>;   // n
@group(0) @binding(4) var<storage, read_write> rank_out:  array<f32>;   // n
@group(0) @binding(5) var<uniform>             params:    Params;

@compute @workgroup_size(64)
fn pr_step(@builtin(global_invocation_id) gid: vec3<u32>) {
  let v = gid.x;
  if (v >= params.n) { return; }

  let start = offsets[v];
  let end   = offsets[v + 1u];
  var acc: f32 = 0.0;
  for (var e: u32 = start; e < end; e = e + 1u) {
    let u = neighbors[e];
    acc = acc + rank_in[u] * inv_deg[u];
  }
  rank_out[v] = params.teleport + params.damping * acc;
}
