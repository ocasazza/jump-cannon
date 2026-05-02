// Edge (line) shader. Indexes into the same `positions` storage buffer
// the node shader reads from, so when the compute force-sim moves a node
// the edges automatically follow — no per-frame CPU rebuild of the edge
// vertex buffer.
//
// Draw call: `draw(0..(2 * n_edges), 0..1)`. Each pair of vertices forms
// one line; vertex N indexes edge N/2 and picks endpoint N&1.
//
// DoF: line primitives have fixed pixel width, so we fake "blur" by
// fading alpha as a function of the average-endpoint CoC.
//
// Layout:
//   group(0) binding(0)  uniform CameraUniform
//   group(0) binding(1)  uniform EffectsUniform
//   group(0) binding(2)  storage<read> array<vec3<f32>>  positions (vec3+pad)
//   group(0) binding(3)  storage<read> array<vec2<u32>>  edges (src,tgt)

struct CameraUniform {
    view_proj: mat4x4<f32>,
    view:      mat4x4<f32>,
    cam_pos:   vec3<f32>,
    _pad0:     f32,
    screen:    vec2<f32>,
    _pad1:     vec2<f32>,
};
// Layout mirrors the Rust `EffectsUniform` byte-for-byte. Keep in sync
// with graph_pipelines.rs and node.wgsl.
struct EffectsUniform {
    focus_plane_z:         f32,
    focus_thickness:       f32,
    cursor_radius_visual:  f32,
    blur_strength:         f32,
    max_coc:               f32,
    edge_alpha_mul:        f32,
    edge_dist_min:         f32,
    edge_dist_max:         f32,
    edge_color:            vec4<f32>,
    edge_min_transparency: f32,
    _pad0:                 f32,
    _pad1:                 f32,
    _pad2:                 f32,
};

@group(0) @binding(0) var<uniform> camera:  CameraUniform;
@group(0) @binding(1) var<uniform> effects: EffectsUniform;
@group(0) @binding(2) var<storage, read> positions: array<vec3<f32>>;
@group(0) @binding(3) var<storage, read> edges:     array<vec2<u32>>;

struct VertexOutput {
    @builtin(position) clip_pos:   vec4<f32>,
    @location(0)       world_z:    f32,
    @location(1)       edge_len:   f32,
    @location(2)       coc:        f32,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOutput {
    let edge_idx = vid / 2u;
    let endpoint = vid & 1u;
    let edge = edges[edge_idx];
    let node_idx = select(edge.x, edge.y, endpoint == 1u);
    let p = positions[node_idx];

    let p_src = positions[edge.x];
    let p_tgt = positions[edge.y];
    let edge_len = length(p_tgt - p_src);

    // Average-midpoint CoC for the whole edge — both endpoints share it
    // so the line's alpha is uniform along its length. Distance is measured
    // in view-space (perpendicular to camera look vector).
    let p_mid = 0.5 * (p_src + p_tgt);
    let view_pos = camera.view * vec4<f32>(p_mid, 1.0);
    let view_dist = -view_pos.z;
    let dz = abs(view_dist - effects.focus_plane_z);
    let half_t = max(effects.focus_thickness * 0.5, 0.001);
    let blur_z = max(dz - half_t, 0.0);
    let coc = min(blur_z * effects.blur_strength, effects.max_coc);

    var out: VertexOutput;
    out.clip_pos = camera.view_proj * vec4<f32>(p, 1.0);
    out.world_z  = p.z;
    out.edge_len = edge_len;
    out.coc      = coc;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Cosmograph linkVisibilityDistanceRange: short edges fully visible,
    // long edges fade toward edge_min_transparency (never disappearing
    // entirely — they keep building density on the dark background).
    let span = max(effects.edge_dist_max - effects.edge_dist_min, 0.001);
    let t = clamp((in.edge_len - effects.edge_dist_min) / span, 0.0, 1.0);
    let visibility = mix(1.0, effects.edge_min_transparency, t);

    // CoC fade — out-of-focus edges still drop alpha when DoF is engaged.
    let focus_atten = 1.0 / (1.0 + in.coc * 0.05);

    let alpha = effects.edge_color.a * effects.edge_alpha_mul
              * visibility * focus_atten;
    return vec4<f32>(effects.edge_color.rgb, alpha);
}
