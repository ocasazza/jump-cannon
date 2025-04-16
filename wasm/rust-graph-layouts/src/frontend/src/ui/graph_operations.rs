use eframe::egui;
use crate::app::App;

/// Render the graph operations section
pub fn render(app: &mut App, ui: &mut egui::Ui) {
    ui.collapsing("Graph Operations", |ui| {
        
        // File upload section
        ui.separator();
        ui.heading("Upload Graph File");
        
        // Ensure file upload state exists
        app.ensure_file_upload_state();
        
        // Get a copy of the current state to work with
        let mut file_type = String::new();
        let mut file_name = None;
        let mut error_message = None;
        
        if let Some(state) = &app.file_upload_state {
            file_type = state.file_type.clone();
            file_name = state.file_name.clone();
            error_message = state.error_message.clone();
        }
        
        // File type selection
        ui.horizontal(|ui| {
            ui.label("File Type:");
            
            egui::ComboBox::from_id_source("file_type_combo")
                .selected_text(&file_type)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(file_type == "JSON", "JSON").clicked() {
                        file_type = "JSON".to_string();
                    }
                    if ui.selectable_label(file_type == "CSV", "CSV").clicked() {
                        file_type = "CSV".to_string();
                    }
                    if ui.selectable_label(file_type == "DOT", "DOT").clicked() {
                        file_type = "DOT".to_string();
                    }
                });
        });
        
        // File upload button and display
        ui.horizontal(|ui| {
            if ui.button("Choose File...").clicked() {
                // Update file type in state before opening file dialog
                if let Some(state) = &mut app.file_upload_state {
                    state.file_type = file_type.clone();
                }
                
                // Trigger file upload
                app.upload_file();
            }
            
            // Display selected file name or placeholder
            if let Some(name) = &file_name {
                ui.label(name);
            } else {
                ui.weak("No file selected");
            }
        });
        
        // Display error message if any
        if let Some(error) = &error_message {
            ui.colored_label(egui::Color32::RED, error);
        }
        
        // Update the file type in the state
        if let Some(state) = &mut app.file_upload_state {
            state.file_type = file_type;
        }
    });
}
