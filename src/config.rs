use clap::Parser;
use std::path::PathBuf;

/// RustSight — Framework-agnostic MCP server for seeing and controlling native GUI apps.
#[derive(Debug, Clone, Parser)]
#[command(name = "rustsight", version, about)]
pub struct Config {
    /// Default settle delay (ms) after input actions before capturing a screenshot.
    #[arg(long, default_value_t = 120)]
    pub settle_ms: u64,

    /// Directory where animated GIF/APNG recordings are saved (defaults to OS temp dir).
    #[arg(long)]
    pub capture_dir: Option<PathBuf>,

    /// Default window width when launching an app without an explicit width.
    #[arg(long)]
    pub default_width: Option<u32>,

    /// Default window height when launching an app without an explicit height.
    #[arg(long)]
    pub default_height: Option<u32>,

    /// Timeout (seconds) for `cargo build` when launching or rebuilding a project.
    #[arg(long, default_value_t = 300)]
    pub build_timeout_secs: u64,

    /// Maximum width (px) of screenshots returned to the MCP client (0 = no downscaling).
    #[arg(long, default_value_t = 1920)]
    pub max_image_width: u32,

    /// Default maximum number of frames returned by `capture_frames` in chat.
    #[arg(long, default_value_t = 3)]
    pub default_max_frames: usize,

    /// Log level filter (e.g., info, debug, warn, error). Logs go to stderr.
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// Permit `list_windows` and tools to target any visible window, not only apps launched by RustSight.
    #[arg(long, default_value_t = false)]
    pub allow_attach: bool,

    /// Permit attaching to windows belonging to specific executable file names (repeatable, case-insensitive, e.g. `notepad.exe`).
    #[arg(long = "allow-process", value_name = "NAME")]
    pub allow_process: Vec<String>,
}

impl Config {
    pub fn resolved_capture_dir(&self) -> PathBuf {
        self.capture_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("rustsight_captures"))
    }
}
