// GPU-resident water-sheet layout kernels, `F64Pos` precision: 64-bit
// fixed-point positions, f32 velocities and forces.
//
// Identical force model, topology, dispatch and fixed order as
// `water_sheet.wgsl`; the only difference is POSITION STORAGE. Every position
// component is a 64-bit fixed-point number with scale 2^-40, stored as two u32
// slots (`buf[2i]` = integer word, `buf[2i+1]` = fraction word), 8 bytes per
// component instead of 4:
//
//     value = hi / 2^8 + lo / 2^40      hi: i32 (integer part * 2^8), lo: u32
//
// Why not an f32 double-double (TwoSum)? Measured on this adapter: exact
// error-free transforms are NOT safe in this stack. wgpu-hal leaves Metal's
// `MTLCompileOptions.fastMathEnabled` at its default (ON), so the shader
// compiler may reassociate floating point, folding `(a + b) - a` to `b` and
// reducing the TwoSum residual to zero — the `lo` word stayed exactly 0 for
// every component and every step. Integer arithmetic is immune: the 64-bit
// add is exact, the position keeps 9.1e-13 absolute resolution over the +-8.4e6
// range a 100M-node sheet needs, and no device feature is required (no
// `SHADER_F64`, no fp64 hardware).
//
// Per step, per node:
//   1. forces are computed in f32 from `hi/2^8 + lo/2^40` rounded to f32 — the
//      same value the f32 kernel's force model sees;
//   2. velocity is integrated in f32: `v = (v + dt*f)*damping`;
//   3. the increment `d = dt*v` (f32) is converted to fixed point and added to
//      the position with an exact 64-bit integer add.
//
// The f64 CPU oracle (`water::cpu::step_cpu_f64pos`) mirrors the operation
// sequence with an f64 position: f32 force, f32 velocity, f32 increment.

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

@group(0) @binding(0) var<storage, read> px_in: array<u32>;
@group(0) @binding(1) var<storage, read> py_in: array<u32>;
@group(0) @binding(2) var<storage, read> pz_in: array<u32>;
@group(0) @binding(3) var<storage, read_write> px_out: array<u32>;
@group(0) @binding(4) var<storage, read_write> py_out: array<u32>;
@group(0) @binding(5) var<storage, read_write> pz_out: array<u32>;
@group(0) @binding(6) var<storage, read_write> vx: array<f32>;
@group(0) @binding(7) var<storage, read_write> vy: array<f32>;
@group(0) @binding(8) var<storage, read_write> vz: array<f32>;
@group(0) @binding(9) var<uniform> params: Params;

const NAIVE_WG: u32 = 256u;
const SHEET_WG: u32 = 8u;
/// Compile-time repulsion radius of the staged sheet kernel (substituted by
/// the engine with `settings.r_sheet`) so the shared tile has a fixed shape.
const R_S_STAGE: u32 = 3u;
const TILE_W: u32 = SHEET_WG + 2u * R_S_STAGE;
const TILE: u32 = TILE_W * TILE_W;

var<workgroup> tile: array<vec3<f32>, TILE>;
var<workgroup> tile_ok: array<u32, TILE>;

// ---------------------------------------------------------------------------
// Fixed-point helpers (64-bit integer position; no SHADER_F64 required).
// ---------------------------------------------------------------------------

/// Decode one fixed-point component to f32 (the force-model view). Both
/// divisions are by powers of two, hence exact; one add rounds to f32.
fn fixed_to_f32(hi: u32, lo: u32) -> f32 {
    return f32(bitcast<i32>(hi)) / 256.0 + f32(lo) / 1099511627776.0;
}

/// Encode an f32 increment as a fixed-point (integer word, fraction word)
/// pair. The scale is a power of two, so `d * 2^40` is exact; `floor` splits
/// it at 2^32 exactly, truncating at most one quantum (2^-40).
fn f32_to_fixed(d: f32) -> vec2<u32> {
    let scaled = d * 1099511627776.0; // * 2^40
    let hi = i32(floor(scaled / 4294967296.0)); // floor(d * 2^8)
    let lo = u32(scaled - f32(hi) * 4294967296.0); // in [0, 2^32)
    return vec2<u32>(bitcast<u32>(hi), lo);
}

/// Exact 64-bit fixed-point add of a stored component and an increment pair.
fn fixed_add(hi: u32, lo: u32, d: vec2<u32>) -> vec2<u32> {
    let nlo = lo + d.y;
    let carry = select(0, 1, nlo < lo);
    let nhi = bitcast<i32>(hi) + bitcast<i32>(d.x) + carry;
    return vec2<u32>(bitcast<u32>(nhi), nlo);
}

/// Position components as f32 — what the force model sees.
fn in_f32_x(i: u32) -> f32 {
    return fixed_to_f32(px_in[2u * i], px_in[2u * i + 1u]);
}
fn in_f32_y(i: u32) -> f32 {
    return fixed_to_f32(py_in[2u * i], py_in[2u * i + 1u]);
}
fn in_f32_z(i: u32) -> f32 {
    return fixed_to_f32(pz_in[2u * i], pz_in[2u * i + 1u]);
}

/// Node `i` position, f32-rounded — what the force model sees.
fn in_pos_f32(i: u32) -> vec3<f32> {
    return vec3<f32>(in_f32_x(i), in_f32_y(i), in_f32_z(i));
}

/// Add a f32 increment to one stored component (exact integer add).
fn advance_x(i: u32, d: f32) {
    let r = fixed_add(px_in[2u * i], px_in[2u * i + 1u], f32_to_fixed(d));
    px_out[2u * i] = r.x;
    px_out[2u * i + 1u] = r.y;
}
fn advance_y(i: u32, d: f32) {
    let r = fixed_add(py_in[2u * i], py_in[2u * i + 1u], f32_to_fixed(d));
    py_out[2u * i] = r.x;
    py_out[2u * i + 1u] = r.y;
}
fn advance_z(i: u32, d: f32) {
    let r = fixed_add(pz_in[2u * i], pz_in[2u * i + 1u], f32_to_fixed(d));
    pz_out[2u * i] = r.x;
    pz_out[2u * i + 1u] = r.y;
}

// ---------------------------------------------------------------------------
// Force model (identical to water_sheet.wgsl / water::cpu::node_force).
// ---------------------------------------------------------------------------

/// Spring from neighbour `j` onto the node at `pi`.
fn spring(acc: vec3<f32>, pi: vec3<f32>, j: u32, rest: f32) -> vec3<f32> {
    let d = in_pos_f32(j) - pi;
    let len = length(d);
    if (len > 1e-9) {
        let t = params.k_bond * (len - rest) / len;
        return acc + t * d;
    }
    return acc;
}

/// Bond springs over the canonical waterSheet topology, in the SAME fixed
/// order `WaterSheetParams::graph` emits: a sheet node sees up, left, right,
/// down; a molecule's O sees H1 then H2; each H sees its O.
fn bond_force(i: u32, pi: vec3<f32>) -> vec3<f32> {
    var acc = vec3<f32>(0.0, 0.0, 0.0);
    let sheet_base = 3u * params.n_mol;
    if (i < sheet_base) {
        let o = (i / 3u) * 3u;
        if (i == o) {
            acc = spring(acc, pi, o + 1u, params.bond_rest);
            acc = spring(acc, pi, o + 2u, params.bond_rest);
        } else {
            acc = spring(acc, pi, o, params.bond_rest);
        }
    } else {
        let local = i - sheet_base;
        let cols = params.cols;
        let r = local / cols;
        let c = local - cols * r;
        if (r > 0u) {
            acc = spring(acc, pi, i - cols, params.sheet_rest);
        }
        if (c > 0u) {
            acc = spring(acc, pi, i - 1u, params.sheet_rest);
        }
        if (c + 1u < cols) {
            acc = spring(acc, pi, i + 1u, params.sheet_rest);
        }
        if (r + 1u < params.rows) {
            acc = spring(acc, pi, i + cols, params.sheet_rest);
        }
    }
    return acc;
}

/// Soft repulsion on the node at `pi` from node `j` (squared-distance cull).
fn repulse(f: vec3<f32>, pi: vec3<f32>, j: u32) -> vec3<f32> {
    let d = pi - in_pos_f32(j);
    let d2 = dot(d, d);
    if (d2 < params.rcut * params.rcut && d2 > 1e-18) {
        let len = sqrt(d2);
        let t = 1.0 - len / params.rcut;
        let w = params.k_rep * (t * t) / len;
        return f + w * d;
    }
    return f;
}

/// Bond springs + windowed repulsion + integration for node `i`. Shared by
/// `step_naive` (all nodes) and `mol_staged` (molecule section).
fn integrate(i: u32) {
    let pi = in_pos_f32(i);
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

    // f32 velocity integration, then f32 increment added to the f64 position.
    let v = vec3<f32>(vx[i], vy[i], vz[i]);
    let nv = (v + params.dt * f) * params.damping;
    vx[i] = nv.x;
    vy[i] = nv.y;
    vz[i] = nv.z;
    let dx = params.dt * nv.x;
    let dy = params.dt * nv.y;
    let dz = params.dt * nv.z;
    advance_x(i, dx);
    advance_y(i, dy);
    advance_z(i, dz);
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

    // Stage the (SHEET_WG + 2R)² neighbourhood as f32-rounded positions: the
    // tile only feeds the f32 force model, so rounding here is exact for that
    // purpose (and keeps the shared tile the same size as the f32 kernel's).
    for (var t = li; t < TILE; t = t + SHEET_WG * SHEET_WG) {
        let tr = t / TILE_W;
        let tc = t - TILE_W * tr;
        let gr = i32(base_r) + i32(tr) - i32(R_S_STAGE);
        let gc = i32(base_c) + i32(tc) - i32(R_S_STAGE);
        let ok = gr >= 0 && gc >= 0 && gr < i32(params.rows) && gc < i32(params.cols);
        if (ok) {
            let j = 3u * params.n_mol + u32(gr) * params.cols + u32(gc);
            tile[t] = in_pos_f32(j);
        }
        tile_ok[t] = select(0u, 1u, ok);
    }
    workgroupBarrier();

    let r = gid.y;
    let c = gid.x;
    if (r < params.rows && c < params.cols) {
        let i = 3u * params.n_mol + r * params.cols + c;
        let pi = in_pos_f32(i);
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
                    if (d2 < params.rcut * params.rcut && d2 > 1e-18) {
                        let len = sqrt(d2);
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
        let dx = params.dt * nv.x;
        let dy = params.dt * nv.y;
        let dz = params.dt * nv.z;
        advance_x(i, dx);
        advance_y(i, dy);
        advance_z(i, dz);
    }
}
