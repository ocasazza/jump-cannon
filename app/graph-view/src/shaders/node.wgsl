// Node shader — screen-space billboarded points with circular SDF.
// Each node is rendered as a 6-vertex (2-triangle) quad in clip space.
// `sizes[i]` is interpreted as a pixel radius. The fragment shader
// discards anything outside the unit disc and smoothstep-AAs the edge.
//
// Depth-of-field (microscope bokeh, v2):
//   The focal band lives in VIEW SPACE: `focus_depth` is the radial
//   distance from the camera, `focus_thickness` the band's full width.
//   Depth error beyond the band becomes a circle-of-confusion in PIXELS
//   via the active projection's `proj_scale` (perspective divides by view
//   depth — near nodes blur faster than far ones; orthographic does
//   not). The vertex shader inflates the screen quad by `coc` pixels;
//   the fragment shader softens the SDF edge AND drops per-pixel
//   intensity by the area ratio (base² / effective²) so total light is
//   approximately conserved — bright sharp dot vs. dim soft disc.
//   FLAG_ATTR swaps the depth read for the per-node attribute buffer
//   (defocus-as-data): blur encodes distance from `focus_attr_center`.
//
// Depth cueing (fog): fragment contrast + alpha attenuate with
// smoothstep(fog_start, fog_end, view_dist) * fog_strength — PyMOL-style
// depth legibility for dense graphs, no bokeh artifacts.
//
// Clipping slab: FLAG_CLIP culls nodes whose view depth falls outside
// [clip_near, clip_far] — a section view through dense structure.
//
// Bindings:
//   group(0) binding(0)  uniform CameraUniform (includes screen + proj_scale)
//   group(0) binding(1)  uniform EffectsUniform
//   group(0) binding(2)  storage<read> array<vec3<f32>> positions
//   group(0) binding(3)  storage<read> array<vec4<f32>> colors
//   group(0) binding(4)  storage<read> array<f32>       sizes (pixels)
//   group(0) binding(5)  storage<read> array<f32>       dim_alpha
//   group(0) binding(6)  storage<read> array<u32>       shape_ids
//   group(0) binding(7)  storage<read> array<f32>       focus_attr [0,1]
//
// Draw call: draw(0..6, 0..n_nodes).

struct CameraUniform {
    view_proj:  mat4x4<f32>,
    view:       mat4x4<f32>,
    cam_pos:    vec3<f32>,
    // Vertical NDC-per-world-unit scale of the active projection
    // (perspective 1/tan(fov/2), ortho 1/half_height).
    proj_scale: f32,
    screen:     vec2<f32>,
    _pad1:      vec2<f32>,
};
// Layout mirrors the Rust `EffectsUniform` byte-for-byte. Keep in sync
// with pipelines.rs and edge.wgsl.
struct EffectsUniform {
    focus_depth:           f32,  // view-space radial focal distance
    focus_thickness:       f32,  // sharp band full width (view-space)
    cursor_radius_visual:  f32,
    aperture:              f32,  // CoC px per unit of relative depth error
    max_coc:               f32,
    edge_alpha_mul:        f32,
    edge_dist_min:         f32,
    edge_dist_max:         f32,
    edge_color:            vec4<f32>,
    edge_min_transparency: f32,
    edge_width:            f32,
    edge_fade_floor:       f32,
    shader_intensity:      f32,
    fog_start:             f32,
    fog_end:               f32,
    fog_strength:          f32,
    clip_near:             f32,
    clip_far:              f32,
    focus_attr_center:     f32,  // [0,1]
    _pad_v2:               vec2<f32>,
    flags:                 u32,
    hovered_node:          u32,
    hovered_edge:          u32,   // u32::MAX = no edge hover (unused in node.wgsl)
    _pad_hover:            u32,
};
// Keep in sync with FLAG_* in pipelines.rs.
const FLAG_DOF:   u32 = 1u;
const FLAG_FOG:   u32 = 2u;
const FLAG_CLIP:  u32 = 4u;
const FLAG_ATTR:  u32 = 8u;
const FLAG_ORTHO: u32 = 16u;

@group(0) @binding(0) var<uniform> camera:  CameraUniform;
@group(0) @binding(1) var<uniform> effects: EffectsUniform;
@group(0) @binding(2) var<storage, read> positions: array<vec3<f32>>;
@group(0) @binding(3) var<storage, read> colors:    array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> sizes:     array<f32>;
// Per-node focus dim factor in [0..1]. 1.0 = full alpha (in the focus
// set or no focus active), <1.0 = dimmed because focus mode is active
// and this node isn't in the focused community.
@group(0) @binding(5) var<storage, read> dim_alpha: array<f32>;
// Per-node shape primitive id. 0=circle, 1=square, 2=triangle,
// 3=diamond, 4=hexagon. Forwarded as a flat-interpolated vertex output
// so the fragment SDF switch is uniform across the sprite quad.
@group(0) @binding(6) var<storage, read> shape_ids: array<u32>;
// Per-node attribute-focus value in [0,1]. Read only when FLAG_ATTR is
// set; blur then encodes distance from `focus_attr_center` instead of
// view depth (defocus as a data channel).
@group(0) @binding(7) var<storage, read> focus_attr: array<f32>;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv:        vec2<f32>,
    @location(1) color:     vec4<f32>,
    @location(2) view_dist: f32,  // radial view-space depth (fog/clip)
    @location(3) coc_ratio: f32,  // base / effective; 1 = sharp, <1 = bokeh
    @location(4) @interpolate(flat) shape_id: u32,
    @location(5) @interpolate(flat) instance_id: u32,
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
    out.shape_id = shape_ids[iid];
    out.instance_id = iid;

    // Cheap rejects first — no transform math required for invisible
    // nodes. Saves a 4×4 mat-vec on culled instances.
    if (inst_color.a < 0.005 || base_size < 0.5) {
        out.clip_pos = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        out.view_dist = 0.0;
        out.coc_ratio = 1.0;
        return out;
    }

    let view_pos = camera.view * vec4<f32>(inst_pos, 1.0);
    let view_dist = -view_pos.z;
    out.view_dist = view_dist;

    var clip = camera.view_proj * vec4<f32>(inst_pos, 1.0);
    if (clip.w <= 0.0) {
        out.clip_pos = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        out.coc_ratio = 1.0;
        return out;
    }

    // Clipping slab: a point sprite lives entirely at its own depth, so
    // a vertex-stage cull is exact (no fragment discard needed).
    if ((effects.flags & FLAG_CLIP) != 0u) {
        if (view_dist < effects.clip_near || view_dist > effects.clip_far) {
            out.clip_pos = vec4<f32>(2.0, 2.0, 2.0, 1.0);
            out.coc_ratio = 1.0;
            return out;
        }
    }

    // CoC only runs when DoF is engaged (FLAG_DOF). coc_ratio = 1.0
    // means "sharp", which the fragment shader's fast path picks up.
    var effective_size = base_size;
    var coc_ratio = 1.0;
    if ((effects.flags & FLAG_DOF) != 0u) {
        // Depth-driven band, or attribute-driven band (defocus-as-data).
        var dz = abs(view_dist - effects.focus_depth);
        if ((effects.flags & FLAG_ATTR) != 0u) {
            // Attribute distance in [0,1] scaled into the depth band's
            // units so `focus_thickness` keeps one meaning.
            dz = abs(focus_attr[iid] - effects.focus_attr_center) * effects.focus_depth;
        }
        let half_t = max(effects.focus_thickness * 0.5, 0.001);
        let err = max(dz - half_t, 0.0);
        // World-space CoC → pixels through the active projection.
        // Perspective divides by view depth (near blurs faster); ortho
        // does not.
        var coc_px = effects.aperture * err * camera.proj_scale * camera.screen.y * 0.5;
        if ((effects.flags & FLAG_ORTHO) == 0u) {
            coc_px = coc_px / max(view_dist, 1.0);
        }
        let coc = min(coc_px, effects.max_coc);
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

// Per-shape signed-distance value normalised so `< 1.0` is interior,
// `1.0` is the edge, `> 1.0` is outside. All SDFs are computed in the
// quad's [-1, 1] uv space; size scaling lives entirely in the vertex
// stage. Using a single scalar lets the existing AA smoothstep work
// uniformly across shapes.
fn shape_sdf(uv: vec2<f32>, shape_id: u32) -> f32 {
    // 0: Circle — historical default.
    if (shape_id == 0u) {
        return length(uv);
    }
    // 1: Square — axis-aligned, fills the quad.
    if (shape_id == 1u) {
        return max(abs(uv.x), abs(uv.y));
    }
    // 2: Triangle — equilateral, point-up. Constants are 1/cos(30°)
    // and tan(30°) so the triangle inscribes the unit disc.
    if (shape_id == 2u) {
        let k = 1.1547005;  // 2 / sqrt(3) — scales so apex sits at uv.y = 1.
        let q = vec2<f32>(abs(uv.x) - 0.5 * (1.0 - uv.y), -uv.y);
        // Signed distance to the triangle, normalised so the bounding
        // disc maps to ~[0, 1.05]. Bounded below by 0 to avoid negative
        // SDF inside (the AA smoothstep only cares about the edge band).
        return max(q.x * 0.8660254 + q.y * 0.5, -uv.y) * k;
    }
    // 3: Diamond — L1 / taxicab distance.
    if (shape_id == 3u) {
        return abs(uv.x) + abs(uv.y);
    }
    // 4: Hexagon — point-up, flat top/bottom.
    if (shape_id == 4u) {
        let q = abs(uv);
        return max(q.x * 0.8660254 + 0.5 * q.y, q.y);
    }
    // Unknown shape id — fall back to circle.
    return length(uv);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let r = shape_sdf(in.uv, in.shape_id);
    if (r > 1.0) {
        discard;
    }

    // Hover treatment: when this fragment belongs to the hovered node,
    // (a) brighten the fill toward white, and (b) paint a hard white
    // inner rim in the annulus r ∈ [0.80, 0.95]. Painted inside the
    // existing SDF disc so we don't need to expand the quad. Reads as
    // an unmistakable selection ring on the hovered glyph.
    let is_hovered = in.instance_id == effects.hovered_node
                  && effects.hovered_node != 0xFFFFFFFFu;
    var fill_rgb = in.color.rgb;
    if (is_hovered) {
        fill_rgb = mix(in.color.rgb, vec3<f32>(1.0, 1.0, 1.0), 0.35);
        if (r > 0.80 && r < 0.95) {
            // White rim, opaque, multiplied by shader_intensity to
            // honor the global intensity scalar.
            return vec4<f32>(1.0, 1.0, 1.0, effects.shader_intensity);
        }
    }

    // Depth cueing: attenuate contrast (and alpha) with view-space
    // distance. Multiplicative — safe for the additive-ish stacked
    // blending both layers use.
    var fog_mul = 1.0;
    if ((effects.flags & FLAG_FOG) != 0u) {
        let f = smoothstep(effects.fog_start, effects.fog_end, in.view_dist)
              * effects.fog_strength;
        fog_mul = 1.0 - clamp(f, 0.0, 1.0);
    }

    let dof_engaged = (effects.flags & FLAG_DOF) != 0u;
    if (!dof_engaged || in.coc_ratio > 0.985) {
        let edge = 1.0 - smoothstep(0.96, 1.0, r);
        return vec4<f32>(
            fill_rgb * fog_mul,
            in.color.a * edge * fog_mul * effects.shader_intensity,
        );
    }

    // Bokeh path for actually out-of-focus nodes.
    let aa_start = max(mix(0.0, 0.95, in.coc_ratio), 0.85);
    let edge = 1.0 - smoothstep(aa_start, 1.0, r);

    // Energy conservation: spreading a bright point across a larger disc
    // means each pixel is dimmer. coc_ratio² ≈ area ratio.
    let intensity_mul = in.coc_ratio * in.coc_ratio;

    return vec4<f32>(
        fill_rgb * fog_mul,
        in.color.a * intensity_mul * edge * fog_mul * effects.shader_intensity,
    );
}
