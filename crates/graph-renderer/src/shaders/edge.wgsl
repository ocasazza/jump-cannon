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
    edge_fade_floor:       f32,   // long-distance asymptotic alpha floor
    shader_intensity:      f32,   // post-process visual-intensity scalar
    hovered_node:          u32,   // u32::MAX = no hover (unused in edge.wgsl)
    _pad_hover0:           u32,
    _pad_hover1:           u32,
    _pad_hover2:           u32,
};

@group(0) @binding(0) var<uniform> camera:  CameraUniform;
@group(0) @binding(1) var<uniform> effects: EffectsUniform;
@group(0) @binding(2) var<storage, read> positions: array<vec3<f32>>;
@group(0) @binding(3) var<storage, read> edges:     array<vec2<u32>>;
// Per-node focus dim factor (1.0 = in focus set / no focus active).
// Edge alpha is multiplied by a function of (dim_src, dim_tgt):
//   both 1.0          → 1.0   (full)
//   exactly one 1.0   → 0.6   (mid)
//   neither           → 0.15  (dim)
@group(0) @binding(4) var<storage, read> dim_alpha: array<f32>;
// Per-edge RGBA multiplier. All-1.0 when EdgeColorBy::None so the
// uniform `edge_color` flows through unchanged; otherwise the
// community/folder/doctype swatch for edges whose endpoints share
// that bucket, with `edge_color` as the bridging-edge fallback.
@group(0) @binding(5) var<storage, read> edge_colors: array<vec4<f32>>;

struct VertexOutput {
    @builtin(position) clip_pos:   vec4<f32>,
    @location(0)       world_z:    f32,
    @location(1)       edge_len:   f32,
    @location(2)       coc:        f32,
    @location(3)       focus_mul:  f32,
    @location(4)       tint:       vec4<f32>,
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

    // Focus-set membership combinator (per spec). 1.0 sentinel = "in set".
    // Special-case: an endpoint with dim_alpha < 0.001 means **filtered
    // out** by the active filter chip selection — collapse focus_mul to
    // 0 so the fragment-shader discard at the bottom culls the edge
    // entirely. Distinct from the 0.25 community-dim path which keeps
    // edges visible but faded.
    let d_s = dim_alpha[edge.x];
    let d_t = dim_alpha[edge.y];
    let s_in = d_s >= 0.999;
    let t_in = d_t >= 0.999;
    var focus_mul: f32 = 1.0;
    if (d_s < 0.001 || d_t < 0.001) {
        focus_mul = 0.0;
    } else if (s_in && t_in) {
        focus_mul = 1.0;
    } else if (s_in || t_in) {
        focus_mul = 0.6;
    } else {
        focus_mul = min(d_s, d_t);
    }

    var out: VertexOutput;
    out.focus_mul = focus_mul;

    // No hard distance cull: long edges fade asymptotically toward
    // `edge_fade_floor` in the fragment shader. Hard culls produced
    // visible popping when nodes drifted across the boundary mid-sim.

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

    // To NDC for the screen-space perpendicular calc. If either endpoint
    // sits at or behind the camera (clip.w ≤ 0) the perspective divide
    // flips signs and the perpendicular spins, producing a single-frame
    // flash across the screen. Guard with a positive-w clamp and zero the
    // expansion when both endpoints are degenerate.
    let w_src = max(clip_src.w, 1e-3);
    let w_tgt = max(clip_tgt.w, 1e-3);
    let w_p   = max(clip_p.w,   1e-3);
    let ndc_src = clip_src.xy / w_src;
    let ndc_tgt = clip_tgt.xy / w_tgt;
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
    // Skip the expansion when the projected vertex is at/behind the
    // camera — keeps the geometry stable instead of flicker-large.
    let w_ok = (clip_src.w > 0.0) && (clip_tgt.w > 0.0) && (clip_p.w > 0.0);
    let off_xy = select(vec2<f32>(0.0, 0.0), offset_ndc, w_ok);
    // Re-multiply by w so perspective interpolation stays correct.
    let clip_offset = vec4<f32>(
        off_xy.x * w_p,
        off_xy.y * w_p,
        0.0,
        0.0,
    );

    out.clip_pos = clip_p + clip_offset;
    out.world_z  = p.z;
    out.edge_len = edge_len;
    out.coc      = coc;
    out.tint     = edge_colors[edge_idx];
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Two-stage continuous fade — no hard cull, no popping.
    //
    //   stage 1: edge_len in [dist_min, dist_max]
    //            visibility goes 1.0 → edge_min_transparency linearly.
    //   stage 2: edge_len in [dist_max, dist_max * 5]
    //            smoothstep-fades from edge_min_transparency down to
    //            edge_fade_floor (an absolute, non-zero floor, e.g.
    //            0.02 of color.a). C¹-continuous at dist_max.
    //   stage 3: edge_len > dist_max * 5
    //            asymptotic 1/(1+x) tail toward edge_fade_floor —
    //            never reaches zero, never discontinuous.
    let dist_min = effects.edge_dist_min;
    let dist_max = effects.edge_dist_max;
    let span1 = max(dist_max - dist_min, 0.001);
    let t1 = clamp((in.edge_len - dist_min) / span1, 0.0, 1.0);
    let near_vis = mix(1.0, effects.edge_min_transparency, t1);

    let span2 = max(dist_max * 4.0, 0.001); // 4× run after dist_max
    let t2 = clamp((in.edge_len - dist_max) / span2, 0.0, 1.0);
    let s2 = t2 * t2 * (3.0 - 2.0 * t2); // smoothstep
    let mid_vis = mix(effects.edge_min_transparency, effects.edge_fade_floor, s2);

    // Asymptotic tail: ratio r ∈ [0, ∞) past the 5× boundary.
    let tail_start = dist_max * 5.0;
    let r = max((in.edge_len - tail_start) / max(dist_max, 0.001), 0.0);
    // 1/(1+r) shrinks from 1 → 0; remap so we approach edge_fade_floor.
    let tail_factor = 1.0 / (1.0 + r);
    let tail_vis = effects.edge_fade_floor * tail_factor
                 + effects.edge_fade_floor * (1.0 - tail_factor) * 0.5;

    // Pick the active stage without branching.
    let in_near = f32(in.edge_len <= dist_max);
    let in_mid  = f32((in.edge_len > dist_max) && (in.edge_len <= tail_start));
    let in_tail = f32(in.edge_len > tail_start);
    let visibility = in_near * near_vis + in_mid * mid_vis + in_tail * tail_vis;

    // CoC fade — out-of-focus edges still drop alpha when DoF is engaged.
    let focus_atten = 1.0 / (1.0 + in.coc * 0.05);

    // Per-edge color: `tint.rgb` is the absolute edge color (the
    // community swatch when endpoints share a bucket, otherwise the
    // uniform `edge_color` fallback for bridging edges, OR the uniform
    // `edge_color` for every edge when EdgeColorBy::None). `tint.a` is
    // the source-alpha multiplier — equal to `effects.edge_color.a` so
    // existing alpha logic stays unchanged.
    let base_rgb = in.tint.rgb;
    let base_a   = in.tint.a;
    let alpha = base_a * effects.edge_alpha_mul
              * visibility * focus_atten * in.focus_mul
              * effects.shader_intensity;
    // Threshold is below perceptual range — kept only to skip pure-zero
    // ROP work, NOT to hide long edges. No popping at this level.
    if (alpha < 1.0e-4) {
        discard;
    }
    return vec4<f32>(base_rgb, alpha);
}
