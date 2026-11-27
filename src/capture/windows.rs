use crate::backend::{
    CapturedFrame, WindowCaptureBackend, WindowId, WindowInfo, WindowRect, burst_frame_interval,
    default_capture_burst,
};
use crate::error::{Result, RustSightError};
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{debug, warn};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_UNSUPPORTED, IDXGIDevice,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, ClientToScreen, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClientRect, GetWindow, GetWindowLongW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, SW_RESTORE, SWP_NOMOVE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos,
    ShowWindow, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, Interface};

struct CachedStaging {
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    texture: ID3D11Texture2D,
}

struct CachedD3d {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    staging: Option<CachedStaging>,
}

// We protect `CachedD3d` (including `ID3D11DeviceContext` and the cached staging texture)
// with a `std::sync::Mutex` rather than `ID3D11Multithread` because `CopyResource` + `Map`/`Unmap`
// + staging texture reuse must execute atomically per frame without another thread mutating or
// unmapping the shared staging texture concurrently.
unsafe impl Send for CachedD3d {}
unsafe impl Sync for CachedD3d {}

impl CachedD3d {
    fn new() -> Result<Self> {
        let (device, context) = create_d3d11_device()?;
        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|e| RustSightError::CaptureFailed(format!("IDXGIDevice cast: {e}")))?;
        let inspectable = unsafe {
            CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device).map_err(|e| {
                RustSightError::CaptureFailed(format!("CreateDirect3D11DeviceFromDXGIDevice: {e}"))
            })?
        };
        let winrt_device: IDirect3DDevice = inspectable
            .cast()
            .map_err(|e| RustSightError::CaptureFailed(format!("IDirect3DDevice cast: {e}")))?;

        Ok(Self {
            device,
            context,
            winrt_device,
            staging: None,
        })
    }

    fn staging_for(
        &mut self,
        src_desc: &D3D11_TEXTURE2D_DESC,
    ) -> std::result::Result<ID3D11Texture2D, WgcAttemptError> {
        if let Some(ref st) = self.staging
            && st.width == src_desc.Width
            && st.height == src_desc.Height
            && st.format == src_desc.Format
        {
            return Ok(st.texture.clone());
        }

        let mut desc = *src_desc;
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;

        let mut staging = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut staging))
                .map_err(|e| classify_d3d_err(&self.device, "CreateTexture2D staging", e))?;
        }
        let texture = staging.ok_or_else(|| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(
                "Missing staging texture".into(),
            ))
        })?;
        self.staging = Some(CachedStaging {
            width: src_desc.Width,
            height: src_desc.Height,
            format: src_desc.Format,
            texture: texture.clone(),
        });
        Ok(texture)
    }
}

pub struct WindowsCaptureBackend {
    cache: Mutex<Option<CachedD3d>>,
}

impl WindowsCaptureBackend {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(None),
        }
    }

    fn lock_cache(&self) -> std::sync::MutexGuard<'_, Option<CachedD3d>> {
        self.cache.lock().unwrap_or_else(|poisoned| {
            warn!("D3D11 capture cache mutex was poisoned; recovering");
            poisoned.into_inner()
        })
    }

    fn run_with_d3d<T, F>(&self, mut op: F) -> Result<T>
    where
        F: FnMut(&mut CachedD3d) -> std::result::Result<T, WgcAttemptError>,
    {
        for attempt in 0..2 {
            let mut guard = self.lock_cache();
            if guard.is_none() {
                *guard = Some(CachedD3d::new()?);
            }
            let cached = guard.as_mut().expect("cache initialized");
            match op(cached) {
                Ok(val) => return Ok(val),
                Err(WgcAttemptError::DeviceLost(err)) if attempt == 0 => {
                    warn!("D3D11 device lost ({err}); dropping cached device and retrying once");
                    *guard = None;
                }
                Err(WgcAttemptError::DeviceLost(err)) | Err(WgcAttemptError::Other(err)) => {
                    return Err(err);
                }
            }
        }
        unreachable!()
    }
}

impl WindowCaptureBackend for WindowsCaptureBackend {
    fn init(&self) -> Result<()> {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        Ok(())
    }

    fn find_window_by_pid(&self, pid: u32) -> Result<Option<WindowInfo>> {
        struct SearchState {
            target_pid: u32,
            found: Option<HWND>,
        }

        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            unsafe {
                let state = &mut *(lparam.0 as *mut SearchState);
                let mut win_pid: u32 = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut win_pid));
                if win_pid != state.target_pid {
                    return BOOL(1);
                }

                if !IsWindowVisible(hwnd).as_bool() {
                    return BOOL(1);
                }

                let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                if (ex_style & WS_EX_TOOLWINDOW.0) != 0 {
                    return BOOL(1);
                }

                let has_owner = GetWindow(hwnd, GW_OWNER).is_ok();
                let is_app_window = (ex_style & WS_EX_APPWINDOW.0) != 0;
                if has_owner && !is_app_window {
                    return BOOL(1);
                }

                let mut rect = RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_ok() {
                    let w = rect.right - rect.left;
                    let h = rect.bottom - rect.top;
                    if w > 10 && h > 10 {
                        state.found = Some(hwnd);
                        return BOOL(0);
                    }
                }

                BOOL(1)
            }
        }

        let mut state = SearchState {
            target_pid: pid,
            found: None,
        };
        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut state as *mut _ as isize));
        }

        match state.found {
            Some(hwnd) => Ok(Some(get_hwnd_info(hwnd)?)),
            None => Ok(None),
        }
    }

    fn list_windows(&self) -> Result<Vec<WindowInfo>> {
        unsafe extern "system" fn enum_all(hwnd: HWND, lparam: LPARAM) -> BOOL {
            unsafe {
                let list = &mut *(lparam.0 as *mut Vec<HWND>);
                if !IsWindowVisible(hwnd).as_bool() {
                    return BOOL(1);
                }
                let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                if (ex_style & WS_EX_TOOLWINDOW.0) != 0 {
                    return BOOL(1);
                }
                let has_owner = GetWindow(hwnd, GW_OWNER).is_ok();
                let is_app_window = (ex_style & WS_EX_APPWINDOW.0) != 0;
                if has_owner && !is_app_window {
                    return BOOL(1);
                }
                let mut rect = RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_ok()
                    && (rect.right - rect.left) > 10
                    && (rect.bottom - rect.top) > 10
                {
                    list.push(hwnd);
                }
                BOOL(1)
            }
        }

        let mut hwnds: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(enum_all), LPARAM(&mut hwnds as *mut _ as isize));
        }

        let mut out = Vec::new();
        for hwnd in hwnds {
            if let Ok(info) = get_hwnd_info(hwnd)
                && !info.title.is_empty()
                && info.rect.width > 0
                && info.rect.height > 0
            {
                out.push(info);
            }
        }
        Ok(out)
    }

    fn get_window_info(&self, window_id: WindowId) -> Result<WindowInfo> {
        let hwnd = hwnd_from_id(window_id)?;
        get_hwnd_info(hwnd)
    }

    fn is_window_alive(&self, window_id: WindowId) -> bool {
        let hwnd = HWND(window_id as *mut c_void);
        unsafe { IsWindow(Some(hwnd)).as_bool() }
    }

    fn capture_window(&self, window_id: WindowId) -> Result<CapturedFrame> {
        let start = Instant::now();
        let hwnd = hwnd_from_id(window_id)?;
        let res = match self.run_with_d3d(|cached| capture_with_wgc(hwnd, cached)) {
            Ok(frame) => Ok(frame),
            Err(wgc_err) => {
                debug!("WGC capture failed ({wgc_err}), falling back to PrintWindow");
                capture_with_print_window(hwnd).map_err(|pw_err| {
                    RustSightError::CaptureFailed(format!(
                        "WGC failed: {wgc_err}; PrintWindow fallback failed: {pw_err}"
                    ))
                })
            }
        };
        debug!(
            "capture_window(window_id={}) took {:.2} ms",
            window_id,
            start.elapsed().as_secs_f64() * 1000.0
        );
        res
    }

    fn capture_burst(
        &self,
        window_id: WindowId,
        duration: Duration,
        fps: u32,
    ) -> Result<Vec<CapturedFrame>> {
        let start = Instant::now();
        let hwnd = hwnd_from_id(window_id)?;
        let res = match self.run_with_d3d(|cached| capture_burst_wgc(hwnd, cached, duration, fps)) {
            Ok(frames) if !frames.is_empty() => Ok(frames),
            Ok(_) => {
                debug!("WGC burst delivered 0 frames (static window), falling back to PrintWindow");
                default_capture_burst(self, window_id, duration, fps)
            }
            Err(e) => {
                debug!("WGC burst failed ({e}), falling back to per-frame capture loop");
                default_capture_burst(self, window_id, duration, fps)
            }
        };
        if let Ok(ref frames) = res {
            let secs = start.elapsed().as_secs_f64().max(1e-6);
            debug!(
                "capture_burst(window_id={}) captured {} frames in {:.2} ms ({:.1} fps)",
                window_id,
                frames.len(),
                secs * 1000.0,
                frames.len() as f64 / secs
            );
        }
        res
    }

    fn resize_window(&self, window_id: WindowId, width: u32, height: u32) -> Result<()> {
        let hwnd = hwnd_from_id(window_id)?;
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                width as i32,
                height as i32,
                SWP_NOMOVE | SWP_NOZORDER,
            )
            .map_err(|e| RustSightError::CaptureFailed(format!("SetWindowPos failed: {e}")))?;
        }
        Ok(())
    }

    fn focus_window(&self, window_id: WindowId) -> Result<()> {
        let hwnd = hwnd_from_id(window_id)?;
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(())
    }
}

fn hwnd_from_id(window_id: WindowId) -> Result<HWND> {
    let hwnd = HWND(window_id as *mut c_void);
    if unsafe { !IsWindow(Some(hwnd)).as_bool() } {
        return Err(RustSightError::WindowNotFound(window_id));
    }
    Ok(hwnd)
}

fn get_hwnd_info(hwnd: HWND) -> Result<WindowInfo> {
    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        let len = GetWindowTextLengthW(hwnd);
        let title = if len > 0 {
            let mut buf = vec![0u16; (len + 1) as usize];
            let copied = GetWindowTextW(hwnd, &mut buf);
            String::from_utf16_lossy(&buf[..copied as usize])
        } else {
            String::new()
        };

        let rect = get_window_bounds(hwnd)?;
        let dpi = GetDpiForWindow(hwnd);
        let dpi_scale = if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 };
        let is_minimized = IsIconic(hwnd).as_bool();

        Ok(WindowInfo {
            id: hwnd.0 as usize as u64,
            pid,
            title,
            rect,
            dpi_scale,
            is_minimized,
        })
    }
}

/// Returns the screen-space client area rectangle (excluding title bar and window borders).
fn get_window_bounds(hwnd: HWND) -> Result<WindowRect> {
    unsafe {
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_ok() {
            let mut origin = POINT { x: 0, y: 0 };
            if ClientToScreen(hwnd, &mut origin).as_bool() {
                let width = (client.right - client.left).max(0) as u32;
                let height = (client.bottom - client.top).max(0) as u32;
                if width > 0 && height > 0 {
                    return Ok(WindowRect {
                        x: origin.x,
                        y: origin.y,
                        width,
                        height,
                    });
                }
            }
        }

        let mut rect = RECT::default();
        let dwm_res = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut _ as *mut c_void,
            size_of::<RECT>() as u32,
        );
        if dwm_res.is_err() {
            GetWindowRect(hwnd, &mut rect)
                .map_err(|e| RustSightError::CaptureFailed(format!("GetWindowRect failed: {e}")))?;
        }
        let width = (rect.right - rect.left).max(0) as u32;
        let height = (rect.bottom - rect.top).max(0) as u32;
        Ok(WindowRect {
            x: rect.left,
            y: rect.top,
            width,
            height,
        })
    }
}

fn create_d3d11_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    unsafe {
        let mut device = None;
        let mut context = None;
        let feature_levels = [D3D_FEATURE_LEVEL_11_0];
        let res = D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        );
        if let Err(e) = res {
            if e.code() == DXGI_ERROR_UNSUPPORTED {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&feature_levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
                .map_err(|e2| {
                    RustSightError::CaptureFailed(format!("D3D11CreateDevice WARP failed: {e2}"))
                })?;
            } else {
                return Err(RustSightError::CaptureFailed(format!(
                    "D3D11CreateDevice failed: {e}"
                )));
            }
        }
        Ok((
            device.ok_or_else(|| RustSightError::CaptureFailed("No D3D11 device".into()))?,
            context.ok_or_else(|| RustSightError::CaptureFailed("No D3D11 context".into()))?,
        ))
    }
}

enum WgcAttemptError {
    DeviceLost(RustSightError),
    Other(RustSightError),
}

fn is_device_lost_error(err: &windows::core::Error) -> bool {
    let code = err.code();
    code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET
}

fn classify_d3d_err(
    device: &ID3D11Device,
    context: &str,
    err: windows::core::Error,
) -> WgcAttemptError {
    let lost = is_device_lost_error(&err) || unsafe { device.GetDeviceRemovedReason().is_err() };
    let wrapped = RustSightError::CaptureFailed(format!("{context}: {err}"));
    if lost {
        WgcAttemptError::DeviceLost(wrapped)
    } else {
        WgcAttemptError::Other(wrapped)
    }
}

struct WgcPoolGuard(Option<Direct3D11CaptureFramePool>);
impl Drop for WgcPoolGuard {
    fn drop(&mut self) {
        if let Some(pool) = self.0.take() {
            let _ = pool.Close();
        }
    }
}

struct WgcSessionGuard(Option<GraphicsCaptureSession>);
impl Drop for WgcSessionGuard {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            let _ = session.Close();
        }
    }
}

struct MappedGuard<'a> {
    context: &'a ID3D11DeviceContext,
    resource: &'a ID3D11Resource,
}
impl Drop for MappedGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            self.context.Unmap(self.resource, 0);
        }
    }
}

fn create_capture_item(hwnd: HWND) -> std::result::Result<GraphicsCaptureItem, WgcAttemptError> {
    unsafe {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| {
                WgcAttemptError::Other(RustSightError::CaptureFailed(format!(
                    "IGraphicsCaptureItemInterop: {e}"
                )))
            })?;
        interop.CreateForWindow(hwnd).map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!(
                "CreateForWindow: {e}"
            )))
        })
    }
}

fn extract_wgc_frame(
    hwnd: HWND,
    cached: &mut CachedD3d,
    frame: &Direct3D11CaptureFrame,
) -> std::result::Result<CapturedFrame, WgcAttemptError> {
    unsafe {
        let surface = frame.Surface().map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!("frame.Surface: {e}")))
        })?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!(
                "IDirect3DDxgiInterfaceAccess: {e}"
            )))
        })?;
        let texture: ID3D11Texture2D = access.GetInterface().map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!(
                "GetInterface<ID3D11Texture2D>: {e}"
            )))
        })?;

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        let staging = cached.staging_for(&desc)?;

        let src_res: ID3D11Resource = texture.cast().map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!(
                "src resource cast: {e}"
            )))
        })?;
        let dst_res: ID3D11Resource = staging.cast().map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!(
                "dst resource cast: {e}"
            )))
        })?;
        cached.context.CopyResource(&dst_res, &src_res);

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        cached
            .context
            .Map(&dst_res, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| classify_d3d_err(&cached.device, "Map staging texture", e))?;
        let _mapped_guard = MappedGuard {
            context: &cached.context,
            resource: &dst_res,
        };

        let tex_w = desc.Width;
        let tex_h = desc.Height;
        let row_pitch = mapped.RowPitch as usize;

        // WGC captures the full GetWindowRect (including title bar and DWM drop-shadow borders).
        // Crop to get_window_bounds() (client area) so the image has no title bar/shadow border
        // and its top-left pixel aligns 1:1 with the client-area origin used for input.
        let mut full_rect = RECT::default();
        let (crop_x, crop_y, crop_w, crop_h) = if GetWindowRect(hwnd, &mut full_rect).is_ok() {
            if let Ok(ext) = get_window_bounds(hwnd) {
                let ox = (ext.x - full_rect.left).max(0) as u32;
                let oy = (ext.y - full_rect.top).max(0) as u32;
                let cw = ext.width.min(tex_w.saturating_sub(ox));
                let ch = ext.height.min(tex_h.saturating_sub(oy));
                if cw > 0 && ch > 0 {
                    (ox, oy, cw, ch)
                } else {
                    (0, 0, tex_w, tex_h)
                }
            } else {
                (0, 0, tex_w, tex_h)
            }
        } else {
            (0, 0, tex_w, tex_h)
        };

        let mut rgba = vec![0u8; (crop_w * crop_h * 4) as usize];
        let src_ptr = mapped.pData as *const u8;
        for y in 0..crop_h as usize {
            let src_y = y + crop_y as usize;
            let src_row = std::slice::from_raw_parts(
                src_ptr.add(src_y * row_pitch + (crop_x as usize * 4)),
                (crop_w * 4) as usize,
            );
            let dst_row = &mut rgba[y * (crop_w as usize * 4)..(y + 1) * (crop_w as usize * 4)];
            for (src_px, dst_px) in src_row
                .as_chunks::<4>()
                .0
                .iter()
                .zip(dst_row.as_chunks_mut::<4>().0.iter_mut())
            {
                // BGRA -> RGBA
                dst_px[0] = src_px[2];
                dst_px[1] = src_px[1];
                dst_px[2] = src_px[0];
                dst_px[3] = 255;
            }
        }

        Ok(CapturedFrame {
            width: crop_w,
            height: crop_h,
            rgba,
        })
    }
}

fn capture_with_wgc(
    hwnd: HWND,
    cached: &mut CachedD3d,
) -> std::result::Result<CapturedFrame, WgcAttemptError> {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            return Err(WgcAttemptError::Other(RustSightError::CaptureFailed(
                "Window is minimized".to_string(),
            )));
        }

        let item = create_capture_item(hwnd)?;
        let size = item.Size().map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!("item.Size: {e}")))
        })?;
        if size.Width <= 0 || size.Height <= 0 {
            return Err(WgcAttemptError::Other(RustSightError::CaptureFailed(
                "Window has zero capture size".to_string(),
            )));
        }

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &cached.winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            1,
            size,
        )
        .map_err(|e| classify_d3d_err(&cached.device, "Direct3D11CaptureFramePool", e))?;
        let _pool_guard = WgcPoolGuard(Some(pool.clone()));

        let session = pool.CreateCaptureSession(&item).map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!(
                "CreateCaptureSession: {e}"
            )))
        })?;
        let _session_guard = WgcSessionGuard(Some(session.clone()));

        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(false);

        session.StartCapture().map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!("StartCapture: {e}")))
        })?;

        let deadline = Instant::now() + Duration::from_millis(300);
        let frame = loop {
            match pool.TryGetNextFrame() {
                Ok(f) => break f,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => {
                    return Err(WgcAttemptError::Other(RustSightError::CaptureFailed(
                        format!("Timed out waiting for WGC frame: {e}"),
                    )));
                }
            }
        };

        extract_wgc_frame(hwnd, cached, &frame)
    }
}

fn capture_burst_wgc(
    hwnd: HWND,
    cached: &mut CachedD3d,
    duration: Duration,
    fps: u32,
) -> std::result::Result<Vec<CapturedFrame>, WgcAttemptError> {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            return Err(WgcAttemptError::Other(RustSightError::CaptureFailed(
                "Window is minimized".to_string(),
            )));
        }

        let item = create_capture_item(hwnd)?;
        let mut current_size = item.Size().map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!("item.Size: {e}")))
        })?;
        if current_size.Width <= 0 || current_size.Height <= 0 {
            return Err(WgcAttemptError::Other(RustSightError::CaptureFailed(
                "Window has zero capture size".to_string(),
            )));
        }

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &cached.winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            current_size,
        )
        .map_err(|e| classify_d3d_err(&cached.device, "Direct3D11CaptureFramePool", e))?;
        let _pool_guard = WgcPoolGuard(Some(pool.clone()));

        let session = pool.CreateCaptureSession(&item).map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!(
                "CreateCaptureSession: {e}"
            )))
        })?;
        let _session_guard = WgcSessionGuard(Some(session.clone()));

        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(false);

        session.StartCapture().map_err(|e| {
            WgcAttemptError::Other(RustSightError::CaptureFailed(format!("StartCapture: {e}")))
        })?;

        let interval = burst_frame_interval(fps);
        let start = Instant::now();
        let end = start + duration;
        let mut frames = Vec::new();
        let mut last_frame: Option<CapturedFrame> = None;

        while Instant::now() < end || frames.is_empty() {
            let tick_start = Instant::now();

            // Recreate the pool if the window was resized mid-burst.
            if let Ok(new_size) = item.Size()
                && new_size.Width > 0
                && new_size.Height > 0
                && (new_size.Width != current_size.Width || new_size.Height != current_size.Height)
            {
                let _ = pool.Recreate(
                    &cached.winrt_device,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    2,
                    new_size,
                );
                current_size = new_size;
            }

            let mut latest: Option<Direct3D11CaptureFrame> = None;
            while let Ok(f) = pool.TryGetNextFrame() {
                latest = Some(f);
            }

            // On the very first tick, wait briefly for WGC to deliver an initial frame.
            if latest.is_none() && last_frame.is_none() {
                let first_wait_end =
                    (Instant::now() + Duration::from_millis(120)).min(end.max(Instant::now()));
                while Instant::now() < first_wait_end {
                    if let Ok(f) = pool.TryGetNextFrame() {
                        latest = Some(f);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }

            if let Some(wgc_frame) = latest {
                if let Ok(content_size) = wgc_frame.ContentSize()
                    && content_size.Width > 0
                    && content_size.Height > 0
                    && (content_size.Width != current_size.Width
                        || content_size.Height != current_size.Height)
                {
                    let _ = pool.Recreate(
                        &cached.winrt_device,
                        DirectXPixelFormat::B8G8R8A8UIntNormalized,
                        2,
                        content_size,
                    );
                    current_size = content_size;
                }
                let captured = extract_wgc_frame(hwnd, cached, &wgc_frame)?;
                last_frame = Some(captured.clone());
                frames.push(captured);
            } else if let Some(ref prev) = last_frame {
                // Static window: reuse previous frame so the burst keeps its timing.
                frames.push(prev.clone());
            } else {
                // Static window delivered no initial WGC frame; let caller fall back to PrintWindow.
                break;
            }

            if Instant::now() >= end {
                break;
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

        Ok(frames)
    }
}

fn capture_with_print_window(hwnd: HWND) -> Result<CapturedFrame> {
    unsafe {
        let bounds = get_window_bounds(hwnd)?;
        if bounds.width == 0 || bounds.height == 0 {
            return Err(RustSightError::CaptureFailed(
                "Window has zero dimensions".to_string(),
            ));
        }

        let hdc_screen = GetDC(None);
        let hdc_mem = CreateCompatibleDC(Some(hdc_screen));

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: bounds.width as i32,
                biHeight: -(bounds.height as i32), // top-down DIB
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut c_void = std::ptr::null_mut();
        let hbitmap = CreateDIBSection(Some(hdc_mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| RustSightError::CaptureFailed(format!("CreateDIBSection: {e}")))?;

        let old_obj = SelectObject(hdc_mem, hbitmap.into());
        // PW_CLIENTONLY (1) | PW_RENDERFULLCONTENT (2) = 3 (client area only, no title bar)
        let ok = PrintWindow(hwnd, hdc_mem, PRINT_WINDOW_FLAGS(3)).as_bool();
        if !ok {
            warn!(
                "PrintWindow PW_CLIENTONLY|PW_RENDERFULLCONTENT returned false; attempting PW_CLIENTONLY=1"
            );
            let _ = PrintWindow(hwnd, hdc_mem, PRINT_WINDOW_FLAGS(1));
        }

        let len = (bounds.width * bounds.height * 4) as usize;
        let mut rgba = vec![0u8; len];
        if !bits.is_null() {
            let src = std::slice::from_raw_parts(bits as *const u8, len);
            for (s, d) in src
                .as_chunks::<4>()
                .0
                .iter()
                .zip(rgba.as_chunks_mut::<4>().0.iter_mut())
            {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = 255;
            }
        }

        SelectObject(hdc_mem, old_obj);
        let _ = DeleteObject(hbitmap.into());
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(None, hdc_screen);

        Ok(CapturedFrame {
            width: bounds.width,
            height: bounds.height,
            rgba,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_d3d_device_is_created_once_and_reused() {
        let backend = WindowsCaptureBackend::new();
        let _ = backend.init();
        let ptr1 = backend
            .run_with_d3d(|cached| Ok(cached.device.as_raw() as usize))
            .expect("first d3d device");
        let ptr2 = backend
            .run_with_d3d(|cached| Ok(cached.device.as_raw() as usize))
            .expect("reused d3d device");
        assert_eq!(ptr1, ptr2);
    }

    #[test]
    #[ignore = "Requires a live desktop window; run manually to benchmark capture speed"]
    fn benchmark_live_window_capture() {
        let backend = WindowsCaptureBackend::new();
        let _ = backend.init();
        let windows = backend.list_windows().expect("list_windows");
        let win = windows
            .into_iter()
            .next()
            .expect("at least one visible window");

        // Warm up cached device
        let _ = backend.capture_window(win.id).expect("warmup");

        let mut single_times = Vec::new();
        for _ in 0..10 {
            let t = Instant::now();
            let _ = backend.capture_window(win.id).expect("single capture");
            single_times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let avg_ms = single_times.iter().sum::<f64>() / single_times.len() as f64;

        let burst_start = Instant::now();
        let burst = backend
            .capture_burst(win.id, Duration::from_secs(2), 30)
            .expect("2s 30fps burst");
        let burst_secs = burst_start.elapsed().as_secs_f64();
        let achieved_fps = burst.len() as f64 / burst_secs;

        println!(
            "WINDOW `{}` ({}x{}): avg_single={:.2} ms, burst_2s_30fps={} frames in {:.2} s ({:.1} fps)",
            win.title,
            win.rect.width,
            win.rect.height,
            avg_ms,
            burst.len(),
            burst_secs,
            achieved_fps
        );
    }
}
