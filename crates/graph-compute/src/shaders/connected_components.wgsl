// GPU connected components — min-label propagation over the symmetrized CSR.
//
// One thread per node v:
//     label_out[v] = min(label_in[v], min_{u ∈ N(v)} label_in[u])
//
// Labels start as each node's own index; iterating the min-gather to a fixed
// point labels every node with the smallest index in its (undirected)
// component — the canonical WCC labeling. Each thread writes only its own row →
// race-free (no atomics needed for the labels). A single shared `changed` flag
// (u32 atomic — WGSL supports integer atomics, unlike f32) signals the host
// whether to iterate again; convergence is in O(diameter) steps.

struct Params {
  n:    u32,
  _p0:  u32,
  _p1:  u32,
  _p2:  u32,
};

@group(0) @binding(0) var<storage, read>        offsets:   array<u32>;   // n + 1
@group(0) @binding(1) var<storage, read>        neighbors: array<u32>;   // m
@group(0) @binding(2) var<storage, read>        label_in:  array<u32>;   // n
@group(0) @binding(3) var<storage, read_write>  label_out: array<u32>;   // n
@group(0) @binding(4) var<storage, read_write>  changed:   atomic<u32>;  // 0/1
@group(0) @binding(5) var<uniform>              params:    Params;

@compute @workgroup_size(64)
fn cc_step(@builtin(global_invocation_id) gid: vec3<u32>,
          @builtin(num_workgroups) nwg: vec3<u32>) {
  // 2-D-tiled dispatch: linear index across rows of nwg.x·64 invocations.
  let v = gid.y * nwg.x * 64u + gid.x;
  if (v >= params.n) { return; }

  let self_label = label_in[v];
  var m = self_label;
  let start = offsets[v];
  let end   = offsets[v + 1u];
  for (var e: u32 = start; e < end; e = e + 1u) {
    m = min(m, label_in[neighbors[e]]);
  }
  label_out[v] = m;
  if (m != self_label) {
    atomicStore(&changed, 1u);
  }
}
