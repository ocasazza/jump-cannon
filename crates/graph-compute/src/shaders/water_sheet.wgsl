// GPU-resident water-sheet layout kernels (`"water-sheet-gpu"`).
//
// Three entry points over one shared bind-group layout (SoA positions in/out
// ping-pong + SoA velocities in place + CSR + params uniform):
//
//   - `step_naive`  — 1-D dispatch over ALL nodes; each invocation reads its
//     repulsion window straight from the global SoA buffers. Workgroups are
//     tiled into 2-D by the host (`workgroup_dims_2d`) when they exceed the
//     per-dimension cap, so `i = gid.y * nwg.x * NAIVE_WG + gid.x`.
//   - `sheet_staged` — 2-D dispatch over the lattice grid with the node's
//     repulsion window staged through workgroup shared memory (classic stencil
//     tiling): the 8×8 workgroup loads a 14×14 tile once, then every thread
//     scans its window from shared memory instead of re-reading global memory.
//   - `mol_staged`  — 1-D dispatch over the molecule section (H bonds only;
//     O windowed repulsion in the molecule grid), paired with `sheet_staged`
//     so the ping-pong OUT buffer's molecule section is refreshed too.
//
// Force model (identical to `water::cpu::node_force`, same fixed order):
//   1. bond springs along CSR edges (O–H at `bond_rest`, sheet at `sheet_rest`);
//   2. bounded-window soft repulsion (zero beyond `rcut`), sheet node vs its
//      r_sheet-window lattice cells, molecule O vs its r_mol-window O's.
// Then semi-implicit Euler: `v = (v + dt·f)·damping; p += dt·v`.
//
// Determinism: one invocation owns one node; forces are fixed-order sums over
// a bounded window; no atomics — CPU and GPU agree up to FP contraction.

struct Params {
    n_nodes: u32,
    n_mol: u32,
    cols: u32,
    rows: u32,
    mol_grid_w: u32,
    r_sheet: u32,
    r_mol: u32,
    _pad0: u32,
    bond_rest: f32,
    sheet_rest: f32,
    k_bond: f32,
    k_rep: f32,
    rcut: f32,
    dt: f32,
    damping: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<storage, read> px_in: array<f32>;
@group(0) @binding(1) var<storage, read> py_in: array<f32>;
@group(0) @binding(2) var<storage, read> pz_in: array<f32>;
@group(0) @binding(3) var<storage, read_write> px_out: array<f32>;
@group(0) @binding(4) var<storage, read_write> py_out: array<f32>;
@group(0) @binding(5) var<storage, read_write> pz_out: array<f32>;
@group(0) @binding(6) var<storage, read_write> vx: array<f32>;
@group(0) @binding(7) var<storage, read_write> vy: array<f32>;
@group(0) @binding(8) var<storage, read_write> vz: array<f32>;
@group(0) @binding(9) var<storage, read> offsets: array<u32>;
@group(0) @binding(10) var<storage, read> neighbors: array<u32>;
@group(0) @binding(11) var<uniform> params: Params;

const NAIVE_WG: u32 = 256u;
const SHEET_WG: u32 = 8u;
/// Compile-time repulsion radius of the staged sheet kernel. The staged path
/// requires `settings.r_sheet == 3` (validated at engine init) so the shared
/// tile has a fixed shape.
const R_S_STAGE: u32 = 3u;
const TILE_W: u32 = SHEET_WG + 2u * R_S_STAGE;
const TILE: u32 = TILE_W * TILE_W;

var<workgroup> tile: array<vec3<f32>, TILE>;
var<workgroup> tile_ok: array<u32, TILE>;

fn rest_for(i: u32) -> f32 {
    return select(params.sheet_rest, params.bond_rest, i < 3u * params.n_mol);
}

fn bond_force(i: u32, f: vec3<f32>) -> vec3<f32> {
    var acc = f;
    let rest = rest_for(i);
    let start = offsets[i];
    let end = offsets[i + 1u];
    for (var k = start; k < end; k = k + 1u) {
        let j = neighbors[k];
        let d = vec3<f32>(
            px_in[j] - px_in[i],
            py_in[j] - py_in[i],
            pz_in[j] - pz_in[i],
        );
        let len = length(d);
        if (len > 1e-9) {
            let t = params.k_bond * (len - rest) / len;
            acc = acc + t * d;
        }
    }
    return acc;
}

fn repulse(f: vec3<f32>, i: u32, j: u32) -> vec3<f32> {
    let d = vec3<f32>(
        px_in[i] - px_in[j],
        py_in[i] - py_in[j],
        pz_in[i] - pz_in[j],
    );
    let len = length(d);
    if (len < params.rcut && len > 1e-9) {
        let t = 1.0 - len / params.rcut;
        let w = params.k_rep * (t * t) / len;
        return f + w * d;
    }
    return f;
}

/// Bond springs + windowed repulsion + integration for node `i`. Shared by
/// `step_naive` (all nodes) and `mol_staged` (molecule section).
fn integrate(i: u32) {
    var f = bond_force(i, vec3<f32>(0.0, 0.0, 0.0));

    let sheet_base = 3u * params.n_mol;
    if (i >= sheet_base) {
        let local = i32(i - sheet_base);
        let cols = i32(params.cols);
        let rows = i32(params.rows);
        let r = local / cols;
        let c = local - cols * r;
        let rad = i32(params.r_sheet);
        for (var dr = -rad; dr <= rad; dr = dr + 1) {
            for (var dc = -rad; dc <= rad; dc = dc + 1) {
                if (dr == 0 && dc == 0) {
                    continue;
                }
                let rr = r + dr;
                let cc = c + dc;
                if (rr < 0 || cc < 0 || rr >= rows || cc >= cols) {
                    continue;
                }
                f = repulse(f, i, sheet_base + u32(rr * cols + cc));
            }
        }
    } else if (i % 3u == 0u) {
        // Oxygen of molecule m: windowed repulsion in the molecule grid.
        let m = i / 3u;
        let w = i32(params.mol_grid_w);
        let mr = i32(m) / w;
        let mc = i32(m) - w * mr;
        let h = i32((params.n_mol + params.mol_grid_w - 1u) / params.mol_grid_w);
        let rad = i32(params.r_mol);
        for (var dr = -rad; dr <= rad; dr = dr + 1) {
            for (var dc = -rad; dc <= rad; dc = dc + 1) {
                if (dr == 0 && dc == 0) {
                    continue;
                }
                let rr = mr + dr;
                let cc = mc + dc;
                if (rr < 0 || cc < 0 || rr >= h || cc >= w) {
                    continue;
                }
                let jm = rr * w + cc;
                if (jm >= i32(params.n_mol)) {
                    continue;
                }
                f = repulse(f, i, u32(jm) * 3u);
            }
        }
    }

    let v = vec3<f32>(vx[i], vy[i], vz[i]);
    let nv = (v + params.dt * f) * params.damping;
    vx[i] = nv.x;
    vy[i] = nv.y;
    vz[i] = nv.z;
    px_out[i] = px_in[i] + params.dt * nv.x;
    py_out[i] = py_in[i] + params.dt * nv.y;
    pz_out[i] = pz_in[i] + params.dt * nv.z;
}

@compute @workgroup_size(NAIVE_WG)
fn step_naive(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    // 2-D-tiled dispatch: linear node index across rows of nwg.x·NAIVE_WG
    // invocations (host tiles into y past the per-dimension cap).
    let i = gid.y * nwg.x * NAIVE_WG + gid.x;
    if (i >= params.n_nodes) {
        return;
    }
    integrate(i);
}

@compute @workgroup_size(NAIVE_WG)
fn mol_staged(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = gid.y * nwg.x * NAIVE_WG + gid.x;
    if (i >= 3u * params.n_mol) {
        return;
    }
    integrate(i);
}

@compute @workgroup_size(SHEET_WG, SHEET_WG)
fn sheet_staged(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let base_r = gid.y - (gid.y % SHEET_WG);
    let base_c = gid.x - (gid.x % SHEET_WG);

    // Stage the (SHEET_WG + 2R)² neighbourhood of this workgroup's tile.
    for (var t = li; t < TILE; t = t + SHEET_WG * SHEET_WG) {
        let tr = t / TILE_W;
        let tc = t - TILE_W * tr;
        let gr = i32(base_r) + i32(tr) - i32(R_S_STAGE);
        let gc = i32(base_c) + i32(tc) - i32(R_S_STAGE);
        let ok = gr >= 0 && gc >= 0 && gr < i32(params.rows) && gc < i32(params.cols);
        if (ok) {
            let j = 3u * params.n_mol + u32(gr) * params.cols + u32(gc);
            tile[t] = vec3<f32>(px_in[j], py_in[j], pz_in[j]);
        }
        tile_ok[t] = select(0u, 1u, ok);
    }
    workgroupBarrier();

    let r = gid.y;
    let c = gid.x;
    if (r < params.rows && c < params.cols) {
        let i = 3u * params.n_mol + r * params.cols + c;
        var f = bond_force(i, vec3<f32>(0.0, 0.0, 0.0));
        let ly = i32(r - base_r);
        let lx = i32(c - base_c);
        let rad = i32(R_S_STAGE);
        for (var dr = -rad; dr <= rad; dr = dr + 1) {
            for (var dc = -rad; dc <= rad; dc = dc + 1) {
                if (dr == 0 && dc == 0) {
                    continue;
                }
                let tr = ly + dr + i32(R_S_STAGE);
                let tc = lx + dc + i32(R_S_STAGE);
                let t = u32(tr * i32(TILE_W) + tc);
                if (tile_ok[t] == 1u) {
                    let j = 3u * params.n_mol + u32(i32(r) + dr) * params.cols + u32(i32(c) + dc);
                    let d = vec3<f32>(
                        px_in[i] - tile[t].x,
                        py_in[i] - tile[t].y,
                        pz_in[i] - tile[t].z,
                    );
                    let len = length(d);
                    if (len < params.rcut && len > 1e-9) {
                        let tt = 1.0 - len / params.rcut;
                        let w = params.k_rep * (tt * tt) / len;
                        f = f + w * d;
                    }
                }
            }
        }

        let v = vec3<f32>(vx[i], vy[i], vz[i]);
        let nv = (v + params.dt * f) * params.damping;
        vx[i] = nv.x;
        vy[i] = nv.y;
        vz[i] = nv.z;
        px_out[i] = px_in[i] + params.dt * nv.x;
        py_out[i] = py_in[i] + params.dt * nv.y;
        pz_out[i] = pz_in[i] + params.dt * nv.z;
    }
}
