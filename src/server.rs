use crate::backend::{
    CapturedFrame, InputBackend, Modifiers, MouseButton, WindowCaptureBackend, WindowId,
    WindowInfo,
};
use crate::capture::{downscale_for_mcp, encode_gif, encode_png_base64, frames_differ};
use crate::config::Config;
use crate::error::{Result as AppResult, RustSightError};
use crate::process::{LaunchSpec, ProcessManager};
use crate::tools::{
    CaptureFramesParams, ClickParams, DragParams, LaunchParams, PressKeyParams, ScrollParams,
    TypeTextParams, WaitIdleParams, WindowIdParams,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock as Content, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct RustSightServer {
    config: Arc<Config>,
    capture: Arc<Box<dyn WindowCaptureBackend>>,
    input: Arc<Box<dyn InputBackend>>,
    processes: Arc<Mutex<ProcessManager>>,
    coord_scales: Arc<Mutex<HashMap<WindowId, f64>>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl RustSightServer {
    pub fn new(
        config: Config,
        capture: Box<dyn WindowCaptureBackend>,
        input: Box<dyn InputBackend>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            capture: Arc::new(capture),
            input: Arc::new(input),
            processes: Arc::new(Mutex::new(ProcessManager::new())),
            coord_scales: Arc::new(Mutex::new(HashMap::new())),
            tool_router: Self::tool_router(),
        }
    }

    pub fn shutdown(&self) {
        if let Ok(mut pm) = self.processes.lock() {
            pm.cleanup_all();
        }
    }

    fn resolve_window(&self, window_id: Option<WindowId>) -> AppResult<WindowInfo> {
        let wid = {
            let pm = self
                .processes
                .lock()
                .map_err(|_| RustSightError::CaptureFailed("ProcessManager lock poisoned".into()))?;
            pm.resolve_window_id(window_id)?
        };
        self.capture.get_window_info(wid)
    }

    fn capture_and_encode(&self, wid: WindowId) -> AppResult<String> {
        let frame = self.capture.capture_window(wid)?;
        let (scaled, scale) = downscale_for_mcp(frame, self.config.max_image_width)?;
        if let Ok(mut scales) = self.coord_scales.lock() {
            scales.insert(wid, scale);
        }
        encode_png_base64(&scaled)
    }

    fn scale_coords(&self, wid: WindowId, x: i32, y: i32) -> (i32, i32) {
        let scale = self
            .coord_scales
            .lock()
            .ok()
            .and_then(|s| s.get(&wid).copied())
            .unwrap_or(1.0);
        (
            (x as f64 * scale).round() as i32,
            (y as f64 * scale).round() as i32,
        )
    }

    fn settle_and_capture(&self, wid: WindowId) -> AppResult<(WindowInfo, String)> {
        std::thread::sleep(Duration::from_millis(self.config.settle_ms));
        let info = self.capture.get_window_info(wid)?;
        let b64 = self.capture_and_encode(wid)?;
        Ok((info, b64))
    }

    fn ok_with_screenshot(summary: String, b64_png: String) -> CallToolResult {
        CallToolResult::success(vec![
            Content::text(summary),
            Content::image(b64_png, "image/png"),
        ])
    }

    fn err_result(err: RustSightError) -> CallToolResult {
        CallToolResult::error(vec![Content::text(err.to_string())])
    }
}

#[tool_router]
impl RustSightServer {
    /// Build (if Cargo project) and launch a native GUI application, wait for its main window, and return its window_id plus the first screenshot.
    #[tool(
        name = "launch",
        description = "Build (if given a Cargo project directory or Cargo.toml) and launch a native GUI application. Tracks the process, finds its main window, optionally resizes it, and returns the window_id plus the first PNG screenshot."
    )]
    async fn launch(&self, Parameters(params): Parameters<LaunchParams>) -> CallToolResult {
        let this = self.clone();
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, String)> {
            let spec = LaunchSpec {
                path_or_cargo_dir: params.path_or_cargo_dir,
                args: params.args,
                width: params.width.or(this.config.default_width),
                height: params.height.or(this.config.default_height),
            };
            let timeout = Duration::from_secs(this.config.build_timeout_secs);
            let info = {
                let mut pm = this.processes.lock().map_err(|_| {
                    RustSightError::CaptureFailed("ProcessManager lock poisoned".into())
                })?;
                pm.launch(spec, this.capture.as_ref().as_ref(), timeout)?
            };
            let b64 = this.capture_and_encode(info.id)?;
            Ok((info, b64))
        })
        .await;

        match res {
            Ok(Ok((info, b64))) => Self::ok_with_screenshot(
                format!(
                    "Launched `{}` (pid={}, window_id={}, size={}x{}, dpi_scale={:.2})",
                    info.title, info.pid, info.id, info.rect.width, info.rect.height, info.dpi_scale
                ),
                b64,
            ),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::LaunchFailed("task".into(), e.to_string())),
        }
    }

    /// Capture a PNG screenshot of the tracked window (or specified window_id).
    #[tool(
        name = "screenshot",
        description = "Capture the current window pixels as a PNG screenshot. Works even if the window is partially covered."
    )]
    async fn screenshot(&self, Parameters(params): Parameters<WindowIdParams>) -> CallToolResult {
        let this = self.clone();
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, String)> {
            let info = this.resolve_window(params.window_id)?;
            let b64 = this.capture_and_encode(info.id)?;
            Ok((info, b64))
        })
        .await;

        match res {
            Ok(Ok((info, b64))) => Self::ok_with_screenshot(
                format!(
                    "Screenshot of `{}` (window_id={}, {}x{}, dpi_scale={:.2})",
                    info.title, info.id, info.rect.width, info.rect.height, info.dpi_scale
                ),
                b64,
            ),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::CaptureFailed(e.to_string())),
        }
    }

    /// Burst-capture frames to observe animations, returning up to 12 evenly spaced PNG frames plus an animated GIF path on disk.
    #[tool(
        name = "capture_frames",
        description = "Burst-capture frames over duration_ms (default 1500ms) at fps (default 10) so animations are visible. Returns up to 12 evenly spaced PNG frames in chat plus an animated GIF saved to disk."
    )]
    async fn capture_frames(
        &self,
        Parameters(params): Parameters<CaptureFramesParams>,
    ) -> CallToolResult {
        let this = self.clone();
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, Vec<String>, String, usize)> {
            let info = this.resolve_window(params.window_id)?;
            let duration_ms = params.duration_ms.unwrap_or(1200).clamp(100, 10_000);
            let fps = params.fps.unwrap_or(8).clamp(1, 30);
            let max_images = params
                .max_frames
                .unwrap_or(this.config.default_max_frames)
                .clamp(1, 12);
            let interval = Duration::from_secs_f64(1.0 / fps as f64);

            let start = Instant::now();
            let end = start + Duration::from_millis(duration_ms);
            let mut frames: Vec<CapturedFrame> = Vec::new();

            while Instant::now() < end {
                let tick_start = Instant::now();
                if let Ok(frame) = this.capture.capture_window(info.id) {
                    frames.push(frame);
                }
                let elapsed = tick_start.elapsed();
                if elapsed < interval {
                    let remaining = (end.saturating_duration_since(Instant::now())).min(interval - elapsed);
                    if !remaining.is_zero() {
                        std::thread::sleep(remaining);
                    }
                }
            }

            if frames.is_empty() {
                frames.push(this.capture.capture_window(info.id)?);
            }

            let total_captured = frames.len();
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let gif_path = this
                .config
                .resolved_capture_dir()
                .join(format!("rustsight_{}_{}.gif", info.id, ts));
            let delay_ms = (1000 / fps).max(20);
            encode_gif(&frames, delay_ms, &gif_path)?;

            // Sample evenly spaced frames and downscale for MCP token efficiency
            let mut sampled_b64 = Vec::new();
            let indices: Vec<usize> = if total_captured <= max_images {
                (0..total_captured).collect()
            } else if max_images == 1 {
                vec![total_captured - 1]
            } else {
                (0..max_images)
                    .map(|i| (i * (total_captured - 1)) / (max_images - 1))
                    .collect()
            };
            for idx in indices {
                let (scaled, scale) =
                    downscale_for_mcp(frames[idx].clone(), this.config.max_image_width)?;
                if let Ok(mut scales) = this.coord_scales.lock() {
                    scales.insert(info.id, scale);
                }
                sampled_b64.push(encode_png_base64(&scaled)?);
            }

            Ok((info, sampled_b64, gif_path.display().to_string(), total_captured))
        })
        .await;

        match res {
            Ok(Ok((info, images, gif_path, total))) => {
                let mut contents = vec![Content::text(format!(
                    "Captured {total} frames of `{}` (window_id={}), returning {} evenly spaced frames.\nAnimated GIF saved to: {gif_path}",
                    info.title,
                    info.id,
                    images.len()
                ))];
                for img in images {
                    contents.push(Content::image(img, "image/png"));
                }
                CallToolResult::success(contents)
            }
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::CaptureFailed(e.to_string())),
        }
    }

    /// Click at window-relative pixel coordinates (x, y) and return a fresh screenshot.
    #[tool(
        name = "click",
        description = "Click at (x, y) in screenshot pixel coordinates relative to the top-left of the window. Supports left/right/middle button and double-click. Waits for the UI to settle and returns a fresh PNG screenshot."
    )]
    async fn click(&self, Parameters(params): Parameters<ClickParams>) -> CallToolResult {
        let this = self.clone();
        let include = params.include_screenshot.unwrap_or(true);
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, Option<String>)> {
            let info = this.resolve_window(params.window_id)?;
            let (sx, sy) = this.scale_coords(info.id, params.x, params.y);
            let btn = MouseButton::parse(params.button.as_deref().unwrap_or("left"))?;
            this.input.click(&info, sx, sy, btn, params.double)?;
            if include {
                let (info, b64) = this.settle_and_capture(info.id)?;
                Ok((info, Some(b64)))
            } else {
                Ok((info, None))
            }
        })
        .await;

        match res {
            Ok(Ok((info, Some(b64)))) => Self::ok_with_screenshot(
                format!("Clicked ({}, {}) on `{}`", params.x, params.y, info.title),
                b64,
            ),
            Ok(Ok((info, None))) => CallToolResult::success(vec![Content::text(format!(
                "Clicked ({}, {}) on `{}`",
                params.x, params.y, info.title
            ))]),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::InputFailed(e.to_string())),
        }
    }

    /// Type text into the focused window and return a fresh screenshot.
    #[tool(
        name = "type_text",
        description = "Type a text string into the target window (focuses the window first). Waits for the UI to settle and returns a fresh PNG screenshot unless include_screenshot=false."
    )]
    async fn type_text(&self, Parameters(params): Parameters<TypeTextParams>) -> CallToolResult {
        let this = self.clone();
        let include = params.include_screenshot.unwrap_or(true);
        let text_len = params.text.chars().count();
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, Option<String>)> {
            let info = this.resolve_window(params.window_id)?;
            this.input.type_text(&info, &params.text)?;
            if include {
                let (info, b64) = this.settle_and_capture(info.id)?;
                Ok((info, Some(b64)))
            } else {
                Ok((info, None))
            }
        })
        .await;

        match res {
            Ok(Ok((info, Some(b64)))) => {
                Self::ok_with_screenshot(format!("Typed {text_len} chars into `{}`", info.title), b64)
            }
            Ok(Ok((info, None))) => CallToolResult::success(vec![Content::text(format!(
                "Typed {text_len} chars into `{}`",
                info.title
            ))]),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::InputFailed(e.to_string())),
        }
    }

    /// Press a key with optional modifiers and return a fresh screenshot.
    #[tool(
        name = "press_key",
        description = "Press a keyboard key with optional modifiers (ctrl, alt, shift, win). Waits for the UI to settle and returns a fresh PNG screenshot unless include_screenshot=false."
    )]
    async fn press_key(&self, Parameters(params): Parameters<PressKeyParams>) -> CallToolResult {
        let this = self.clone();
        let include = params.include_screenshot.unwrap_or(true);
        let key_desc = params.key.clone();
        let mods_desc = params.modifiers.clone();
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, Option<String>)> {
            let info = this.resolve_window(params.window_id)?;
            let mods = Modifiers::from_list(&params.modifiers);
            this.input.press_key(&info, &params.key, &mods)?;
            if include {
                let (info, b64) = this.settle_and_capture(info.id)?;
                Ok((info, Some(b64)))
            } else {
                Ok((info, None))
            }
        })
        .await;

        match res {
            Ok(Ok((info, Some(b64)))) => Self::ok_with_screenshot(
                format!("Pressed `{key_desc}` (mods={mods_desc:?}) on `{}`", info.title),
                b64,
            ),
            Ok(Ok((info, None))) => CallToolResult::success(vec![Content::text(format!(
                "Pressed `{key_desc}` (mods={mods_desc:?}) on `{}`",
                info.title
            ))]),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::InputFailed(e.to_string())),
        }
    }

    /// Scroll at window-relative coordinates (x, y) and return a fresh screenshot.
    #[tool(
        name = "scroll",
        description = "Scroll at (x, y) in screenshot coordinates. Positive dy scrolls down, negative dy scrolls up. Waits for the UI to settle and returns a fresh PNG screenshot unless include_screenshot=false."
    )]
    async fn scroll(&self, Parameters(params): Parameters<ScrollParams>) -> CallToolResult {
        let this = self.clone();
        let include = params.include_screenshot.unwrap_or(true);
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, Option<String>)> {
            let info = this.resolve_window(params.window_id)?;
            let (sx, sy) = this.scale_coords(info.id, params.x, params.y);
            this.input.scroll(&info, sx, sy, params.dx, params.dy)?;
            if include {
                let (info, b64) = this.settle_and_capture(info.id)?;
                Ok((info, Some(b64)))
            } else {
                Ok((info, None))
            }
        })
        .await;

        match res {
            Ok(Ok((info, Some(b64)))) => Self::ok_with_screenshot(
                format!("Scrolled ({}, {}) at ({}, {}) on `{}`", params.dx, params.dy, params.x, params.y, info.title),
                b64,
            ),
            Ok(Ok((info, None))) => CallToolResult::success(vec![Content::text(format!(
                "Scrolled ({}, {}) at ({}, {}) on `{}`",
                params.dx, params.dy, params.x, params.y, info.title
            ))]),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::InputFailed(e.to_string())),
        }
    }

    /// Drag from (from_x, from_y) to (to_x, to_y) in window-relative coordinates and return a fresh screenshot.
    #[tool(
        name = "drag",
        description = "Press left mouse button at (from_x, from_y), drag smoothly to (to_x, to_y), and release. Waits for the UI to settle and returns a fresh PNG screenshot unless include_screenshot=false."
    )]
    async fn drag(&self, Parameters(params): Parameters<DragParams>) -> CallToolResult {
        let this = self.clone();
        let include = params.include_screenshot.unwrap_or(true);
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, Option<String>)> {
            let info = this.resolve_window(params.window_id)?;
            let (fx, fy) = this.scale_coords(info.id, params.from_x, params.from_y);
            let (tx, ty) = this.scale_coords(info.id, params.to_x, params.to_y);
            this.input.drag(&info, fx, fy, tx, ty)?;
            if include {
                let (info, b64) = this.settle_and_capture(info.id)?;
                Ok((info, Some(b64)))
            } else {
                Ok((info, None))
            }
        })
        .await;

        match res {
            Ok(Ok((info, Some(b64)))) => Self::ok_with_screenshot(
                format!("Dragged ({}, {}) -> ({}, {}) on `{}`", params.from_x, params.from_y, params.to_x, params.to_y, info.title),
                b64,
            ),
            Ok(Ok((info, None))) => CallToolResult::success(vec![Content::text(format!(
                "Dragged ({}, {}) -> ({}, {}) on `{}`",
                params.from_x, params.from_y, params.to_x, params.to_y, info.title
            ))]),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::InputFailed(e.to_string())),
        }
    }

    /// Wait until consecutive frames of the window stop changing, then return a fresh screenshot.
    #[tool(
        name = "wait_idle",
        description = "Wait until the window stops changing visually (compares consecutive frames) up to timeout_ms (default 5000ms), then returns a fresh PNG screenshot."
    )]
    async fn wait_idle(&self, Parameters(params): Parameters<WaitIdleParams>) -> CallToolResult {
        let this = self.clone();
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, String, u128)> {
            let info = this.resolve_window(params.window_id)?;
            let timeout_ms = params.timeout_ms.unwrap_or(5000);
            let start = Instant::now();
            let deadline = start + Duration::from_millis(timeout_ms);

            let mut prev = this.capture.capture_window(info.id)?;
            loop {
                std::thread::sleep(Duration::from_millis(120));
                let curr = this.capture.capture_window(info.id)?;
                if !frames_differ(&prev, &curr, 0.001) {
                    let (scaled, scale) =
                        downscale_for_mcp(curr, this.config.max_image_width)?;
                    if let Ok(mut scales) = this.coord_scales.lock() {
                        scales.insert(info.id, scale);
                    }
                    let b64 = encode_png_base64(&scaled)?;
                    return Ok((info, b64, start.elapsed().as_millis()));
                }
                if Instant::now() >= deadline {
                    return Err(RustSightError::IdleTimeout(timeout_ms));
                }
                prev = curr;
            }
        })
        .await;

        match res {
            Ok(Ok((info, b64, elapsed))) => Self::ok_with_screenshot(
                format!(
                    "Window `{}` (window_id={}) settled after {elapsed} ms",
                    info.title, info.id
                ),
                b64,
            ),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::CaptureFailed(e.to_string())),
        }
    }

    /// Kill the currently launched app, rebuild it with `cargo build`, relaunch it, and return the new window_id and screenshot.
    #[tool(
        name = "rebuild",
        description = "Kill the current application, re-run `cargo build` using the previous launch target, relaunch it, and return the new window_id and fresh PNG screenshot while keeping the MCP session alive."
    )]
    async fn rebuild(&self) -> CallToolResult {
        let this = self.clone();
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, String)> {
            let timeout = Duration::from_secs(this.config.build_timeout_secs);
            let info = {
                let mut pm = this.processes.lock().map_err(|_| {
                    RustSightError::CaptureFailed("ProcessManager lock poisoned".into())
                })?;
                pm.rebuild(this.capture.as_ref().as_ref(), timeout)?
            };
            let b64 = this.capture_and_encode(info.id)?;
            Ok((info, b64))
        })
        .await;

        match res {
            Ok(Ok((info, b64))) => Self::ok_with_screenshot(
                format!(
                    "Rebuilt and relaunched `{}` (pid={}, window_id={}, size={}x{})",
                    info.title, info.pid, info.id, info.rect.width, info.rect.height
                ),
                b64,
            ),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::LaunchFailed("rebuild".into(), e.to_string())),
        }
    }

    /// Close the tracked application window and terminate its process.
    #[tool(
        name = "close",
        description = "Close the launched application window and terminate its child process."
    )]
    async fn close(&self, Parameters(params): Parameters<WindowIdParams>) -> CallToolResult {
        let this = self.clone();
        let res = tokio::task::spawn_blocking(move || -> AppResult<WindowId> {
            let mut pm = this.processes.lock().map_err(|_| {
                RustSightError::CaptureFailed("ProcessManager lock poisoned".into())
            })?;
            pm.close(params.window_id)
        })
        .await;

        match res {
            Ok(Ok(wid)) => CallToolResult::success(vec![Content::text(format!(
                "Closed window {wid} and terminated its process."
            ))]),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::CaptureFailed(e.to_string())),
        }
    }
}

#[tool_handler]
impl ServerHandler for RustSightServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("rustsight", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "RustSight lets you launch, see (via PNG screenshots & GIF frame bursts), and control any native GUI application. \
                 Call `launch` with a Cargo project directory or binary path first, then interact using `click`, `type_text`, `press_key`, `scroll`, `drag`, `capture_frames`, `wait_idle`, `rebuild`, and `close`.",
            )
    }
}
