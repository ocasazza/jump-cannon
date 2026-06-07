// Hub-aware (load-balanced) weighted SpMV: y = A·x over a CSR matrix, tuned for
// power-law graphs where a few hub rows have enormous degree while most rows are
// short.
//
// Strategy — workgroup-per-row (a "warp/CTA-per-row" segmented reduction):
//   * One workgroup is launched per row v (dispatch n workgroups).
//   * The WORKGROUP_SIZE threads of that workgroup cooperatively walk row v,
//     each thread striding over the row's edges by WORKGROUP_SIZE:
//         lane l handles edges start+l, start+l+W, start+l+2W, …
//   * Partial sums land in workgroup-shared memory and a tree reduction collapses
//     them to y[v], written by lane 0.
//
// Why this load-balances power-law graphs: the thread-per-row baseline assigns a
// whole hub row (degree ~n) to a single thread, so one lane serializes the entire
// reduction while its 63 neighbours sit idle — the long pole. Here the hub row's
// cost is shared across the workgroup, while a short row simply leaves most lanes
// idle for one cheap pass (no worse than thread-per-row for that row). Result is
// identical to the baseline up to f32 summation-order tolerance (the reduction
// order differs), so it is an oracle-equivalent variant, not an approximation.
//
// Accumulation is f32 throughout (weights/x are f32 storage).

struct Params {
  n:   u32,
  _p0: u32,
  _p1: u32,
  _p2: u32,
};

@group(0) @binding(0) var<storage, read>       offsets:   array<u32>;   // n + 1
@group(0) @binding(1) var<storage, read>       neighbors: array<u32>;   // m (columns)
@group(0) @binding(2) var<storage, read>       weights:   array<f32>;   // m (values)
@group(0) @binding(3) var<storage, read>       x:         array<f32>;   // n
@group(0) @binding(4) var<storage, read_write> y:         array<f32>;   // n
@group(0) @binding(5) var<uniform>             params:    Params;

const WG: u32 = 64u;

var<workgroup> partials: array<f32, 64>;

@compute @workgroup_size(64)
fn spmv_hybrid(
  @builtin(workgroup_id)        wid: vec3<u32>,
  @builtin(local_invocation_id) lid: vec3<u32>,
) {
  let v = wid.x;          // one workgroup == one row
  let lane = lid.x;       // 0 .. WG-1

  // Out-of-range rows: still participate in the barriers so control flow is
  // uniform, but contribute nothing and write nothing.
  var start: u32 = 0u;
  var end:   u32 = 0u;
  if (v < params.n) {
    start = offsets[v];
    end   = offsets[v + 1u];
  }

  // Each lane reduces its strided slice of the row into a private accumulator.
  var acc: f32 = 0.0;
  var e: u32 = start + lane;
  loop {
    if (e >= end) { break; }
    acc = acc + weights[e] * x[neighbors[e]];
    e = e + WG;
  }
  partials[lane] = acc;

  // Workgroup tree reduction over partials[0 .. WG-1].
  workgroupBarrier();
  var stride: u32 = WG >> 1u;
  loop {
    if (stride == 0u) { break; }
    if (lane < stride) {
      partials[lane] = partials[lane] + partials[lane + stride];
    }
    workgroupBarrier();
    stride = stride >> 1u;
  }

  if (lane == 0u && v < params.n) {
    y[v] = partials[0];
  }
}
