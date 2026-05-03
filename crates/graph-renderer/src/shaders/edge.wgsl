// Edge shader. Indexes into the same `positions` storage buffer the
// node shader reads from, so when the compute force-sim moves a node
// the edges automatically follow — no per-frame CPU rebuild of the
// edge vertex buffer.
//
// Fat-line draw call: `draw(0..(6 * n_edges), 0..1)`. Each edge becomes
// a screen-space quad of width `effects.edge_width` pixels:
//   vid layout (per edge): 0=src-, 1=tgt-, 2=tgt+, 3=src-, 4=tgt+, 5=src+
//   where ± is the offset perpendicular to the edge direction in clip
//   space, scaled to ½ pixel-width.
//
// DoF: per-fragment CoC is faked via average-endpoint distance to the
// focus plane; fat lines also stretch DoF naturally because the alpha
// fade interpolates across the quad.
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
    edge_width:            f32,   // pixels — fat-line half-width × 2
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
    // Per-edge quad: 6 verts → 2 tris.
    //   0=src-, 1=tgt-, 2=tgt+, 3=src-, 4=tgt+, 5=src+
    let edge_idx = vid / 6u;
    let corner   = vid % 6u;
    let endpoint = (corner == 1u) || (corner == 2u) || (corner == 4u);
    let side     = (corner == 2u) || (corner == 4u) || (corner == 5u);

    let edge = edges[edge_idx];
    let p_src = positions[edge.x];
    let p_tgt = positions[edge.y];
    let p     = select(p_src, p_tgt, endpoint);
    let edge_len = length(p_tgt - p_src);

    var out: VertexOutput;

    // LOD cull: at very long edge lengths the alpha-fade curve has settled
    // to its floor (`edge_min_transparency * edge_alpha_mul * color.a`) —
    // if that combined alpha falls below ~2 % the edge is invisible and
    // we just pay rasterization + alpha-blend ROP for nothing. Push the
    // endpoints outside the clip volume so the rasterizer drops the line.
    let floor_alpha = effects.edge_color.a
                    * effects.edge_alpha_mul
                    * effects.edge_min_transparency;
    let cull_thresh = effects.edge_dist_max * 4.0;
    if ((floor_alpha < 0.02 && edge_len > effects.edge_dist_max)
        || edge_len > cull_thresh) {
        out.clip_pos = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        out.world_z  = 0.0;
        out.edge_len = edge_len;
        out.coc      = 0.0;
        return out;
    }

    // CoC computation only runs when DoF is engaged (focus_thickness <
    // 1e6 sentinel). Skipping saves a mat-vec + a handful of scalar ops
    // per edge endpoint when DoF is off (the default).
    var coc = 0.0;
    if (effects.focus_thickness < 1.0e6) {
        let p_mid = 0.5 * (p_src + p_tgt);
        let view_pos = camera.view * vec4<f32>(p_mid, 1.0);
        let view_dist = -view_pos.z;
        let dz = abs(view_dist - effects.focus_plane_z);
        let half_t = max(effects.focus_thickness * 0.5, 0.001);
        let blur_z = max(dz - half_t, 0.0);
        coc = min(blur_z * effects.blur_strength, effects.max_coc);
    }

    // Project both endpoints to clip space, perform the perpendicular
    // expansion in NDC, then write the offset clip position back. This
    // gives a constant *pixel* width regardless of camera distance.
    let clip_src = camera.view_proj * vec4<f32>(p_src, 1.0);
    let clip_tgt = camera.view_proj * vec4<f32>(p_tgt, 1.0);
    let clip_p   = select(clip_src, clip_tgt, endpoint);

    // To NDC for the screen-space perpendicular calc.
    let ndc_src = clip_src.xy / max(clip_src.w, 1e-4);
    let ndc_tgt = clip_tgt.xy / max(clip_tgt.w, 1e-4);
    var dir = ndc_tgt - ndc_src;
    let dlen = max(length(dir), 1e-4);
    dir = dir / dlen;
    // Perpendicular in screen space (xy plane).
    var perp = vec2<f32>(-dir.y, dir.x);
    // Account for non-square viewport so the offset is true pixel-width.
    let aspect = camera.screen.x / max(camera.screen.y, 1.0);
    perp.x = perp.x / aspect;
    let half_w_px = max(effects.edge_width, 0.0) * 0.5;
    let half_w_ndc = vec2<f32>(
        2.0 * half_w_px / max(camera.screen.x, 1.0),
        2.0 * half_w_px / max(camera.screen.y, 1.0),
    );
    let signed = select(-1.0, 1.0, side);
    let offset_ndc = perp * vec2<f32>(half_w_ndc.x * aspect, half_w_ndc.y) * signed;
    // Re-multiply by w so perspective interpolation stays correct.
    let clip_offset = vec4<f32>(
        offset_ndc.x * clip_p.w,
        offset_ndc.y * clip_p.w,
        0.0,
        0.0,
    );

    out.clip_pos = clip_p + clip_offset;
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
    // Skip near-invisible fragments — saves alpha-blend ROP cost on dense
    // overdrawn edge bundles. Threshold is chosen well below visible.
    if (alpha < 0.02) {
        discard;
    }
    return vec4<f32>(effects.edge_color.rgb, alpha);
}
