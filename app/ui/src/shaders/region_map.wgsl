// Region-map underlay shader — GMap-style cluster regions computed
// entirely on the GPU (Gansner, Hu, Kobourov, "GMap: Drawing Graphs as
// Maps", 2010). The plane is tiled into per-node Voronoi cells over a
// fixed GRID_SIZE x GRID_SIZE screen-space grid via jump-flooding
// (Rong & Tan, I3D 2006). Cells sharing a cluster id merge into a
// region; cells farther than `radius_cells` from every node become
// ocean (transparent). Cost is independent of node count except for the
// single O(n) seed pass.
//
// Pipeline stages (each a SEPARATE compute pass so the implicit
// inter-pass barrier orders reads-after-writes; dispatches within one
// compute pass may overlap in WebGPU and must not depend on each other):
//   region_clear  one lane per cell   -> grid_a set to EMPTY
//   region_seed   one lane per node   -> project + atomicMin into grid_a
//   region_jfa    one lane per cell   -> ping-pong a<->b for each step
//   region_prune  one lane per cell   -> ocean past radius_cells (grid_a)
//   region_vs/fs  fullscreen triangle -> paint regions + outlines
//
// Grid storage choice: WGSL forbids `vec<atomic<u32>>`, so the packed
// seed and the cluster id are kept in TWO separate `array<atomic<u32>>`
// buffers per grid (grid_*_seed / grid_*_cluster) rather than one
// `array<vec2<u32>>`. This lets `region_seed` resolve same-cell races
// deterministically with `atomicMin` on each component; every other
// stage reads with `atomicLoad` / writes with `atomicStore`. The draw
// fragment shader also reads the grid through `atomicLoad` (read_write
// storage is permitted in the fragment stage).
//
// Packed seed encoding: a seed cell (sx, sy) with sx,sy in [0, GRID) is
// packed as `sx | (sy << 16)`. Valid packed values are <= 0x01FF01FF for
// a 512 grid, always below the EMPTY sentinel 0xFFFFFFFF, so `atomicMin`
// from EMPTY latches the first (and only) coordinate written to a cell.

const EMPTY: u32 = 0xFFFFFFFFu;
const WORKGROUP_SIZE: u32 = 64u;
const BIG_DIST2: u32 = 0x7FFFFFFFu;

// Outline is the region fill colour multiplied by this factor, drawn
// opaque so cluster borders read as hard coastlines over the fill.
const OUTLINE_DARKEN: f32 = 0.45;

struct RegionParams {
    grid_size:    u32,
    radius_cells: f32,
    fill_alpha:   f32,
    outline:      u32,
    n_nodes:      u32,
    palette_len:  u32,
    mode:         u32,
    level:        u32,
    n_levels:     u32,
    _pad0:        u32,
    _pad1:        u32,
    _pad2:        u32,
};

// Per-JFA-pass state, bound with a dynamic uniform offset so all passes
// can be recorded into one encoder (a plain uniform written per pass via
// queue.write_buffer would collapse to the last value before the command
// buffer executes).
struct StepParams {
    step:     u32,
    src_is_a: u32,
    _p0:      u32,
    _p1:      u32,
};

// Only the leading view_proj of the shared CameraUniform is read here;
// the full uniform (view, cam_pos, screen, ...) lives in the same buffer
// but is not needed for seed projection.
struct RegionCamera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> params: RegionParams;
@group(0) @binding(1) var<uniform> camera: RegionCamera;
// Shared positions buffer (xyz = world position, w = mass). Seed only.
@group(0) @binding(2) var<storage, read> positions: array<vec4<f32>>;
// Per-node cluster id, level-major: id for node i at level k lives at
// `k * n_nodes + i`. The active level is `params.level`. Seed only.
@group(0) @binding(3) var<storage, read> cluster_ids: array<u32>;
@group(0) @binding(4) var<storage, read_write> grid_a_seed:    array<atomic<u32>>;
@group(0) @binding(5) var<storage, read_write> grid_a_cluster: array<atomic<u32>>;
@group(0) @binding(6) var<storage, read_write> grid_b_seed:    array<atomic<u32>>;
@group(0) @binding(7) var<storage, read_write> grid_b_cluster: array<atomic<u32>>;
// Cluster-id -> RGBA palette (draw only). Length params.palette_len >= 1.
@group(0) @binding(8) var<storage, read> palette: array<vec4<f32>>;

@group(1) @binding(0) var<uniform> step_p: StepParams;

// Linear lane index for a dispatch that may have spilled into Y. Matches
// dispatch_1d on the Rust side: groups laid out row-major with
// num_workgroups.x groups per row.
fn linear_index(gid: vec3<u32>, nwg: vec3<u32>) -> u32 {
    return gid.x + gid.y * nwg.x * WORKGROUP_SIZE;
}

fn cell_count() -> u32 {
    return params.grid_size * params.grid_size;
}

// NDC (y-up, [-1, 1]) -> integer grid cell. Row 0 is the top of the
// screen (+y). Shared by seed and draw so regions overlay exactly where
// the nodes project.
fn ndc_to_cell(ndc: vec2<f32>) -> vec2<u32> {
    let g = f32(params.grid_size);
    let last = params.grid_size - 1u;
    let u = clamp((ndc.x * 0.5 + 0.5) * g, 0.0, g - 1.0);
    let v = clamp((0.5 - ndc.y * 0.5) * g, 0.0, g - 1.0);
    return vec2<u32>(min(u32(u), last), min(u32(v), last));
}

// Squared cell-space distance from cell (cx, cy) to a packed seed.
fn cell_dist2(cx: u32, cy: u32, packed: u32) -> u32 {
    let sx = i32(packed & 0xFFFFu);
    let sy = i32(packed >> 16u);
    let dx = i32(cx) - sx;
    let dy = i32(cy) - sy;
    return u32(dx * dx + dy * dy);
}

@compute @workgroup_size(64)
fn region_clear(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let cell = linear_index(gid, nwg);
    if (cell >= cell_count()) { return; }
    // Only grid_a is cleared; the first JFA pass overwrites every grid_b
    // cell from grid_a, so grid_b needs no initialisation.
    atomicStore(&grid_a_seed[cell], EMPTY);
    atomicStore(&grid_a_cluster[cell], EMPTY);
}

@compute @workgroup_size(64)
fn region_seed(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    // One lane per node; n may exceed 65535*64, so recover the index
    // through the 2-D dispatch spill.
    let i = linear_index(gid, nwg);
    if (i >= params.n_nodes) { return; }

    let world = positions[i].xyz;
    let clip = camera.view_proj * vec4<f32>(world, 1.0);
    // Reject nodes behind the camera (same test as node.wgsl).
    if (clip.w <= 0.0) { return; }
    let ndc = clip.xy / clip.w;
    // The grid only covers the viewport; off-screen nodes seed nothing.
    if (ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0) { return; }

    let cell = ndc_to_cell(ndc);
    let idx = cell.y * params.grid_size + cell.x;
    // A seed cell's nearest seed is itself; store its own coordinate.
    let packed = cell.x | (cell.y << 16u);
    // Every node landing in this cell packs the identical coordinate, so
    // atomicMin on the seed only latches "occupied". The cluster winner
    // is made deterministic under races by taking the smallest id.
    atomicMin(&grid_a_seed[idx], packed);
    atomicMin(&grid_a_cluster[idx], cluster_ids[params.level * params.n_nodes + i]);
}

fn load_seed(src_is_a: u32, idx: u32) -> u32 {
    if (src_is_a == 1u) { return atomicLoad(&grid_a_seed[idx]); }
    return atomicLoad(&grid_b_seed[idx]);
}
fn load_cluster(src_is_a: u32, idx: u32) -> u32 {
    if (src_is_a == 1u) { return atomicLoad(&grid_a_cluster[idx]); }
    return atomicLoad(&grid_b_cluster[idx]);
}
// Store into the destination grid, i.e. the one that is NOT the source.
fn store_dst(src_is_a: u32, idx: u32, seed: u32, cluster: u32) {
    if (src_is_a == 1u) {
        atomicStore(&grid_b_seed[idx], seed);
        atomicStore(&grid_b_cluster[idx], cluster);
    } else {
        atomicStore(&grid_a_seed[idx], seed);
        atomicStore(&grid_a_cluster[idx], cluster);
    }
}

@compute @workgroup_size(64)
fn region_jfa(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let cell = linear_index(gid, nwg);
    if (cell >= cell_count()) { return; }
    let grid = params.grid_size;
    let cx = cell % grid;
    let cy = cell / grid;
    let src = step_p.src_is_a;
    let step = i32(step_p.step);

    // Seed the best with this cell's current source value.
    var best_seed = load_seed(src, cell);
    var best_cluster = load_cluster(src, cell);
    var best_d2 = BIG_DIST2;
    if (best_seed != EMPTY) {
        best_d2 = cell_dist2(cx, cy, best_seed);
    }

    // Examine the 8 neighbours at +/- step.
    for (var oy: i32 = -1; oy <= 1; oy = oy + 1) {
        for (var ox: i32 = -1; ox <= 1; ox = ox + 1) {
            if (ox == 0 && oy == 0) { continue; }
            let nx = i32(cx) + ox * step;
            let ny = i32(cy) + oy * step;
            if (nx < 0 || ny < 0 || nx >= i32(grid) || ny >= i32(grid)) { continue; }
            let nidx = u32(ny) * grid + u32(nx);
            let ns = load_seed(src, nidx);
            if (ns == EMPTY) { continue; }
            let d2 = cell_dist2(cx, cy, ns);
            // Nearest wins; ties broken by smaller packed seed so the
            // result is deterministic regardless of neighbour order.
            if (d2 < best_d2 || (d2 == best_d2 && ns < best_seed)) {
                best_d2 = d2;
                best_seed = ns;
                best_cluster = load_cluster(src, nidx);
            }
        }
    }

    store_dst(src, cell, best_seed, best_cluster);
}

@compute @workgroup_size(64)
fn region_prune(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    // Runs on grid_a (the final ping-pong result). Cells whose nearest
    // seed is farther than radius_cells become ocean.
    let cell = linear_index(gid, nwg);
    if (cell >= cell_count()) { return; }
    let s = atomicLoad(&grid_a_seed[cell]);
    if (s == EMPTY) { return; }
    let grid = params.grid_size;
    let cx = cell % grid;
    let cy = cell / grid;
    let d2 = f32(cell_dist2(cx, cy, s));
    let r = params.radius_cells;
    if (d2 > r * r) {
        atomicStore(&grid_a_seed[cell], EMPTY);
        atomicStore(&grid_a_cluster[cell], EMPTY);
    }
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn region_vs(@builtin(vertex_index) vid: u32) -> VsOut {
    // Single oversized triangle covering the viewport.
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let c = corners[vid];
    var out: VsOut;
    out.pos = vec4<f32>(c, 0.0, 1.0);
    out.ndc = c;
    return out;
}

@fragment
fn region_fs(in: VsOut) -> @location(0) vec4<f32> {
    let grid = params.grid_size;
    let cell = ndc_to_cell(in.ndc);
    let cx = cell.x;
    let cy = cell.y;
    let idx = cy * grid + cx;

    let s = atomicLoad(&grid_a_seed[idx]);
    if (s == EMPTY) {
        // Ocean: transparent, let the cleared background show through.
        discard;
    }
    let plen = max(params.palette_len, 1u);
    let id = atomicLoad(&grid_a_cluster[idx]);
    let base = palette[id % plen].rgb;

    if (params.outline != 0u) {
        // 4-neighbour boundary test. A cell abutting ocean or a
        // different cluster is a coastline; in-bounds neighbours only
        // (screen-edge cells draw no spurious outline).
        var border = false;
        if (cx > 0u) {
            let n = cy * grid + (cx - 1u);
            if (atomicLoad(&grid_a_seed[n]) == EMPTY || atomicLoad(&grid_a_cluster[n]) != id) { border = true; }
        }
        if (cx + 1u < grid) {
            let n = cy * grid + (cx + 1u);
            if (atomicLoad(&grid_a_seed[n]) == EMPTY || atomicLoad(&grid_a_cluster[n]) != id) { border = true; }
        }
        if (cy > 0u) {
            let n = (cy - 1u) * grid + cx;
            if (atomicLoad(&grid_a_seed[n]) == EMPTY || atomicLoad(&grid_a_cluster[n]) != id) { border = true; }
        }
        if (cy + 1u < grid) {
            let n = (cy + 1u) * grid + cx;
            if (atomicLoad(&grid_a_seed[n]) == EMPTY || atomicLoad(&grid_a_cluster[n]) != id) { border = true; }
        }
        if (border) {
            return vec4<f32>(base * OUTLINE_DARKEN, 1.0);
        }
    }

    return vec4<f32>(base, params.fill_alpha);
}
