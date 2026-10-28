//! Per-OS [`Platform`](crate::engine::Platform) implementations.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use nugget_core::Result;

use crate::clipboard::Clipboard;
use crate::engine::Platform as _;
use crate::{A11yConfig, Accessibility, ComputerUse};
use std::sync::Arc;

#[cfg(target_os = "linux")]
pub fn system_with(config: A11yConfig) -> Box<dyn Accessibility> {
    Box::new(crate::Backend::new(linux::AtspiPlatform::new(), config))
}

#[cfg(target_os = "macos")]
pub fn system_with(config: A11yConfig) -> Box<dyn Accessibility> {
    Box::new(crate::Backend::new(macos::AxPlatform::new(), config))
}

#[cfg(target_os = "windows")]
pub fn system_with(config: A11yConfig) -> Box<dyn Accessibility> {
    Box::new(crate::Backend::new(windows::UiaPlatform::new(), config))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn system_with(_config: A11yConfig) -> Box<dyn Accessibility> {
    Box::new(crate::Unsupported)
}

#[cfg(target_os = "linux")]
pub fn system_clipboard() -> Result<Box<dyn Clipboard>> {
    linux::AtspiPlatform::new().clipboard()
}

#[cfg(target_os = "macos")]
pub fn system_clipboard() -> Result<Box<dyn Clipboard>> {
    macos::AxPlatform::new().clipboard()
}

#[cfg(target_os = "windows")]
pub fn system_clipboard() -> Result<Box<dyn Clipboard>> {
    windows::UiaPlatform::new().clipboard()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn system_clipboard() -> Result<Box<dyn Clipboard>> {
    Err(nugget_core::Error::Unsupported(
        "no clipboard backend on this platform".into(),
    ))
}

#[cfg(target_os = "linux")]
pub fn system_computer_with(config: A11yConfig) -> Arc<dyn ComputerUse> {
    Arc::new(crate::Backend::new(linux::AtspiPlatform::new(), config))
}

#[cfg(target_os = "macos")]
pub fn system_computer_with(config: A11yConfig) -> Arc<dyn ComputerUse> {
    Arc::new(crate::Backend::new(macos::AxPlatform::new(), config))
}

#[cfg(target_os = "windows")]
pub fn system_computer_with(config: A11yConfig) -> Arc<dyn ComputerUse> {
    Arc::new(crate::Backend::new(windows::UiaPlatform::new(), config))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn system_computer_with(_config: A11yConfig) -> Arc<dyn ComputerUse> {
    Arc::new(crate::Unsupported)
}
