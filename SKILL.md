---
name: rustsight
description: Install, configure, and use the RustSight MCP server so AI coding agents can launch, see (PNG screenshots & GIF bursts), and control any native Windows GUI application (egui, iced, Dioxus, Tauri, Slint, Win32).
---

# RustSight — Native GUI Vision & Control

Use this skill when the user asks you to install RustSight from `https://github.com/p4han/RustSight` or when you need to visually inspect and interact with a native GUI application you are building.

## Installation (for AI Coding Agents)

When the user points you to `https://github.com/p4han/RustSight` and asks you to install it, run these steps:

1. **Install the `rustsight` binary** via Cargo:
   ```powershell
   cargo install --git https://github.com/p4han/RustSight rustsight
   ```
   Verify it is on `PATH`:
   ```powershell
   rustsight --version
   ```

2. **Install this skill** into your local skills directory (if applicable for your harness):
   - Copy `SKILL.md` to your agent's skills folder as `rustsight/SKILL.md` (e.g., `~/.claude/skills/rustsight/SKILL.md`, `~/.cursor/skills/rustsight/SKILL.md`, or `~/.gemini/config/skills/rustsight/SKILL.md`).

3. **Register the MCP server** for the user's client:
   - **Claude Code**:
     ```powershell
     claude mcp add rustsight -- rustsight
     ```
   - **Cursor** (`.cursor/mcp.json` or `~/.cursor/mcp.json`):
     ```json
     {
       "mcpServers": {
         "rustsight": {
           "command": "rustsight",
           "args": []
         }
       }
     }
     ```
   - **Antigravity** (`mcp_config.json`):
     ```json
     {
       "mcpServers": {
         "rustsight": {
           "command": "rustsight",
           "args": []
         }
       }
     }
     ```
   *(If `rustsight` is not on `PATH`, use `%USERPROFILE%\.cargo\bin\rustsight.exe` as `"command"`.)*

   **Optional attach flags (disabled by default for privacy/safety):**
   By default, RustSight only accesses windows it launched (including their child processes). To attach to already-running windows, pass `--allow-process <NAME>` (repeatable, case-insensitive, e.g. `"--allow-process", "notepad.exe"`) or `--allow-attach` (allows all visible windows) in `"args"`.

## Usage Workflow

1. **Launch** the app under test: call `launch(path_or_cargo_dir, width?, height?)` — if given a Cargo project directory, RustSight runs `cargo build`, spawns the binary, and returns the `window_id` plus the first PNG screenshot.
2. **Inspect & Interact**:
   - `click(x, y, button?, double?)` — click at screenshot pixel coordinates.
   - `type_text(text)` / `press_key(key, modifiers?)` — type or send shortcuts.
   - `scroll(x, y, dx?, dy?)` / `drag(from_x, from_y, to_x, to_y)` — scroll or drag.
   - `capture_frames(duration_ms?, fps?)` — inspect animations (returns up to 12 PNGs + an animated GIF path).
   - `wait_idle(timeout_ms?)` — wait until frames stop changing.
   Every action tool waits for the UI to settle and automatically returns a fresh PNG screenshot.
3. **Iterate**: edit the GUI code, call `rebuild()` to kill, recompile, and relaunch while keeping the MCP session alive, and call `close()` when finished.
