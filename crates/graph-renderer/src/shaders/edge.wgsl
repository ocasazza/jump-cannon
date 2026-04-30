struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_pos:   vec3<f32>,
    _pad:      f32,
};
@group(0) @binding(0) var<uniform> camera: CameraUniform;

@vertex
fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return camera.view_proj * vec4<f32>(position, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.45, 0.45, 0.55, 0.4);
}
