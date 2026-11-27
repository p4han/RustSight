# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-11-28

### Added
- Windows UI Automation (`IUIAutomation`) backend with `get_ui_tree`, `find_element`, and `click_element` tools, COM RAII guard, traversal deadline, and password-field masking (`[password field]`).
- `hover`, `focus`, and `resize_window` MCP tools.
- `get_logs` MCP tool with per-process `stdout`/`stderr` ring buffer (2000 lines / 256 KB cap) and last-40-line error tails on launch failure.
- Cargo targeting options on `launch` (`bin`, `package`, `release`, `features`, `all_features`) and strict binary artifact selection.
- Persistent `capture_burst` on `WindowCaptureBackend` with cached D3D11/WinRT devices, reusable staging textures, and RAII session/pool guards.
- Adaptive settle (`--settle-max-ms`, default `1000`) and non-fatal `wait_idle` with configurable `threshold`.
- `--window-timeout-secs` CLI flag (default `15`).
- Cross-platform stub backends on non-Windows targets so `cargo check` and `cargo test` succeed on Linux.
- Dependabot configuration, Linux CI job, `SKILL.md` sync check, and SHA256 release checksums.

### Changed
- `launch` and `rebuild` no longer hold the `ProcessManager` mutex during `cargo build` or window wait; concurrent builds are gated by `launch_gate`.
- Stateless `coord_scale` mapping across all capture and input tools, with out-of-bounds coordinate rejection and image metadata in every image-returning tool response.
- `README.md` and `SKILL.md` updated to match all tools, parameters, and CLI flags.

### Fixed
- Foreground focus verification (`GetForegroundWindow()`) with bounded `AttachThreadInput` and `Alt`-tap retries, skipping `SendInput` and using `PostMessageW` fallback or `RustSightError::FocusFailed` when focus fails.
- Poison-recovering mutex lock helper (`lock_recover`) so a single panic does not break the server.

### Security
- Restricted `list_windows` and all window-targeting tools (including `close`) to processes launched by RustSight (and their children) by default; added `--allow-attach` and `--allow-process <NAME>` opt-in flags.

## [0.1.0] - 2026-11-26

### Added
- Initial release of RustSight MCP server with `launch`, `list_windows`, `screenshot`, `capture_frames`, `click`, `type_text`, `press_key`, `scroll`, `drag`, `wait_idle`, `rebuild`, and `close`.
