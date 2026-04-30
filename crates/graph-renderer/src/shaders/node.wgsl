struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_pos:   vec3<f32>,
    _pad:      f32,
};
@group(0) @binding(0) var<uniform> camera: CameraUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal:   vec3<f32>,
};
struct InstanceInput {
    @location(2) inst_pos:   vec3<f32>,
    @location(3) inst_color: vec4<f32>,
    @location(4) inst_size:  f32,
};
struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_normal:   vec3<f32>,
    @location(1) color:          vec4<f32>,
};

@vertex
fn vs_main(v: VertexInput, inst: InstanceInput) -> VertexOutput {
    var out: VertexOutput;
    let world_pos = inst.inst_pos + v.position * inst.inst_size;
    out.clip_pos = camera.view_proj * vec4<f32>(world_pos, 1.0);
    out.world_normal = v.normal;
    out.color = inst.inst_color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let l = normalize(vec3<f32>(0.5, 0.7, 1.0));
    let lambert = max(dot(n, l), 0.0) * 0.7 + 0.3;
    return vec4<f32>(in.color.rgb * lambert, in.color.a);
}
