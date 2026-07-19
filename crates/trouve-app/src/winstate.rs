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
    /// Splitter positions (logical px); 0 = never dragged, keep the
    /// app.slint defaults. The middle panel stretches, so absolute side
    /// widths survive window resizes gracefully.
    #[serde(default)]
    pub left_width: u32,
    #[serde(default)]
    pub right_width: u32,
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
            left_width: 0,
            right_width: 0,
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
            && (self.left_width == 0 || (100..=2000).contains(&self.left_width))
            && (self.right_width == 0 || (100..=8000).contains(&self.right_width))
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

/// Appearance preferences: theme id, base font size/family, reduce motion.
/// Client-side like the window geometry — themes restyle this frontend, not
/// the protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Appearance {
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Base (body) size in px; the design's 13px scales everything else.
    #[serde(default = "default_font_size")]
    pub font_size: u32,
    /// Empty = the system default font.
    #[serde(default)]
    pub font_family: String,
    #[serde(default)]
    pub reduce_motion: bool,
}

/// Desktop notification preferences. Client-side: whether *this* frontend
/// pops system notifications is not protocol state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notifications {
    /// Master switch; off suppresses everything below.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// A turn finished (agent is done / next queued prompt starts).
    #[serde(default = "default_true")]
    pub on_finish: bool,
    /// A turn failed.
    #[serde(default = "default_true")]
    pub on_fail: bool,
    /// The agent is blocked on the user: approval request or question.
    #[serde(default = "default_true")]
    pub on_attention: bool,
    /// Ask the notification server to play a sound too.
    #[serde(default)]
    pub sound: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            enabled: true,
            on_finish: true,
            on_fail: true,
            on_attention: true,
            sound: false,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_theme() -> String {
    "dark".into()
}

fn default_font_size() -> u32 {
    13
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            font_size: default_font_size(),
            font_family: String::new(),
            reduce_motion: false,
        }
    }
}

impl Appearance {
    /// Clamp a hand-edited or corrupt file back to something renderable.
    fn sanitized(mut self) -> Self {
        self.font_size = self.font_size.clamp(9, 24);
        self
    }
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

pub fn load_appearance() -> Appearance {
    let read = || {
        let text = std::fs::read_to_string(config_path("appearance.json")?).ok()?;
        serde_json::from_str::<Appearance>(&text).ok()
    };
    read().unwrap_or_default().sanitized()
}

pub fn save_appearance(appearance: &Appearance) {
    write_json(config_path("appearance.json"), appearance);
}

pub fn load_notifications() -> Notifications {
    let read = || {
        let text = std::fs::read_to_string(config_path("notifications.json")?).ok()?;
        serde_json::from_str::<Notifications>(&text).ok()
    };
    read().unwrap_or_default()
}

pub fn save_notifications(notifications: &Notifications) {
    write_json(config_path("notifications.json"), notifications);
}

/// The workspace sidebar order is a frontend preference: other clients may
/// arrange the same server's workspaces differently.
pub fn load_workspace_order() -> Vec<String> {
    let read = || {
        let text = std::fs::read_to_string(config_path("workspace-order.json")?).ok()?;
        serde_json::from_str::<Vec<String>>(&text).ok()
    };
    read().unwrap_or_default()
}

pub fn save_workspace_order(order: &[String]) {
    write_json(config_path("workspace-order.json"), &order);
}

/// Display order of the PR dashboard's groups (group keys), a frontend
/// preference like the workspace sidebar order.
pub fn load_pr_group_order() -> Vec<String> {
    let read = || {
        let text = std::fs::read_to_string(config_path("pr-group-order.json")?).ok()?;
        serde_json::from_str::<Vec<String>>(&text).ok()
    };
    read().unwrap_or_default()
}

pub fn save_pr_group_order(order: &[String]) {
    write_json(config_path("pr-group-order.json"), &order);
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
        assert!(
            !WindowState {
                height: 99999,
                ..ok
            }
            .sane()
        );
        assert!(!WindowState { x: -99999, ..ok }.sane());
    }
}
