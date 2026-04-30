// Node shader — screen-space billboarded points with circular SDF.
// Each node is rendered as a 6-vertex (2-triangle) quad in clip space.
// `sizes[i]` is interpreted as a pixel radius. The fragment shader
// discards anything outside the unit disc and smoothstep-AAs the edge.
//
// Bindings:
//   group(0) binding(0)  uniform CameraUniform (now includes screen)
//   group(0) binding(1)  uniform EffectsUniform (focus plane)
//   group(0) binding(2)  storage<read> array<vec3<f32>> positions
//   group(0) binding(3)  storage<read> array<vec4<f32>> colors
//   group(0) binding(4)  storage<read> array<f32>       sizes (pixels)
//
// Draw call: draw(0..6, 0..n_nodes).

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
@group(0) @binding(3) var<storage, read> colors:    array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> sizes:     array<f32>;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv:      vec2<f32>,
    @location(1) color:   vec4<f32>,
    @location(2) world_z: f32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VertexOutput {
    // Two triangles forming a [-1,1]² quad. Vertex order: (0,0)=(-1,-1),
    // (1,0)=(1,-1), (2,0)=(-1,1), then (1,1)=(1,-1), (1,2)=(1,1), (1,1)=(-1,1).
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
    let inst_color = colors[iid];
    let inst_size  = sizes[iid];

    var clip = camera.view_proj * vec4<f32>(inst_pos, 1.0);

    // Offset in NDC. Multiply by clip.w so the perspective divide cancels
    // out and the on-screen pixel size stays constant regardless of depth.
    let screen = max(camera.screen, vec2<f32>(1.0, 1.0));
    let px = vec2<f32>(inst_size, inst_size) / screen * 2.0;
    clip = vec4<f32>(clip.xy + corner * px * clip.w, clip.zw);

    var out: VertexOutput;
    out.clip_pos = clip;
    out.uv = corner;
    out.color = inst_color;
    out.world_z = inst_pos.z;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let r = length(in.uv);
    if (r > 1.0) {
        discard;
    }
    let edge_aa = 1.0 - smoothstep(0.95, 1.0, r);

    // Microscope focus plane: nodes outside the focus band fade to 15% alpha.
    let dist_z = abs(in.world_z - effects.focus_plane_z);
    let half_t = max(effects.focus_thickness * 0.5, 1.0);
    let in_focus = 1.0 - smoothstep(half_t, half_t * 2.0, dist_z);
    let alpha_mul = mix(0.15, 1.0, in_focus);

    return vec4<f32>(in.color.rgb, in.color.a * alpha_mul * edge_aa);
}
