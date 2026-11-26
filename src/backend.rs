use crate::error::Result;
use serde::{Deserialize, Serialize};

/// Opaque platform window identifier (e.g., HWND cast to u64 on Windows).
pub type WindowId = u64;

/// Physical pixel rectangle in screen coordinates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Metadata about a tracked or discovered native window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: WindowId,
    pub pid: u32,
    pub title: String,
    pub rect: WindowRect,
    /// DPI scale factor (e.g., 1.0 for 96 DPI, 1.5 for 144 DPI, 2.0 for 192 DPI).
    pub dpi_scale: f32,
    pub is_minimized: bool,
}

/// Raw RGBA8 pixel buffer captured from a window.
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8 bytes (`width * height * 4`).
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" | "l" | "" => Ok(Self::Left),
            "right" | "r" => Ok(Self::Right),
            "middle" | "m" => Ok(Self::Middle),
            other => Err(crate::error::RustSightError::InvalidMouseButton(
                other.to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl Modifiers {
    pub fn from_list(mods: &[String]) -> Self {
        let mut out = Self::default();
        for m in mods {
            match m.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" => out.ctrl = true,
                "alt" | "option" => out.alt = true,
                "shift" => out.shift = true,
                "win" | "super" | "meta" | "cmd" | "command" => out.meta = true,
                _ => {}
            }
        }
        out
    }
}

/// Platform-agnostic window capture & inspection backend.
pub trait WindowCaptureBackend: Send + Sync {
    /// Initialize platform-specific state (e.g., DPI awareness).
    fn init(&self) -> Result<()>;

    /// Find the main visible top-level window belonging to `pid` (or its child processes).
    fn find_window_by_pid(&self, pid: u32) -> Result<Option<WindowInfo>>;

    /// List all visible top-level application windows.
    fn list_windows(&self) -> Result<Vec<WindowInfo>>;

    /// Get updated metadata for a specific window.
    fn get_window_info(&self, window_id: WindowId) -> Result<WindowInfo>;

    /// Check whether a window still exists and is valid.
    #[allow(dead_code)]
    fn is_window_alive(&self, window_id: WindowId) -> bool;

    /// Capture the window's pixels as an RGBA8 frame.
    fn capture_window(&self, window_id: WindowId) -> Result<CapturedFrame>;

    /// Resize the window's client/outer size.
    fn resize_window(&self, window_id: WindowId, width: u32, height: u32) -> Result<()>;

    /// Bring the window to the foreground / focus it for input.
    #[allow(dead_code)]
    fn focus_window(&self, window_id: WindowId) -> Result<()>;
}

/// Platform-agnostic input injection backend.
/// Coordinates `(x, y)` are in captured screenshot pixels relative to the top-left of the window.
pub trait InputBackend: Send + Sync {
    fn click(
        &self,
        window: &WindowInfo,
        x: i32,
        y: i32,
        button: MouseButton,
        double: bool,
    ) -> Result<()>;

    fn type_text(&self, window: &WindowInfo, text: &str) -> Result<()>;

    fn press_key(&self, window: &WindowInfo, key: &str, modifiers: &Modifiers) -> Result<()>;

    fn scroll(&self, window: &WindowInfo, x: i32, y: i32, dx: i32, dy: i32) -> Result<()>;

    fn drag(
        &self,
        window: &WindowInfo,
        from_x: i32,
        from_y: i32,
        to_x: i32,
        to_y: i32,
    ) -> Result<()>;
}
