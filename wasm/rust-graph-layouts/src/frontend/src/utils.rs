use egui::Color32;
use std::u32;

/// Convert a hex color string to Color32
pub fn hex_to_color32(hex: &str) -> Option<Color32> {
    let hex = hex.trim_start_matches('#');
    if let Ok(rgb) = u32::from_str_radix(hex, 16) {
        let r = ((rgb >> 16) & 0xFF) as u8;
        let g = ((rgb >> 8) & 0xFF) as u8;
        let b = (rgb & 0xFF) as u8;
        Some(Color32::from_rgb(r, g, b))
    } else {
        None
    }
}
