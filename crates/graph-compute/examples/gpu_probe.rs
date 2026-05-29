//! GPU probe: reports whether this host can bring up a wgpu adapter via the
//! exact path the layout engines use (`EngineCtx::try_new_gpu`), and lists every
//! adapter wgpu can see across all backends. Diagnoses whether the GPU layout
//! engines (`fa2-brute`, `fa2-bh`) can run here vs. falling back to CPU engines.
//!
//! Usage: `cargo run -p graph-compute --example gpu_probe`
//! (Under the command sandbox no adapter is visible; run with the sandbox off.)

use graph_compute::EngineCtx;

fn main() {
    // Enumerate every adapter across all backends (Metal / Vulkan / DX12 / GL),
    // so this shows Apple, NVIDIA, AMD, and Intel GPUs regardless of platform.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    let all = instance.enumerate_adapters(wgpu::Backends::all());
    println!("wgpu sees {} adapter(s):", all.len());
    for (i, a) in all.iter().enumerate() {
        let info = a.get_info();
        println!(
            "  [{i}] {:?} | {} | {:?} | driver: {} {}",
            info.backend, info.name, info.device_type, info.driver, info.driver_info
        );
    }

    // Exercise the real engine path: high-performance adapter selection.
    match EngineCtx::try_new_gpu().gpu {
        Some(gpu) => {
            let a = &gpu.adapter_info;
            println!(
                "\nEngineCtx::try_new_gpu => GPU AVAILABLE: {:?} | {} | {:?}",
                a.backend, a.name, a.device_type
            );
            println!("GPU layout engines (fa2-brute, fa2-bh) CAN run on this host.");
        }
        None => println!("\nEngineCtx::try_new_gpu => NO GPU (CPU engines only)."),
    }
}
