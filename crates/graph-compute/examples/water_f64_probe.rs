//! Probe whether this adapter exposes `wgpu::Features::SHADER_F64` (native
//! WGSL f64). The water-sheet `F64` mode requires it; `F64Pos` does not use it
//! (it stores positions in 64-bit fixed-point u32 pairs).
//!
//! ```text
//! cargo run --release -p graph-compute --example water_f64_probe
//! ```

use graph_compute::EngineCtx;

fn main() {
    let ctx = EngineCtx::try_new_gpu();
    let Some(gpu) = ctx.gpu else {
        println!("probe: no wgpu adapter");
        return;
    };
    println!(
        "adapter: {} ({:?})",
        gpu.adapter_info.name, gpu.adapter_info.backend
    );

    // `build_gpu_ctx` requests the device with `Features::empty()`, so a
    // device-level check of the SHARED ctx device is not sufficient: re-request
    // the adapter and ask for the feature explicitly.
    let backends = wgpu::util::backend_bits_from_env().unwrap_or_else(wgpu::Backends::all);
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::util::power_preference_from_env()
            .unwrap_or(wgpu::PowerPreference::HighPerformance),
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("adapter (ctx already found one)");
    let limits = adapter.limits();
    let device = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("water_f64_probe"),
        required_features: wgpu::Features::SHADER_F64,
        required_limits: wgpu::Limits {
            max_storage_buffer_binding_size: limits.max_storage_buffer_binding_size,
            max_buffer_size: limits.max_buffer_size,
            ..wgpu::Limits::default()
        },
        memory_hints: wgpu::MemoryHints::Performance,
    }, None));
    match device {
        Ok(device) => {
            println!("probe: SHADER_F64 supported = true");
            let _ = device;
        }
        Err(e) => {
            println!("probe: SHADER_F64 supported = false");
            eprintln!("  request_device(SHADER_F64) error: {e}");
        }
    }
}
