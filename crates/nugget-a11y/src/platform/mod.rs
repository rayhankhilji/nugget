//! Per-OS [`Platform`](crate::engine::Platform) implementations.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use crate::{A11yConfig, Accessibility};

#[cfg(target_os = "linux")]
pub fn system_with(config: A11yConfig) -> Box<dyn Accessibility> {
    {
        let _ = config;
        Box::new(crate::Unsupported)
    }
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
