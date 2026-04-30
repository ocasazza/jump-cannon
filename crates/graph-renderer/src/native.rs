//! Native binary: spins up a winit window, hands its surface to the shared
//! `Renderer`, and runs an event loop that fans key/mouse events through
//! `InputState`. Intended for quick iteration without booting a browser.

use std::sync::Arc;

use graph_renderer::input::{InputState, Key};
use graph_renderer::renderer::{GraphData, RendererConfig};
use graph_renderer::Renderer;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

fn main() {
    env_logger::init();
    let event_loop = EventLoop::new().unwrap();
    let mut app = App::default();
    event_loop.run_app(&mut app).unwrap();
}

#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    input: InputState,
    last_t: Option<std::time::Instant>,
    last_cursor: Option<(f64, f64)>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window_attrs = Window::default_attributes().with_title("graph-renderer");
        let window = Arc::new(event_loop.create_window(window_attrs).unwrap());
        self.window = Some(window.clone());

        // Demo graph: triangle of three nodes, three edges.
        let graph = GraphData {
            positions: vec![-100.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 100.0, 0.0],
            edges: vec![0, 1, 1, 2, 2, 0],
            colors: vec![
                0.9, 0.3, 0.3, 1.0, 0.3, 0.9, 0.3, 1.0, 0.3, 0.3, 0.9, 1.0,
            ],
            sizes: vec![10.0, 10.0, 10.0],
        };

        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone()).unwrap();
        let size = window.inner_size();
        let renderer = pollster::block_on(Renderer::new(
            instance,
            surface,
            RendererConfig {
                width: size.width,
                height: size.height,
            },
            graph,
        ))
        .unwrap();
        self.renderer = Some(renderer);
        self.last_t = Some(std::time::Instant::now());
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput {
                event: kev, ..
            } => {
                if let PhysicalKey::Code(code) = kev.physical_key {
                    let mapped = match code {
                        KeyCode::KeyW => Some(Key::W),
                        KeyCode::KeyA => Some(Key::A),
                        KeyCode::KeyS => Some(Key::S),
                        KeyCode::KeyD => Some(Key::D),
                        KeyCode::KeyQ => Some(Key::Q),
                        KeyCode::KeyE => Some(Key::E),
                        KeyCode::KeyR => Some(Key::R),
                        KeyCode::KeyF => Some(Key::F),
                        KeyCode::ShiftLeft | KeyCode::ShiftRight => Some(Key::Shift),
                        _ => None,
                    };
                    if let Some(k) = mapped {
                        match kev.state {
                            ElementState::Pressed => self.input.press(k),
                            ElementState::Released => self.input.release(k),
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    self.input.mouse_dragging = state == ElementState::Pressed;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = (position.x, position.y);
                if let Some(prev) = self.last_cursor {
                    if self.input.mouse_dragging {
                        self.input.mouse_delta.0 += (p.0 - prev.0) as f32;
                        self.input.mouse_delta.1 += (p.1 - prev.1) as f32;
                    }
                }
                self.last_cursor = Some(p);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 50.0,
                };
                self.input.wheel_delta += dy;
            }
            WindowEvent::RedrawRequested => {
                let now = std::time::Instant::now();
                let dt = self
                    .last_t
                    .map(|t| (now - t).as_secs_f32())
                    .unwrap_or(0.0)
                    .min(0.1);
                self.last_t = Some(now);

                if let Some(r) = &mut self.renderer {
                    self.input.apply_to_camera(&mut r.camera, dt);
                    if let Err(e) = r.render() {
                        log::warn!("render: {e:?}");
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}
