use std::collections::{HashSet, HashMap};
use shared::types::{Graph, LayoutAlgorithm, GlobalRenderOptions, Viewport};

// Custom time implementation for cross-platform support
#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, Debug)]
pub struct Instant(f64);

#[cfg(target_arch = "wasm32")]
impl Instant {
    pub fn now() -> Self {
        let window = web_sys::window().expect("should have a window in this context");
        let performance = window.performance().expect("performance should be available");
        Self(performance.now())
    }

    pub fn elapsed(&self) -> f64 {
        let now = Self::now();
        now.0 - self.0
    }
}

/// Animation state for smooth transitions
pub struct AnimationState {
    pub start_time: Instant,
    pub duration: u32,
    pub initial_positions: HashMap<String, Option<(f64, f64)>>,
    pub final_positions: HashMap<String, Option<(f64, f64)>>,
}

/// File upload state
pub struct FileUploadState {
    pub file_type: String,
    pub file_content: String,
    pub file_name: Option<String>,
    pub error_message: Option<String>,
}

/// Main application state
pub struct App {
    pub graph: Graph,
    pub layout: LayoutAlgorithm,
    pub global_options: GlobalRenderOptions,
    pub selected_nodes: HashSet<String>,
    pub selected_edges: HashSet<String>,
    pub viewport: Viewport,
    pub auto_center: bool,
    pub layout_applied: bool,
    pub layout_debounce_timer: Option<Instant>,
    pub animation_state: Option<AnimationState>,
    pub file_upload_state: Option<FileUploadState>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            graph: Graph::new(),
            layout: LayoutAlgorithm::Dagre(shared::types::DagreLayoutOptions::default()),
            global_options: GlobalRenderOptions::default(),
            selected_nodes: HashSet::new(),
            selected_edges: HashSet::new(),
            viewport: Viewport::default(),
            auto_center: true,
            layout_applied: false,
            layout_debounce_timer: None,
            animation_state: None,
            file_upload_state: None,
        }
    }
}

impl App {
    /// Handle file upload for both desktop and web platforms
    pub fn upload_file(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.upload_file_native();
        
        #[cfg(target_arch = "wasm32")]
        self.upload_file_web();
    }
    
    /// Handle file upload for desktop platforms using native file dialog
    #[cfg(not(target_arch = "wasm32"))]
    fn upload_file_native(&mut self) {
        // Get file type filter based on selected file type
        let file_type = self.file_upload_state.as_ref().map(|s| s.file_type.clone()).unwrap_or_else(|| "JSON".to_string());
        
        // Open native file dialog
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Graph Files", &["json", "csv", "dot", "gv"])
            .set_title("Open Graph File")
            .pick_file() {
            
            // Read file content
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    // Get file extension to determine file type
                    let extension = path.extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.to_lowercase())
                        .unwrap_or_else(|| "json".to_string());
                    
                    // Map extension to file type
                    let file_type = match extension.as_str() {
                        "json" => "JSON",
                        "csv" => "CSV",
                        "dot" | "gv" => "DOT",
                        _ => "JSON", // Default to JSON
                    };
                    
                    // Get file name
                    let file_name = path.file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name.to_string());
                    
                    // Update file upload state
                    if let Some(state) = &mut self.file_upload_state {
                        state.file_content = content.clone();
                        state.file_type = file_type.to_string();
                        state.file_name = file_name;
                        state.error_message = None;
                    }
                    
                    // Process the file
                    if let Some(error) = self.process_file_upload(file_type.to_string(), content) {
                        if let Some(state) = &mut self.file_upload_state {
                            state.error_message = Some(error);
                        }
                    }
                },
                Err(e) => {
                    if let Some(state) = &mut self.file_upload_state {
                        state.error_message = Some(format!("Error reading file: {}", e));
                    }
                }
            }
        }
    }
    
    /// Handle file upload for web platforms using browser file input
    #[cfg(target_arch = "wasm32")]
    fn upload_file_web(&mut self) {
        use wasm_bindgen::prelude::*;
        use web_sys::{FileReader, HtmlInputElement};
        use wasm_bindgen::JsCast;
        
        let window = web_sys::window().expect("no global window exists");
        let document = window.document().expect("should have a document on window");
        
        // Create a file input element
        let input: HtmlInputElement = document.create_element("input")
            .expect("should be able to create input element")
            .dyn_into::<HtmlInputElement>()
            .expect("should be an input element");
        
        // Set input attributes
        input.set_type("file");
        input.set_accept(".json,.csv,.dot,.gv");
        
        // Set style using the style property of HtmlElement
        // First cast to HtmlElement to access the style property
        let html_element: &web_sys::HtmlElement = input.dyn_ref().unwrap();
        let style = html_element.style();
        let _ = style.set_property("display", "none");
        
        // Add input to document body
        let body = document.body().expect("document should have a body");
        body.append_child(&input).expect("should be able to append input to body");
        
        // Create a closure to handle file selection
        let app_ptr = self as *mut App;
        let app_ptr_clone = app_ptr;
        let closure = Closure::wrap(Box::new(move |event: web_sys::Event| {
            let input: HtmlInputElement = event.target()
                .expect("event should have a target")
                .dyn_into::<HtmlInputElement>()
                .expect("target should be an input element");
            
            // Get the selected file
            if let Some(file_list) = input.files() {
                if let Some(file) = file_list.get(0) {
                    // Create a FileReader to read the file content
                    let reader = FileReader::new().expect("should be able to create FileReader");
                    let reader_clone = reader.clone();
                    
                    // Clone the file name and create a string to avoid moving the file into the closure
                    let file_name = file.name();
                    let file_name_clone = file_name.clone();
                    
                    // Create a closure to handle file load
                    let app_ptr = app_ptr_clone;
                    let onload_closure = Closure::wrap(Box::new(move |_event: web_sys::Event| {
                        // Get file content as text
                        let content = reader_clone.result()
                            .expect("should have result")
                            .as_string()
                            .expect("result should be a string");
                        
                        // Get extension from file name
                        let extension = file_name_clone.split('.').last()
                            .map(|ext| ext.to_lowercase())
                            .unwrap_or_else(|| "json".to_string());
                        
                        // Map extension to file type
                        let file_type = match extension.as_str() {
                            "json" => "JSON",
                            "csv" => "CSV",
                            "dot" | "gv" => "DOT",
                            _ => "JSON", // Default to JSON
                        };
                        
                        // Update file upload state
                        unsafe {
                            if let Some(app) = app_ptr.as_mut() {
                                if let Some(state) = &mut app.file_upload_state {
                                    state.file_content = content.clone();
                                    state.file_type = file_type.to_string();
                                    state.file_name = Some(file_name_clone.to_string());
                                    state.error_message = None;
                                }
                                
                                // Process the file
                                if let Some(error) = app.process_file_upload(file_type.to_string(), content) {
                                    if let Some(state) = &mut app.file_upload_state {
                                        state.error_message = Some(error);
                                    }
                                }
                            }
                        }
                        
                        // Remove the input element
                        let window = web_sys::window().expect("no global window exists");
                        let document = window.document().expect("should have a document on window");
                        let body = document.body().expect("document should have a body");
                        if let Some(input_element) = document.get_element_by_id("file-upload-input") {
                            body.remove_child(&input_element).expect("should be able to remove input");
                        }
                    }) as Box<dyn FnMut(_)>);
                    
                    // Set onload handler
                    reader.set_onload(Some(onload_closure.as_ref().unchecked_ref()));
                    onload_closure.forget();
                    
                    // Read the file as text
                    reader.read_as_text(&file).expect("should be able to read file");
                }
            }
            
            // Remove the input element
            body.remove_child(&input).expect("should be able to remove input");
        }) as Box<dyn FnMut(_)>);
        
        // Set onchange handler
        input.set_onchange(Some(closure.as_ref().unchecked_ref()));
        closure.forget();
        
        // Set an ID for the input element
        input.set_id("file-upload-input");
        
        // Click the input to open file dialog
        input.click();
    }
}
