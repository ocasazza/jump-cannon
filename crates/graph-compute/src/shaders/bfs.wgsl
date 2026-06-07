// GPU single-source BFS — unweighted shortest-path distance relaxation over the
// symmetrized CSR.
//
// One thread per node v:
//     dist_out[v] = min(dist_in[v], 1 + min_{u ∈ N(v), reachable} dist_in[u])
//
// Iterating to a fixed point yields exact hop distances from the source
// (Bellman-Ford with unit weights; converges in O(diameter) steps). The source
// is seeded to 0 and all other nodes to INF (0xFFFFFFFF) by the host. Each
// thread writes only its own row → race-free; a u32 atomic `changed` flag tells
// the host whether to iterate again. Unreachable nodes keep INF.

const INF: u32 = 4294967295u; // 0xFFFFFFFF

struct Params {
  n:   u32,
  _p0: u32,
  _p1: u32,
  _p2: u32,
};

@group(0) @binding(0) var<storage, read>        offsets:   array<u32>;   // n + 1
@group(0) @binding(1) var<storage, read>        neighbors: array<u32>;   // m
@group(0) @binding(2) var<storage, read>        dist_in:   array<u32>;   // n
@group(0) @binding(3) var<storage, read_write>  dist_out:  array<u32>;   // n
@group(0) @binding(4) var<storage, read_write>  changed:   atomic<u32>;  // 0/1
@group(0) @binding(5) var<uniform>              params:    Params;

@compute @workgroup_size(64)
fn bfs_step(@builtin(global_invocation_id) gid: vec3<u32>,
          @builtin(num_workgroups) nwg: vec3<u32>) {
  // 2-D-tiled dispatch: linear index across rows of nwg.x·64 invocations.
  let v = gid.y * nwg.x * 64u + gid.x;
  if (v >= params.n) { return; }

  let cur = dist_in[v];
  var best = cur;
  let start = offsets[v];
  let end   = offsets[v + 1u];
  for (var e: u32 = start; e < end; e = e + 1u) {
    let du = dist_in[neighbors[e]];
    // Guard against INF + 1 wrapping to 0 (WGSL u32 wraps on overflow).
    if (du != INF) {
      best = min(best, du + 1u);
    }
  }
  dist_out[v] = best;
  if (best != cur) {
    atomicStore(&changed, 1u);
  }
}
