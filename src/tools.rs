use crate::backend::WindowId;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LaunchParams {
    /// Path to a Cargo project directory (containing Cargo.toml), a Cargo.toml file, or a prebuilt executable binary.
    pub path_or_cargo_dir: String,
    /// Optional command-line arguments passed to the launched application.
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional initial window width in pixels.
    pub width: Option<u32>,
    /// Optional initial window height in pixels.
    pub height: Option<u32>,
    /// Optional Cargo binary target name (`--bin <NAME>`). Required when a Cargo build produces multiple binaries.
    pub bin: Option<String>,
    /// Optional Cargo package name (`-p <NAME>`).
    pub package: Option<String>,
    /// Build in release mode (`--release`, default: false).
    #[serde(default)]
    pub release: bool,
    /// Optional Cargo features to enable (`--features ...`).
    #[serde(default)]
    pub features: Vec<String>,
    /// Enable all Cargo features (`--all-features`, default: false).
    #[serde(default)]
    pub all_features: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WindowIdParams {
    /// Target window ID. Defaults to the currently active launched window.
    pub window_id: Option<WindowId>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CropRegion {
    /// Left X coordinate in full-window screenshot pixels (as returned to the agent).
    pub x: u32,
    /// Top Y coordinate in full-window screenshot pixels (as returned to the agent).
    pub y: u32,
    /// Width of the region in full-window screenshot pixels.
    pub width: u32,
    /// Height of the region in full-window screenshot pixels.
    pub height: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScreenshotParams {
    /// Target window ID. Defaults to the currently active launched window.
    pub window_id: Option<WindowId>,
    /// Optional sub-region in full-window screenshot coordinates to crop (saves tokens; input coordinates remain in full-window screenshot space).
    pub region: Option<CropRegion>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CaptureFramesParams {
    /// Target window ID. Defaults to the currently active launched window.
    pub window_id: Option<WindowId>,
    /// Total capture duration in milliseconds (default: 1200, max: 10000).
    pub duration_ms: Option<u64>,
    /// Target capture frame rate in frames per second (default: 8, max: 30).
    pub fps: Option<u32>,
    /// Maximum number of evenly spaced PNG frames to return in chat (default: 3, max: 12).
    pub max_frames: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClickParams {
    /// X coordinate in pixels relative to the top-left of the captured window screenshot.
    pub x: i32,
    /// Y coordinate in pixels relative to the top-left of the captured window screenshot.
    pub y: i32,
    /// Mouse button: "left" (default), "right", or "middle".
    pub button: Option<String>,
    /// Whether to perform a double-click (default: false).
    #[serde(default)]
    pub double: bool,
    /// Optional target window ID. Defaults to the active window.
    pub window_id: Option<WindowId>,
    /// Whether to return a fresh screenshot after clicking (default: true). Set false to save tokens during multi-step sequences.
    pub include_screenshot: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TypeTextParams {
    /// Text string to type into the focused window. Supports Unicode and newlines.
    pub text: String,
    /// Optional target window ID. Defaults to the active window.
    pub window_id: Option<WindowId>,
    /// Whether to return a fresh screenshot after typing (default: true).
    pub include_screenshot: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PressKeyParams {
    /// Key name to press (e.g., "enter", "tab", "escape", "backspace", "delete", "space", "up", "down", "left", "right", "home", "end", "f1"-"f12", "a"-"z", "0"-"9").
    pub key: String,
    /// Optional modifier keys: "ctrl", "alt", "shift", "win" / "super" / "meta".
    #[serde(default)]
    pub modifiers: Vec<String>,
    /// Optional target window ID. Defaults to the active window.
    pub window_id: Option<WindowId>,
    /// Whether to return a fresh screenshot after pressing the key (default: true).
    pub include_screenshot: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScrollParams {
    /// X coordinate in pixels of the most recent full-window screenshot where the scroll occurs.
    pub x: i32,
    /// Y coordinate in pixels of the most recent full-window screenshot where the scroll occurs.
    pub y: i32,
    /// Horizontal scroll delta (positive = right, negative = left).
    #[serde(default)]
    pub dx: i32,
    /// Vertical scroll delta (positive = scroll down, negative = scroll up; small values <= 20 are treated as wheel notches).
    #[serde(default)]
    pub dy: i32,
    /// Optional target window ID. Defaults to the active window.
    pub window_id: Option<WindowId>,
    /// Whether to return a fresh screenshot after scrolling (default: true).
    pub include_screenshot: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DragParams {
    /// Starting X coordinate in pixels of the most recent full-window screenshot.
    pub from_x: i32,
    /// Starting Y coordinate in pixels of the most recent full-window screenshot.
    pub from_y: i32,
    /// Ending X coordinate in pixels of the most recent full-window screenshot.
    pub to_x: i32,
    /// Ending Y coordinate in pixels of the most recent full-window screenshot.
    pub to_y: i32,
    /// Optional target window ID. Defaults to the active window.
    pub window_id: Option<WindowId>,
    /// Whether to return a fresh screenshot after dragging (default: true).
    pub include_screenshot: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitIdleParams {
    /// Maximum time to wait in milliseconds (default: 5000).
    pub timeout_ms: Option<u64>,
    /// Optional target window ID. Defaults to the active window.
    pub window_id: Option<WindowId>,
    /// Fraction of changed pixels (0.0 to 1.0, default 0.001) below which consecutive frames are considered idle.
    pub threshold: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetLogsParams {
    /// Target window ID. Defaults to the currently active launched window.
    pub window_id: Option<WindowId>,
    /// Number of trailing log lines to return (default: 200).
    pub tail: Option<usize>,
    /// Whether to clear the log buffer after reading (default: false).
    #[serde(default)]
    pub clear: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetUiTreeParams {
    /// Target window ID. Defaults to the currently active launched window.
    pub window_id: Option<WindowId>,
    /// Maximum traversal depth (default: 6).
    pub max_depth: Option<usize>,
    /// Maximum number of nodes to return (default: 300).
    pub max_nodes: Option<usize>,
    /// Whether to include offscreen elements (default: false).
    #[serde(default)]
    pub include_offscreen: bool,
    /// Whether to also attach a fresh PNG screenshot (default: false).
    #[serde(default)]
    pub include_screenshot: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindElementParams {
    /// Case-insensitive substring to match against the element's accessible name.
    pub name: Option<String>,
    /// Exact UI Automation role / control type (e.g., "Button", "Edit", "MenuItem", "Text").
    pub role: Option<String>,
    /// Exact UI Automation ID (`automation_id`).
    pub automation_id: Option<String>,
    /// Target window ID. Defaults to the currently active launched window.
    pub window_id: Option<WindowId>,
    /// Maximum number of matching elements to return (default: 20).
    pub max_results: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClickElementParams {
    /// Case-insensitive substring to match against the element's accessible name.
    pub name: Option<String>,
    /// Exact UI Automation role / control type (e.g., "Button", "Edit", "MenuItem").
    pub role: Option<String>,
    /// Exact UI Automation ID (`automation_id`).
    pub automation_id: Option<String>,
    /// Zero-based index when multiple elements match. Required if more than one element matches.
    pub index: Option<usize>,
    /// Mouse button: "left" (default), "right", or "middle".
    pub button: Option<String>,
    /// Whether to perform a double-click (default: false).
    #[serde(default)]
    pub double: bool,
    /// Target window ID. Defaults to the currently active launched window.
    pub window_id: Option<WindowId>,
    /// Whether to return a fresh screenshot after clicking (default: true).
    pub include_screenshot: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HoverParams {
    /// X coordinate in pixels of the most recent full-window screenshot.
    pub x: i32,
    /// Y coordinate in pixels of the most recent full-window screenshot.
    pub y: i32,
    /// Optional target window ID. Defaults to the active window.
    pub window_id: Option<WindowId>,
    /// Whether to return a fresh screenshot after moving the cursor (default: true).
    pub include_screenshot: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResizeWindowParams {
    /// New client width in pixels (100 to 8000).
    pub width: u32,
    /// New client height in pixels (100 to 8000).
    pub height: u32,
    /// Optional target window ID. Defaults to the active window.
    pub window_id: Option<WindowId>,
}
