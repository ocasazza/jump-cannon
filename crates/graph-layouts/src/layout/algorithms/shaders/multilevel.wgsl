// Fully-GPU multilevel coarsening seed.
//
// Given a fine CSR graph on the device, this pipeline builds a cascade of
// coarser graphs by heavy-edge matching (HEM) edge contraction, lays out the
// coarsest level, and prolongs positions back down level by level. The host
// never reads a single count back: every dispatch runs over a host-known
// worst-case *capacity* and guards its lanes against device-computed counts
// read from a small per-level `meta` buffer. Coarse node / edge indices are
// clamped to their level capacity so a pathological graph that fails to
// coarsen at the assumed geometric ratio degrades the seed rather than
// overflowing a buffer.
//
// The sort and scan kernels below mirror `octree.wgsl` deliberately: the
// octree's radix sort and exclusive scans are bound hard to the octree's own
// scratch buffers (fixed sizes, fixed bind groups), so reusing them for the
// coarse-edge sort — a different element count keyed on a different value —
// would have required growing the octree module with generic entry points.
// Per the task's fallback, the shared sort/scan kernels live here instead,
// re-declared with this module's own bindings; the octree pipelines are left
// untouched.
//
// No f32 atomics (coarse mass is summed as 16.16 fixed-point in a u32 atomic;
// mass = 1 + log2(deg) so the fractional part fits 16 bits without loss at the
// magnitudes involved). Every kernel is 64 lanes wide and recovers its lane
// index through `linear_index`, so a dispatch may spill into the Y dimension.

const WORKGROUP_SIZE: u32 = 64u;
const SENTINEL: u32 = 0xFFFFFFFFu;
const UNMATCHED: u32 = 0xFFFFFFFFu;
const SELF_PROP: u32 = 0xFFFFFFFFu;
const HUB: u32 = 32u;

// ---- Group 0: storage pool -------------------------------------------------
//
// A generous module-global binding namespace; each pipeline's bind-group
// layout names only the subset its entry point statically references, and
// different pipelines may bind the same slot to different buffers (the octree
// pattern). Roles are documented per binding.
@group(0) @binding(0)  var<storage, read>        f_off:      array<u32>;            // finer CSR offsets (len n_fine+1)
@group(0) @binding(1)  var<storage, read>        f_neigh:    array<u32>;            // finer CSR neighbours (len f_slots)
@group(0) @binding(2)  var<storage, read>        f_ewt:      array<f32>;            // finer edge weights (len f_slots)
@group(0) @binding(3)  var<storage, read>        f_pos:      array<vec4<f32>>;      // finer positions (xyz + mass in .w)
@group(0) @binding(4)  var<storage, read_write>  matched:    array<u32>;            // per finer node: UNMATCHED or rep=min(i,j)
@group(0) @binding(5)  var<storage, read_write>  propose:    array<u32>;            // per finer node: proposed partner or SELF_PROP
@group(0) @binding(6)  var<storage, read_write>  parent:     array<u32>;            // per finer node: coarse index (this cascade step)
@group(0) @binding(7)  var<storage, read>        f_meta:     array<u32>;            // finer meta [nc, mc, slots, _]
@group(0) @binding(8)  var<storage, read_write>  slot_row:   array<u32>;            // per finer CSR slot: owning row id
@group(0) @binding(9)  var<storage, read>        c_off:      array<u32>;            // coarse CSR offsets (read)
@group(0) @binding(10) var<storage, read_write>  c_neigh:    array<u32>;            // coarse CSR neighbours
@group(0) @binding(11) var<storage, read_write>  c_ewt:      array<f32>;            // coarse edge weights
@group(0) @binding(12) var<storage, read_write>  c_pos:      array<vec4<f32>>;      // coarse positions (xyz + mass in .w)
@group(0) @binding(13) var<storage, read_write>  c_cursor:   array<atomic<u32>>;    // atomic fill cursor (init = c_off)
@group(0) @binding(14) var<storage, read_write>  c_mass:     array<atomic<u32>>;    // coarse mass, 16.16 fixed point
@group(0) @binding(15) var<storage, read_write>  c_meta:     array<u32>;            // coarse meta [nc, mc, slots, _]
@group(0) @binding(16) var<storage, read_write>  edge_min:   array<u32>;            // per unique coarse edge: min endpoint
@group(0) @binding(17) var<storage, read_write>  edge_max:   array<u32>;            // per unique coarse edge: max endpoint
@group(0) @binding(18) var<storage, read_write>  edge_wt:    array<atomic<u32>>;    // per unique coarse edge: multiplicity
@group(0) @binding(19) var<storage, read_write>  wbuf:       array<u32>;            // scan work buffer (rep flags / uniq flags -> scanned)
@group(0) @binding(20) var<storage, read_write>  scan_data:  array<u32>;            // generic scan target (bound = the array being scanned)
@group(0) @binding(21) var<storage, read_write>  scan_bs:    array<u32>;            // generic scan block sums
@group(0) @binding(22) var<storage, read_write>  sk:         array<u32>;            // radix sort key (src side)
@group(0) @binding(23) var<storage, read_write>  sk_alt:     array<u32>;            // radix sort key (dst side)
@group(0) @binding(24) var<storage, read_write>  sv:         array<u32>;            // radix sort payload (src side)
@group(0) @binding(25) var<storage, read_write>  sv_alt:     array<u32>;            // radix sort payload (dst side)
@group(0) @binding(26) var<storage, read_write>  histogram:  array<u32>;            // radix per-(digit,block) histogram
@group(0) @binding(27) var<storage, read_write>  virt_csr:   array<u32>;            // packed node_to_virt_offsets(nc+1) ++ virt_real_idx
@group(0) @binding(28) var<storage, read_write>  virt_eoff:  array<u32>;            // per virtual vertex: CSR edge slice offsets
@group(0) @binding(29) var<storage, read_write>  c_off_atom: array<atomic<u32>>;    // coarse degrees, atomic view of the off buffer
@group(0) @binding(30) var<storage, read>        pos_src:    array<vec4<f32>>;      // coarser positions (prolong source)

// ---- Group 1: uniforms -----------------------------------------------------

struct MlParams {
    n_fine_cap:      u32,   // finer node dispatch cap
    slots_fine_cap:  u32,   // finer directed-slot dispatch cap (== sort_n)
    nc_cap:          u32,   // this level's node capacity
    mc_cap:          u32,   // this level's undirected-edge capacity

    round:           u32,   // matching round (0..2), for tie-break hashing
    seed:            u32,   // rng salt
    level:           u32,   // level index from fine (0 = fine)
    _pad0:           u32,

    spring_len:      f32,   // world unit; jitter = 0.5 * spring_len
    radius:          f32,   // coarsest ball radius
    mass_scale:      f32,   // fixed-point scale (65536.0)
    _pad1:           f32,

    sort_nblocks:    u32,   // ceil(slots_fine_cap / 64), radix histogram stride
    _pad2:           u32,
    _pad3:           u32,
    _pad4:           u32,
};

struct ScanDims {
    len:     u32,
    nblocks: u32,
    _pad0:   u32,
    _pad1:   u32,
};

// SimParams mirrors force.wgsl field-for-field so `spring_step_weighted` can
// bind the same coarse params uniform the reused force_step pipeline reads.
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
    repulsion_mode: u32,
    bh_theta: f32,

    n_octree: u32,
    repulsion_samples: u32,
    step_index: u32,
    force_model: u32,

    tfdp_alpha: f32,
    tfdp_beta: f32,
    tfdp_gamma: f32,
    tfdp_k: f32,
};

@group(1) @binding(0) var<uniform> params:     MlParams;
@group(1) @binding(1) var<uniform> scan_dims:  ScanDims;
@group(1) @binding(2) var<uniform> sp_params:  SimParams;

// Current radix digit shift (0, 4, ... 28), baked per pipeline like octree.
override PASS_SHIFT: u32 = 0u;

fn linear_index(gid: vec3<u32>, nwg: vec3<u32>) -> u32 {
    return gid.x + gid.y * nwg.x * WORKGROUP_SIZE;
}
fn block_index(wid: vec3<u32>, nwg: vec3<u32>) -> u32 {
    return wid.x + wid.y * nwg.x;
}

// PCG output hash (pcg-random.org): one mul + xorshift, stateless.
fn pcg_hash(state: u32) -> u32 {
    let s = state * 747796405u + 2891336453u;
    let word = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (word >> 22u) ^ word;
}
fn hashf(x: u32) -> f32 {
    return f32(pcg_hash(x)) / 4294967295.0;
}

// ---- Finer-count helpers (read device meta, no host readback) --------------
fn n_fine() -> u32 { return f_meta[0]; }
fn f_slots() -> u32 { return f_meta[2]; }
fn nc() -> u32 { return c_meta[0]; }
fn mc() -> u32 { return c_meta[1]; }

// ---- Setup fills -----------------------------------------------------------

@compute @workgroup_size(64)
fn init_matched(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= params.n_fine_cap) { return; }
    matched[i] = UNMATCHED;
}

// Fill f_ewt (bound here as writable via the sk slot? no) — dedicated fill of
// the fine level's unit weights. Bound: writes the coarse-weight buffer slot.
@compute @workgroup_size(64)
fn fill_ones(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= params.slots_fine_cap) { return; }
    c_ewt[i] = 1.0;
}

// ---- Heavy-edge matching ---------------------------------------------------

@compute @workgroup_size(64)
fn hem_propose(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= n_fine()) { return; }
    if (matched[i] != UNMATCHED) { propose[i] = SELF_PROP; return; }
    let start = f_off[i];
    let end = f_off[i + 1u];
    var best_j: u32 = SELF_PROP;
    var best_w: f32 = -1.0;
    var best_h: u32 = 0u;
    for (var k = start; k < end; k = k + 1u) {
        let j = f_neigh[k];
        if (j == i) { continue; }
        if (matched[j] != UNMATCHED) { continue; }
        let w = f_ewt[k];
        let lo = min(i, j);
        let hi = max(i, j);
        let h = pcg_hash(lo * 0x9E3779B9u ^ (hi * 0x85EBCA6Bu) ^ (params.round * 0xC2B2AE35u) ^ params.seed);
        if (w > best_w || (w == best_w && h > best_h)) {
            best_w = w;
            best_h = h;
            best_j = j;
        }
    }
    propose[i] = best_j;
}

@compute @workgroup_size(64)
fn hem_match(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= n_fine()) { return; }
    if (matched[i] != UNMATCHED) { return; }
    let j = propose[i];
    if (j == SELF_PROP) { return; }
    if (propose[j] == i) {
        matched[i] = min(i, j);
    }
}

// rep flag = 1 for representatives (the min of a matched pair) and for
// unmatched singletons. Written into the scan work buffer (pre-zeroed).
@compute @workgroup_size(64)
fn rep_flag(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= n_fine()) { return; }
    let m = matched[i];
    if (m == UNMATCHED || m == i) {
        wbuf[i] = 1u;
    }
}

@compute @workgroup_size(64)
fn write_nc(@builtin(local_invocation_id) lid: vec3<u32>) {
    if (lid.x != 0u) { return; }
    // wbuf has been exclusive-scanned; wbuf[n_fine] is the total rep count.
    c_meta[0] = min(wbuf[n_fine()], params.nc_cap);
}

@compute @workgroup_size(64)
fn assign_parent(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= n_fine()) { return; }
    let m = matched[i];
    var cidx: u32;
    if (m == UNMATCHED || m == i) {
        cidx = wbuf[i];
    } else {
        cidx = wbuf[m];
    }
    if (cidx >= params.nc_cap) { cidx = params.nc_cap - 1u; }
    parent[i] = cidx;
}

// ---- Coarse mass -----------------------------------------------------------

@compute @workgroup_size(64)
fn mass_accum(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= n_fine()) { return; }
    let p = parent[i];
    if (p >= params.nc_cap) { return; }
    let m = f_pos[i].w;
    atomicAdd(&c_mass[p], u32(m * params.mass_scale));
}

// c_pos[i] = (0, 0, 0, coarse_mass). xyz is set later by seed_ball / prolong,
// which preserve the .w mass this kernel writes.
@compute @workgroup_size(64)
fn place_mass(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= nc()) { return; }
    let m = f32(atomicLoad(&c_mass[i])) / params.mass_scale;
    c_pos[i] = vec4<f32>(0.0, 0.0, 0.0, m);
}

// ---- Coarse edge expansion -------------------------------------------------

// One lane per finer node writes its row id across its CSR slot range.
@compute @workgroup_size(64)
fn row_of_slot(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= n_fine()) { return; }
    let start = f_off[i];
    let end = f_off[i + 1u];
    for (var k = start; k < end; k = k + 1u) {
        slot_row[k] = i;
    }
}

// One lane per finer directed slot: emit the (min,max) coarse endpoint pair
// keyed for the two-pass sort. sk = max (primary sort field), sv = min. Self
// pairs (same parent) and padding slots become SENTINEL and sort to the end.
@compute @workgroup_size(64)
fn expand_pairs(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let slot = linear_index(gid, nwg);
    if (slot >= params.slots_fine_cap) { return; }
    if (slot >= f_slots()) {
        sk[slot] = SENTINEL;
        sv[slot] = SENTINEL;
        return;
    }
    let row = slot_row[slot];
    let col = f_neigh[slot];
    let ps = parent[row];
    let pt = parent[col];
    if (ps == pt) {
        sk[slot] = SENTINEL;
        sv[slot] = SENTINEL;
        return;
    }
    sk[slot] = max(ps, pt);
    sv[slot] = min(ps, pt);
}

// ---- Radix sort (stable LSD, 4-bit digits) ---------------------------------

var<workgroup> h_dig: array<u32, 64>;

@compute @workgroup_size(64)
fn radix_histogram(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let g = block_index(wid, nwg);
    let idx = g * 64u + lid.x;
    var d = 16u;
    if (idx < params.slots_fine_cap) { d = (sk[idx] >> PASS_SHIFT) & 0xFu; }
    h_dig[lid.x] = d;
    workgroupBarrier();
    if (lid.x < 16u && g < params.sort_nblocks) {
        var c = 0u;
        for (var k = 0u; k < 64u; k = k + 1u) {
            if (h_dig[k] == lid.x) { c = c + 1u; }
        }
        histogram[lid.x * params.sort_nblocks + g] = c;
    }
}

var<workgroup> s_dig: array<u32, 64>;

@compute @workgroup_size(64)
fn radix_scatter(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let g = block_index(wid, nwg);
    let idx = g * 64u + lid.x;
    var d = 16u;
    var key = 0u;
    var val = 0u;
    if (idx < params.slots_fine_cap) {
        key = sk[idx];
        val = sv[idx];
        d = (key >> PASS_SHIFT) & 0xFu;
    }
    s_dig[lid.x] = d;
    workgroupBarrier();
    if (idx < params.slots_fine_cap && g < params.sort_nblocks) {
        var rank = 0u;
        for (var k = 0u; k < lid.x; k = k + 1u) {
            if (s_dig[k] == d) { rank = rank + 1u; }
        }
        let dest = histogram[d * params.sort_nblocks + g] + rank;
        sk_alt[dest] = key;
        sv_alt[dest] = val;
    }
}

// Swap sort key and payload in place, between the by-max and by-min passes.
@compute @workgroup_size(64)
fn pair_swap(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= params.slots_fine_cap) { return; }
    let t = sk[i];
    sk[i] = sv[i];
    sv[i] = t;
}

// ---- Generic exclusive scans (reduce -> serial block scan -> fixup) --------

var<workgroup> sc_tmp: array<u32, 64>;
var<workgroup> sc_carry: u32;

@compute @workgroup_size(64)
fn scan_local_u32(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let block = block_index(wid, nwg);
    let idx = block * 64u + lid.x;
    var v = 0u;
    if (idx < scan_dims.len) { v = scan_data[idx]; }
    sc_tmp[lid.x] = v;
    workgroupBarrier();
    if (lid.x == 0u) {
        var acc = 0u;
        for (var k = 0u; k < 64u; k = k + 1u) {
            let t = sc_tmp[k];
            sc_tmp[k] = acc;
            acc = acc + t;
        }
        scan_bs[block] = acc;
    }
    workgroupBarrier();
    if (idx < scan_dims.len) { scan_data[idx] = sc_tmp[lid.x]; }
}

@compute @workgroup_size(64)
fn scan_serial_u32(@builtin(local_invocation_id) lid: vec3<u32>) {
    let m = scan_dims.nblocks;
    if (lid.x == 0u) { sc_carry = 0u; }
    workgroupBarrier();
    var base = 0u;
    loop {
        if (base >= m) { break; }
        let idx = base + lid.x;
        var v = 0u;
        if (idx < m) { v = scan_bs[idx]; }
        sc_tmp[lid.x] = v;
        workgroupBarrier();
        if (lid.x == 0u) {
            var acc = sc_carry;
            for (var k = 0u; k < 64u; k = k + 1u) {
                let t = sc_tmp[k];
                sc_tmp[k] = acc;
                acc = acc + t;
            }
            sc_carry = acc;
        }
        workgroupBarrier();
        if (idx < m) { scan_bs[idx] = sc_tmp[lid.x]; }
        workgroupBarrier();
        base = base + 64u;
    }
}

@compute @workgroup_size(64)
fn scan_fixup_u32(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let block = block_index(wid, nwg);
    let idx = block * 64u + lid.x;
    if (idx < scan_dims.len) {
        scan_data[idx] = scan_data[idx] + scan_bs[block];
    }
}

// ---- Coarse edge dedup + CSR build -----------------------------------------

// Mark the first slot of each run of identical sorted (min,max) pairs. sk is
// the sorted min, sv the aligned max. Written into the (pre-zeroed) scan work
// buffer; the following scan turns per-slot flags into per-slot edge ids.
@compute @workgroup_size(64)
fn edge_flag(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let k = linear_index(gid, nwg);
    if (k >= params.slots_fine_cap) { return; }
    let mn = sk[k];
    if (mn == SENTINEL) { return; }
    var start = false;
    if (k == 0u) {
        start = true;
    } else if (sk[k - 1u] != mn || sv[k - 1u] != sv[k]) {
        start = true;
    }
    if (start) { wbuf[k] = 1u; }
}

@compute @workgroup_size(64)
fn write_mc(@builtin(local_invocation_id) lid: vec3<u32>) {
    if (lid.x != 0u) { return; }
    // wbuf exclusive-scanned; the plateau at the tail equals the unique count.
    c_meta[1] = min(wbuf[params.slots_fine_cap], params.mc_cap);
}

// One lane per sorted slot: exclusive-scan value at k (wbuf) is this edge's
// id; accumulate multiplicity, and at the run start record the endpoints.
@compute @workgroup_size(64)
fn edge_accumulate(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let k = linear_index(gid, nwg);
    if (k >= params.slots_fine_cap) { return; }
    let mn = sk[k];
    if (mn == SENTINEL) { return; }
    let eid = wbuf[k];
    if (eid >= params.mc_cap) { return; }
    atomicAdd(&edge_wt[eid], 1u);
    var start = false;
    if (k == 0u) {
        start = true;
    } else if (sk[k - 1u] != mn || sv[k - 1u] != sv[k]) {
        start = true;
    }
    if (start) {
        edge_min[eid] = mn;
        edge_max[eid] = sv[k];
    }
}

// Degree count per coarse endpoint (both directions), into the off buffer
// viewed as atomics. Cleared before this pass; scanned into offsets after.
@compute @workgroup_size(64)
fn coarse_deg(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let e = linear_index(gid, nwg);
    if (e >= mc()) { return; }
    atomicAdd(&c_off_atom[edge_min[e]], 1u);
    atomicAdd(&c_off_atom[edge_max[e]], 1u);
}

@compute @workgroup_size(64)
fn write_slots(@builtin(local_invocation_id) lid: vec3<u32>) {
    if (lid.x != 0u) { return; }
    // c_off exclusive-scanned; c_off[nc] is the total directed slot count.
    c_meta[2] = c_off[nc()];
}

// Fill coarse CSR both directions via an atomic cursor seeded from c_off.
@compute @workgroup_size(64)
fn coarse_fill(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let e = linear_index(gid, nwg);
    if (e >= mc()) { return; }
    let a = edge_min[e];
    let b = edge_max[e];
    let w = f32(atomicLoad(&edge_wt[e]));
    let pa = atomicAdd(&c_cursor[a], 1u);
    c_neigh[pa] = b;
    c_ewt[pa] = w;
    let pb = atomicAdd(&c_cursor[b], 1u);
    c_neigh[pb] = a;
    c_ewt[pb] = w;
}

// ---- Coarse Tigr virtual CSR ----------------------------------------------

// nv[i] = max(1, ceil(deg/HUB)) written into the packed virt_csr offset
// region (pre-zeroed [0, nc_cap+1)); the following scan turns it into
// node_to_virt_offsets and the tail plateau into n_virtual.
@compute @workgroup_size(64)
fn coarse_nv(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= nc()) { return; }
    let deg = c_off[i + 1u] - c_off[i];
    virt_csr[i] = max(1u, (deg + HUB - 1u) / HUB);
}

// One lane per coarse node fills its virtual-vertex slots: real index into the
// packed tail (virt_csr[nc+1+v]) and CSR edge-slice starts into virt_eoff.
@compute @workgroup_size(64)
fn coarse_virt_fill(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    let ncv = nc();
    if (i >= ncv) { return; }
    let base = virt_csr[i];
    let next = virt_csr[i + 1u];
    let nvi = next - base;
    let estart = c_off[i];
    let eend = c_off[i + 1u];
    for (var c = 0u; c < nvi; c = c + 1u) {
        let v = base + c;
        virt_csr[ncv + 1u + v] = i;
        virt_eoff[v] = estart + c * HUB;
    }
    // The last coarse node closes the offset array with the total slot count.
    if (i == ncv - 1u) {
        virt_eoff[next] = eend;
    }
}

// ---- Seed + prolong --------------------------------------------------------

fn unit_dir(salt: u32) -> vec3<f32> {
    let x = hashf(salt * 0x9E3779B9u + 1u) * 2.0 - 1.0;
    let y = hashf(salt * 0x85EBCA6Bu + 2u) * 2.0 - 1.0;
    let z = hashf(salt * 0xC2B2AE35u + 3u) * 2.0 - 1.0;
    let v = vec3<f32>(x, y, z);
    let len = length(v);
    if (len < 1e-6) { return vec3<f32>(1.0, 0.0, 0.0); }
    return v / len;
}

// Coarsest-level seed: hash each node into a solid ball of radius `radius`.
// Preserves the mass placed in .w.
@compute @workgroup_size(64)
fn seed_ball(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= nc()) { return; }
    let dir = unit_dir(i + params.seed);
    let u = hashf((i + params.seed) * 2654435761u + 7u);
    let r = params.radius * pow(u, 1.0 / 3.0);
    let m = c_pos[i].w;
    c_pos[i] = vec4<f32>(dir * r, m);
}

// Prolong: each finer node inherits its parent's coarser position plus a small
// jitter, so contracted pairs separate. Preserves the finer .w mass (set by
// place_mass for coarse destinations, or the original fine mass at level 0).
@compute @workgroup_size(64)
fn prolong(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= n_fine()) { return; }
    let p = parent[i];
    let base = pos_src[p].xyz;
    let j = vec3<f32>(
        hashf((i + params.seed) * 0x9E3779B9u + 11u) * 2.0 - 1.0,
        hashf((i + params.seed) * 0x85EBCA6Bu + 13u) * 2.0 - 1.0,
        hashf((i + params.seed) * 0xC2B2AE35u + 17u) * 2.0 - 1.0,
    );
    let m = c_pos[i].w;
    c_pos[i] = vec4<f32>(base + j * (0.5 * params.spring_len), m);
}

// ---- Weighted spring kernel (coarse levels) --------------------------------
//
// Mirrors force.wgsl `spring_step` (same virtual-vertex gather contract) but
// scales each edge's attraction by its coarse weight. force.wgsl `force_step`
// is reused unchanged to gather these partials and integrate.
@group(0) @binding(31) var<storage, read_write> spring_partial: array<vec3<f32>>;   // per-virtual-vertex spring partial (force_step gathers)

fn attraction_force(d: vec3<f32>) -> vec3<f32> {
    let dist = max(length(d), 0.01);
    if (sp_params.force_model == 1u) {
        let r = dist / max(sp_params.spring_len, 1e-6);
        let mag = sp_params.tfdp_alpha * (r + sp_params.tfdp_beta * r / (1.0 + r * r));
        return (d / dist) * (sp_params.spring_k * sp_params.spring_len * mag);
    }
    let stretch = dist - sp_params.spring_len;
    return (d / dist) * (sp_params.spring_k * stretch);
}

@compute @workgroup_size(64)
fn spring_step_weighted(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let v = linear_index(gid, nwg);
    let n_virtual = virt_csr[sp_params.n_nodes];
    if (v >= n_virtual) { return; }
    let i = virt_csr[sp_params.n_nodes + 1u + v];
    let pos = f_pos[i].xyz;
    let estart = virt_eoff[v];
    let eend = virt_eoff[v + 1u];
    var f = vec3<f32>(0.0, 0.0, 0.0);
    for (var k = estart; k < eend; k = k + 1u) {
        let other = c_neigh[k];
        let w = c_ewt[k];
        f = f + attraction_force(f_pos[other].xyz - pos) * w;
    }
    spring_partial[v] = f;
}
