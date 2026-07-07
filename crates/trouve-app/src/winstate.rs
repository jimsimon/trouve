//! Persisted window geometry: position, size, and maximized flag, saved to
//! the user config dir and restored on launch. A background poll (Slint has
//! no move/resize callbacks) writes changes as the user drags the window.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// Where the user left off, per session and thread: the last open session
/// (restored on launch), each session's last open thread (restored when the
/// session is clicked), and each thread's chat scroll offset — a Slint
/// viewport-y, so 0 or negative — restored when the thread is opened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Resume {
    #[serde(default)]
    pub session_id: String,
    /// session id → last open thread id.
    #[serde(default)]
    pub session_threads: HashMap<String, String>,
    /// thread id → chat scroll offset.
    #[serde(default)]
    pub thread_scroll: HashMap<String, f32>,
}

fn config_path(file: &str) -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("trouve").join(file))
}

fn state_path() -> Option<PathBuf> {
    config_path("window.json")
}

fn resume_path() -> Option<PathBuf> {
    config_path("resume.json")
}

/// The stored state, if present and plausible.
pub fn load() -> Option<WindowState> {
    let text = std::fs::read_to_string(state_path()?).ok()?;
    let state: WindowState = serde_json::from_str(&text).ok()?;
    state.sane().then_some(state)
}

/// Best-effort persist; a failed write only costs restore-on-next-launch.
pub fn save(state: &WindowState) {
    write_json(state_path(), state);
}

pub fn load_resume() -> Resume {
    let read = || {
        let text = std::fs::read_to_string(resume_path()?).ok()?;
        serde_json::from_str::<Resume>(&text).ok()
    };
    read().unwrap_or_default()
}

pub fn save_resume(resume: &Resume) {
    write_json(resume_path(), resume);
}

fn write_json<T: Serialize>(path: Option<PathBuf>, value: &T) {
    let Some(path) = path else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(value) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_flat_resume_format_still_parses() {
        let old = r#"{"session_id":"s1","thread_id":"t1","scroll":-42.0}"#;
        let resume: Resume = serde_json::from_str(old).unwrap();
        assert_eq!(resume.session_id, "s1");
        // The flat thread/scroll bookmark isn't carried over; the maps
        // start empty and refill as the user navigates.
        assert!(resume.session_threads.is_empty());
        assert!(resume.thread_scroll.is_empty());
    }

    #[test]
    fn insane_geometry_is_rejected() {
        let ok = WindowState::default();
        assert!(ok.sane());
        assert!(!WindowState { width: 0, ..ok }.sane());
        assert!(!WindowState { height: 99999, ..ok }.sane());
        assert!(!WindowState { x: -99999, ..ok }.sane());
    }
}
