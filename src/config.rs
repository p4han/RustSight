use clap::Parser;
use std::path::PathBuf;

/// RustSight — Framework-agnostic MCP server for seeing and controlling native GUI apps.
#[derive(Debug, Clone, Parser)]
#[command(name = "rustsight", version, about)]
pub struct Config {
    /// Default settle delay (ms) after input actions before capturing a screenshot.
    #[arg(long, default_value_t = 120)]
    pub settle_ms: u64,

    /// Maximum total wait time (ms) for adaptive settle after input actions before capturing.
    #[arg(long, default_value_t = 1000)]
    pub settle_max_ms: u64,

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

    /// Timeout (seconds) waiting for a launched app's main window to appear.
    #[arg(long, default_value_t = 15)]
    pub window_timeout_secs: u64,

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_cli_parsing() {
        let def = Config::try_parse_from(["rustsight"]).expect("default config");
        assert_eq!(def.settle_ms, 120);
        assert_eq!(def.settle_max_ms, 1000);
        assert_eq!(def.build_timeout_secs, 300);
        assert_eq!(def.window_timeout_secs, 15);
        assert_eq!(def.max_image_width, 1920);
        assert_eq!(def.default_max_frames, 3);
        assert_eq!(def.log_level, "info");
        assert!(!def.allow_attach);
        assert!(def.allow_process.is_empty());

        let parsed = Config::try_parse_from([
            "rustsight",
            "--settle-ms",
            "50",
            "--settle-max-ms",
            "800",
            "--max-image-width",
            "0",
            "--allow-attach",
            "--allow-process",
            "notepad.exe",
            "--allow-process",
            "calc.exe",
        ])
        .expect("parsed config");
        assert_eq!(parsed.settle_ms, 50);
        assert_eq!(parsed.settle_max_ms, 800);
        assert_eq!(parsed.max_image_width, 0);
        assert!(parsed.allow_attach);
        assert_eq!(parsed.allow_process, vec!["notepad.exe", "calc.exe"]);
    }
}
