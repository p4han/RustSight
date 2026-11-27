use crate::backend::{UiNode, UiTreeBackend, UiTreeOptions, UiTreeResult, WindowId, WindowRect};
use crate::error::{Result, RustSightError};
use crate::ui_tree::mask_password_text;
use std::ffi::c_void;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationElement,
    IUIAutomationTreeWalker, TreeScope_Element, UIA_AutomationIdPropertyId,
    UIA_BoundingRectanglePropertyId, UIA_CONTROLTYPE_ID, UIA_ClassNamePropertyId,
    UIA_ControlTypePropertyId, UIA_CustomControlTypeId, UIA_HasKeyboardFocusPropertyId,
    UIA_IsEnabledPropertyId, UIA_IsOffscreenPropertyId, UIA_IsPasswordPropertyId,
    UIA_NamePropertyId,
};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

pub struct WindowsUiTreeBackend;

impl WindowsUiTreeBackend {
    pub fn new() -> Self {
        Self
    }
}

/// RAII guard for COM initialization on `tokio::task::spawn_blocking` worker threads.
/// Initializes MTA (`COINIT_MULTITHREADED`) and balances `CoUninitialize` on every exit path.
struct ComGuard {
    should_uninit: bool,
}

impl ComGuard {
    fn init_mta() -> Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == S_OK || hr == S_FALSE {
            Ok(Self {
                should_uninit: true,
            })
        } else if hr == RPC_E_CHANGED_MODE {
            // Thread already initialized COM with another apartment model; do not uninitialize it.
            Ok(Self {
                should_uninit: false,
            })
        } else {
            Err(RustSightError::CaptureFailed(format!(
                "CoInitializeEx failed: {hr:?}"
            )))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            unsafe { CoUninitialize() };
        }
    }
}

impl UiTreeBackend for WindowsUiTreeBackend {
    fn get_ui_tree(&self, window_id: WindowId, opts: UiTreeOptions) -> Result<UiTreeResult> {
        let hwnd = HWND(window_id as *mut c_void);
        if hwnd.0.is_null() || unsafe { !IsWindow(Some(hwnd)).as_bool() } {
            return Err(RustSightError::WindowNotFound(window_id));
        }

        let _com = ComGuard::init_mta()?;

        unsafe {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(|e| {
                    RustSightError::CaptureFailed(format!("CoCreateInstance(CUIAutomation): {e}"))
                })?;

            let cache_req = build_cache_request(&automation)?;
            let walker = automation
                .ControlViewWalker()
                .map_err(|e| RustSightError::CaptureFailed(format!("ControlViewWalker: {e}")))?;

            let root_el = automation
                .ElementFromHandleBuildCache(hwnd, &cache_req)
                .map_err(|e| {
                    RustSightError::CaptureFailed(format!("ElementFromHandleBuildCache: {e}"))
                })?;

            let deadline = Instant::now() + opts.timeout;
            let mut visited = 0usize;
            let mut truncated_count = 0usize;
            let mut timed_out = false;

            let root = walk_element(
                &walker,
                &cache_req,
                &root_el,
                0,
                opts,
                deadline,
                &mut visited,
                &mut truncated_count,
                &mut timed_out,
            );

            Ok(UiTreeResult {
                roots: root.into_iter().collect(),
                truncated_count,
                timed_out,
            })
        }
    }
}

unsafe fn build_cache_request(automation: &IUIAutomation) -> Result<IUIAutomationCacheRequest> {
    let req = unsafe {
        automation
            .CreateCacheRequest()
            .map_err(|e| RustSightError::CaptureFailed(format!("CreateCacheRequest: {e}")))?
    };
    let props = [
        UIA_ControlTypePropertyId,
        UIA_NamePropertyId,
        UIA_AutomationIdPropertyId,
        UIA_ClassNamePropertyId,
        UIA_IsEnabledPropertyId,
        UIA_HasKeyboardFocusPropertyId,
        UIA_IsOffscreenPropertyId,
        UIA_IsPasswordPropertyId,
        UIA_BoundingRectanglePropertyId,
    ];
    for prop in props {
        let _ = unsafe { req.AddProperty(prop) };
    }
    let _ = unsafe { req.SetTreeScope(TreeScope_Element) };
    Ok(req)
}

#[allow(clippy::too_many_arguments)]
unsafe fn walk_element(
    walker: &IUIAutomationTreeWalker,
    cache_req: &IUIAutomationCacheRequest,
    el: &IUIAutomationElement,
    depth: usize,
    opts: UiTreeOptions,
    deadline: Instant,
    visited: &mut usize,
    truncated_count: &mut usize,
    timed_out: &mut bool,
) -> Option<UiNode> {
    if Instant::now() >= deadline {
        *timed_out = true;
        *truncated_count += 1;
        return None;
    }
    if *visited >= opts.max_nodes {
        *truncated_count += 1;
        return None;
    }

    let is_offscreen = unsafe { el.CachedIsOffscreen().map(|b| b.as_bool()).unwrap_or(false) };
    if !opts.include_offscreen && is_offscreen {
        return None;
    }

    *visited += 1;

    let ctrl_type = unsafe { el.CachedControlType().unwrap_or(UIA_CustomControlTypeId) };
    let is_password = unsafe { el.CachedIsPassword().map(|b| b.as_bool()).unwrap_or(false) };
    let raw_name = unsafe { el.CachedName().map(|b| b.to_string()).unwrap_or_default() };
    let name = mask_password_text(is_password, &raw_name);
    let automation_id = unsafe {
        el.CachedAutomationId()
            .map(|b| b.to_string())
            .unwrap_or_default()
    };
    let class_name = unsafe {
        el.CachedClassName()
            .map(|b| b.to_string())
            .unwrap_or_default()
    };
    let is_enabled = unsafe { el.CachedIsEnabled().map(|b| b.as_bool()).unwrap_or(true) };
    let has_keyboard_focus = unsafe {
        el.CachedHasKeyboardFocus()
            .map(|b| b.as_bool())
            .unwrap_or(false)
    };
    let rect = unsafe { el.CachedBoundingRectangle().unwrap_or_default() };
    let win_rect = WindowRect {
        x: rect.left,
        y: rect.top,
        width: (rect.right - rect.left).max(0) as u32,
        height: (rect.bottom - rect.top).max(0) as u32,
    };

    let mut children = Vec::new();
    if depth < opts.max_depth
        && let Ok(mut child) = unsafe { walker.GetFirstChildElementBuildCache(el, cache_req) }
    {
        loop {
            if let Some(node) = unsafe {
                walk_element(
                    walker,
                    cache_req,
                    &child,
                    depth + 1,
                    opts,
                    deadline,
                    visited,
                    truncated_count,
                    timed_out,
                )
            } {
                children.push(node);
            }
            if *timed_out || *visited >= opts.max_nodes {
                break;
            }
            match unsafe { walker.GetNextSiblingElementBuildCache(&child, cache_req) } {
                Ok(next) => child = next,
                Err(_) => break,
            }
        }
    }

    Some(UiNode {
        role: control_type_name(ctrl_type).to_string(),
        name,
        automation_id,
        class_name,
        is_enabled,
        has_keyboard_focus,
        is_offscreen,
        rect: win_rect,
        children,
    })
}

fn control_type_name(id: UIA_CONTROLTYPE_ID) -> &'static str {
    match id.0 {
        50000 => "Button",
        50001 => "Calendar",
        50002 => "CheckBox",
        50003 => "ComboBox",
        50004 => "Edit",
        50005 => "Hyperlink",
        50006 => "Image",
        50007 => "ListItem",
        50008 => "List",
        50009 => "Menu",
        50010 => "MenuBar",
        50011 => "MenuItem",
        50012 => "ProgressBar",
        50013 => "RadioButton",
        50014 => "ScrollBar",
        50015 => "Slider",
        50016 => "Spinner",
        50017 => "StatusBar",
        50018 => "Tab",
        50019 => "TabItem",
        50020 => "Text",
        50021 => "ToolBar",
        50022 => "ToolTip",
        50023 => "Tree",
        50024 => "TreeItem",
        50026 => "Group",
        50027 => "Thumb",
        50028 => "DataGrid",
        50029 => "DataItem",
        50030 => "Document",
        50031 => "SplitButton",
        50032 => "Window",
        50033 => "Pane",
        _ => "Custom",
    }
}
