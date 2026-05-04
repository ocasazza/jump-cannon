// Node shader — screen-space billboarded points with circular SDF.
// Each node is rendered as a 6-vertex (2-triangle) quad in clip space.
// `sizes[i]` is interpreted as a pixel radius. The fragment shader
// discards anything outside the unit disc and smoothstep-AAs the edge.
//
// Depth-of-field (microscope bokeh):
//   Out-of-focus nodes spread into a circle-of-confusion (CoC). The vertex
//   shader inflates the screen quad by `coc` pixels; the fragment shader
//   softens the SDF edge over a wider transition AND drops per-pixel
//   intensity by the area ratio (base² / effective²) so total light is
//   approximately conserved — bright sharp dot vs. dim soft disc.
//
// Bindings:
//   group(0) binding(0)  uniform CameraUniform (now includes screen)
//   group(0) binding(1)  uniform EffectsUniform (focus plane + DoF)
//   group(0) binding(2)  storage<read> array<vec3<f32>> positions
//   group(0) binding(3)  storage<read> array<vec4<f32>> colors
//   group(0) binding(4)  storage<read> array<f32>       sizes (pixels)
//
// Draw call: draw(0..6, 0..n_nodes).

struct CameraUniform {
    view_proj: mat4x4<f32>,
    view:      mat4x4<f32>,
    cam_pos:   vec3<f32>,
    _pad0:     f32,
    screen:    vec2<f32>,
    _pad1:     vec2<f32>,
};
// Layout mirrors the Rust `EffectsUniform` byte-for-byte. Keep in sync
// with graph_pipelines.rs and edge.wgsl.
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
    edge_width:            f32,
    edge_fade_floor:       f32,
    _pad2:                 f32,
};

@group(0) @binding(0) var<uniform> camera:  CameraUniform;
@group(0) @binding(1) var<uniform> effects: EffectsUniform;
@group(0) @binding(2) var<storage, read> positions: array<vec3<f32>>;
@group(0) @binding(3) var<storage, read> colors:    array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> sizes:     array<f32>;
// Per-node focus dim factor in [0..1]. 1.0 = full alpha (in the focus
// set or no focus active), <1.0 = dimmed because focus mode is active
// and this node isn't in the focused community.
@group(0) @binding(5) var<storage, read> dim_alpha: array<f32>;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv:        vec2<f32>,
    @location(1) color:     vec4<f32>,
    @location(2) world_z:   f32,
    @location(3) coc_ratio: f32,  // base / effective; 1 = sharp, <1 = bokeh
};

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
    );
    let corner = corners[vid];

    let inst_pos   = positions[iid];
    var inst_color = colors[iid];
    let base_size  = sizes[iid];
    // Apply per-node focus dim factor (multiplicative on alpha).
    let dim = dim_alpha[iid];
    inst_color.a = inst_color.a * dim;

    var out: VertexOutput;
    out.uv = corner;
    out.color = inst_color;
    out.world_z = inst_pos.z;

    // Cheap rejects first — no transform math required for invisible
    // nodes. Saves a 4×4 mat-vec on culled instances.
    if (inst_color.a < 0.005 || base_size < 0.5) {
        out.clip_pos = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        out.coc_ratio = 1.0;
        return out;
    }

    var clip = camera.view_proj * vec4<f32>(inst_pos, 1.0);
    if (clip.w <= 0.0) {
        out.clip_pos = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        out.coc_ratio = 1.0;
        return out;
    }

    // DoF off (the default — focus_thickness >= 1e6) skips the CoC
    // computation entirely. coc_ratio = 1.0 means "sharp", which the
    // fragment shader's fast path picks up.
    var effective_size = base_size;
    var coc_ratio = 1.0;
    if (effects.focus_thickness < 1.0e6) {
        let view_pos = camera.view * vec4<f32>(inst_pos, 1.0);
        let view_dist = -view_pos.z;
        let dz = abs(view_dist - effects.focus_plane_z);
        let half_t = max(effects.focus_thickness * 0.5, 0.001);
        let blur_z = max(dz - half_t, 0.0);
        let coc = min(blur_z * effects.blur_strength, effects.max_coc);
        effective_size = base_size + coc;
        coc_ratio = base_size / max(effective_size, 1.0);
    }
    out.coc_ratio = coc_ratio;

    let screen = max(camera.screen, vec2<f32>(1.0, 1.0));
    let px = vec2<f32>(effective_size, effective_size) / screen * 2.0;
    clip = vec4<f32>(clip.xy + corner * px * clip.w, clip.zw);
    out.clip_pos = clip;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let r = length(in.uv);
    if (r > 1.0) {
        discard;
    }

    // DoF is "off" by default — focus_thickness is set to ~1e9 so the
    // whole scene sits inside the focus band. In that mode every node
    // takes the sharp path (cosmograph-style hard SDF disc, no halo).
    // Bokeh only kicks in when the caller explicitly narrows the band.
    let dof_engaged = effects.focus_thickness < 1.0e6;
    if (!dof_engaged || in.coc_ratio > 0.985) {
        let edge = 1.0 - smoothstep(0.96, 1.0, r);
        return vec4<f32>(in.color.rgb, in.color.a * edge);
    }

    // Bokeh path for actually out-of-focus nodes.
    let aa_start = max(mix(0.0, 0.95, in.coc_ratio), 0.85);
    let edge = 1.0 - smoothstep(aa_start, 1.0, r);

    // Energy conservation: spreading a bright point across a larger disc
    // means each pixel is dimmer. coc_ratio² ≈ area ratio.
    let intensity_mul = in.coc_ratio * in.coc_ratio;

    return vec4<f32>(in.color.rgb, in.color.a * intensity_mul * edge);
}
