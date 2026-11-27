use thiserror::Error;

#[derive(Debug, Error)]
pub enum RustSightError {
    #[error("No active window. Call `launch` first or provide a valid `window_id`.")]
    NoActiveWindow,

    #[error("Window {0} not found or has been closed.")]
    WindowNotFound(u64),

    #[error("Build failed for `{path}` (exit code {code:?}):\n{output}")]
    BuildFailed {
        path: String,
        code: Option<i32>,
        output: String,
    },

    #[error("Could not locate executable binary for `{0}`: {1}")]
    BinaryNotFound(String, String),

    #[error("Failed to launch process `{0}`: {1}")]
    LaunchFailed(String, String),

    #[error("Window did not appear within {0} ms for process {1}.")]
    WindowTimeout(u64, u32),

    #[error("Screen capture failed: {0}")]
    CaptureFailed(String),

    #[error("Input failed: {0}")]
    InputFailed(String),

    #[error("Focus failed: {0}")]
    FocusFailed(String),

    #[error(
        "Invalid key name `{0}`. Supported keys include letters, digits, enter, tab, escape, backspace, delete, space, up, down, left, right, home, end, pageup, pagedown, f1-f12."
    )]
    InvalidKey(String),

    #[error("Invalid mouse button `{0}`. Expected `left`, `right`, or `middle`.")]
    InvalidMouseButton(String),

    #[error("Timed out after {0} ms waiting for window to become idle.")]
    IdleTimeout(u64),

    #[error("No previous launch configuration to rebuild. Call `launch` first.")]
    NothingToRebuild,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Image encoding error: {0}")]
    Image(String),
}

pub type Result<T> = std::result::Result<T, RustSightError>;
