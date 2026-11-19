// GPU-resident water-sheet layout kernels, native `F64` precision.
//
// Requires the `wgpu::Features::SHADER_F64` device feature (`#enable f64`).
// Positions, velocities AND forces are f64 end to end; the force model,
// topology, dispatch shape and fixed summation order are identical to
// `water_sheet.wgsl` (which remains the f32 kernel). The params uniform stays
// f32 (shared with the other modes) and is widened exactly at load.
//
// The engine only builds this module when the device reports SHADER_F64. On an
// adapter without it (Apple GPUs expose no fp64) the engine rejects `F64` with
// an error naming the mode; precision is never silently downgraded.

#enable f64

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

@group(0) @binding(0) var<storage, read> px_in: array<f64>;
@group(0) @binding(1) var<storage, read> py_in: array<f64>;
@group(0) @binding(2) var<storage, read> pz_in: array<f64>;
@group(0) @binding(3) var<storage, read_write> px_out: array<f64>;
@group(0) @binding(4) var<storage, read_write> py_out: array<f64>;
@group(0) @binding(5) var<storage, read_write> pz_out: array<f64>;
@group(0) @binding(6) var<storage, read_write> vx: array<f64>;
@group(0) @binding(7) var<storage, read_write> vy: array<f64>;
@group(0) @binding(8) var<storage, read_write> vz: array<f64>;
@group(0) @binding(9) var<uniform> params: Params;

const NAIVE_WG: u32 = 256u;
const SHEET_WG: u32 = 8u;
/// Compile-time repulsion radius of the staged sheet kernel (substituted by
/// the engine with `settings.r_sheet`) so the shared tile has a fixed shape.
const R_S_STAGE: u32 = 3u;
const TILE_W: u32 = SHEET_WG + 2u * R_S_STAGE;
const TILE: u32 = TILE_W * TILE_W;

var<workgroup> tile: array<vec3<f64>, TILE>;
var<workgroup> tile_ok: array<u32, TILE>;

fn in_pos(i: u32) -> vec3<f64> {
    return vec3<f64>(px_in[i], py_in[i], pz_in[i]);
}

/// Spring from neighbour `j` onto the node at `pi`.
fn spring(acc: vec3<f64>, pi: vec3<f64>, j: u32, rest: f64) -> vec3<f64> {
    let d = in_pos(j) - pi;
    let len = length(d);
    if (len > 1e-9) {
        let t = f64(params.k_bond) * (len - rest) / len;
        return acc + t * d;
    }
    return acc;
}

/// Bond springs over the canonical waterSheet topology, in the SAME fixed
/// order `WaterSheetParams::graph` emits: a sheet node sees up, left, right,
/// down; a molecule's O sees H1 then H2; each H sees its O.
fn bond_force(i: u32, pi: vec3<f64>) -> vec3<f64> {
    var acc = vec3<f64>(0.0, 0.0, 0.0);
    let sheet_base = 3u * params.n_mol;
    if (i < sheet_base) {
        let o = (i / 3u) * 3u;
        if (i == o) {
            acc = spring(acc, pi, o + 1u, f64(params.bond_rest));
            acc = spring(acc, pi, o + 2u, f64(params.bond_rest));
        } else {
            acc = spring(acc, pi, o, f64(params.bond_rest));
        }
    } else {
        let local = i - sheet_base;
        let cols = params.cols;
        let r = local / cols;
        let c = local - cols * r;
        if (r > 0u) {
            acc = spring(acc, pi, i - cols, f64(params.sheet_rest));
        }
        if (c > 0u) {
            acc = spring(acc, pi, i - 1u, f64(params.sheet_rest));
        }
        if (c + 1u < cols) {
            acc = spring(acc, pi, i + 1u, f64(params.sheet_rest));
        }
        if (r + 1u < params.rows) {
            acc = spring(acc, pi, i + cols, f64(params.sheet_rest));
        }
    }
    return acc;
}

/// Soft repulsion on the node at `pi` from node `j` (squared-distance cull).
fn repulse(f: vec3<f64>, pi: vec3<f64>, j: u32) -> vec3<f64> {
    let d = pi - in_pos(j);
    let d2 = dot(d, d);
    let rcut = f64(params.rcut);
    if (d2 < rcut * rcut && d2 > 1e-18) {
        let len = sqrt(d2);
        let t = 1.0 - len / rcut;
        let w = f64(params.k_rep) * (t * t) / len;
        return f + w * d;
    }
    return f;
}

/// Bond springs + windowed repulsion + integration for node `i`.
fn integrate(i: u32) {
    let pi = in_pos(i);
    var f = bond_force(i, pi);

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
                f = repulse(f, pi, sheet_base + u32(rr * cols + cc));
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
                f = repulse(f, pi, u32(jm) * 3u);
            }
        }
    }

    let dt = f64(params.dt);
    let damping = f64(params.damping);
    let v = vec3<f64>(vx[i], vy[i], vz[i]);
    let nv = (v + dt * f) * damping;
    vx[i] = nv.x;
    vy[i] = nv.y;
    vz[i] = nv.z;
    px_out[i] = pi.x + dt * nv.x;
    py_out[i] = pi.y + dt * nv.y;
    pz_out[i] = pi.z + dt * nv.z;
}

@compute @workgroup_size(NAIVE_WG)
fn step_naive(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
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
            tile[t] = in_pos(j);
        }
        tile_ok[t] = select(0u, 1u, ok);
    }
    workgroupBarrier();

    let r = gid.y;
    let c = gid.x;
    if (r < params.rows && c < params.cols) {
        let i = 3u * params.n_mol + r * params.cols + c;
        let pi = in_pos(i);
        var f = bond_force(i, pi);
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
                    let d = pi - tile[t];
                    let d2 = dot(d, d);
                    let rcut = f64(params.rcut);
                    if (d2 < rcut * rcut && d2 > 1e-18) {
                        let len = sqrt(d2);
                        let tt = 1.0 - len / rcut;
                        let w = f64(params.k_rep) * (tt * tt) / len;
                        f = f + w * d;
                    }
                }
            }
        }

        let dt = f64(params.dt);
        let damping = f64(params.damping);
        let v = vec3<f64>(vx[i], vy[i], vz[i]);
        let nv = (v + dt * f) * damping;
        vx[i] = nv.x;
        vy[i] = nv.y;
        vz[i] = nv.z;
        px_out[i] = pi.x + dt * nv.x;
        py_out[i] = pi.y + dt * nv.y;
        pz_out[i] = pi.z + dt * nv.z;
    }
}
