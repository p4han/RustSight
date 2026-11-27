#[cfg(windows)]
pub mod windows;

#[cfg(not(windows))]
use crate::backend::WindowId;
use crate::backend::{UiNode, UiTreeBackend, UiTreeOptions, UiTreeResult, WindowInfo, WindowRect};
use crate::capture::coord_scale;
#[cfg(not(windows))]
use crate::error::{Result, RustSightError};

pub fn create_ui_tree_backend() -> Box<dyn UiTreeBackend> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsUiTreeBackend::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(StubUiTreeBackend)
    }
}

#[cfg(not(windows))]
struct StubUiTreeBackend;

#[cfg(not(windows))]
impl UiTreeBackend for StubUiTreeBackend {
    fn get_ui_tree(&self, _window_id: WindowId, _opts: UiTreeOptions) -> Result<UiTreeResult> {
        Err(RustSightError::CaptureFailed(
            "UI Automation tree inspection is currently only supported on Windows.".to_string(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct FlatUiElement {
    pub id: usize,
    pub depth: usize,
    pub role: String,
    pub name: String,
    pub automation_id: String,
    pub is_enabled: bool,
    pub has_keyboard_focus: bool,
    #[allow(dead_code)]
    pub x: i32,
    #[allow(dead_code)]
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub center_x: i32,
    pub center_y: i32,
}

/// Mask sensitive text when a UI element is marked as a password field.
pub fn mask_password_text(is_password: bool, text: &str) -> String {
    if is_password {
        "[password field]".to_string()
    } else {
        text.to_string()
    }
}

/// Convert a physical screen-space `node_rect` into the client-area screenshot coordinate space,
/// clipping to the window's screenshot bounds. Returns `(x, y, w, h, center_x, center_y)` or
/// `None` if the element lies completely outside the window client area.
pub fn screen_rect_to_screenshot(
    node_rect: WindowRect,
    win_rect: WindowRect,
    max_image_width: u32,
) -> Option<(i32, i32, u32, u32, i32, i32)> {
    if win_rect.width == 0 || win_rect.height == 0 || node_rect.width == 0 || node_rect.height == 0
    {
        return None;
    }

    let win_left = win_rect.x as i64;
    let win_top = win_rect.y as i64;
    let win_right = win_left + win_rect.width as i64;
    let win_bottom = win_top + win_rect.height as i64;

    let n_left = node_rect.x as i64;
    let n_top = node_rect.y as i64;
    let n_right = n_left + node_rect.width as i64;
    let n_bottom = n_top + node_rect.height as i64;

    let clip_left = n_left.max(win_left);
    let clip_top = n_top.max(win_top);
    let clip_right = n_right.min(win_right);
    let clip_bottom = n_bottom.min(win_bottom);

    if clip_right <= clip_left || clip_bottom <= clip_top {
        return None;
    }

    let rel_x = (clip_left - win_left) as f64;
    let rel_y = (clip_top - win_top) as f64;
    let rel_w = (clip_right - clip_left) as f64;
    let rel_h = (clip_bottom - clip_top) as f64;

    let scale = coord_scale(win_rect.width, max_image_width);
    let img_w = if scale == 1.0 {
        win_rect.width
    } else {
        max_image_width
    };
    let img_h = ((win_rect.height as f64) / scale).round().max(1.0) as u32;

    let x = ((rel_x / scale).round() as i32).clamp(0, img_w.saturating_sub(1) as i32);
    let y = ((rel_y / scale).round() as i32).clamp(0, img_h.saturating_sub(1) as i32);
    let w = ((rel_w / scale).round() as u32).clamp(1, img_w.saturating_sub(x as u32).max(1));
    let h = ((rel_h / scale).round() as u32).clamp(1, img_h.saturating_sub(y as u32).max(1));

    let cx = (x + (w as i32) / 2).clamp(0, img_w.saturating_sub(1) as i32);
    let cy = (y + (h as i32) / 2).clamp(0, img_h.saturating_sub(1) as i32);
    Some((x, y, w, h, cx, cy))
}

/// Flatten a [`UiTreeResult`] into screenshot-space [`FlatUiElement`]s, enforcing `max_depth`,
/// `max_nodes`, and `include_offscreen`, and returning `(elements, truncated_count)`.
pub fn flatten_ui_tree(
    tree: &UiTreeResult,
    win_info: &WindowInfo,
    opts: UiTreeOptions,
    max_image_width: u32,
) -> (Vec<FlatUiElement>, usize) {
    let mut out = Vec::new();
    let mut truncated = tree.truncated_count;

    fn count_subtree(node: &UiNode) -> usize {
        1 + node.children.iter().map(count_subtree).sum::<usize>()
    }

    #[allow(clippy::too_many_arguments)]
    fn visit(
        node: &UiNode,
        depth: usize,
        win_info: &WindowInfo,
        opts: UiTreeOptions,
        max_image_width: u32,
        out: &mut Vec<FlatUiElement>,
        truncated: &mut usize,
    ) {
        if !opts.include_offscreen && node.is_offscreen {
            return;
        }
        if depth > opts.max_depth {
            *truncated += count_subtree(node);
            return;
        }
        if out.len() >= opts.max_nodes {
            *truncated += count_subtree(node);
            return;
        }

        if let Some((x, y, width, height, center_x, center_y)) =
            screen_rect_to_screenshot(node.rect, win_info.rect, max_image_width)
        {
            let id = out.len();
            out.push(FlatUiElement {
                id,
                depth,
                role: node.role.clone(),
                name: node.name.clone(),
                automation_id: node.automation_id.clone(),
                is_enabled: node.is_enabled,
                has_keyboard_focus: node.has_keyboard_focus,
                x,
                y,
                width,
                height,
                center_x,
                center_y,
            });
        }

        for child in &node.children {
            visit(
                child,
                depth + 1,
                win_info,
                opts,
                max_image_width,
                out,
                truncated,
            );
        }
    }

    for root in &tree.roots {
        visit(
            root,
            0,
            win_info,
            opts,
            max_image_width,
            &mut out,
            &mut truncated,
        );
    }
    (out, truncated)
}

/// Filter flattened UI elements using case-insensitive substring match on `name`
/// and exact match on `role` and `automation_id`.
pub fn filter_elements<'a>(
    elements: &'a [FlatUiElement],
    name: Option<&str>,
    role: Option<&str>,
    automation_id: Option<&str>,
) -> Vec<&'a FlatUiElement> {
    let name_needle = name.map(|s| s.to_ascii_lowercase());
    elements
        .iter()
        .filter(|el| {
            if let Some(ref needle) = name_needle
                && !el.name.to_ascii_lowercase().contains(needle)
            {
                return false;
            }
            if let Some(r) = role
                && el.role != r
            {
                return false;
            }
            if let Some(aid) = automation_id
                && el.automation_id != aid
            {
                return false;
            }
            true
        })
        .collect()
}

pub fn format_element_line(el: &FlatUiElement, indent: bool) -> String {
    let pad = if indent {
        "  ".repeat(el.depth)
    } else {
        String::new()
    };
    let mut parts = vec![format!("{pad}[id={}] {}", el.id, el.role)];
    if !el.name.is_empty() {
        parts.push(format!("{:?}", el.name));
    }
    if !el.automation_id.is_empty() {
        parts.push(format!("automation_id={:?}", el.automation_id));
    }
    parts.push(format!("center=({}, {})", el.center_x, el.center_y));
    parts.push(format!("size={}x{}", el.width, el.height));

    let mut flags = Vec::new();
    if !el.is_enabled {
        flags.push("disabled");
    }
    if el.has_keyboard_focus {
        flags.push("focused");
    }
    if !flags.is_empty() {
        parts.push(format!("[{}]", flags.join(", ")));
    }
    parts.join(" ")
}

pub fn format_ui_tree_output(
    elements: &[FlatUiElement],
    truncated: usize,
    timed_out: bool,
    win_info: &WindowInfo,
) -> String {
    if elements.is_empty() {
        return format!(
            "No accessibility elements exposed by `{}` (window_id={}). Frameworks such as egui (without AccessKit) or custom-drawn UIs expose no UI Automation tree; use screenshot-based `click(x, y)` instead.",
            win_info.title, win_info.id
        );
    }

    let mut lines: Vec<String> = elements
        .iter()
        .map(|el| format_element_line(el, true))
        .collect();

    if truncated > 0 || timed_out {
        let reason = if timed_out {
            " (traversal deadline reached)"
        } else {
            ""
        };
        lines.push(format!(
            "... truncated {truncated} additional node(s){reason}. Increase `max_depth` or `max_nodes`, or use `find_element`."
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_window(w: u32, h: u32) -> WindowInfo {
        WindowInfo {
            id: 1,
            pid: 100,
            title: "Test".into(),
            rect: WindowRect {
                x: 100,
                y: 200,
                width: w,
                height: h,
            },
            dpi_scale: 1.0,
            is_minimized: false,
        }
    }

    fn make_node(
        role: &str,
        name: &str,
        aid: &str,
        rect: WindowRect,
        children: Vec<UiNode>,
    ) -> UiNode {
        UiNode {
            role: role.into(),
            name: name.into(),
            automation_id: aid.into(),
            class_name: String::new(),
            is_enabled: true,
            has_keyboard_focus: false,
            is_offscreen: false,
            rect,
            children,
        }
    }

    #[test]
    fn screen_rect_converts_downscales_and_clips() {
        let win = WindowRect {
            x: 100,
            y: 200,
            width: 3840,
            height: 2160,
        };
        // Node at (100+200, 200+100) with size 400x200 in 2x downscaled (1920) space -> (100, 50) 200x100
        let node = WindowRect {
            x: 300,
            y: 300,
            width: 400,
            height: 200,
        };
        let (x, y, w, h, cx, cy) = screen_rect_to_screenshot(node, win, 1920).unwrap();
        assert_eq!((x, y, w, h), (100, 50, 200, 100));
        assert_eq!((cx, cy), (200, 100));

        // Partially outside left/top gets clipped to (0, 0)
        let clipped = WindowRect {
            x: 50,
            y: 150,
            width: 150,
            height: 150,
        };
        let (x2, y2, w2, h2, _, _) = screen_rect_to_screenshot(clipped, win, 1920).unwrap();
        assert_eq!((x2, y2, w2, h2), (0, 0, 50, 50));

        // Completely outside returns None
        let outside = WindowRect {
            x: 0,
            y: 0,
            width: 50,
            height: 50,
        };
        assert!(screen_rect_to_screenshot(outside, win, 1920).is_none());
    }

    #[test]
    fn flatten_respects_depth_nodes_offscreen_and_truncation_message() {
        let win = sample_window(800, 600);
        let r = WindowRect {
            x: 110,
            y: 210,
            width: 100,
            height: 40,
        };
        let mut off = make_node("Button", "Hidden", "", r, vec![]);
        off.is_offscreen = true;

        let leaf = make_node("Text", "Leaf", "", r, vec![]);
        let child = make_node("Button", "OK", "ok_btn", r, vec![leaf]);
        let root = make_node("Window", "Main", "", r, vec![child, off]);
        let tree = UiTreeResult {
            roots: vec![root],
            truncated_count: 0,
            timed_out: false,
        };

        // Depth limit 1 excludes `leaf` (depth 2); offscreen excluded
        let opts = UiTreeOptions {
            max_depth: 1,
            max_nodes: 300,
            include_offscreen: false,
            timeout: std::time::Duration::from_secs(1),
        };
        let (els, trunc) = flatten_ui_tree(&tree, &win, opts, 1920);
        assert_eq!(els.len(), 2);
        assert_eq!(trunc, 1);
        let txt = format_ui_tree_output(&els, trunc, false, &win);
        assert!(txt.contains("truncated 1 additional node(s)"));

        // Node limit 1 truncates child + leaf (2 nodes)
        let opts_nodes = UiTreeOptions {
            max_depth: 6,
            max_nodes: 1,
            include_offscreen: false,
            timeout: std::time::Duration::from_secs(1),
        };
        let (els2, trunc2) = flatten_ui_tree(&tree, &win, opts_nodes, 1920);
        assert_eq!(els2.len(), 1);
        assert_eq!(trunc2, 2);
    }

    #[test]
    fn filter_elements_matches_name_case_insensitively_and_exact_role_aid() {
        let win = sample_window(800, 600);
        let r = WindowRect {
            x: 110,
            y: 210,
            width: 100,
            height: 40,
        };
        let tree = UiTreeResult {
            roots: vec![make_node(
                "Window",
                "App",
                "",
                r,
                vec![
                    make_node("Button", "Save File", "btn_save", r, vec![]),
                    make_node("Button", "Save As", "btn_save_as", r, vec![]),
                    make_node("MenuItem", "Save File", "menu_save", r, vec![]),
                ],
            )],
            truncated_count: 0,
            timed_out: false,
        };
        let (els, _) = flatten_ui_tree(&tree, &win, UiTreeOptions::default(), 1920);

        let by_name = filter_elements(&els, Some("save"), None, None);
        assert_eq!(by_name.len(), 3);

        let by_role = filter_elements(&els, Some("SAVE"), Some("Button"), None);
        assert_eq!(by_role.len(), 2);

        let by_aid = filter_elements(&els, None, None, Some("btn_save_as"));
        assert_eq!(by_aid.len(), 1);
        assert_eq!(by_aid[0].name, "Save As");
    }

    #[test]
    fn password_masking_hides_value() {
        assert_eq!(mask_password_text(true, "s3cr3t"), "[password field]");
        assert_eq!(mask_password_text(false, "Submit"), "Submit");
    }
}
