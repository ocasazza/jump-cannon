// Half-precision weighted SpMV: y = A·x with f16 STORAGE, f32 ACCUMULATE.
//
// `weights` and `x` are stored as f16 packed two-per-u32 (half the memory of
// f32 — the win for large weighted matrices, e.g. chemical-sim graphs at 8M+
// nodes) and decoded with the core `unpack2x16float` builtin. This avoids the
// `enable f16;` extension, which Naga does not yet implement in wgpu 23, so it
// runs on Metal/Vulkan/DX12 today without the SHADER_F16 device feature.
// Products promote to f32 and accumulate in f32; `y` is f32.
//
//   value at logical index i  =  unpack2x16float(packed[i >> 1])[i & 1]
// (low 16 bits = even index, high 16 bits = odd index — matches the host packer)

struct Params {
  n:   u32,
  _p0: u32,
  _p1: u32,
  _p2: u32,
};

@group(0) @binding(0) var<storage, read>       offsets:   array<u32>;   // n + 1
@group(0) @binding(1) var<storage, read>       neighbors: array<u32>;   // m
@group(0) @binding(2) var<storage, read>       weights:   array<u32>;   // ceil(m/2) packed f16
@group(0) @binding(3) var<storage, read>       x:         array<u32>;   // ceil(n/2) packed f16
@group(0) @binding(4) var<storage, read_write> y:         array<f32>;   // n
@group(0) @binding(5) var<uniform>             params:    Params;

@compute @workgroup_size(64)
fn spmv_f16(@builtin(global_invocation_id) gid: vec3<u32>) {
  let v = gid.x;
  if (v >= params.n) { return; }

  var acc: f32 = 0.0;
  let start = offsets[v];
  let end   = offsets[v + 1u];
  for (var e: u32 = start; e < end; e = e + 1u) {
    let c = neighbors[e];
    let w  = unpack2x16float(weights[e >> 1u])[e & 1u];
    let xc = unpack2x16float(x[c >> 1u])[c & 1u];
    acc = acc + w * xc;
  }
  y[v] = acc;
}
