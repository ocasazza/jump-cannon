// Node (sphere) shader. Reads positions/colors/sizes from storage buffers
// shared with the graph-layouts compute pipeline — no per-frame CPU copy.
//
// Layout:
//   group(0) binding(0)  uniform CameraUniform
//   group(0) binding(1)  uniform EffectsUniform (focus plane)
//   group(0) binding(2)  storage<read> array<vec3<f32>>  positions  (vec3+pad)
//   group(0) binding(3)  storage<read> array<vec4<f32>>  colors
//   group(0) binding(4)  storage<read> array<f32>        sizes

struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_pos:   vec3<f32>,
    _pad:      f32,
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

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal:   vec3<f32>,
};
struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_normal:   vec3<f32>,
    @location(1) color:          vec4<f32>,
    @location(2) world_z:        f32,
};

@vertex
fn vs_main(v: VertexInput, @builtin(instance_index) iid: u32) -> VertexOutput {
    var out: VertexOutput;
    let inst_pos   = positions[iid];
    let inst_color = colors[iid];
    let inst_size  = sizes[iid];
    let world_pos = inst_pos + v.position * inst_size;
    out.clip_pos = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.world_normal = v.normal;
    out.color = inst_color;
    out.world_z = world_pos.z;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let l = normalize(vec3<f32>(0.5, 0.7, 1.0));
    let lambert = max(dot(n, l), 0.0) * 0.7 + 0.3;

    // Microscope focus plane: nodes outside the focus band fade to 15% alpha.
    let dist_z = abs(in.world_z - effects.focus_plane_z);
    let half_t = max(effects.focus_thickness * 0.5, 1.0);
    let in_focus = 1.0 - smoothstep(half_t, half_t * 2.0, dist_z);
    let alpha_mul = mix(0.15, 1.0, in_focus);

    return vec4<f32>(in.color.rgb * lambert, in.color.a * alpha_mul);
}
