use crate::backend::{CapturedFrame, WindowCaptureBackend, WindowId, WindowInfo, WindowRect};
use crate::error::{Result, RustSightError};
use std::ffi::c_void;
use std::mem::size_of;
use std::time::{Duration, Instant};
use tracing::{debug, warn};
use windows::core::{BOOL, Interface};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, DXGI_ERROR_UNSUPPORTED};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
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
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, SW_RESTORE, SWP_NOMOVE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos,
    ShowWindow, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

pub struct WindowsCaptureBackend;

impl WindowsCaptureBackend {
    pub fn new() -> Self {
        Self
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

    fn get_window_info(&self, window_id: WindowId) -> Result<WindowInfo> {
        let hwnd = hwnd_from_id(window_id)?;
        get_hwnd_info(hwnd)
    }

    fn is_window_alive(&self, window_id: WindowId) -> bool {
        let hwnd = HWND(window_id as *mut c_void);
        unsafe { IsWindow(Some(hwnd)).as_bool() }
    }

    fn capture_window(&self, window_id: WindowId) -> Result<CapturedFrame> {
        let hwnd = hwnd_from_id(window_id)?;
        match capture_with_wgc(hwnd) {
            Ok(frame) => Ok(frame),
            Err(wgc_err) => {
                debug!("WGC capture failed ({wgc_err}), falling back to PrintWindow");
                capture_with_print_window(hwnd).map_err(|pw_err| {
                    RustSightError::CaptureFailed(format!(
                        "WGC failed: {wgc_err}; PrintWindow fallback failed: {pw_err}"
                    ))
                })
            }
        }
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

fn get_window_bounds(hwnd: HWND) -> Result<WindowRect> {
    unsafe {
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

fn capture_with_wgc(hwnd: HWND) -> Result<CapturedFrame> {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            return Err(RustSightError::CaptureFailed(
                "Window is minimized".to_string(),
            ));
        }

        let (d3d_device, d3d_context) = create_d3d11_device()?;
        let dxgi_device: IDXGIDevice = d3d_device
            .cast()
            .map_err(|e| RustSightError::CaptureFailed(format!("IDXGIDevice cast: {e}")))?;

        let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device).map_err(|e| {
            RustSightError::CaptureFailed(format!("CreateDirect3D11DeviceFromDXGIDevice: {e}"))
        })?;
        let winrt_device: IDirect3DDevice = inspectable
            .cast()
            .map_err(|e| RustSightError::CaptureFailed(format!("IDirect3DDevice cast: {e}")))?;

        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| {
            RustSightError::CaptureFailed(format!("IGraphicsCaptureItemInterop: {e}"))
        })?;
        let item: GraphicsCaptureItem = interop
            .CreateForWindow(hwnd)
            .map_err(|e| RustSightError::CaptureFailed(format!("CreateForWindow: {e}")))?;

        let size = item
            .Size()
            .map_err(|e| RustSightError::CaptureFailed(format!("item.Size: {e}")))?;
        if size.Width <= 0 || size.Height <= 0 {
            return Err(RustSightError::CaptureFailed(
                "Window has zero capture size".to_string(),
            ));
        }

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            1,
            size,
        )
        .map_err(|e| {
            RustSightError::CaptureFailed(format!("Direct3D11CaptureFramePool: {e}"))
        })?;

        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|e| RustSightError::CaptureFailed(format!("CreateCaptureSession: {e}")))?;

        // Best-effort: hide yellow capture border & cursor if supported on this Windows version
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(false);

        session
            .StartCapture()
            .map_err(|e| RustSightError::CaptureFailed(format!("StartCapture: {e}")))?;

        let deadline = Instant::now() + Duration::from_millis(1200);
        let frame = loop {
            match pool.TryGetNextFrame() {
                Ok(f) => break f,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => {
                    let _ = session.Close();
                    let _ = pool.Close();
                    return Err(RustSightError::CaptureFailed(format!(
                        "Timed out waiting for WGC frame: {e}"
                    )));
                }
            }
        };

        let surface = frame
            .Surface()
            .map_err(|e| RustSightError::CaptureFailed(format!("frame.Surface: {e}")))?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|e| {
            RustSightError::CaptureFailed(format!("IDirect3DDxgiInterfaceAccess: {e}"))
        })?;
        let texture: ID3D11Texture2D = access
            .GetInterface()
            .map_err(|e| RustSightError::CaptureFailed(format!("GetInterface<ID3D11Texture2D>: {e}")))?;

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;

        let mut staging = None;
        d3d_device
            .CreateTexture2D(&desc, None, Some(&mut staging))
            .map_err(|e| RustSightError::CaptureFailed(format!("CreateTexture2D staging: {e}")))?;
        let staging = staging
            .ok_or_else(|| RustSightError::CaptureFailed("Missing staging texture".into()))?;

        let src_res: ID3D11Resource = texture
            .cast()
            .map_err(|e| RustSightError::CaptureFailed(format!("src resource cast: {e}")))?;
        let dst_res: ID3D11Resource = staging
            .cast()
            .map_err(|e| RustSightError::CaptureFailed(format!("dst resource cast: {e}")))?;
        d3d_context.CopyResource(&dst_res, &src_res);

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        d3d_context
            .Map(&dst_res, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| RustSightError::CaptureFailed(format!("Map staging texture: {e}")))?;

        let tex_w = desc.Width;
        let tex_h = desc.Height;
        let row_pitch = mapped.RowPitch as usize;

        // WGC captures the full GetWindowRect (including invisible DWM drop-shadow borders).
        // Crop to DWMWA_EXTENDED_FRAME_BOUNDS so the image has no black/shadow border and
        // coordinates align 1:1 with get_window_bounds().
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
            for (src_px, dst_px) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
                // BGRA -> RGBA
                dst_px[0] = src_px[2];
                dst_px[1] = src_px[1];
                dst_px[2] = src_px[0];
                dst_px[3] = 255;
            }
        }

        d3d_context.Unmap(&dst_res, 0);
        let _ = session.Close();
        let _ = pool.Close();

        Ok(CapturedFrame {
            width: crop_w,
            height: crop_h,
            rgba,
        })
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
        // PW_RENDERFULLCONTENT = 2
        let ok = PrintWindow(hwnd, hdc_mem, PRINT_WINDOW_FLAGS(2)).as_bool();
        if !ok {
            warn!("PrintWindow returned false; attempting PW_CLIENTONLY=0");
            let _ = PrintWindow(hwnd, hdc_mem, PRINT_WINDOW_FLAGS(0));
        }

        let len = (bounds.width * bounds.height * 4) as usize;
        let mut rgba = vec![0u8; len];
        if !bits.is_null() {
            let src = std::slice::from_raw_parts(bits as *const u8, len);
            for (s, d) in src.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
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
