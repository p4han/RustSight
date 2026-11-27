use crate::backend::{
    CapturedFrame, InputBackend, Modifiers, MouseButton, WindowCaptureBackend, WindowId, WindowInfo,
};
use crate::capture::{
    coord_scale, crop_frame, downscale_for_mcp, encode_gif, encode_png_base64, frames_differ,
    image_to_physical_coords,
};
use crate::config::Config;
use crate::error::{Result as AppResult, RustSightError};
use crate::process::{
    LaunchSpec, ProcessManager, is_pid_or_descendant_tracked, prepare_launch, process_exe_name,
};
use crate::tools::{
    CaptureFramesParams, ClickParams, DragParams, GetLogsParams, LaunchParams, PressKeyParams,
    ScreenshotParams, ScrollParams, TypeTextParams, WaitIdleParams, WindowIdParams,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock as Content, Implementation, ServerCapabilities, ServerConfig,
};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct RustSightServer {
    config: Arc<Config>,
    capture: Arc<Box<dyn WindowCaptureBackend>>,
    input: Arc<Box<dyn InputBackend>>,
    processes: Arc<Mutex<ProcessManager>>,
    /// Serializes `launch` and `rebuild` so concurrent Cargo builds do not contend on `target/`.
    /// Other tools never acquire this gate.
    launch_gate: Arc<Mutex<()>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

fn lock_recover<'a, T>(mutex: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("{name} mutex was poisoned; recovering inner state");
        poisoned.into_inner()
    })
}

struct CropBox {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

struct EncodedImage {
    base64_png: String,
    width: u32,
    height: u32,
    scale: f64,
}

struct SettledCapture {
    img: EncodedImage,
    unsettled_ms: Option<u128>,
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
            launch_gate: Arc::new(Mutex::new(())),
            tool_router: Self::tool_router(),
        }
    }

    pub fn shutdown(&self) {
        let mut pm = lock_recover(&self.processes, "ProcessManager");
        pm.cleanup_all();
    }

    fn try_lock_launch_gate(&self, op: &str) -> AppResult<MutexGuard<'_, ()>> {
        match self.launch_gate.try_lock() {
            Ok(guard) => Ok(guard),
            Err(TryLockError::Poisoned(poisoned)) => {
                tracing::warn!("launch_gate mutex was poisoned; recovering inner state");
                Ok(poisoned.into_inner())
            }
            Err(TryLockError::WouldBlock) => Err(RustSightError::LaunchFailed(
                op.to_string(),
                "Another launch or rebuild is already in progress; wait for it to finish before starting a new one.".to_string(),
            )),
        }
    }

    fn resolve_window(&self, window_id: Option<WindowId>) -> AppResult<WindowInfo> {
        let (wid, tracked_pids) = {
            let pm = lock_recover(&self.processes, "ProcessManager");
            (pm.resolve_window_id(window_id)?, pm.tracked_pids())
        };
        let info = self.capture.get_window_info(wid)?;
        if !self.is_window_info_allowed(&info, &tracked_pids) {
            return Err(RustSightError::WindowNotAllowed {
                window_id: info.id,
                pid: info.pid,
                title: info.title,
            });
        }
        Ok(info)
    }

    fn close_window_checked(&self, window_id: Option<WindowId>) -> AppResult<WindowId> {
        let info = self.resolve_window(window_id)?;
        let mut pm = lock_recover(&self.processes, "ProcessManager");
        pm.close(Some(info.id))
    }

    fn is_window_info_allowed(&self, info: &WindowInfo, tracked_pids: &HashSet<u32>) -> bool {
        if is_pid_or_descendant_tracked(info.pid, tracked_pids) {
            return true;
        }
        if self.config.allow_attach {
            return is_window_allowed(
                info.pid,
                tracked_pids,
                true,
                &self.config.allow_process,
                None,
            );
        }
        if self.config.allow_process.is_empty() {
            return is_window_allowed(info.pid, tracked_pids, false, &[], None);
        }
        let proc_name = process_exe_name(info.pid);
        is_window_allowed(
            info.pid,
            tracked_pids,
            false,
            &self.config.allow_process,
            proc_name.as_deref(),
        )
    }

    fn encode_frame(&self, frame: CapturedFrame) -> AppResult<EncodedImage> {
        let (scaled, scale) = downscale_for_mcp(frame, self.config.max_image_width)?;
        let width = scaled.width;
        let height = scaled.height;
        let base64_png = encode_png_base64(&scaled)?;
        Ok(EncodedImage {
            base64_png,
            width,
            height,
            scale,
        })
    }

    fn capture_and_encode(&self, wid: WindowId) -> AppResult<EncodedImage> {
        let frame = self.capture.capture_window(wid)?;
        self.encode_frame(frame)
    }

    fn resolve_image_coords(&self, info: &WindowInfo, x: i32, y: i32) -> AppResult<(i32, i32)> {
        if info.rect.width == 0 || info.rect.height == 0 {
            return Err(RustSightError::InputFailed(format!(
                "Window `{}` has zero client dimensions ({}x{})",
                info.title, info.rect.width, info.rect.height
            )));
        }
        let scale = coord_scale(info.rect.width, self.config.max_image_width);
        let img_w = if scale == 1.0 {
            info.rect.width
        } else {
            self.config.max_image_width
        };
        let img_h = ((info.rect.height as f64) / scale).round().max(1.0) as u32;
        if x < 0 || y < 0 || x >= img_w as i32 || y >= img_h as i32 {
            return Err(RustSightError::InputFailed(format!(
                "Coordinates ({x}, {y}) are outside screenshot bounds ({img_w}x{img_h} image px, scale={scale:.3}x, physical={}x{}). Expected 0 <= x < {img_w} and 0 <= y < {img_h}.",
                info.rect.width, info.rect.height
            )));
        }
        let (px, py) = image_to_physical_coords(x, y, info.rect.width, self.config.max_image_width);
        Ok((
            px.clamp(0, info.rect.width.saturating_sub(1) as i32),
            py.clamp(0, info.rect.height.saturating_sub(1) as i32),
        ))
    }

    fn settle_and_capture(&self, wid: WindowId) -> AppResult<(WindowInfo, SettledCapture)> {
        let start = Instant::now();
        let min_settle = Duration::from_millis(self.config.settle_ms);
        let max_settle =
            Duration::from_millis(self.config.settle_max_ms.max(self.config.settle_ms));
        if !min_settle.is_zero() {
            std::thread::sleep(min_settle);
        }

        let mut prev = self.capture.capture_window(wid)?;
        let mut unsettled_ms = None;

        if start.elapsed() < max_settle {
            loop {
                let remaining = max_settle.saturating_sub(start.elapsed());
                if remaining.is_zero() {
                    unsettled_ms = Some(start.elapsed().as_millis());
                    break;
                }
                std::thread::sleep(remaining.min(Duration::from_millis(40)));
                let curr = self.capture.capture_window(wid)?;
                match evaluate_settle_step(&prev, &curr, start.elapsed(), max_settle, 0.001) {
                    SettleDecision::Settled => {
                        prev = curr;
                        break;
                    }
                    SettleDecision::TimedOut => {
                        prev = curr;
                        unsettled_ms = Some(start.elapsed().as_millis());
                        break;
                    }
                    SettleDecision::KeepWaiting => {
                        prev = curr;
                    }
                }
            }
        }

        let info = self.capture.get_window_info(wid)?;
        let img = self.encode_frame(prev)?;
        Ok((info, SettledCapture { img, unsettled_ms }))
    }

    fn format_settle_suffix(unsettled_ms: Option<u128>) -> String {
        match unsettled_ms {
            Some(ms) => format!(" — UI was still changing after {ms} ms"),
            None => String::new(),
        }
    }

    fn run_wait_idle(
        &self,
        window_id: Option<WindowId>,
        timeout_ms: u64,
        threshold: f64,
    ) -> AppResult<(WindowInfo, EncodedImage, u128, bool)> {
        if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
            return Err(RustSightError::InputFailed(format!(
                "Invalid `threshold` {threshold}: expected a finite number in [0.0, 1.0]."
            )));
        }
        let info = self.resolve_window(window_id)?;
        let start = Instant::now();
        let deadline = start + Duration::from_millis(timeout_ms);

        let mut prev = self.capture.capture_window(info.id)?;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let img = self.encode_frame(prev)?;
                return Ok((info, img, start.elapsed().as_millis(), false));
            }
            let chunk = remaining.min(Duration::from_millis(240));
            let burst = self.capture.capture_burst(info.id, chunk, 8)?;
            for curr in burst {
                if !frames_differ(&prev, &curr, threshold) {
                    let img = self.encode_frame(curr)?;
                    return Ok((info, img, start.elapsed().as_millis(), true));
                }
                prev = curr;
            }
            if Instant::now() >= deadline {
                let img = self.encode_frame(prev)?;
                return Ok((info, img, start.elapsed().as_millis(), false));
            }
        }
    }

    fn format_image_meta(img: &EncodedImage, info: &WindowInfo) -> String {
        format!(
            "image={}x{}, scale={:.3}x, dpi_scale={:.2}",
            img.width, img.height, img.scale, info.dpi_scale
        )
    }

    fn ok_with_screenshot(summary: String, img: EncodedImage) -> CallToolResult {
        CallToolResult::success(vec![
            Content::text(summary),
            Content::image(img.base64_png, "image/png"),
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
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, EncodedImage)> {
            let _gate = this.try_lock_launch_gate("launch")?;
            let spec = LaunchSpec {
                path_or_cargo_dir: params.path_or_cargo_dir,
                args: params.args,
                width: params.width.or(this.config.default_width),
                height: params.height.or(this.config.default_height),
                bin: params.bin,
                package: params.package,
                release: params.release,
                features: params.features,
                all_features: params.all_features,
            };
            let build_timeout = Duration::from_secs(this.config.build_timeout_secs);
            let window_timeout = Duration::from_secs(this.config.window_timeout_secs);
            let prepared = prepare_launch(
                spec,
                this.capture.as_ref().as_ref(),
                build_timeout,
                window_timeout,
            )?;
            let info = {
                let mut pm = lock_recover(&this.processes, "ProcessManager");
                pm.register_launched(prepared)
            };
            let img = this.capture_and_encode(info.id)?;
            Ok((info, img))
        })
        .await;

        match res {
            Ok(Ok((info, img))) => Self::ok_with_screenshot(
                format!(
                    "Launched `{}` (pid={}, window_id={}, {})",
                    info.title,
                    info.pid,
                    info.id,
                    Self::format_image_meta(&img, &info)
                ),
                img,
            ),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::LaunchFailed("task".into(), e.to_string())),
        }
    }

    /// List visible top-level GUI windows allowed by the active attach policy (id, pid, title, size).
    #[tool(
        name = "list_windows",
        description = "List visible top-level GUI windows allowed by the current policy (by default only apps launched by RustSight; use --allow-attach or --allow-process to attach to other apps)."
    )]
    async fn list_windows(&self) -> CallToolResult {
        let this = self.clone();
        let res = tokio::task::spawn_blocking(move || -> AppResult<Vec<WindowInfo>> {
            let tracked_pids = {
                let pm = lock_recover(&this.processes, "ProcessManager");
                pm.tracked_pids()
            };
            let windows = this.capture.list_windows()?;
            Ok(windows
                .into_iter()
                .filter(|w| this.is_window_info_allowed(w, &tracked_pids))
                .collect())
        })
        .await;
        match res {
            Ok(Ok(windows)) => {
                let lines: Vec<String> = windows
                    .into_iter()
                    .map(|w| {
                        format!(
                            "window_id={} pid={} {}x{} `{}`",
                            w.id, w.pid, w.rect.width, w.rect.height, w.title
                        )
                    })
                    .collect();
                let body = if lines.is_empty() {
                    if !self.config.allow_attach && self.config.allow_process.is_empty() {
                        "No allowed windows found. Attach mode is off by default (only windows launched by RustSight are accessible). To enable attaching to existing windows, restart RustSight with `--allow-attach` or `--allow-process <NAME>` (e.g., `--allow-process notepad.exe`).".to_string()
                    } else if !self.config.allow_attach {
                        "No allowed windows found matching launched apps or `--allow-process`."
                            .to_string()
                    } else {
                        "No visible windows found.".to_string()
                    }
                } else {
                    lines.join("\n")
                };
                CallToolResult::success(vec![Content::text(body)])
            }
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::CaptureFailed(e.to_string())),
        }
    }

    /// Capture a PNG screenshot of the tracked window (or specified window_id), optionally cropped to a sub-region.
    #[tool(
        name = "screenshot",
        description = "Capture the current window client area as a PNG screenshot. Optionally pass `region` ({x, y, width, height}) in full-window screenshot coordinates (pixels of the most recent full-window screenshot) to crop to a specific widget and save tokens. Input tools always use full-window screenshot coordinates."
    )]
    async fn screenshot(&self, Parameters(params): Parameters<ScreenshotParams>) -> CallToolResult {
        let this = self.clone();
        let res = tokio::task::spawn_blocking(
            move || -> AppResult<(WindowInfo, EncodedImage, Option<CropBox>)> {
                let info = this.resolve_window(params.window_id)?;
                if let Some(reg) = params.region {
                    let frame = this.capture.capture_window(info.id)?;
                    let full_scale = coord_scale(frame.width, this.config.max_image_width);
                    let phys_x = (reg.x as f64 * full_scale).round() as u32;
                    let phys_y = (reg.y as f64 * full_scale).round() as u32;
                    let phys_w = (reg.width as f64 * full_scale).round() as u32;
                    let phys_h = (reg.height as f64 * full_scale).round() as u32;
                    let cropped = crop_frame(&frame, phys_x, phys_y, phys_w, phys_h)?;
                    let actual_x = phys_x.min(frame.width.saturating_sub(1));
                    let actual_y = phys_y.min(frame.height.saturating_sub(1));
                    let crop_box = CropBox {
                        x: (actual_x as f64 / full_scale).round() as u32,
                        y: (actual_y as f64 / full_scale).round() as u32,
                        width: (cropped.width as f64 / full_scale).round().max(1.0) as u32,
                        height: (cropped.height as f64 / full_scale).round().max(1.0) as u32,
                    };
                    let img = this.encode_frame(cropped)?;
                    Ok((info, img, Some(crop_box)))
                } else {
                    let img = this.capture_and_encode(info.id)?;
                    Ok((info, img, None))
                }
            },
        )
        .await;

        match res {
            Ok(Ok((info, img, Some(crop)))) => Self::ok_with_screenshot(
                format!(
                    "Cropped screenshot of `{}` (window_id={}, {}). Crop region in full-screenshot space: origin=({}, {}), size={}x{}. Click/scroll/drag always use full-screenshot coordinates — add ({}, {}) to crop-local coordinates.",
                    info.title,
                    info.id,
                    Self::format_image_meta(&img, &info),
                    crop.x,
                    crop.y,
                    crop.width,
                    crop.height,
                    crop.x,
                    crop.y
                ),
                img,
            ),
            Ok(Ok((info, img, None))) => Self::ok_with_screenshot(
                format!(
                    "Screenshot of `{}` (window_id={}, {})",
                    info.title,
                    info.id,
                    Self::format_image_meta(&img, &info)
                ),
                img,
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
        let res = tokio::task::spawn_blocking(
            move || -> AppResult<(WindowInfo, Vec<EncodedImage>, String, usize)> {
                let info = this.resolve_window(params.window_id)?;
                let duration_ms = params.duration_ms.unwrap_or(1200).clamp(100, 10_000);
                let fps = params.fps.unwrap_or(8).clamp(1, 30);
                let max_images = params
                    .max_frames
                    .unwrap_or(this.config.default_max_frames)
                    .clamp(1, 12);
                let frames =
                    this.capture
                        .capture_burst(info.id, Duration::from_millis(duration_ms), fps)?;

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
                let mut sampled_imgs = Vec::new();
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
                    sampled_imgs.push(this.encode_frame(frames[idx].clone())?);
                }

                Ok((
                    info,
                    sampled_imgs,
                    gif_path.display().to_string(),
                    total_captured,
                ))
            },
        )
        .await;

        match res {
            Ok(Ok((info, images, gif_path, total))) => {
                let meta = images
                    .first()
                    .map(|img| Self::format_image_meta(img, &info))
                    .unwrap_or_else(|| format!("dpi_scale={:.2}", info.dpi_scale));
                let mut contents = vec![Content::text(format!(
                    "Captured {total} frames of `{}` (window_id={}, {meta}), returning {} evenly spaced frames.\nAnimated GIF saved to: {gif_path}",
                    info.title,
                    info.id,
                    images.len()
                ))];
                for img in images {
                    contents.push(Content::image(img.base64_png, "image/png"));
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
        description = "Click at (x, y) in pixels of the most recent full-window screenshot (relative to the top-left of the client area). Supports left/right/middle button and double-click. Waits for the UI to settle and returns a fresh PNG screenshot."
    )]
    async fn click(&self, Parameters(params): Parameters<ClickParams>) -> CallToolResult {
        let this = self.clone();
        let include = params.include_screenshot.unwrap_or(true);
        let res = tokio::task::spawn_blocking(
            move || -> AppResult<(WindowInfo, Option<SettledCapture>)> {
                let info = this.resolve_window(params.window_id)?;
                let (sx, sy) = this.resolve_image_coords(&info, params.x, params.y)?;
                let btn = MouseButton::parse(params.button.as_deref().unwrap_or("left"))?;
                this.input.click(&info, sx, sy, btn, params.double)?;
                if include {
                    let (info, cap) = this.settle_and_capture(info.id)?;
                    Ok((info, Some(cap)))
                } else {
                    Ok((info, None))
                }
            },
        )
        .await;

        match res {
            Ok(Ok((info, Some(cap)))) => Self::ok_with_screenshot(
                format!(
                    "Clicked ({}, {}) on `{}` ({}){}",
                    params.x,
                    params.y,
                    info.title,
                    Self::format_image_meta(&cap.img, &info),
                    Self::format_settle_suffix(cap.unsettled_ms)
                ),
                cap.img,
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
        let res = tokio::task::spawn_blocking(
            move || -> AppResult<(WindowInfo, Option<SettledCapture>)> {
                let info = this.resolve_window(params.window_id)?;
                this.input.type_text(&info, &params.text)?;
                if include {
                    let (info, cap) = this.settle_and_capture(info.id)?;
                    Ok((info, Some(cap)))
                } else {
                    Ok((info, None))
                }
            },
        )
        .await;

        match res {
            Ok(Ok((info, Some(cap)))) => Self::ok_with_screenshot(
                format!(
                    "Typed {text_len} chars into `{}` ({}){}",
                    info.title,
                    Self::format_image_meta(&cap.img, &info),
                    Self::format_settle_suffix(cap.unsettled_ms)
                ),
                cap.img,
            ),
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
        let res = tokio::task::spawn_blocking(
            move || -> AppResult<(WindowInfo, Option<SettledCapture>)> {
                let info = this.resolve_window(params.window_id)?;
                let mods = Modifiers::from_list(&params.modifiers);
                this.input.press_key(&info, &params.key, &mods)?;
                if include {
                    let (info, cap) = this.settle_and_capture(info.id)?;
                    Ok((info, Some(cap)))
                } else {
                    Ok((info, None))
                }
            },
        )
        .await;

        match res {
            Ok(Ok((info, Some(cap)))) => Self::ok_with_screenshot(
                format!(
                    "Pressed `{key_desc}` (mods={mods_desc:?}) on `{}` ({}){}",
                    info.title,
                    Self::format_image_meta(&cap.img, &info),
                    Self::format_settle_suffix(cap.unsettled_ms)
                ),
                cap.img,
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
        description = "Scroll at (x, y) in pixels of the most recent full-window screenshot. Positive dy scrolls down, negative dy scrolls up. Waits for the UI to settle and returns a fresh PNG screenshot unless include_screenshot=false."
    )]
    async fn scroll(&self, Parameters(params): Parameters<ScrollParams>) -> CallToolResult {
        let this = self.clone();
        let include = params.include_screenshot.unwrap_or(true);
        let res = tokio::task::spawn_blocking(
            move || -> AppResult<(WindowInfo, Option<SettledCapture>)> {
                let info = this.resolve_window(params.window_id)?;
                let (sx, sy) = this.resolve_image_coords(&info, params.x, params.y)?;
                this.input.scroll(&info, sx, sy, params.dx, params.dy)?;
                if include {
                    let (info, cap) = this.settle_and_capture(info.id)?;
                    Ok((info, Some(cap)))
                } else {
                    Ok((info, None))
                }
            },
        )
        .await;

        match res {
            Ok(Ok((info, Some(cap)))) => Self::ok_with_screenshot(
                format!(
                    "Scrolled ({}, {}) at ({}, {}) on `{}` ({}){}",
                    params.dx,
                    params.dy,
                    params.x,
                    params.y,
                    info.title,
                    Self::format_image_meta(&cap.img, &info),
                    Self::format_settle_suffix(cap.unsettled_ms)
                ),
                cap.img,
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
        description = "Press left mouse button at (from_x, from_y) and drag smoothly to (to_x, to_y), using pixels of the most recent full-window screenshot. Waits for the UI to settle and returns a fresh PNG screenshot unless include_screenshot=false."
    )]
    async fn drag(&self, Parameters(params): Parameters<DragParams>) -> CallToolResult {
        let this = self.clone();
        let include = params.include_screenshot.unwrap_or(true);
        let res = tokio::task::spawn_blocking(
            move || -> AppResult<(WindowInfo, Option<SettledCapture>)> {
                let info = this.resolve_window(params.window_id)?;
                let (fx, fy) = this.resolve_image_coords(&info, params.from_x, params.from_y)?;
                let (tx, ty) = this.resolve_image_coords(&info, params.to_x, params.to_y)?;
                this.input.drag(&info, fx, fy, tx, ty)?;
                if include {
                    let (info, cap) = this.settle_and_capture(info.id)?;
                    Ok((info, Some(cap)))
                } else {
                    Ok((info, None))
                }
            },
        )
        .await;

        match res {
            Ok(Ok((info, Some(cap)))) => Self::ok_with_screenshot(
                format!(
                    "Dragged ({}, {}) -> ({}, {}) on `{}` ({}){}",
                    params.from_x,
                    params.from_y,
                    params.to_x,
                    params.to_y,
                    info.title,
                    Self::format_image_meta(&cap.img, &info),
                    Self::format_settle_suffix(cap.unsettled_ms)
                ),
                cap.img,
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
        description = "Wait until the window stops changing visually (compares consecutive frames using optional `threshold`, default 0.001) up to timeout_ms (default 5000ms), then returns a fresh PNG screenshot. Never fails on timeout; returns the latest frame with a note if still changing."
    )]
    async fn wait_idle(&self, Parameters(params): Parameters<WaitIdleParams>) -> CallToolResult {
        let this = self.clone();
        let timeout_ms = params.timeout_ms.unwrap_or(5000);
        let threshold = params.threshold.unwrap_or(0.001);
        let res = tokio::task::spawn_blocking(move || {
            this.run_wait_idle(params.window_id, timeout_ms, threshold)
        })
        .await;

        match res {
            Ok(Ok((info, img, elapsed, true))) => Self::ok_with_screenshot(
                format!(
                    "Window `{}` (window_id={}) settled after {elapsed} ms ({})",
                    info.title,
                    info.id,
                    Self::format_image_meta(&img, &info)
                ),
                img,
            ),
            Ok(Ok((info, img, _elapsed, false))) => Self::ok_with_screenshot(
                format!(
                    "Window did not settle within {timeout_ms} ms (still changing, for example an animation or blinking caret) ({})",
                    Self::format_image_meta(&img, &info)
                ),
                img,
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
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, EncodedImage)> {
            let _gate = this.try_lock_launch_gate("rebuild")?;
            let (spec, old_proc) = {
                let mut pm = lock_recover(&this.processes, "ProcessManager");
                pm.take_rebuild_target()?
            };
            // Kill/drop the previous process outside the ProcessManager lock.
            drop(old_proc);

            let build_timeout = Duration::from_secs(this.config.build_timeout_secs);
            let window_timeout = Duration::from_secs(this.config.window_timeout_secs);
            let prepared = prepare_launch(
                spec,
                this.capture.as_ref().as_ref(),
                build_timeout,
                window_timeout,
            )?;
            let info = {
                let mut pm = lock_recover(&this.processes, "ProcessManager");
                pm.register_launched(prepared)
            };
            let img = this.capture_and_encode(info.id)?;
            Ok((info, img))
        })
        .await;

        match res {
            Ok(Ok((info, img))) => Self::ok_with_screenshot(
                format!(
                    "Rebuilt and relaunched `{}` (pid={}, window_id={}, {})",
                    info.title,
                    info.pid,
                    info.id,
                    Self::format_image_meta(&img, &info)
                ),
                img,
            ),
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::LaunchFailed(
                "rebuild".into(),
                e.to_string(),
            )),
        }
    }

    /// Read buffered stdout/stderr log lines from a launched application.
    #[tool(
        name = "get_logs",
        description = "Return recent stdout/stderr log lines captured from a launched application (default last 200 lines; stderr lines are prefixed with [stderr]). Optionally pass `clear=true` to empty the buffer after reading."
    )]
    async fn get_logs(&self, Parameters(params): Parameters<GetLogsParams>) -> CallToolResult {
        let this = self.clone();
        let tail = params.tail.unwrap_or(200);
        let clear = params.clear;
        let res = tokio::task::spawn_blocking(move || -> AppResult<(WindowInfo, Vec<String>)> {
            let info = this.resolve_window(params.window_id)?;
            let pm = lock_recover(&this.processes, "ProcessManager");
            let lines = pm.get_logs(info.id, tail, clear);
            Ok((info, lines))
        })
        .await;

        match res {
            Ok(Ok((info, lines))) => {
                let body = if lines.is_empty() {
                    format!(
                        "No logs captured for `{}` (window_id={}).",
                        info.title, info.id
                    )
                } else {
                    lines.join("\n")
                };
                CallToolResult::success(vec![Content::text(body)])
            }
            Ok(Err(e)) => Self::err_result(e),
            Err(e) => Self::err_result(RustSightError::CaptureFailed(e.to_string())),
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
            this.close_window_checked(params.window_id)
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
                 Call `launch` with a Cargo project directory or binary path first, then interact using `click`, `type_text`, `press_key`, `scroll`, `drag`, `capture_frames`, `wait_idle`, `get_logs`, `rebuild`, and `close`.",
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettleDecision {
    Settled,
    TimedOut,
    KeepWaiting,
}

fn evaluate_settle_step(
    prev: &CapturedFrame,
    curr: &CapturedFrame,
    elapsed: Duration,
    max_wait: Duration,
    threshold: f64,
) -> SettleDecision {
    if !frames_differ(prev, curr, threshold) {
        SettleDecision::Settled
    } else if elapsed >= max_wait {
        SettleDecision::TimedOut
    } else {
        SettleDecision::KeepWaiting
    }
}

/// Pure policy check deciding whether a window with `window_pid` may be accessed.
pub fn is_window_allowed(
    window_pid: u32,
    tracked_pids: &HashSet<u32>,
    allow_attach: bool,
    allow_process: &[String],
    process_name: Option<&str>,
) -> bool {
    if tracked_pids.contains(&window_pid) || allow_attach {
        return true;
    }
    let Some(name) = process_name else {
        return false;
    };
    allow_process
        .iter()
        .any(|allowed| allowed.trim().eq_ignore_ascii_case(name.trim()))
}

#[cfg(test)]
mod tests {
    use super::{AppResult, RustSightError, is_window_allowed};
    use std::collections::HashSet;

    #[test]
    fn launched_pid_is_allowed_by_default() {
        let tracked = HashSet::from([1234]);
        assert!(is_window_allowed(1234, &tracked, false, &[], None));
    }

    #[test]
    fn unknown_pid_is_denied_by_default() {
        let tracked = HashSet::from([1234]);
        assert!(!is_window_allowed(
            5678,
            &tracked,
            false,
            &[],
            Some("notepad.exe")
        ));
    }

    #[test]
    fn allow_attach_allows_any_window() {
        let tracked = HashSet::new();
        assert!(is_window_allowed(5678, &tracked, true, &[], None));
    }

    #[test]
    fn allow_process_allows_only_matching_names_case_insensitively() {
        let tracked = HashSet::new();
        let allowed = vec!["NotePad.exe".to_string(), "calc.exe".to_string()];
        assert!(is_window_allowed(
            5678,
            &tracked,
            false,
            &allowed,
            Some("notepad.exe")
        ));
        assert!(is_window_allowed(
            5679,
            &tracked,
            false,
            &allowed,
            Some("CALC.EXE")
        ));
        assert!(!is_window_allowed(
            5680,
            &tracked,
            false,
            &allowed,
            Some("1password.exe")
        ));
    }

    #[test]
    fn unreadable_process_name_is_denied_even_with_allow_process() {
        let tracked = HashSet::new();
        let allowed = vec!["notepad.exe".to_string()];
        assert!(!is_window_allowed(5678, &tracked, false, &allowed, None));
    }

    #[test]
    fn close_rejects_unlaunched_window_when_restricted_and_allows_with_attach() {
        use crate::backend::{
            CapturedFrame, InputBackend, Modifiers, MouseButton, WindowCaptureBackend, WindowId,
            WindowInfo, WindowRect,
        };
        use crate::config::Config;
        use clap::Parser;

        struct MockCapture(WindowInfo);
        impl WindowCaptureBackend for MockCapture {
            fn init(&self) -> AppResult<()> {
                Ok(())
            }
            fn find_window_by_pid(&self, _pid: u32) -> AppResult<Option<WindowInfo>> {
                Ok(None)
            }
            fn list_windows(&self) -> AppResult<Vec<WindowInfo>> {
                Ok(vec![self.0.clone()])
            }
            fn get_window_info(&self, window_id: WindowId) -> AppResult<WindowInfo> {
                if window_id == self.0.id {
                    Ok(self.0.clone())
                } else {
                    Err(RustSightError::WindowNotFound(window_id))
                }
            }
            fn is_window_alive(&self, window_id: WindowId) -> bool {
                window_id == self.0.id
            }
            fn capture_window(&self, _window_id: WindowId) -> AppResult<CapturedFrame> {
                Ok(CapturedFrame {
                    width: 100,
                    height: 100,
                    rgba: vec![0; 100 * 100 * 4],
                })
            }
            fn resize_window(&self, _window_id: WindowId, _w: u32, _h: u32) -> AppResult<()> {
                Ok(())
            }
            fn focus_window(&self, _window_id: WindowId) -> AppResult<()> {
                Ok(())
            }
        }

        struct NoopInput;
        impl InputBackend for NoopInput {
            fn click(
                &self,
                _: &WindowInfo,
                _: i32,
                _: i32,
                _: MouseButton,
                _: bool,
            ) -> AppResult<()> {
                Ok(())
            }
            fn type_text(&self, _: &WindowInfo, _: &str) -> AppResult<()> {
                Ok(())
            }
            fn press_key(&self, _: &WindowInfo, _: &str, _: &Modifiers) -> AppResult<()> {
                Ok(())
            }
            fn scroll(&self, _: &WindowInfo, _: i32, _: i32, _: i32, _: i32) -> AppResult<()> {
                Ok(())
            }
            fn drag(&self, _: &WindowInfo, _: i32, _: i32, _: i32, _: i32) -> AppResult<()> {
                Ok(())
            }
        }

        let foreign_win = WindowInfo {
            id: 9999,
            pid: 424242,
            title: "Foreign App".to_string(),
            rect: WindowRect {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
            dpi_scale: 1.0,
            is_minimized: false,
        };

        let restricted_cfg = Config::parse_from(["rustsight"]);
        let restricted = super::RustSightServer::new(
            restricted_cfg,
            Box::new(MockCapture(foreign_win.clone())),
            Box::new(NoopInput),
        );
        let err = restricted.close_window_checked(Some(9999)).unwrap_err();
        assert!(matches!(err, RustSightError::WindowNotAllowed { .. }));

        let attach_cfg = Config::parse_from(["rustsight", "--allow-attach"]);
        let allowed = super::RustSightServer::new(
            attach_cfg,
            Box::new(MockCapture(foreign_win)),
            Box::new(NoopInput),
        );
        assert_eq!(allowed.close_window_checked(Some(9999)).unwrap(), 9999);
    }

    #[test]
    fn adaptive_settle_decision_logic() {
        use crate::backend::CapturedFrame;
        use std::time::Duration;

        let f1 = CapturedFrame {
            width: 2,
            height: 2,
            rgba: vec![10; 16],
        };
        let f_same = f1.clone();
        let mut f_diff = f1.clone();
        f_diff.rgba[0] = 250;

        assert_eq!(
            super::evaluate_settle_step(
                &f1,
                &f_same,
                Duration::from_millis(50),
                Duration::from_millis(500),
                0.001
            ),
            super::SettleDecision::Settled
        );
        assert_eq!(
            super::evaluate_settle_step(
                &f1,
                &f_diff,
                Duration::from_millis(200),
                Duration::from_millis(500),
                0.001
            ),
            super::SettleDecision::KeepWaiting
        );
        assert_eq!(
            super::evaluate_settle_step(
                &f1,
                &f_diff,
                Duration::from_millis(500),
                Duration::from_millis(500),
                0.001
            ),
            super::SettleDecision::TimedOut
        );
    }

    #[test]
    fn wait_idle_timeout_returns_non_fatal_latest_frame_with_note() {
        use crate::backend::{
            CapturedFrame, InputBackend, Modifiers, MouseButton, WindowCaptureBackend, WindowId,
            WindowInfo, WindowRect,
        };
        use crate::config::Config;
        use clap::Parser;
        use std::sync::atomic::{AtomicU8, Ordering};

        struct ChangingCapture(AtomicU8);
        impl WindowCaptureBackend for ChangingCapture {
            fn init(&self) -> AppResult<()> {
                Ok(())
            }
            fn find_window_by_pid(&self, _pid: u32) -> AppResult<Option<WindowInfo>> {
                Ok(None)
            }
            fn list_windows(&self) -> AppResult<Vec<WindowInfo>> {
                Ok(Vec::new())
            }
            fn get_window_info(&self, window_id: WindowId) -> AppResult<WindowInfo> {
                Ok(WindowInfo {
                    id: window_id,
                    pid: 1,
                    title: "Animated".into(),
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
            fn capture_window(&self, _window_id: WindowId) -> AppResult<CapturedFrame> {
                let v = self.0.fetch_add(40, Ordering::Relaxed);
                Ok(CapturedFrame {
                    width: 4,
                    height: 4,
                    rgba: vec![v; 4 * 4 * 4],
                })
            }
            fn resize_window(&self, _window_id: WindowId, _w: u32, _h: u32) -> AppResult<()> {
                Ok(())
            }
            fn focus_window(&self, _window_id: WindowId) -> AppResult<()> {
                Ok(())
            }
        }

        struct NoopInput;
        impl InputBackend for NoopInput {
            fn click(
                &self,
                _: &WindowInfo,
                _: i32,
                _: i32,
                _: MouseButton,
                _: bool,
            ) -> AppResult<()> {
                Ok(())
            }
            fn type_text(&self, _: &WindowInfo, _: &str) -> AppResult<()> {
                Ok(())
            }
            fn press_key(&self, _: &WindowInfo, _: &str, _: &Modifiers) -> AppResult<()> {
                Ok(())
            }
            fn scroll(&self, _: &WindowInfo, _: i32, _: i32, _: i32, _: i32) -> AppResult<()> {
                Ok(())
            }
            fn drag(&self, _: &WindowInfo, _: i32, _: i32, _: i32, _: i32) -> AppResult<()> {
                Ok(())
            }
        }

        let cfg = Config::parse_from(["rustsight", "--allow-attach"]);
        let server = super::RustSightServer::new(
            cfg,
            Box::new(ChangingCapture(AtomicU8::new(0))),
            Box::new(NoopInput),
        );
        let (info, _img, _elapsed, settled) = server
            .run_wait_idle(Some(777), 80, 0.001)
            .expect("wait_idle timeout must be non-fatal");
        assert_eq!(info.id, 777);
        assert!(!settled);
        assert!(server.run_wait_idle(Some(777), 80, 1.5).is_err());
    }
}
