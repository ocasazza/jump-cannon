// GPU SGD stress-majorization kernel (sparse / pivot stress).
//
// One thread per node. Each node optimizes its `k` pivot terms against the
// *start-of-sweep* positions (read from `pos_in`), moving only itself, and
// writes the result to `pos_out`. Because every read is from `pos_in` and every
// write targets a distinct `pos_out[i]`, the dispatch is conflict-free — the
// Jacobi form of the s_gd2 pivot update (Zheng/Pawar/Goodman + Ortmann sparse
// stress). The Rust side ping-pongs `pos_in`/`pos_out` between sweeps and feeds
// the annealed step size `eta` in the uniform block.
//
// Per term (node i, pivot node j at graph distance d): weight w = d^-2, capped
// step mu = min(1, w*eta), and we slide i along (x_i - x_j) so the Euclidean
// distance moves toward d. At mu = 1 this projects i exactly onto the sphere of
// radius d around the (fixed) pivot. Terms are applied Gauss-Seidel within the
// node's own k pivots (each updates the running local position p), which is
// safe since all of i's terms touch only i.

struct Params {
    n: u32,
    k: u32,
    eta: f32,
    _pad: f32,
};

@group(0) @binding(0) var<storage, read> pos_in: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> pos_out: array<vec4<f32>>;
// Global node indices of the k pivots (length k).
@group(0) @binding(2) var<storage, read> pivot_nodes: array<u32>;
// Row-major per-node pivot distances: dist[i*k + t] is node i's graph distance
// to pivot t. A sentinel of 0.0 means "skip" (self, or unreachable component).
@group(0) @binding(3) var<storage, read> dist: array<f32>;
@group(0) @binding(4) var<uniform> params: Params;

@compute @workgroup_size(64)
fn sgd_step(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n) {
        return;
    }
    let k = params.k;
    var p = pos_in[i].xyz;

    for (var t: u32 = 0u; t < k; t = t + 1u) {
        let j = pivot_nodes[t];
        if (j == i) {
            continue;
        }
        let d = dist[i * k + t];
        if (d <= 0.0) {
            continue; // self / unreachable sentinel
        }
        let q = pos_in[j].xyz;
        let delta = p - q;
        let mag = max(length(delta), 1e-6);
        let w = 1.0 / (d * d);
        let mu = min(1.0, w * params.eta);
        let r = mu * (mag - d) / mag;
        p = p - r * delta;
    }

    pos_out[i] = vec4<f32>(p, 0.0);
}
