//! Persisted window geometry: position, size, and maximized flag, saved to
//! the user config dir and restored on launch. A background poll (Slint has
//! no move/resize callbacks) writes changes as the user drags the window.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        // Matches the AppWindow preferred size; position is left to the
        // window manager.
        Self {
            x: 0,
            y: 0,
            width: 1400,
            height: 900,
            maximized: false,
        }
    }
}

impl WindowState {
    /// Guards against a corrupt file or a monitor layout change placing the
    /// window somewhere unusable.
    fn sane(&self) -> bool {
        (200..=16000).contains(&self.width)
            && (200..=16000).contains(&self.height)
            && (-16000..=16000).contains(&self.x)
            && (-16000..=16000).contains(&self.y)
    }
}

fn state_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("trouve").join("window.json"))
}

/// The stored state, if present and plausible.
pub fn load() -> Option<WindowState> {
    let text = std::fs::read_to_string(state_path()?).ok()?;
    let state: WindowState = serde_json::from_str(&text).ok()?;
    state.sane().then_some(state)
}

/// Best-effort persist; a failed write only costs restore-on-next-launch.
pub fn save(state: &WindowState) {
    let Some(path) = state_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insane_geometry_is_rejected() {
        let ok = WindowState::default();
        assert!(ok.sane());
        assert!(!WindowState { width: 0, ..ok }.sane());
        assert!(!WindowState { height: 99999, ..ok }.sane());
        assert!(!WindowState { x: -99999, ..ok }.sane());
    }
}
