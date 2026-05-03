// 3D force-directed layout compute shader.
//
// One dispatch = one simulation step. Repulsion is bounded by a uniform
// 3D voxel grid: each thread visits the 27 neighboring cells of its cell
// and only does pairwise work against those occupants — O(n) with bounded
// per-cell occupancy. Spring forces are O(degree) via CSR adjacency
// (edge_offsets / edge_neighbors). Integration is semi-implicit Euler with
// per-node mass and velocity damping.
//
// Bindings (force_step pipeline):
//   @group(0) @binding(0) positions_in       (read)
//   @group(0) @binding(1) positions_out      (read_write)
//   @group(0) @binding(2) velocities         (read_write)
//   @group(0) @binding(3) edge_offsets       (read)   length n+1
//   @group(0) @binding(4) edge_neighbors     (read)   length 2*m
//   @group(0) @binding(5) params             (uniform)
//   @group(0) @binding(6) cell_offsets       (read)   length n_cells+1
//   @group(0) @binding(7) cell_nodes         (read)   length n
//   @group(0) @binding(8) mass               (read)   length n
//   @group(0) @binding(9) energy_out         (read_write) length n  (max disp proxy)
//
// Bindings (grid build pipelines: clear_cell_counts / count_cells /
// scan_cell_offsets / scatter_cells). All four entry points share a single
// bind group layout so they can be dispatched against one bind group:
//   @group(1) @binding(0) gb_positions_in    (read)            length n
//   @group(1) @binding(1) gb_params          (uniform)
//   @group(1) @binding(2) gb_cell_counts     (atomic<u32>)     length n_cells
//   @group(1) @binding(3) gb_cell_cursor     (atomic<u32>)     length n_cells
//   @group(1) @binding(4) gb_cell_offsets    (read_write u32)  length n_cells+1
//   @group(1) @binding(5) gb_cell_nodes      (read_write u32)  length n

struct SimParams {
    repulsion: f32,
    spring_k: f32,
    spring_len: f32,
    gravity: f32,
    damping: f32,
    dt: f32,
    cursor_radius: f32,
    cursor_strength: f32,
    cursor_pos: vec3<f32>,
    n_nodes: u32,
    n_edges: u32,
    repulsion_radius: f32,
    grid_cell_size: f32,
    grid_enabled: u32,        // 0 = naive O(n^2), 1 = grid
    grid_origin: vec3<f32>,
    n_cells: u32,
    grid_dim: vec3<u32>,
    _pad0: u32,
};

@group(0) @binding(0) var<storage, read>       positions_in:    array<vec3<f32>>;
@group(0) @binding(1) var<storage, read_write> positions_out:   array<vec3<f32>>;
@group(0) @binding(2) var<storage, read_write> velocities:      array<vec3<f32>>;
@group(0) @binding(3) var<storage, read>       edge_offsets:    array<u32>;
@group(0) @binding(4) var<storage, read>       edge_neighbors:  array<u32>;
@group(0) @binding(5) var<uniform>             params:          SimParams;
@group(0) @binding(6) var<storage, read>       cell_offsets:    array<u32>;
@group(0) @binding(7) var<storage, read>       cell_nodes:      array<u32>;
@group(0) @binding(8) var<storage, read>       mass:            array<f32>;
@group(0) @binding(9) var<storage, read_write> energy_out:      array<f32>;

// ---- Grid-build bindings (group 1) -----------------------------------------
@group(1) @binding(0) var<storage, read>       gb_positions_in: array<vec3<f32>>;
@group(1) @binding(1) var<uniform>             gb_params:       SimParams;
@group(1) @binding(2) var<storage, read_write> gb_cell_counts:  array<atomic<u32>>;
@group(1) @binding(3) var<storage, read_write> gb_cell_cursor:  array<atomic<u32>>;
@group(1) @binding(4) var<storage, read_write> gb_cell_offsets: array<u32>;
@group(1) @binding(5) var<storage, read_write> gb_cell_nodes:   array<u32>;

fn gb_cell_for(pos: vec3<f32>) -> u32 {
    let inv = 1.0 / gb_params.grid_cell_size;
    let rel = (pos - gb_params.grid_origin) * inv;
    let dim_x = i32(gb_params.grid_dim.x);
    let dim_y = i32(gb_params.grid_dim.y);
    let dim_z = i32(gb_params.grid_dim.z);
    var ix = i32(floor(rel.x));
    var iy = i32(floor(rel.y));
    var iz = i32(floor(rel.z));
    if (ix < 0) { ix = 0; } else if (ix >= dim_x) { ix = dim_x - 1; }
    if (iy < 0) { iy = 0; } else if (iy >= dim_y) { iy = dim_y - 1; }
    if (iz < 0) { iz = 0; } else if (iz >= dim_z) { iz = dim_z - 1; }
    return u32(ix)
        + u32(iy) * gb_params.grid_dim.x
        + u32(iz) * gb_params.grid_dim.x * gb_params.grid_dim.y;
}

@compute @workgroup_size(64)
fn clear_cell_counts(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= gb_params.n_cells) { return; }
    atomicStore(&gb_cell_counts[i], 0u);
    atomicStore(&gb_cell_cursor[i], 0u);
    // cell_offsets has length n_cells+1; clear the tail at thread 0.
    if (i == 0u) {
        gb_cell_offsets[gb_params.n_cells] = 0u;
    }
    gb_cell_offsets[i] = 0u;
}

@compute @workgroup_size(64)
fn count_cells(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= gb_params.n_nodes) { return; }
    let cell = gb_cell_for(gb_positions_in[i]);
    atomicAdd(&gb_cell_counts[cell], 1u);
}

// Single-workgroup, single-thread sequential exclusive prefix sum over
// cell_counts → cell_offsets. n_cells is bounded (≤ 64^3 = 262144) so this
// is fine: ~0.1ms for the worst case, dominated by memory ops not compute.
// We do this on one thread to avoid a multi-pass GPU scan; the alternative
// (CPU readback + upload) would force a mid-frame submit + map_async.
@compute @workgroup_size(1)
fn scan_cell_offsets(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x != 0u) { return; }
    var acc: u32 = 0u;
    let n = gb_params.n_cells;
    for (var c: u32 = 0u; c < n; c = c + 1u) {
        gb_cell_offsets[c] = acc;
        acc = acc + atomicLoad(&gb_cell_counts[c]);
    }
    gb_cell_offsets[n] = acc;
}

@compute @workgroup_size(64)
fn scatter_cells(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= gb_params.n_nodes) { return; }
    let cell = gb_cell_for(gb_positions_in[i]);
    let slot = atomicAdd(&gb_cell_cursor[cell], 1u);
    gb_cell_nodes[gb_cell_offsets[cell] + slot] = i;
}

@compute @workgroup_size(64)
fn force_step(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.n_nodes) {
        return;
    }

    let pos = positions_in[i];
    var vel = velocities[i];
    var force = vec3<f32>(0.0, 0.0, 0.0);

    let r_clip = params.repulsion_radius;
    // Use a "very large" finite value as the no-clip sentinel. f32::MAX
    // (3.4028235e+38) overflows naga's WGSL constant parser, so use a value
    // far larger than any plausible distance² instead.
    let r_clip2 = select(1.0e+18, r_clip * r_clip, r_clip > 0.0);

    // ---- Repulsion ---------------------------------------------------------
    if (params.grid_enabled == 1u) {
        // Walk 27 neighbor cells.
        let inv_cell = 1.0 / params.grid_cell_size;
        let rel = (pos - params.grid_origin) * inv_cell;
        let cx = i32(floor(rel.x));
        let cy = i32(floor(rel.y));
        let cz = i32(floor(rel.z));
        let dim_x = i32(params.grid_dim.x);
        let dim_y = i32(params.grid_dim.y);
        let dim_z = i32(params.grid_dim.z);
        for (var dz: i32 = -1; dz <= 1; dz = dz + 1) {
            let nz = cz + dz;
            if (nz < 0 || nz >= dim_z) { continue; }
            for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
                let ny = cy + dy;
                if (ny < 0 || ny >= dim_y) { continue; }
                for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
                    let nx = cx + dx;
                    if (nx < 0 || nx >= dim_x) { continue; }
                    let cell_idx = u32(nx) + u32(ny) * params.grid_dim.x
                        + u32(nz) * params.grid_dim.x * params.grid_dim.y;
                    let start = cell_offsets[cell_idx];
                    let end   = cell_offsets[cell_idx + 1u];
                    for (var k: u32 = start; k < end; k = k + 1u) {
                        let j = cell_nodes[k];
                        if (j == i) { continue; }
                        let d = pos - positions_in[j];
                        let dist2 = dot(d, d);
                        if (dist2 > r_clip2) { continue; }
                        let dist2c = max(dist2, 0.01);
                        force = force + d * (params.repulsion * mass[j] / dist2c);
                    }
                }
            }
        }
    } else {
        for (var j: u32 = 0u; j < params.n_nodes; j = j + 1u) {
            if (j == i) { continue; }
            let d = pos - positions_in[j];
            let dist2 = dot(d, d);
            if (dist2 > r_clip2) { continue; }
            let dist2c = max(dist2, 0.01);
            force = force + d * (params.repulsion * mass[j] / dist2c);
        }
    }

    // ---- Springs (O(degree)) -----------------------------------------------
    let start = edge_offsets[i];
    let end   = edge_offsets[i + 1u];
    for (var k: u32 = start; k < end; k = k + 1u) {
        let other = edge_neighbors[k];
        let d = positions_in[other] - pos;
        let dist = max(length(d), 0.01);
        let stretch = dist - params.spring_len;
        force = force + (d / dist) * (params.spring_k * stretch);
    }

    // ---- Gravity towards origin --------------------------------------------
    force = force - pos * params.gravity;

    // ---- Cursor force (radial, falloff to 0 at radius) ---------------------
    if (params.cursor_radius > 0.0) {
        let cd = pos - params.cursor_pos;
        let cdist = max(length(cd), 0.01);
        if (cdist < params.cursor_radius) {
            let falloff = 1.0 - (cdist / params.cursor_radius);
            force = force + (cd / cdist) * (params.cursor_strength * falloff);
        }
    }

    // ---- Integrate (per-node mass) -----------------------------------------
    let m = max(mass[i], 1.0);
    let accel = force / m;
    vel = (vel + accel * params.dt) * params.damping;
    let new_pos = pos + vel * params.dt;

    velocities[i] = vel;
    positions_out[i] = new_pos;

    // Track per-node KE proxy = |vel|^2. CPU reduces.
    energy_out[i] = dot(vel, vel);
}
