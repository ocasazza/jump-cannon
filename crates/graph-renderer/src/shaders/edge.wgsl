// Edge (line) shader. Indexes into the same `positions` storage buffer
// the node shader reads from, so when the compute force-sim moves a node
// the edges automatically follow — no per-frame CPU rebuild of the edge
// vertex buffer.
//
// Draw call: `draw(0..(2 * n_edges), 0..1)`. Each pair of vertices forms
// one line; vertex N indexes edge N/2 and picks endpoint N&1.
//
// Layout:
//   group(0) binding(0)  uniform CameraUniform
//   group(0) binding(1)  uniform EffectsUniform
//   group(0) binding(2)  storage<read> array<vec3<f32>>  positions (vec3+pad)
//   group(0) binding(3)  storage<read> array<vec2<u32>>  edges (src,tgt)

struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_pos:   vec3<f32>,
    _pad0:     f32,
    screen:    vec2<f32>,
    _pad1:     vec2<f32>,
};
struct EffectsUniform {
    focus_plane_z:        f32,
    focus_thickness:      f32,
    cursor_radius_visual: f32,
    _pad:                 f32,
};

@group(0) @binding(0) var<uniform> camera:  CameraUniform;
@group(0) @binding(1) var<uniform> effects: EffectsUniform;
@group(0) @binding(2) var<storage, read> positions: array<vec3<f32>>;
@group(0) @binding(3) var<storage, read> edges:     array<vec2<u32>>;

struct VertexOutput {
    @builtin(position) clip_pos:   vec4<f32>,
    @location(0)       world_z:    f32,
    @location(1)       edge_len:   f32,  // world-space length of this edge
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOutput {
    let edge_idx = vid / 2u;
    let endpoint = vid & 1u;
    let edge = edges[edge_idx];
    let node_idx = select(edge.x, edge.y, endpoint == 1u);
    let p = positions[node_idx];

    // Length is the same for both endpoints — pass to fragment for
    // length-based alpha falloff (long edges = transparent, avoid grey-mass).
    let p_src = positions[edge.x];
    let p_tgt = positions[edge.y];
    let edge_len = length(p_tgt - p_src);

    var out: VertexOutput;
    out.clip_pos = camera.view_proj * vec4<f32>(p, 1.0);
    out.world_z  = p.z;
    out.edge_len = edge_len;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Focus-plane attenuation
    let dist_z = abs(in.world_z - effects.focus_plane_z);
    let half_t = max(effects.focus_thickness * 0.5, 1.0);
    let in_focus = 1.0 - smoothstep(half_t, half_t * 2.0, dist_z);

    // Length-based alpha: short edges full alpha, long edges fade.
    // Reference length ≈ 50 world units; falls off gradually.
    let len_factor = 1.0 / (1.0 + in.edge_len / 30.0);
    let base_alpha = mix(0.05, 0.35, len_factor);

    let alpha = base_alpha * mix(0.10, 1.0, in_focus);
    return vec4<f32>(0.55, 0.62, 0.78, alpha);
}
