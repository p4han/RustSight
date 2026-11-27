use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

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

/// Compute the per-frame interval for a burst capture at `fps` (clamped to `[1, 60]`).
pub fn burst_frame_interval(fps: u32) -> Duration {
    Duration::from_secs_f64(1.0 / fps.clamp(1, 60) as f64)
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

    /// Burst-capture frames over `duration` at target `fps`.
    /// Default implementation loops `capture_window` at the requested interval.
    fn capture_burst(
        &self,
        window_id: WindowId,
        duration: Duration,
        fps: u32,
    ) -> Result<Vec<CapturedFrame>> {
        default_capture_burst(self, window_id, duration, fps)
    }

    /// Resize the window's client/outer size.
    fn resize_window(&self, window_id: WindowId, width: u32, height: u32) -> Result<()>;

    /// Bring the window to the foreground / focus it for input.
    #[allow(dead_code)]
    fn focus_window(&self, window_id: WindowId) -> Result<()>;
}

/// Shared default burst-capture loop used by mock/fallback backends.
pub fn default_capture_burst<B: WindowCaptureBackend + ?Sized>(
    backend: &B,
    window_id: WindowId,
    duration: Duration,
    fps: u32,
) -> Result<Vec<CapturedFrame>> {
    let interval = burst_frame_interval(fps);
    let start = Instant::now();
    let end = start + duration;
    let mut frames = Vec::new();

    while Instant::now() < end {
        let tick_start = Instant::now();
        if let Ok(frame) = backend.capture_window(window_id) {
            frames.push(frame);
        }
        let elapsed = tick_start.elapsed();
        if elapsed < interval {
            let remaining = end
                .saturating_duration_since(Instant::now())
                .min(interval - elapsed);
            if !remaining.is_zero() {
                std::thread::sleep(remaining);
            }
        }
    }

    if frames.is_empty() {
        frames.push(backend.capture_window(window_id)?);
    }
    Ok(frames)
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

    fn mouse_move(&self, window: &WindowInfo, x: i32, y: i32) -> Result<()>;

    fn focus(&self, window: &WindowInfo) -> Result<()>;
}

/// A node in the native UI accessibility tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiNode {
    pub role: String,
    pub name: String,
    pub automation_id: String,
    pub class_name: String,
    pub is_enabled: bool,
    pub has_keyboard_focus: bool,
    pub is_offscreen: bool,
    /// Bounding rectangle in physical screen pixels.
    pub rect: WindowRect,
    pub children: Vec<UiNode>,
}

#[derive(Debug, Clone, Copy)]
pub struct UiTreeOptions {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub include_offscreen: bool,
    pub timeout: Duration,
}

impl Default for UiTreeOptions {
    fn default() -> Self {
        Self {
            max_depth: 6,
            max_nodes: 300,
            include_offscreen: false,
            timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UiTreeResult {
    pub roots: Vec<UiNode>,
    pub truncated_count: usize,
    pub timed_out: bool,
}

/// Platform-agnostic UI accessibility tree backend.
pub trait UiTreeBackend: Send + Sync {
    fn get_ui_tree(&self, window_id: WindowId, opts: UiTreeOptions) -> Result<UiTreeResult>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBurstBackend;
    impl WindowCaptureBackend for MockBurstBackend {
        fn init(&self) -> Result<()> {
            Ok(())
        }
        fn find_window_by_pid(&self, _pid: u32) -> Result<Option<WindowInfo>> {
            Ok(None)
        }
        fn list_windows(&self) -> Result<Vec<WindowInfo>> {
            Ok(Vec::new())
        }
        fn get_window_info(&self, window_id: WindowId) -> Result<WindowInfo> {
            Ok(WindowInfo {
                id: window_id,
                pid: 0,
                title: "Mock".into(),
                rect: WindowRect {
                    x: 0,
                    y: 0,
                    width: 4,
                    height: 4,
                },
                dpi_scale: 1.0,
                is_minimized: false,
            })
        }
        fn is_window_alive(&self, _window_id: WindowId) -> bool {
            true
        }
        fn capture_window(&self, _window_id: WindowId) -> Result<CapturedFrame> {
            Ok(CapturedFrame {
                width: 4,
                height: 4,
                rgba: vec![255; 4 * 4 * 4],
            })
        }
        fn resize_window(&self, _window_id: WindowId, _width: u32, _height: u32) -> Result<()> {
            Ok(())
        }
        fn focus_window(&self, _window_id: WindowId) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn burst_frame_interval_clamps_fps() {
        assert_eq!(burst_frame_interval(0), Duration::from_secs(1));
        assert_eq!(burst_frame_interval(10), Duration::from_millis(100));
        assert_eq!(
            burst_frame_interval(1000),
            Duration::from_secs_f64(1.0 / 60.0)
        );
    }

    #[test]
    fn default_capture_burst_returns_frames_for_duration() {
        let backend = MockBurstBackend;
        let start = Instant::now();
        let frames = backend
            .capture_burst(1, Duration::from_millis(120), 20)
            .expect("default burst");
        let elapsed = start.elapsed();
        assert!(!frames.is_empty());
        assert!(elapsed >= Duration::from_millis(90));
    }

    #[test]
    fn mouse_button_parse_handles_aliases_and_invalid() {
        assert_eq!(MouseButton::parse("left").unwrap(), MouseButton::Left);
        assert_eq!(MouseButton::parse("L").unwrap(), MouseButton::Left);
        assert_eq!(MouseButton::parse("").unwrap(), MouseButton::Left);
        assert_eq!(MouseButton::parse(" right ").unwrap(), MouseButton::Right);
        assert_eq!(MouseButton::parse("m").unwrap(), MouseButton::Middle);
        assert!(MouseButton::parse("side").is_err());
    }

    #[test]
    fn modifiers_from_list_parses_all_aliases() {
        let mods = Modifiers::from_list(&[
            "ctrl".into(),
            "Option".into(),
            "SHIFT".into(),
            "cmd".into(),
            "unknown".into(),
        ]);
        assert!(mods.ctrl);
        assert!(mods.alt);
        assert!(mods.shift);
        assert!(mods.meta);
    }
}
