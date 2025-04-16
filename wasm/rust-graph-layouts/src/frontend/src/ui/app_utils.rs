use crate::app::{App, Instant};

impl App {
    /// Check if the debounce timer has elapsed and apply layout if needed
    pub fn check_debounce_timer(&mut self) {
        if let Some(timer) = self.layout_debounce_timer {
            // Use a 300ms debounce time
            #[cfg(not(target_arch = "wasm32"))]
            {
                let debounce_duration = std::time::Duration::from_millis(300);
                if timer.elapsed() >= debounce_duration {
                    // Timer has elapsed, apply layout and reset timer
                    self.apply_layout();
                    self.layout_debounce_timer = None;
                }
            }
            
            #[cfg(target_arch = "wasm32")]
            {
                let debounce_ms = 300.0;
                if timer.elapsed() >= debounce_ms {
                    // Timer has elapsed, apply layout and reset timer
                    self.apply_layout();
                    self.layout_debounce_timer = None;
                }
            }
        }
    }
    
    /// Schedule a layout application after the debounce period
    pub fn schedule_layout_update(&mut self) {
        // Reset the timer to now whenever a layout option changes
        self.layout_debounce_timer = Some(Instant::now());
    }
}
