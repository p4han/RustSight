use crate::backend::{InputBackend, Modifiers, MouseButton, WindowInfo};
use crate::error::{Result, RustSightError};
use std::ffi::c_void;
use std::mem::size_of;
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
    VIRTUAL_KEY, VK_0, VK_1, VK_2, VK_3, VK_4, VK_5, VK_6, VK_7, VK_8, VK_9, VK_A, VK_B, VK_BACK,
    VK_C, VK_CONTROL, VK_D, VK_DELETE, VK_DOWN, VK_E, VK_END, VK_ESCAPE, VK_F, VK_F1, VK_F2, VK_F3,
    VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_F10, VK_F11, VK_F12, VK_G, VK_H, VK_HOME, VK_I,
    VK_INSERT, VK_J, VK_K, VK_L, VK_LEFT, VK_LWIN, VK_M, VK_MENU, VK_N, VK_NEXT, VK_O, VK_OEM_1,
    VK_OEM_2, VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA, VK_OEM_MINUS,
    VK_OEM_PERIOD, VK_OEM_PLUS, VK_P, VK_PRIOR, VK_Q, VK_R, VK_RETURN, VK_RIGHT, VK_S, VK_SHIFT,
    VK_SPACE, VK_T, VK_TAB, VK_U, VK_UP, VK_V, VK_W, VK_X, VK_Y, VK_Z, VkKeyScanW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GA_ROOT, GA_ROOTOWNER, GetAncestor, GetForegroundWindow, GetSystemMetrics,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, PostMessageW, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_RESTORE, SetForegroundWindow,
    ShowWindow, WM_ACTIVATE, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETFOCUS,
};

pub struct WindowsInputBackend;

impl WindowsInputBackend {
    pub fn new() -> Self {
        Self
    }
}

impl InputBackend for WindowsInputBackend {
    fn click(
        &self,
        window: &WindowInfo,
        x: i32,
        y: i32,
        button: MouseButton,
        double: bool,
    ) -> Result<()> {
        if !focus_or_fallback(window, || post_click_fallback(window, x, y, button, double))? {
            return Ok(());
        }
        let (nx, ny) = to_virtual_desktop_coords(window, x, y);
        let (down_flag, up_flag) = match button {
            MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        };

        let move_evt = mouse_input(
            nx,
            ny,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        );
        if send_inputs(&[move_evt]).is_err() {
            return post_click_fallback(window, x, y, button, double);
        }
        thread::sleep(Duration::from_millis(15));

        let clicks = if double { 2 } else { 1 };
        for i in 0..clicks {
            let down = mouse_input(
                nx,
                ny,
                0,
                down_flag | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            );
            let up = mouse_input(
                nx,
                ny,
                0,
                up_flag | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            );
            if send_inputs(&[down, up]).is_err() {
                return post_click_fallback(window, x, y, button, double);
            }
            if double && i == 0 {
                thread::sleep(Duration::from_millis(40));
            }
        }
        Ok(())
    }

    fn type_text(&self, window: &WindowInfo, text: &str) -> Result<()> {
        if !focus_or_fallback(window, || post_type_fallback(window, text))? {
            return Ok(());
        }
        let mut inputs = Vec::with_capacity(text.len() * 2);
        for unit in text.encode_utf16() {
            if unit == b'\n' as u16 {
                inputs.push(key_vk_input(VK_RETURN, false));
                inputs.push(key_vk_input(VK_RETURN, true));
            } else if unit == b'\r' as u16 {
                continue;
            } else {
                inputs.push(key_unicode_input(unit, false));
                inputs.push(key_unicode_input(unit, true));
            }
        }
        if !inputs.is_empty() && send_inputs(&inputs).is_err() {
            return post_type_fallback(window, text);
        }
        Ok(())
    }

    fn press_key(&self, window: &WindowInfo, key: &str, modifiers: &Modifiers) -> Result<()> {
        let vk = parse_virtual_key(key)?;

        let mut mod_vks = Vec::new();
        if modifiers.ctrl {
            mod_vks.push(VK_CONTROL);
        }
        if modifiers.alt {
            mod_vks.push(VK_MENU);
        }
        if modifiers.shift {
            mod_vks.push(VK_SHIFT);
        }
        if modifiers.meta {
            mod_vks.push(VK_LWIN);
        }

        if !focus_or_fallback(window, || {
            if modifiers.meta {
                return Err(RustSightError::InputFailed(
                    "PostMessageW fallback does not support `win`/`meta` modifier shortcuts".into(),
                ));
            }
            post_key_fallback(window, vk, &mod_vks)
        })? {
            return Ok(());
        }

        let mut inputs = Vec::new();
        for &m in &mod_vks {
            inputs.push(key_vk_input(m, false));
        }
        inputs.push(key_vk_input(vk, false));
        inputs.push(key_vk_input(vk, true));
        for &m in mod_vks.iter().rev() {
            inputs.push(key_vk_input(m, true));
        }

        if send_inputs(&inputs).is_err() {
            return post_key_fallback(window, vk, &mod_vks);
        }
        Ok(())
    }

    fn scroll(&self, window: &WindowInfo, x: i32, y: i32, dx: i32, dy: i32) -> Result<()> {
        if !focus_or_fallback(window, || post_scroll_fallback(window, x, y, dx, dy))? {
            return Ok(());
        }
        let (nx, ny) = to_virtual_desktop_coords(window, x, y);
        let move_evt = mouse_input(
            nx,
            ny,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        );
        if send_inputs(&[move_evt]).is_err() {
            return post_scroll_fallback(window, x, y, dx, dy);
        }
        thread::sleep(Duration::from_millis(15));

        let mut events = Vec::new();
        if dy != 0 {
            // Positive dy in UI conventions = scroll down; Windows WHEEL_DELTA < 0 = scroll down.
            let wheel_delta = if dy.abs() <= 20 { -dy * 120 } else { -dy };
            events.push(mouse_input(0, 0, wheel_delta as u32, MOUSEEVENTF_WHEEL));
        }
        if dx != 0 {
            let hwheel_delta = if dx.abs() <= 20 { dx * 120 } else { dx };
            events.push(mouse_input(0, 0, hwheel_delta as u32, MOUSEEVENTF_HWHEEL));
        }
        if !events.is_empty() && send_inputs(&events).is_err() {
            return post_scroll_fallback(window, x, y, dx, dy);
        }
        Ok(())
    }

    fn drag(
        &self,
        window: &WindowInfo,
        from_x: i32,
        from_y: i32,
        to_x: i32,
        to_y: i32,
    ) -> Result<()> {
        if !focus_or_fallback(window, || {
            post_drag_fallback(window, from_x, from_y, to_x, to_y)
        })? {
            return Ok(());
        }
        let (start_nx, start_ny) = to_virtual_desktop_coords(window, from_x, from_y);

        if send_inputs(&[
            mouse_input(
                start_nx,
                start_ny,
                0,
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            ),
            mouse_input(
                start_nx,
                start_ny,
                0,
                MOUSEEVENTF_LEFTDOWN | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            ),
        ])
        .is_err()
        {
            return post_drag_fallback(window, from_x, from_y, to_x, to_y);
        }

        let steps = 12;
        for step in 1..=steps {
            let t = step as f64 / steps as f64;
            let cx = from_x as f64 + (to_x - from_x) as f64 * t;
            let cy = from_y as f64 + (to_y - from_y) as f64 * t;
            let (nx, ny) = to_virtual_desktop_coords(window, cx.round() as i32, cy.round() as i32);
            send_inputs(&[mouse_input(
                nx,
                ny,
                0,
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            )])?;
            thread::sleep(Duration::from_millis(10));
        }

        let (end_nx, end_ny) = to_virtual_desktop_coords(window, to_x, to_y);
        send_inputs(&[mouse_input(
            end_nx,
            end_ny,
            0,
            MOUSEEVENTF_LEFTUP | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )])?;
        Ok(())
    }

    fn mouse_move(&self, window: &WindowInfo, x: i32, y: i32) -> Result<()> {
        if !focus_or_fallback(window, || post_move_fallback(window, x, y))? {
            return Ok(());
        }
        let (nx, ny) = to_virtual_desktop_coords(window, x, y);
        let move_evt = mouse_input(
            nx,
            ny,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        );
        if send_inputs(&[move_evt]).is_err() {
            return post_move_fallback(window, x, y);
        }
        Ok(())
    }

    fn focus(&self, window: &WindowInfo) -> Result<()> {
        focus_hwnd(window)
    }
}

const MAX_FOCUS_WAIT: Duration = Duration::from_millis(250);

fn is_valid_hwnd(window: &WindowInfo) -> bool {
    let hwnd = HWND(window.id as *mut c_void);
    !hwnd.0.is_null() && unsafe { IsWindow(Some(hwnd)).as_bool() }
}

fn is_window_foreground(target: HWND) -> bool {
    if target.0.is_null() {
        return false;
    }
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        if fg == target {
            return true;
        }
        let target_root = GetAncestor(target, GA_ROOT);
        let target_root_owner = GetAncestor(target, GA_ROOTOWNER);
        let fg_root = GetAncestor(fg, GA_ROOT);
        let fg_root_owner = GetAncestor(fg, GA_ROOTOWNER);

        fg == target_root
            || fg == target_root_owner
            || fg_root == target
            || fg_root_owner == target
            || (!target_root.0.is_null() && target_root == fg_root)
    }
}

fn hwnd_title(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len > 0 {
        String::from_utf16_lossy(&buf[..len as usize])
    } else {
        String::new()
    }
}

fn try_attach_thread_focus(hwnd: HWND) {
    unsafe {
        let fg = GetForegroundWindow();
        let cur_tid = GetCurrentThreadId();
        let fg_tid = if fg.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(fg, None)
        };
        let target_tid = GetWindowThreadProcessId(hwnd, None);

        let attached_fg =
            fg_tid != 0 && fg_tid != cur_tid && AttachThreadInput(cur_tid, fg_tid, true).as_bool();
        let attached_target = target_tid != 0
            && target_tid != cur_tid
            && target_tid != fg_tid
            && AttachThreadInput(cur_tid, target_tid, true).as_bool();

        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);

        if attached_target {
            let _ = AttachThreadInput(cur_tid, target_tid, false);
        }
        if attached_fg {
            let _ = AttachThreadInput(cur_tid, fg_tid, false);
        }
    }
}

fn focus_or_fallback<F>(window: &WindowInfo, fallback: F) -> Result<bool>
where
    F: FnOnce() -> Result<()>,
{
    match focus_hwnd(window) {
        Ok(()) => Ok(true),
        Err(focus_err) => {
            if !is_valid_hwnd(window) {
                return Err(focus_err);
            }
            fallback().map_err(|fallback_err| {
                RustSightError::FocusFailed(format!(
                    "{focus_err}; PostMessageW fallback also failed: {fallback_err}"
                ))
            })?;
            Ok(false)
        }
    }
}

fn focus_hwnd(window: &WindowInfo) -> Result<()> {
    let hwnd = HWND(window.id as *mut c_void);
    if !is_valid_hwnd(window) {
        return Err(RustSightError::FocusFailed(format!(
            "Target window 0x{:x} ({:?}) is invalid or has been closed",
            window.id, window.title
        )));
    }

    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            thread::sleep(Duration::from_millis(50));
        } else if is_window_foreground(hwnd) {
            return Ok(());
        }

        let _ = SetForegroundWindow(hwnd);
    }
    thread::sleep(Duration::from_millis(20));

    if is_window_foreground(hwnd) {
        return Ok(());
    }

    let retry_start = Instant::now();
    for attempt in 0..3 {
        if retry_start.elapsed() >= MAX_FOCUS_WAIT {
            break;
        }
        match attempt {
            0 | 2 => try_attach_thread_focus(hwnd),
            1 => {
                // Tapping Alt satisfies Windows' foreground lock heuristic in many desktop sessions.
                if send_inputs(&[key_vk_input(VK_MENU, false), key_vk_input(VK_MENU, true)]).is_ok()
                {
                    unsafe {
                        let _ = BringWindowToTop(hwnd);
                        let _ = SetForegroundWindow(hwnd);
                    }
                }
            }
            _ => unreachable!(),
        }
        thread::sleep(Duration::from_millis(25));
        if is_window_foreground(hwnd) {
            return Ok(());
        }
    }

    let fg = unsafe { GetForegroundWindow() };
    let fg_id = fg.0 as usize;
    let fg_title = hwnd_title(fg);
    Err(RustSightError::FocusFailed(format!(
        "Could not bring window 0x{:x} ({:?}) to foreground; current foreground window is 0x{:x} ({:?}). Windows may be blocking SetForegroundWindow (e.g., an elevated window is active, foreground lock is held, or the desktop is locked)",
        window.id, window.title, fg_id, fg_title
    )))
}

/// Convert window-relative screenshot pixel coordinates `(x, y)` to normalized `[0, 65535]`
/// virtual desktop coordinates for `SendInput`.
fn to_virtual_desktop_coords(window: &WindowInfo, x: i32, y: i32) -> (i32, i32) {
    let screen_x = window.rect.x + x;
    let screen_y = window.rect.y + y;

    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);

        let nx = (((screen_x - vx) as f64 * 65535.0) / (vw - 1).max(1) as f64)
            .round()
            .clamp(0.0, 65535.0) as i32;
        let ny = (((screen_y - vy) as f64 * 65535.0) / (vh - 1).max(1) as f64)
            .round()
            .clamp(0.0, 65535.0) as i32;
        (nx, ny)
    }
}

fn mouse_input(
    dx: i32,
    dy: i32,
    mouse_data: u32,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: mouse_data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_vk_input(vk: VIRTUAL_KEY, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if key_up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_unicode_input(code_unit: u16, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: code_unit,
                dwFlags: if key_up {
                    KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                } else {
                    KEYEVENTF_UNICODE
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_inputs(inputs: &[INPUT]) -> Result<()> {
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        let os_err = std::io::Error::last_os_error();
        return Err(RustSightError::InputFailed(format!(
            "SendInput only dispatched {sent}/{} events (OS error: {os_err})",
            inputs.len()
        )));
    }
    Ok(())
}

fn make_lparam(x: i32, y: i32) -> LPARAM {
    LPARAM((((y as i16 as u16 as usize) << 16) | (x as i16 as u16 as usize)) as isize)
}

fn to_client_coords(hwnd: HWND, window: &WindowInfo, x: i32, y: i32) -> (i32, i32) {
    let mut pt = POINT {
        x: window.rect.x + x,
        y: window.rect.y + y,
    };
    unsafe {
        let _ = ScreenToClient(hwnd, &mut pt);
    }
    (pt.x, pt.y)
}

fn post_move_fallback(window: &WindowInfo, x: i32, y: i32) -> Result<()> {
    let hwnd = HWND(window.id as *mut c_void);
    let (cx, cy) = to_client_coords(hwnd, window, x, y);
    let lparam = make_lparam(cx, cy);
    unsafe {
        PostMessageW(Some(hwnd), WM_MOUSEMOVE, WPARAM(0), lparam)
            .map_err(|e| RustSightError::InputFailed(format!("PostMessageW WM_MOUSEMOVE: {e}")))?;
    }
    Ok(())
}

fn post_click_fallback(
    window: &WindowInfo,
    x: i32,
    y: i32,
    button: MouseButton,
    double: bool,
) -> Result<()> {
    let hwnd = HWND(window.id as *mut c_void);
    let (cx, cy) = to_client_coords(hwnd, window, x, y);
    let lparam = make_lparam(cx, cy);
    let (down_msg, up_msg, btn_flag) = match button {
        MouseButton::Left => (WM_LBUTTONDOWN, WM_LBUTTONUP, 0x0001usize),
        MouseButton::Right => (WM_RBUTTONDOWN, WM_RBUTTONUP, 0x0002usize),
        MouseButton::Middle => (WM_MBUTTONDOWN, WM_MBUTTONUP, 0x0010usize),
    };
    unsafe {
        // Ensure the target window processes activation & focus even when SetForegroundWindow is blocked by UIPI
        let _ = PostMessageW(Some(hwnd), WM_ACTIVATE, WPARAM(2), LPARAM(0));
        let _ = PostMessageW(Some(hwnd), WM_SETFOCUS, WPARAM(0), LPARAM(0));
        let clicks = if double { 2 } else { 1 };
        for i in 0..clicks {
            // Post WM_MOUSEMOVE, down, and up together so TrackMouseEvent's WM_MOUSELEAVE
            // cannot fire between press and release and cancel the click.
            let _ = PostMessageW(Some(hwnd), WM_MOUSEMOVE, WPARAM(0), lparam);
            PostMessageW(Some(hwnd), down_msg, WPARAM(btn_flag), lparam)
                .map_err(|e| RustSightError::InputFailed(format!("PostMessageW down: {e}")))?;
            PostMessageW(Some(hwnd), up_msg, WPARAM(0), lparam)
                .map_err(|e| RustSightError::InputFailed(format!("PostMessageW up: {e}")))?;
            if double && i == 0 {
                thread::sleep(Duration::from_millis(40));
            }
        }
    }
    Ok(())
}

fn post_type_fallback(window: &WindowInfo, text: &str) -> Result<()> {
    let hwnd = HWND(window.id as *mut c_void);
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_ACTIVATE, WPARAM(2), LPARAM(0));
        let _ = PostMessageW(Some(hwnd), WM_SETFOCUS, WPARAM(0), LPARAM(0));
        for unit in text.encode_utf16() {
            if unit == b'\n' as u16 {
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_KEYDOWN,
                    WPARAM(VK_RETURN.0 as usize),
                    LPARAM(1),
                );
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_KEYUP,
                    WPARAM(VK_RETURN.0 as usize),
                    LPARAM(1),
                );
            } else if unit != b'\r' as u16 {
                let vk_scan = VkKeyScanW(unit);
                if vk_scan != -1 {
                    let vk = (vk_scan & 0xff) as usize;
                    let _ = PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(vk), LPARAM(1));
                    PostMessageW(Some(hwnd), WM_CHAR, WPARAM(unit as usize), LPARAM(1)).map_err(
                        |e| RustSightError::InputFailed(format!("PostMessageW WM_CHAR: {e}")),
                    )?;
                    let _ = PostMessageW(Some(hwnd), WM_KEYUP, WPARAM(vk), LPARAM(1));
                } else {
                    PostMessageW(Some(hwnd), WM_CHAR, WPARAM(unit as usize), LPARAM(1)).map_err(
                        |e| RustSightError::InputFailed(format!("PostMessageW WM_CHAR: {e}")),
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn post_key_fallback(window: &WindowInfo, vk: VIRTUAL_KEY, mod_vks: &[VIRTUAL_KEY]) -> Result<()> {
    let hwnd = HWND(window.id as *mut c_void);
    unsafe {
        for &m in mod_vks {
            let _ = PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(m.0 as usize), LPARAM(1));
        }
        PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(vk.0 as usize), LPARAM(1))
            .map_err(|e| RustSightError::InputFailed(format!("PostMessageW WM_KEYDOWN: {e}")))?;
        let _ = PostMessageW(Some(hwnd), WM_KEYUP, WPARAM(vk.0 as usize), LPARAM(1));
        for &m in mod_vks.iter().rev() {
            let _ = PostMessageW(Some(hwnd), WM_KEYUP, WPARAM(m.0 as usize), LPARAM(1));
        }
    }
    Ok(())
}

fn post_scroll_fallback(window: &WindowInfo, x: i32, y: i32, dx: i32, dy: i32) -> Result<()> {
    let hwnd = HWND(window.id as *mut c_void);
    let (cx, cy) = to_client_coords(hwnd, window, x, y);
    let screen_lparam = make_lparam(window.rect.x + x, window.rect.y + y);
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_MOUSEMOVE, WPARAM(0), make_lparam(cx, cy));
        if dy != 0 {
            let delta = if dy.abs() <= 20 { -dy * 120 } else { -dy };
            let wparam = WPARAM((delta as i16 as u16 as usize) << 16);
            PostMessageW(Some(hwnd), WM_MOUSEWHEEL, wparam, screen_lparam).map_err(|e| {
                RustSightError::InputFailed(format!("PostMessageW WM_MOUSEWHEEL: {e}"))
            })?;
        }
        if dx != 0 {
            let delta = if dx.abs() <= 20 { dx * 120 } else { dx };
            let wparam = WPARAM((delta as i16 as u16 as usize) << 16);
            PostMessageW(Some(hwnd), WM_MOUSEHWHEEL, wparam, screen_lparam).map_err(|e| {
                RustSightError::InputFailed(format!("PostMessageW WM_MOUSEHWHEEL: {e}"))
            })?;
        }
    }
    Ok(())
}

fn post_drag_fallback(
    window: &WindowInfo,
    from_x: i32,
    from_y: i32,
    to_x: i32,
    to_y: i32,
) -> Result<()> {
    let hwnd = HWND(window.id as *mut c_void);
    let (sx, sy) = to_client_coords(hwnd, window, from_x, from_y);
    let (ex, ey) = to_client_coords(hwnd, window, to_x, to_y);
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_MOUSEMOVE, WPARAM(0), make_lparam(sx, sy));
        PostMessageW(
            Some(hwnd),
            WM_LBUTTONDOWN,
            WPARAM(0x0001),
            make_lparam(sx, sy),
        )
        .map_err(|e| RustSightError::InputFailed(format!("PostMessageW drag down: {e}")))?;
        let steps = 12;
        for step in 1..=steps {
            let t = step as f64 / steps as f64;
            let cx = sx as f64 + (ex - sx) as f64 * t;
            let cy = sy as f64 + (ey - sy) as f64 * t;
            let _ = PostMessageW(
                Some(hwnd),
                WM_MOUSEMOVE,
                WPARAM(0x0001),
                make_lparam(cx.round() as i32, cy.round() as i32),
            );
            thread::sleep(Duration::from_millis(10));
        }
        PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), make_lparam(ex, ey))
            .map_err(|e| RustSightError::InputFailed(format!("PostMessageW drag up: {e}")))?;
    }
    Ok(())
}

fn parse_virtual_key(key: &str) -> Result<VIRTUAL_KEY> {
    let k = key.trim().to_ascii_lowercase();
    let vk = match k.as_str() {
        "enter" | "return" => VK_RETURN,
        "tab" => VK_TAB,
        "escape" | "esc" => VK_ESCAPE,
        "backspace" | "bs" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "insert" | "ins" => VK_INSERT,
        "space" | " " => VK_SPACE,
        "up" | "arrowup" => VK_UP,
        "down" | "arrowdown" => VK_DOWN,
        "left" | "arrowleft" => VK_LEFT,
        "right" | "arrowright" => VK_RIGHT,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" | "pgup" => VK_PRIOR,
        "pagedown" | "pgdn" => VK_NEXT,
        "f1" => VK_F1,
        "f2" => VK_F2,
        "f3" => VK_F3,
        "f4" => VK_F4,
        "f5" => VK_F5,
        "f6" => VK_F6,
        "f7" => VK_F7,
        "f8" => VK_F8,
        "f9" => VK_F9,
        "f10" => VK_F10,
        "f11" => VK_F11,
        "f12" => VK_F12,
        "a" => VK_A,
        "b" => VK_B,
        "c" => VK_C,
        "d" => VK_D,
        "e" => VK_E,
        "f" => VK_F,
        "g" => VK_G,
        "h" => VK_H,
        "i" => VK_I,
        "j" => VK_J,
        "k" => VK_K,
        "l" => VK_L,
        "m" => VK_M,
        "n" => VK_N,
        "o" => VK_O,
        "p" => VK_P,
        "q" => VK_Q,
        "r" => VK_R,
        "s" => VK_S,
        "t" => VK_T,
        "u" => VK_U,
        "v" => VK_V,
        "w" => VK_W,
        "x" => VK_X,
        "y" => VK_Y,
        "z" => VK_Z,
        "0" => VK_0,
        "1" => VK_1,
        "2" => VK_2,
        "3" => VK_3,
        "4" => VK_4,
        "5" => VK_5,
        "6" => VK_6,
        "7" => VK_7,
        "8" => VK_8,
        "9" => VK_9,
        "-" | "minus" => VK_OEM_MINUS,
        "=" | "equal" | "plus" => VK_OEM_PLUS,
        "[" => VK_OEM_4,
        "]" => VK_OEM_6,
        "\\" => VK_OEM_5,
        ";" => VK_OEM_1,
        "'" => VK_OEM_7,
        "," => VK_OEM_COMMA,
        "." => VK_OEM_PERIOD,
        "/" => VK_OEM_2,
        "`" => VK_OEM_3,
        _ => return Err(RustSightError::InvalidKey(key.to_string())),
    };
    Ok(vk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::WindowRect;

    fn dummy_window(id: u64) -> WindowInfo {
        WindowInfo {
            id,
            pid: 0,
            title: "Dummy".to_string(),
            rect: WindowRect {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
            dpi_scale: 1.0,
            is_minimized: false,
        }
    }

    #[test]
    fn invalid_hwnd_is_not_foreground_and_focus_fails() {
        assert!(!is_window_foreground(HWND(std::ptr::null_mut())));
        let win = dummy_window(0);
        let err = focus_hwnd(&win).unwrap_err();
        assert!(matches!(err, RustSightError::FocusFailed(_)));
    }

    #[test]
    fn input_actions_return_focus_failed_on_invalid_hwnd() {
        let backend = WindowsInputBackend::new();
        let win = dummy_window(0);
        assert!(matches!(
            backend.click(&win, 10, 10, MouseButton::Left, false),
            Err(RustSightError::FocusFailed(_))
        ));
        assert!(matches!(
            backend.type_text(&win, "hello"),
            Err(RustSightError::FocusFailed(_))
        ));
        assert!(matches!(
            backend.press_key(&win, "enter", &Modifiers::default()),
            Err(RustSightError::FocusFailed(_))
        ));
        assert!(matches!(
            backend.scroll(&win, 10, 10, 0, 1),
            Err(RustSightError::FocusFailed(_))
        ));
        assert!(matches!(
            backend.drag(&win, 10, 10, 20, 20),
            Err(RustSightError::FocusFailed(_))
        ));
    }

    #[test]
    fn focus_or_fallback_runs_fallback_only_when_focus_fails() {
        let win = dummy_window(0);
        let mut called = false;
        let res = focus_or_fallback(&win, || {
            called = true;
            Ok(())
        });
        assert!(matches!(res, Err(RustSightError::FocusFailed(_))));
        assert!(!called, "fallback must not run for invalid HWND");
    }

    #[test]
    fn parse_virtual_key_handles_common_keys_and_invalid() {
        assert_eq!(parse_virtual_key("enter").unwrap(), VK_RETURN);
        assert_eq!(parse_virtual_key("ESC").unwrap(), VK_ESCAPE);
        assert_eq!(parse_virtual_key("a").unwrap(), VK_A);
        assert_eq!(parse_virtual_key("9").unwrap(), VK_9);
        assert_eq!(parse_virtual_key("f12").unwrap(), VK_F12);
        assert_eq!(parse_virtual_key("/").unwrap(), VK_OEM_2);
        assert!(matches!(
            parse_virtual_key("nonexistent_key"),
            Err(RustSightError::InvalidKey(_))
        ));
    }
}
