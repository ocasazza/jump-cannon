use egui::{Color32, Stroke, Pos2, Vec2};
use shared::types::Graph;
use crate::app::{App, AnimationState, FileUploadState};
use crate::utils::hex_to_color32;
use crate::layout;
use crate::file_parser;

impl App {
    /// Apply the current layout algorithm to the graph
    pub fn apply_layout(&mut self) {
        // Check if animation is enabled
        let animate = self.layout.base_options().animate;
        let animation_duration = self.layout.base_options().animation_duration;
        
        if animate {
            // Store the initial positions of the nodes
            let mut initial_positions = std::collections::HashMap::new();
            for (id, node) in &self.graph.nodes {
                initial_positions.insert(id.clone(), node.position);
            }
            
            // Create a clone of the graph to calculate the final positions
            let mut final_graph = self.graph.clone();
            
            // Apply the layout to the cloned graph to get final positions
            if let Err(e) = layout::apply_layout(&mut final_graph, &self.layout) {
                eprintln!("Layout error: {}", e);
                // If layout fails, fall back to non-animated layout
                if let Err(e) = layout::apply_layout(&mut self.graph, &self.layout) {
                    eprintln!("Layout error: {}", e);
                }
            } else {
                // Store the final positions
                let mut final_positions = std::collections::HashMap::new();
                for (id, node) in &final_graph.nodes {
                    final_positions.insert(id.clone(), node.position);
                }
                
                // Set up animation state
                let start_time = crate::app::Instant::now();
                
                // Store animation state in the App struct
                self.animation_state = Some(AnimationState {
                    start_time,
                    duration: animation_duration,
                    initial_positions,
                    final_positions,
                });
            }
        } else {
            // If animation is disabled, just apply the layout directly
            if let Err(e) = layout::apply_layout(&mut self.graph, &self.layout) {
                eprintln!("Layout error: {}", e);
            }
        }
        
        // Only center the graph if auto_center is true AND this is the first layout application
        // or if explicitly requested by the user via the "Reset View" button
        if self.auto_center && !self.layout_applied {
            self.center_graph();
            self.layout_applied = true;
        }
    }
    
    /// Update animation state if an animation is in progress
    pub fn update_animation(&mut self) -> bool {
        if let Some(animation_state) = &self.animation_state {
            #[cfg(not(target_arch = "wasm32"))]
            let elapsed = animation_state.start_time.elapsed().as_millis() as u32;
            
            #[cfg(target_arch = "wasm32")]
            let elapsed = animation_state.start_time.elapsed() as u32;
            
            if elapsed >= animation_state.duration {
                // Animation is complete, apply final positions
                for (id, final_pos) in &animation_state.final_positions {
                    if let Some(node) = self.graph.nodes.get_mut(id) {
                        node.position = *final_pos;
                    }
                }
                
                // Clear animation state
                self.animation_state = None;
                
                // Animation is complete
                return false;
            } else {
                // Animation is in progress, interpolate positions
                let progress = elapsed as f64 / animation_state.duration as f64;
                
                for (id, final_pos) in &animation_state.final_positions {
                    if let Some(node) = self.graph.nodes.get_mut(id) {
                        let initial_pos = animation_state.initial_positions.get(id).unwrap_or(&None);
                        
                        if let (Some(initial), Some(final_pos)) = (initial_pos, final_pos) {
                            // Linear interpolation between initial and final positions
                            let x = initial.0 + (final_pos.0 - initial.0) * progress;
                            let y = initial.1 + (final_pos.1 - initial.1) * progress;
                            node.position = Some((x, y));
                        } else if let Some(final_pos) = final_pos {
                            // If initial position is None, just use the final position
                            node.position = Some(*final_pos);
                        }
                    }
                }
                
                // Animation is still in progress
                return true;
            }
        }
        
        // No animation in progress
        false
    }
    
    /// Apply zoom at a specific point
    pub fn apply_zoom(&mut self, pos: egui::Pos2, zoom_factor: f64) {
        // Get the position under the cursor in graph coordinates
        let graph_x = (pos.x as f64 - self.viewport.pan_x) / self.viewport.zoom;
        let graph_y = (pos.y as f64 - self.viewport.pan_y) / self.viewport.zoom;
        
        // Apply zoom
        self.viewport.zoom *= zoom_factor;
        
        // Limit zoom range to prevent extreme values
        self.viewport.zoom = self.viewport.zoom.clamp(0.1, 10.0);
        
        // Adjust pan to keep the point under cursor fixed
        self.viewport.pan_x = pos.x as f64 - graph_x * self.viewport.zoom;
        self.viewport.pan_y = pos.y as f64 - graph_y * self.viewport.zoom;
    }
    
    /// Center the graph in the viewport
    pub fn center_graph(&mut self) {
        if self.graph.nodes.is_empty() {
            return;
        }
        
        // Find the bounding box of all nodes
        let mut min_x = f64::MAX;
        let mut min_y = f64::MAX;
        let mut max_x = f64::MIN;
        let mut max_y = f64::MIN;
        
        for node in self.graph.nodes.values() {
            if let Some(pos) = node.position {
                min_x = min_x.min(pos.0);
                min_y = min_y.min(pos.1);
                max_x = max_x.max(pos.0);
                max_y = max_y.max(pos.1);
            }
        }
        
        // Calculate the center of the graph
        let graph_center_x = (min_x + max_x) / 2.0;
        let graph_center_y = (min_y + max_y) / 2.0;
        
        // Calculate the size of the graph
        let graph_width = max_x - min_x;
        let graph_height = max_y - min_y;
        
        // Adjust zoom to fit the graph if it's too large
        // We'll use a fixed screen size for now, but ideally this would be dynamic
        let screen_width = 800.0;
        let screen_height = 600.0;
        
        // Calculate zoom to fit the graph with some padding
        let padding = 50.0;
        let zoom_x = (screen_width - padding * 2.0) / graph_width.max(1.0);
        let zoom_y = (screen_height - padding * 2.0) / graph_height.max(1.0);
        
        // Use the smaller zoom to ensure the entire graph fits
        if self.layout.base_options().fit {
            self.viewport.zoom = zoom_x.min(zoom_y);
        }
        
        // Calculate the center of the screen
        let screen_center_x = screen_width / 2.0;
        let screen_center_y = screen_height / 2.0;
        
        // Set the pan to center the graph
        self.viewport.pan_x = screen_center_x - graph_center_x * self.viewport.zoom;
        self.viewport.pan_y = screen_center_y - graph_center_y * self.viewport.zoom;
    }
    
    /// Load a graph from file content
    pub fn load_graph_from_content(&mut self, content: &str, file_type: &str) -> Result<(), String> {
        match file_parser::parse_graph_file(content, file_type) {
            Ok(graph) => {
                self.graph = graph;
                self.layout_applied = false;
                self.apply_layout();
                Ok(())
            },
            Err(e) => Err(e),
        }
    }
    
    /// Process file upload UI state
    pub fn process_file_upload(&mut self, file_type: String, file_content: String) -> Option<String> {
        if file_content.trim().is_empty() {
            return Some("File content cannot be empty".to_string());
        }
        
        match self.load_graph_from_content(&file_content, &file_type) {
            Ok(_) => None,
            Err(e) => Some(format!("Error loading graph: {}", e)),
        }
    }
    
    /// Initialize file upload state if it doesn't exist
    pub fn ensure_file_upload_state(&mut self) {
        if self.file_upload_state.is_none() {
            self.file_upload_state = Some(FileUploadState {
                file_content: String::new(),
                file_type: "JSON".to_string(),
                file_name: None,
                error_message: None,
            });
        }
    }
    
    /// Render the graph
    pub fn render_graph(&self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
        
        let painter = ui.painter();
        
        // Set background color based on dark mode
        let bg_color = if self.global_options.dark_mode {
            Color32::from_rgb(30, 30, 30)
        } else {
            Color32::from_rgb(240, 240, 240)
        };
        
        painter.rect_filled(rect, 0.0, bg_color);
        
        // Draw edges
        for edge in self.graph.edges.values() {
            if let (Some(source), Some(target)) = (
                self.graph.nodes.get(&edge.source).and_then(|n| n.position),
                self.graph.nodes.get(&edge.target).and_then(|n| n.position),
            ) {
                let start = Pos2::new(
                    (source.0 * self.viewport.zoom + self.viewport.pan_x) as f32,
                    (source.1 * self.viewport.zoom + self.viewport.pan_y) as f32,
                );
                let end = Pos2::new(
                    (target.0 * self.viewport.zoom + self.viewport.pan_x) as f32,
                    (target.1 * self.viewport.zoom + self.viewport.pan_y) as f32,
                );
                
                let color = if self.selected_edges.contains(&edge.id) {
                    Color32::YELLOW
                } else if self.global_options.dark_mode {
                    hex_to_color32(self.global_options.edge_color.as_str()).unwrap_or(Color32::GRAY)
                } else {
                    hex_to_color32(self.global_options.edge_color.as_str()).unwrap_or(Color32::DARK_GRAY)
                };
                
                painter.line_segment(
                    [start, end],
                    Stroke::new(self.global_options.edge_width as f32, color),
                );
            }
        }
        
        // Draw nodes
        for node in self.graph.nodes.values() {
            if let Some(position) = node.position {
                let pos = Pos2::new(
                    (position.0 * self.viewport.zoom + self.viewport.pan_x) as f32,
                    (position.1 * self.viewport.zoom + self.viewport.pan_y) as f32,
                );
                
                let color = if self.selected_nodes.contains(&node.id) {
                    Color32::YELLOW
                } else if self.global_options.dark_mode {
                    hex_to_color32(self.global_options.node_color.as_str()).unwrap_or(Color32::LIGHT_BLUE)
                } else {
                    hex_to_color32(self.global_options.node_color.as_str()).unwrap_or(Color32::BLUE)
                };
                
                painter.circle_filled(
                    pos,
                    self.global_options.node_size as f32,
                    color,
                );
                
                // Draw labels if enabled
                if self.global_options.show_labels {
                    let label = node.metadata.get("label")
                        .map(|v| match v {
                            shared::types::MetadataValue::String(s) => s.clone(),
                            _ => node.id.clone(),
                        })
                        .unwrap_or_else(|| node.id.clone());
                    
                    let text_color = if self.global_options.dark_mode {
                        Color32::WHITE
                    } else {
                        Color32::BLACK
                    };
                    
                    painter.text(
                        pos + Vec2::new(0.0, self.global_options.node_size as f32 + 5.0),
                        egui::Align2::CENTER_TOP,
                        label,
                        egui::FontId::proportional(self.global_options.label_size as f32),
                        text_color,
                    );
                }
            }
        }
    }
}
