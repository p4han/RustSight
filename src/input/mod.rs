#[cfg(windows)]
pub mod windows;

use crate::backend::InputBackend;

pub fn create_input_backend() -> Box<dyn InputBackend> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsInputBackend::new())
    }
    #[cfg(not(windows))]
    {
        compile_error!("Only Windows is currently supported; macOS/Linux backends can be added behind InputBackend.")
    }
}
