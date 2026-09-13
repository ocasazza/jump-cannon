// Fully-GPU Barnes-Hut octree build.
//
// The host never reads positions back or builds a tree CPU-side. This file
// is a self-contained pipeline of compute kernels that, given the current
// positions buffer, produces the same on-wire `OctNode` array that
// `force.wgsl` walks with its stackless rope.
//
// Structure (a level-by-level *linear* octree over Morton-sorted bodies):
//   1. bbox_clear / bbox_reduce / bbox_finalize
//        world bounding box via order-preserving-u32 atomicMin/atomicMax,
//        then a 1-workgroup pass pads it into a cube root.
//   2. morton_assign
//        30-bit (10 bits/axis) Z-order key per body; ids[i] = i.
//   3. radix_histogram / scan / radix_scatter  (8 passes, 4-bit LSD digits)
//        stable radix sort of (key, id), ping-ponging a<->b.
//   4. level_flags
//        per sorted position, one u32 whose bit L marks "a level-L node
//        starts here" (prefix_L differs from the previous body).
//   5. com_input + scan  (exclusive prefix sums of mass and mass*pos)
//        COM of any sorted body range is a difference of two prefix entries.
//   6. node_count + scan
//        per-body count of emitted nodes, exclusive-scanned into DFS base
//        indices.
//   7. node_emit
//        writes each (start position, level) node: center/half from the
//        Morton prefix, COM/mass from the prefix sums, next/skip rope.
//   8. finalize_aux
//        body_rank[id] = sorted rank; oct_aux[0] = total node count.
//
// No f32 atomics (floats are mapped to order-preserving u32 for the bbox
// reduce). No bottom-up pointer chasing. Every kernel is 64 lanes wide and
// recovers its lane index through `linear_index` so a dispatch may spill
// into the Y dimension past 65535 workgroups.

const OCT_END: u32 = 0xFFFFFFFFu;
const OCT_BODY_INTERNAL: u32 = 0xFFFFFFFFu;
const WORKGROUP_SIZE: u32 = 64u;

// Must match `OctNodeRaw` on the Rust side (three 16-byte vec4 chunks):
//   pos_size: (center.xyz, half_extent)
//   com_mass: (com.xyz, total_mass)
//   links:    (body_idx | OCT_BODY_INTERNAL, next_idx, skip_idx, child_count)
struct OctNode {
    pos_size: vec4<f32>,
    com_mass: vec4<f32>,
    links:    vec4<u32>,
};

struct OctBuildParams {
    // Body count.
    n: u32,
    // ceil(n / 64) — the number of 64-lane sort blocks; also the stride
    // between digit bins in the radix histogram.
    n_blocks: u32,
    // Octree node capacity (oct_nodes length). node_emit never writes past it.
    cap: u32,
    _pad: u32,
};

struct ScanDims {
    // Length of the scanned array.
    len: u32,
    // Number of 64-blocks (== ceil(len / 64)); the block-sum array length.
    nblocks: u32,
    _pad0: u32,
    _pad1: u32,
};

// Current radix digit shift (0, 4, 8, ... 28), baked per pipeline.
override PASS_SHIFT: u32 = 0u;

// ---- Group 0: storage ------------------------------------------------------
@group(0) @binding(0)  var<storage, read>       positions_in:  array<vec4<f32>>;
// bbox layout: [0..3] = min encoded, [3..6] = max encoded (order-preserving u32).
@group(0) @binding(1)  var<storage, read_write> bbox:          array<atomic<u32>>;
// oct_world[0] = (world_min.xyz, cube_edge); oct_world[1] = (center.xyz, half).
@group(0) @binding(2)  var<storage, read_write> oct_world:     array<vec4<f32>>;
// keys / ids are bound to the "src" side of the current sort pass; *_alt to
// the "dst" side. Downstream kernels bind the fully-sorted buffers here.
@group(0) @binding(3)  var<storage, read_write> keys:          array<u32>;
@group(0) @binding(4)  var<storage, read_write> keys_alt:      array<u32>;
@group(0) @binding(5)  var<storage, read_write> ids:           array<u32>;
@group(0) @binding(6)  var<storage, read_write> ids_alt:       array<u32>;
// histogram[bin * n_blocks + group] — one slot per (digit, sort block).
@group(0) @binding(7)  var<storage, read_write> histogram:     array<u32>;
// Exclusive prefix sums of vec4(mass, mass*x, mass*y, mass*z) over sorted
// bodies; length n+1 with [n] = total.
@group(0) @binding(8)  var<storage, read_write> com_prefix:    array<vec4<f32>>;
// Per sorted position: bit L set when a level-L node starts here.
@group(0) @binding(9)  var<storage, read_write> flags:         array<u32>;
// Per-body emitted-node count, then exclusive-scanned DFS base index;
// length n+1 with [n] = total node count.
@group(0) @binding(10) var<storage, read_write> node_base:     array<u32>;
@group(0) @binding(11) var<storage, read_write> oct_nodes:     array<OctNode>;
// oct_aux[0] = node count; oct_aux[1 + i] = sorted rank of real body i.
@group(0) @binding(12) var<storage, read_write> oct_aux:       array<u32>;
// Generic scan work bindings (a scanned buffer is bound here per use).
@group(0) @binding(13) var<storage, read_write> scan_data_u32: array<u32>;
@group(0) @binding(14) var<storage, read_write> scan_bs_u32:   array<u32>;
@group(0) @binding(15) var<storage, read_write> scan_data_v4:  array<vec4<f32>>;
@group(0) @binding(16) var<storage, read_write> scan_bs_v4:    array<vec4<f32>>;

// ---- Group 1: uniforms -----------------------------------------------------
@group(1) @binding(0) var<uniform> params:    OctBuildParams;
@group(1) @binding(1) var<uniform> scan_dims: ScanDims;

fn linear_index(gid: vec3<u32>, nwg: vec3<u32>) -> u32 {
    return gid.x + gid.y * nwg.x * WORKGROUP_SIZE;
}

fn block_index(wid: vec3<u32>, nwg: vec3<u32>) -> u32 {
    return wid.x + wid.y * nwg.x;
}

// f32 -> order-preserving u32: flip all bits when the sign bit is set,
// otherwise flip only the sign bit. Larger float => larger u32.
fn f2u(f: f32) -> u32 {
    let b = bitcast<u32>(f);
    let mask = select(0x80000000u, 0xFFFFFFFFu, (b & 0x80000000u) != 0u);
    return b ^ mask;
}
fn u2f(u: u32) -> f32 {
    let mask = select(0xFFFFFFFFu, 0x80000000u, (u & 0x80000000u) != 0u);
    return bitcast<f32>(u ^ mask);
}

// ---- 1. Bounding box -------------------------------------------------------

@compute @workgroup_size(64)
fn bbox_clear(@builtin(local_invocation_id) lid: vec3<u32>) {
    if (lid.x < 3u) {
        atomicStore(&bbox[lid.x], 0xFFFFFFFFu);      // min seed (== +inf order)
    } else if (lid.x < 6u) {
        atomicStore(&bbox[lid.x], 0u);               // max seed (== -inf order)
    }
}

var<workgroup> wg_lo: array<vec3<u32>, 64>;
var<workgroup> wg_hi: array<vec3<u32>, 64>;

@compute @workgroup_size(64)
fn bbox_reduce(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    var lo = vec3<u32>(0xFFFFFFFFu, 0xFFFFFFFFu, 0xFFFFFFFFu);
    var hi = vec3<u32>(0u, 0u, 0u);
    if (i < params.n) {
        let p = positions_in[i].xyz;
        if (p.x == p.x && p.y == p.y && p.z == p.z) {
            let e = vec3<u32>(f2u(p.x), f2u(p.y), f2u(p.z));
            lo = e;
            hi = e;
        }
    }
    wg_lo[lid.x] = lo;
    wg_hi[lid.x] = hi;
    workgroupBarrier();
    var s = 32u;
    loop {
        if (s == 0u) { break; }
        if (lid.x < s) {
            wg_lo[lid.x] = min(wg_lo[lid.x], wg_lo[lid.x + s]);
            wg_hi[lid.x] = max(wg_hi[lid.x], wg_hi[lid.x + s]);
        }
        workgroupBarrier();
        s = s >> 1u;
    }
    if (lid.x == 0u) {
        atomicMin(&bbox[0], wg_lo[0].x);
        atomicMin(&bbox[1], wg_lo[0].y);
        atomicMin(&bbox[2], wg_lo[0].z);
        atomicMax(&bbox[3], wg_hi[0].x);
        atomicMax(&bbox[4], wg_hi[0].y);
        atomicMax(&bbox[5], wg_hi[0].z);
    }
}

@compute @workgroup_size(64)
fn bbox_finalize(@builtin(local_invocation_id) lid: vec3<u32>) {
    if (lid.x != 0u) { return; }
    let mn = vec3<f32>(
        u2f(atomicLoad(&bbox[0])),
        u2f(atomicLoad(&bbox[1])),
        u2f(atomicLoad(&bbox[2])),
    );
    let mx = vec3<f32>(
        u2f(atomicLoad(&bbox[3])),
        u2f(atomicLoad(&bbox[4])),
        u2f(atomicLoad(&bbox[5])),
    );
    var center = 0.5 * (mn + mx);
    var extent = max(max(mx.x - mn.x, mx.y - mn.y), mx.z - mn.z);
    // No finite bodies contributed (min/max still seeded): default box.
    if (!(extent == extent) || !(center.x == center.x) || !(center.y == center.y) || !(center.z == center.z)) {
        center = vec3<f32>(0.0, 0.0, 0.0);
        extent = 2.0;
    }
    var half = 0.5 * extent;
    // Degenerate (all coincident): keep the point as center, give it size.
    if (half <= 0.0) { half = 1.0; }
    half = half * 1.05;
    let wmin = center - vec3<f32>(half, half, half);
    oct_world[0] = vec4<f32>(wmin, 2.0 * half);
    oct_world[1] = vec4<f32>(center, half);
}

// ---- 2. Morton keys --------------------------------------------------------

fn expand_bits(v: u32) -> u32 {
    var x = v & 0x3FFu;
    x = (x | (x << 16u)) & 0x030000FFu;
    x = (x | (x << 8u)) & 0x0300F00Fu;
    x = (x | (x << 4u)) & 0x030C30C3u;
    x = (x | (x << 2u)) & 0x09249249u;
    return x;
}

@compute @workgroup_size(64)
fn morton_assign(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= params.n) { return; }
    let p = positions_in[i].xyz;
    let wmin = oct_world[0].xyz;
    let edge = oct_world[0].w;
    let inv = 1024.0 / max(edge, 1e-20);
    // Quantize to [0, 1023]; NaN -> 0 so a stray non-finite body can't
    // poison u32 conversion.
    let fx = select(0.0, clamp(floor((p.x - wmin.x) * inv), 0.0, 1023.0), p.x == p.x);
    let fy = select(0.0, clamp(floor((p.y - wmin.y) * inv), 0.0, 1023.0), p.y == p.y);
    let fz = select(0.0, clamp(floor((p.z - wmin.z) * inv), 0.0, 1023.0), p.z == p.z);
    let xi = u32(fx);
    let yi = u32(fy);
    let zi = u32(fz);
    // Interleave: x at bits 0,3,..27; y at 1,4,..28; z at 2,5,..29. The top
    // 3 bits are the MSBs of z,y,x, so prefix_L = key >> (30 - 3L).
    keys[i] = expand_bits(xi) | (expand_bits(yi) << 1u) | (expand_bits(zi) << 2u);
    ids[i] = i;
}

// ---- 3. Radix sort ---------------------------------------------------------

var<workgroup> h_dig: array<u32, 64>;

@compute @workgroup_size(64)
fn radix_histogram(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let g = block_index(wid, nwg);
    let idx = g * 64u + lid.x;
    var d = 16u;                                     // sentinel: not a real bin
    if (idx < params.n) { d = (keys[idx] >> PASS_SHIFT) & 0xFu; }
    h_dig[lid.x] = d;
    workgroupBarrier();
    if (lid.x < 16u && g < params.n_blocks) {
        var c = 0u;
        for (var k = 0u; k < 64u; k = k + 1u) {
            if (h_dig[k] == lid.x) { c = c + 1u; }
        }
        histogram[lid.x * params.n_blocks + g] = c;
    }
}

var<workgroup> s_dig: array<u32, 64>;

@compute @workgroup_size(64)
fn radix_scatter(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let g = block_index(wid, nwg);
    let idx = g * 64u + lid.x;
    var d = 16u;
    var key = 0u;
    var id = 0u;
    if (idx < params.n) {
        key = keys[idx];
        id = ids[idx];
        d = (key >> PASS_SHIFT) & 0xFu;
    }
    s_dig[lid.x] = d;
    workgroupBarrier();
    if (idx < params.n && g < params.n_blocks) {
        // Stable rank within this block among same-digit lanes below us.
        var rank = 0u;
        for (var k = 0u; k < lid.x; k = k + 1u) {
            if (s_dig[k] == d) { rank = rank + 1u; }
        }
        let dest = histogram[d * params.n_blocks + g] + rank;
        keys_alt[dest] = key;
        ids_alt[dest] = id;
    }
}

// ---- Generic exclusive scans (reduce -> serial block scan -> fixup) --------
//
// Each `*_local` kernel exclusive-scans its own 64-block and writes the
// block total; the single-workgroup `*_serial` kernel exclusive-scans the
// block totals in place, looping over chunks so it works for any number of
// blocks; `*_fixup` adds the scanned block offset back. No assumption that
// the block sums fit one workgroup.

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
    if (idx < scan_dims.len) { v = scan_data_u32[idx]; }
    sc_tmp[lid.x] = v;
    workgroupBarrier();
    if (lid.x == 0u) {
        var acc = 0u;
        for (var k = 0u; k < 64u; k = k + 1u) {
            let t = sc_tmp[k];
            sc_tmp[k] = acc;
            acc = acc + t;
        }
        scan_bs_u32[block] = acc;
    }
    workgroupBarrier();
    if (idx < scan_dims.len) { scan_data_u32[idx] = sc_tmp[lid.x]; }
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
        if (idx < m) { v = scan_bs_u32[idx]; }
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
        if (idx < m) { scan_bs_u32[idx] = sc_tmp[lid.x]; }
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
        scan_data_u32[idx] = scan_data_u32[idx] + scan_bs_u32[block];
    }
}

var<workgroup> scv_tmp: array<vec4<f32>, 64>;
var<workgroup> scv_carry: vec4<f32>;

@compute @workgroup_size(64)
fn scan_local_v4(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let block = block_index(wid, nwg);
    let idx = block * 64u + lid.x;
    var v = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    if (idx < scan_dims.len) { v = scan_data_v4[idx]; }
    scv_tmp[lid.x] = v;
    workgroupBarrier();
    if (lid.x == 0u) {
        var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        for (var k = 0u; k < 64u; k = k + 1u) {
            let t = scv_tmp[k];
            scv_tmp[k] = acc;
            acc = acc + t;
        }
        scan_bs_v4[block] = acc;
    }
    workgroupBarrier();
    if (idx < scan_dims.len) { scan_data_v4[idx] = scv_tmp[lid.x]; }
}

@compute @workgroup_size(64)
fn scan_serial_v4(@builtin(local_invocation_id) lid: vec3<u32>) {
    let m = scan_dims.nblocks;
    if (lid.x == 0u) { scv_carry = vec4<f32>(0.0, 0.0, 0.0, 0.0); }
    workgroupBarrier();
    var base = 0u;
    loop {
        if (base >= m) { break; }
        let idx = base + lid.x;
        var v = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        if (idx < m) { v = scan_bs_v4[idx]; }
        scv_tmp[lid.x] = v;
        workgroupBarrier();
        if (lid.x == 0u) {
            var acc = scv_carry;
            for (var k = 0u; k < 64u; k = k + 1u) {
                let t = scv_tmp[k];
                scv_tmp[k] = acc;
                acc = acc + t;
            }
            scv_carry = acc;
        }
        workgroupBarrier();
        if (idx < m) { scan_bs_v4[idx] = scv_tmp[lid.x]; }
        workgroupBarrier();
        base = base + 64u;
    }
}

@compute @workgroup_size(64)
fn scan_fixup_v4(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let block = block_index(wid, nwg);
    let idx = block * 64u + lid.x;
    if (idx < scan_dims.len) {
        scan_data_v4[idx] = scan_data_v4[idx] + scan_bs_v4[block];
    }
}

// ---- 4. Level flags --------------------------------------------------------

@compute @workgroup_size(64)
fn level_flags(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= params.n) { return; }
    let ki = keys[i];
    var f = 0u;
    if (i == 0u) {
        f = 0x7FFu;                                  // bits 0..10 (all levels)
    } else {
        let kp = keys[i - 1u];
        for (var L = 0u; L <= 10u; L = L + 1u) {
            let sh = 30u - 3u * L;
            if ((ki >> sh) != (kp >> sh)) { f = f | (1u << L); }
        }
    }
    flags[i] = f;
}

// ---- 5. COM prefix sums ----------------------------------------------------

@compute @workgroup_size(64)
fn com_input(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i > params.n) { return; }
    if (i == params.n) {
        com_prefix[params.n] = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        return;
    }
    let id = ids[i];
    let p = positions_in[id];
    let m = p.w;
    com_prefix[i] = vec4<f32>(m, m * p.x, m * p.y, m * p.z);
}

// ---- 6/7. Node counting + emission -----------------------------------------

// Coarsest level at which position i starts a node, or OCT_END if i is not a
// boundary (its body is subsumed by an earlier body's cell at every level).
fn lmin_at(i: u32) -> u32 {
    let f = flags[i];
    if (f == 0u) { return OCT_END; }
    return firstTrailingBit(f);
}

// (Lmin, Llast) for the emitted node range at position i. Lmin == OCT_END
// means "no node here". Llast is where the node becomes a leaf: the shallower
// of "next body diverges" and max depth 10.
fn emitted_range(i: u32) -> vec2<u32> {
    let lmin = lmin_at(i);
    if (lmin == OCT_END) { return vec2<u32>(OCT_END, 0u); }
    var lmin_next: u32;
    if (i + 1u < params.n) {
        let ln = lmin_at(i + 1u);
        // No boundary at i+1 => i+1 shares i's cell at every level => this
        // node only bottoms out at max depth.
        lmin_next = select(ln, 11u, ln == OCT_END);
    } else {
        // Last body: alone in its cell, single-body leaf at Lmin.
        lmin_next = 0u;
    }
    var llast = max(lmin, lmin_next);
    if (llast > 10u) { llast = 10u; }
    return vec2<u32>(lmin, llast);
}

@compute @workgroup_size(64)
fn node_count(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i > params.n) { return; }
    if (i == params.n) {
        node_base[params.n] = 0u;                    // scan sentinel -> total
        return;
    }
    let r = emitted_range(i);
    var c = 0u;
    if (r.x != OCT_END) { c = r.y - r.x + 1u; }
    node_base[i] = c;
}

// First sorted position > i whose level-L prefix differs from body i's (or n).
fn upper_bound_level(i: u32, ki: u32, L: u32) -> u32 {
    let sh = 30u - 3u * L;
    let p = ki >> sh;
    var lo = i;
    var hi = params.n;
    loop {
        if (lo >= hi) { break; }
        let mid = (lo + hi) >> 1u;
        if ((keys[mid] >> sh) > p) { hi = mid; } else { lo = mid + 1u; }
    }
    return lo;
}

// Per-axis cell coordinate (top L Morton bits of each axis) at level L.
fn cell_coords(key: u32, L: u32) -> vec3<u32> {
    var cx = 0u;
    var cy = 0u;
    var cz = 0u;
    for (var j = 0u; j < L; j = j + 1u) {
        let b = 3u * (9u - j);
        cx = (cx << 1u) | ((key >> (b + 0u)) & 1u);
        cy = (cy << 1u) | ((key >> (b + 1u)) & 1u);
        cz = (cz << 1u) | ((key >> (b + 2u)) & 1u);
    }
    return vec3<u32>(cx, cy, cz);
}

@compute @workgroup_size(64)
fn node_emit(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= params.n) { return; }
    let r = emitted_range(i);
    if (r.x == OCT_END) { return; }
    let lmin = r.x;
    let llast = r.y;
    let base = node_base[i];
    let ki = keys[i];
    let wmin = oct_world[0].xyz;
    let edge = oct_world[0].w;
    let ci = com_prefix[i];
    for (var L = lmin; L <= llast; L = L + 1u) {
        let ni = base + (L - lmin);
        if (ni >= params.cap) { continue; }          // capacity guard
        let e = upper_bound_level(i, ki, L);
        let cnt = e - i;
        let cell = cell_coords(ki, L);
        let cs = edge / f32(1u << L);
        let center = wmin + (vec3<f32>(f32(cell.x), f32(cell.y), f32(cell.z)) + vec3<f32>(0.5, 0.5, 0.5)) * cs;
        let half = 0.5 * cs;
        let sum = com_prefix[e] - ci;
        let mass = sum.x;
        var com = vec3<f32>(0.0, 0.0, 0.0);
        if (mass > 0.0) { com = sum.yzw / mass; }
        var skip = OCT_END;
        if (e < params.n) { skip = node_base[e]; }
        var links: vec4<u32>;
        if (L < llast) {
            // Internal: first child is (i, L+1) at ni + 1.
            links = vec4<u32>(OCT_BODY_INTERNAL, ni + 1u, skip, cnt);
        } else if (cnt == 1u) {
            // Single-body leaf: store the real body index.
            links = vec4<u32>(ids[i], OCT_END, skip, 1u);
        } else {
            // Multi-body max-depth leaf: bodies coincide to grid resolution.
            // body slot holds the sorted start; the force kernel removes self
            // by cell containment.
            links = vec4<u32>(i, OCT_END, skip, cnt);
        }
        oct_nodes[ni] = OctNode(vec4<f32>(center, half), vec4<f32>(com, mass), links);
    }
}

// ---- 8. Aux (body_rank + node count) ---------------------------------------

@compute @workgroup_size(64)
fn finalize_aux(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let i = linear_index(gid, nwg);
    if (i >= params.n) { return; }
    oct_aux[1u + ids[i]] = i;
    if (i == 0u) {
        oct_aux[0] = node_base[params.n];
    }
}
