use ::std::path::PathBuf;
use ::std::time::Instant;

pub struct UiEffects {
    pub flash_space_freed: bool,
    pub current_path_is_red: bool,
    pub deletion_in_progress: bool,
    pub loading_progress_indicator: u64,
    pub last_read_path: Option<PathBuf>,
    /// What was last copied to the clipboard, as copied, and until when the title shows it.
    ///
    /// The deadline travels with the text rather than relying on a later message to clear it, so
    /// a frame drawn after it passes never shows a stale copy — whatever else is drawn meanwhile,
    /// and even if the redraw asked for at the deadline is lost.
    pub clipboard_flash: Option<(String, Instant)>,
}

impl UiEffects {
    pub fn new() -> Self {
        Self {
            flash_space_freed: false,
            current_path_is_red: false,
            deletion_in_progress: false,
            loading_progress_indicator: 0,
            last_read_path: None,
            clipboard_flash: None,
        }
    }
    /// The copied text to show in the title at `now`, if its flash has not yet expired.
    pub fn clipboard_flash_at(&self, now: Instant) -> Option<&str> {
        self.clipboard_flash
            .as_ref()
            .filter(|(_, until)| now < *until)
            .map(|(text, _)| text.as_str())
    }
    pub fn increment_loading_progress_indicator(&mut self) {
        // increasing and decreasing this number will increase
        // the scanning text animation speed
        self.loading_progress_indicator += 3;
    }
}
