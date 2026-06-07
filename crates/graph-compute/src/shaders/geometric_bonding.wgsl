// Dynamic-edge (self-assembly) bonding stage — GPU port (P4).
//
// Atomics-free, sort-based, deterministic. WebGPU has NO f32 atomics, and we
// also avoid u32 atomics so the result is bit-reproducible run-to-run (the
// solved-case canary methodology depends on it). The pipeline mirrors the CPU
// `update_dynamic_bonds` exactly so a CPU<->GPU equivalence gate can assert the
// two backends produce the SAME bond set on the same frozen configuration:
//
//   1. calc_hash      — each node's linearised cell id (cell = r_break), O(n).
//   2. (host) counting-sort the node ids by cell id (a stable, deterministic
//      sort done on the CPU from the hashes read back — the design's
//      `radix/counting sort` step; kept host-side so it is exactly reproducible
//      and atomics-free, while the O(n) hash + the O(n·27) candidate scan + the
//      O(pairs) bond decisions run on the GPU).
//   3. find_cell_start — first/last sorted index per cell, O(n).
//   4. scan_candidates — for each node, scan the 27 (3x3x3) neighbour cells and
//      emit candidate pairs (j>i, class-compatible, within r_bond) into a
//      per-node candidate slate, O(n·k).
//
// The hysteretic break of over-stretched existing bonds and the conflict-free
// valence-cap accept/reject are done on the host from the candidate slate +
// surviving-bond set (a single deterministic serial pass over sorted candidate
// keys — exactly the CPU algorithm), because the accept/reject is a sequential
// dependency (a bond accepted earlier consumes valence later candidates see).
// This keeps the GPU doing the embarrassingly-parallel O(n)/O(n·27) work (hash +
// candidate generation) and the host doing the inherently-serial O(pairs)
// accept/reject — atomics-free on both sides, byte-identical to CPU.

struct BondParams {
    n_nodes: u32,
    grid_min_x: f32,
    grid_min_y: f32,
    grid_min_z: f32,
    inv_cell: f32,     // 1 / r_break
    grid_dim_x: u32,   // number of cells along each axis (cubic grid)
    grid_dim_y: u32,
    grid_dim_z: u32,
    r_bond2: f32,      // r_bond^2 (creation cutoff, squared)
    class_affinity_dim: u32,
    max_candidates: u32, // per-node candidate slate capacity
    _pad: u32,
};

@group(0) @binding(0) var<storage, read>       positions:   array<vec4<f32>>;
@group(0) @binding(1) var<storage, read>       node_class:  array<u32>;
@group(0) @binding(2) var<uniform>             params:      BondParams;
// Per-node cell hash (linear cell id), written by calc_hash.
@group(0) @binding(3) var<storage, read_write> cell_hash:   array<u32>;
// sorted_nodes[s] = node id at sorted position s (counting-sorted by cell, host).
@group(0) @binding(4) var<storage, read>       sorted_nodes: array<u32>;
// cell_start[c] / cell_end[c] = [start, end) range into sorted_nodes for cell c.
@group(0) @binding(5) var<storage, read>       cell_start:  array<u32>;
@group(0) @binding(6) var<storage, read>       cell_end:    array<u32>;
@group(0) @binding(7) var<storage, read>       class_affinity: array<f32>;
// Per-node candidate slate: cand[i*max_candidates + k] = partner j (j>i) or
// 0xFFFFFFFF for an empty slot. cand_count[i] = number of valid entries.
@group(0) @binding(8) var<storage, read_write> cand:        array<u32>;
@group(0) @binding(9) var<storage, read_write> cand_count:  array<u32>;

const EMPTY: u32 = 0xFFFFFFFFu;

fn cell_coord(p: vec3<f32>) -> vec3<i32> {
    let cx = i32(floor((p.x - params.grid_min_x) * params.inv_cell));
    let cy = i32(floor((p.y - params.grid_min_y) * params.inv_cell));
    let cz = i32(floor((p.z - params.grid_min_z) * params.inv_cell));
    return vec3<i32>(cx, cy, cz);
}

fn clamp_coord(c: vec3<i32>) -> vec3<i32> {
    return vec3<i32>(
        clamp(c.x, 0, i32(params.grid_dim_x) - 1),
        clamp(c.y, 0, i32(params.grid_dim_y) - 1),
        clamp(c.z, 0, i32(params.grid_dim_z) - 1),
    );
}

fn linear_cell(c: vec3<i32>) -> u32 {
    let cc = clamp_coord(c);
    return u32(cc.x)
        + u32(cc.y) * params.grid_dim_x
        + u32(cc.z) * params.grid_dim_x * params.grid_dim_y;
}

fn bond_compatible(ci: u32, cj: u32) -> bool {
    let dim = params.class_affinity_dim;
    if (dim == 0u) {
        return true;
    }
    if (ci >= dim || cj >= dim) {
        return false;
    }
    return class_affinity[ci * dim + cj] > 0.0;
}

// ---- 1. hash each node into its linear cell id -----------------------------
@compute @workgroup_size(64)
fn calc_hash(@builtin(global_invocation_id) gid: vec3<u32>,
             @builtin(num_workgroups) nwg: vec3<u32>) {
    // 2-D-tiled dispatch: linear node index (host tiles into y past the 65535
    // per-dim cap, so the lipid sim scales past ~4.2M particles).
    let i = gid.y * nwg.x * 64u + gid.x;
    if (i >= params.n_nodes) { return; }
    cell_hash[i] = linear_cell(cell_coord(positions[i].xyz));
}

// ---- 3. candidate scan over the 27 neighbour cells -------------------------
// (Step 2, the counting sort + find_cell_start, is done host-side from the
//  hashes; cell_start/cell_end/sorted_nodes arrive as inputs.)
@compute @workgroup_size(64)
fn scan_candidates(@builtin(global_invocation_id) gid: vec3<u32>,
                   @builtin(num_workgroups) nwg: vec3<u32>) {
    // 2-D-tiled dispatch: linear node index (see calc_hash).
    let i = gid.y * nwg.x * 64u + gid.x;
    if (i >= params.n_nodes) { return; }

    let pi = positions[i].xyz;
    let ci = node_class[i];
    let base = i * params.max_candidates;
    var count: u32 = 0u;
    let ic = cell_coord(pi);

    for (var dz: i32 = -1; dz <= 1; dz = dz + 1) {
        for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
            for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
                let nc = vec3<i32>(ic.x + dx, ic.y + dy, ic.z + dz);
                // Skip out-of-grid neighbour cells (no wraparound).
                if (nc.x < 0 || nc.y < 0 || nc.z < 0
                    || nc.x >= i32(params.grid_dim_x)
                    || nc.y >= i32(params.grid_dim_y)
                    || nc.z >= i32(params.grid_dim_z)) {
                    continue;
                }
                let cell = linear_cell(nc);
                let s = cell_start[cell];
                let e = cell_end[cell];
                for (var t: u32 = s; t < e; t = t + 1u) {
                    let j = sorted_nodes[t];
                    if (j <= i) { continue; }          // j>i dedupes the pair
                    if (!bond_compatible(ci, node_class[j])) { continue; }
                    let d = positions[j].xyz - pi;
                    if (dot(d, d) <= params.r_bond2) {
                        if (count < params.max_candidates) {
                            cand[base + count] = j;
                            count = count + 1u;
                        }
                    }
                }
            }
        }
    }
    cand_count[i] = count;
}

// ---- GPU dynamic-edge spring (relaxes a bonded config) ---------------------
// A standalone harmonic-spring kernel over the compacted dynamic-edge buffer,
// used by the equivalence gate to relax a seeded bond config on GPU the same way
// the CPU `accumulate_edge_forces` + `integrate` does. One thread per node; each
// thread walks its own bonds via a per-node bond CSR (offsets + flat partner
// list) and accumulates the spring force, then integrates (damped, no thermostat
// — the equivalence gate runs at T=0 for determinism).

struct SpringParams {
    n_nodes: u32,
    bond_stiffness: f32,
    rest_len: f32,       // dynamic-bond rest length (= r_bond)
    time_step: f32,
    damping: f32,
    max_step: f32,
    _p0: u32,
    _p1: u32,
};

@group(0) @binding(0) var<storage, read_write> sp_positions:  array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> sp_velocities: array<vec4<f32>>;
@group(0) @binding(2) var<uniform>             sp_params:     SpringParams;
// Per-node bond adjacency: bond_off[i]..bond_off[i+1] index into bond_adj.
@group(0) @binding(3) var<storage, read>       bond_off:      array<u32>;
@group(0) @binding(4) var<storage, read>       bond_adj:      array<u32>;

@compute @workgroup_size(64)
fn spring_step(@builtin(global_invocation_id) gid: vec3<u32>,
               @builtin(num_workgroups) nwg: vec3<u32>) {
    // 2-D-tiled dispatch: linear node index (see calc_hash).
    let i = gid.y * nwg.x * 64u + gid.x;
    if (i >= sp_params.n_nodes) { return; }
    let pi = sp_positions[i].xyz;
    var force = vec3<f32>(0.0, 0.0, 0.0);
    let beg = bond_off[i];
    let end = bond_off[i + 1u];
    for (var a: u32 = beg; a < end; a = a + 1u) {
        let j = bond_adj[a];
        let d = sp_positions[j].xyz - pi;
        let r = max(length(d), 1e-6);
        let f = sp_params.bond_stiffness * (r - sp_params.rest_len) / r;
        force = force + d * f;
    }
    var vel = sp_velocities[i].xyz;
    vel = (vel + force * sp_params.time_step) * sp_params.damping;
    var disp = vel * sp_params.time_step;
    if (sp_params.max_step > 0.0 && length(disp) > sp_params.max_step) {
        disp = normalize(disp) * sp_params.max_step;
        vel = disp / sp_params.time_step;
    }
    sp_velocities[i] = vec4<f32>(vel, 0.0);
    sp_positions[i] = vec4<f32>(pi + disp, 0.0);
}
