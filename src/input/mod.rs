#[cfg(windows)]
pub mod windows;

use crate::backend::InputBackend;

#[cfg(not(windows))]
use crate::backend::{Modifiers, MouseButton, WindowInfo};
#[cfg(not(windows))]
use crate::error::{Result, RustSightError};

pub fn create_input_backend() -> Box<dyn InputBackend> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsInputBackend::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(StubInputBackend)
    }
}

#[cfg(not(windows))]
struct StubInputBackend;

#[cfg(not(windows))]
impl InputBackend for StubInputBackend {
    fn click(
        &self,
        _window: &WindowInfo,
        _x: i32,
        _y: i32,
        _button: MouseButton,
        _double: bool,
    ) -> Result<()> {
        Err(RustSightError::InputFailed(
            "RustSight currently supports Windows only".into(),
        ))
    }
    fn type_text(&self, _window: &WindowInfo, _text: &str) -> Result<()> {
        Err(RustSightError::InputFailed(
            "RustSight currently supports Windows only".into(),
        ))
    }
    fn press_key(&self, _window: &WindowInfo, _key: &str, _modifiers: &Modifiers) -> Result<()> {
        Err(RustSightError::InputFailed(
            "RustSight currently supports Windows only".into(),
        ))
    }
    fn scroll(&self, _window: &WindowInfo, _x: i32, _y: i32, _dx: i32, _dy: i32) -> Result<()> {
        Err(RustSightError::InputFailed(
            "RustSight currently supports Windows only".into(),
        ))
    }
    fn drag(
        &self,
        _window: &WindowInfo,
        _from_x: i32,
        _from_y: i32,
        _to_x: i32,
        _to_y: i32,
    ) -> Result<()> {
        Err(RustSightError::InputFailed(
            "RustSight currently supports Windows only".into(),
        ))
    }
    fn mouse_move(&self, _window: &WindowInfo, _x: i32, _y: i32) -> Result<()> {
        Err(RustSightError::InputFailed(
            "RustSight currently supports Windows only".into(),
        ))
    }
    fn focus(&self, _window: &WindowInfo) -> Result<()> {
        Err(RustSightError::InputFailed(
            "RustSight currently supports Windows only".into(),
        ))
    }
}
