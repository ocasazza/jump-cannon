//! egui_wgpu callback that drives our graph layer inside eframe's pass.
//!
//! GraphPipelines is registered into eframe's wgpu state's
//! CallbackResources at App::new (via cc.wgpu_render_state). Every frame
//! the App emits a single GraphCallback into the central panel's painter,
//! and egui_wgpu calls our prepare()/paint() with the same device/queue
//! egui itself uses.

use crate::graph_pipelines::GraphPipelines;

#[derive(Default)]
pub struct GraphCallback {
    pub screen_px: [f32; 2],
}

impl egui_wgpu::CallbackTrait for GraphCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(pipes) = resources.get_mut::<GraphPipelines>() {
            pipes.set_screen(self.screen_px);
            pipes.compute_step(device, queue, encoder);
            pipes.write_uniforms(queue, self.screen_px);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        rpass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(pipes) = resources.get::<GraphPipelines>() {
            pipes.draw(rpass);
        }
    }
}
