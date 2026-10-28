//! Computer use for Nugget's agent (M4): [`nugget_agent::Tool`]s that drive any app through its
//! accessibility tree ([`nugget_a11y::ComputerUse`]) by stable element id, never by guessing at
//! pixels, and check every change by re-reading the window.
//!
//! - [`tools`] / [`tools_with`]: `observe`, `click`, `type_text`, `press_key`, `set_value`,
//!   `scroll`, `open_app`, `wait_for`. They share the last snapshot of each window so
//!   `Tool::risk` and `Tool::describe`
//!   can look an element up by id without touching the OS.
//! - [`render()`]: the compact tree rendering the model reads. Screen text is untrusted: it is
//!   escaped so it cannot forge an element line, an id or a fence, and every result that carries
//!   it is marked [`untrusted`](nugget_agent::ToolResult::untrusted) so the loop fences it.
//! - Every mutating tool observes the element and a fingerprint of its window before and after
//!   the call. `Tool::verify` passes only when the backend saw the
//!   effect *and* the after-observation shows it (the value reads back, the item is selected,
//!   the element or window changed); it fails when the observation contradicts the intent, and is
//!   unverifiable otherwise.
//! - Screenshot fallback ([`ComputerConfig::screenshot_fallback`], off by default): only when a
//!   window has no actionable element, a PNG goes into the result's `data` (for the user and the
//!   log); nothing is ever clicked by coordinates.
//!
//! # Risk
//!
//! `declared_risk` is the worst case; `risk(args)` refines it from the cached element. Words
//! match whole words of the element's name, description and placeholder, case-insensitively
//! (lists in [`risk`], extendable through [`RiskKeywords`]); the highest matching class wins.
//!
//! | call | risk |
//! |------|------|
//! | `observe`, `scroll`, `wait_for` | Observe |
//! | `click` on an element not in the last snapshot | **Send** (fail-safe: forces a confirmation) |
//! | `click`, label has send/submit/post/publish/share/invite ("Reply" and "Forward" only open a draft; its Send button is what sends) | Send |
//! | `click`, label has delete/remove/discard/erase/archive/empty trash/unsubscribe/cancel subscription | Destroy |
//! | `click`, label has pay/buy/purchase/place order/book/subscribe/donate/transfer/sign (not sign in/up/out); a check box or switch is never Send or Spend by itself | Spend |
//! | `click`, label has password/permission/security/install/allow/grant/2fa/authorize/trust | Security |
//! | `click` on OK/Yes/Continue/Confirm/… | the class of its dialog's text and the window title |
//! | any other `click` (fields, tabs, list items, links, ordinary buttons) | Edit |
//! | `type_text` / `set_value` / `press_key` in a terminal emulator ([`risk::TERMINAL_APPS`], [`ComputerConfig::terminal_apps`], or a `terminal` raw role) | Security |
//! | `type_text` / `set_value` into a secret-looking field (secure, or labelled password/PIN/OTP/…) | Security |
//! | `type_text` / `set_value` with an `element` id not in the last snapshot | Send |
//! | other `type_text` / `set_value` | Edit |
//! | `press_key` `enter` (any modifiers) | Send; Edit when focus is a multi-line text area and no ctrl/cmd is held; at least the click risk when focus is a button |
//! | `press_key` `delete`/`backspace` with shift or cmd, or outside a text field (or focus unknown) | Destroy |
//! | `press_key` `space` on a button | its click risk; Send when focus is unknown |
//! | `press_key` `cmd+q`/`ctrl+q`/`alt+f4` | Destroy |
//! | other `press_key` | Edit |
//! | `open_app` | Edit (Security when the name has a security word) |
//!
//! Secure (password) fields are never typed into or set: the tools refuse, and so does the
//! backend. Their contents are masked in confirmation previews and in the action log
//! (`Tool::redact`).

mod config;
mod observe;
pub mod render;
pub mod risk;
mod state;
mod tools;

use std::sync::Arc;

use nugget_a11y::ComputerUse;
use nugget_agent::Tool;

pub use config::{ComputerConfig, RiskKeywords};
pub use render::{RenderOptions, render};
pub use tools::{ComputerTool, KEYS_MAX, Kind, SCROLL_MAX, TYPE_TEXT_MAX};

/// The computer-use tools over this OS's accessibility backend.
pub fn tools(config: ComputerConfig) -> Vec<Arc<dyn Tool>> {
    tools_with(nugget_a11y::system_computer(), config)
}

/// The computer-use tools over `cu` (a real backend, or a fake in tests and the bench). The
/// tools share one snapshot cache.
pub fn tools_with(cu: Arc<dyn ComputerUse>, config: ComputerConfig) -> Vec<Arc<dyn Tool>> {
    let shared = state::Shared::new(cu, config);
    Kind::ALL
        .into_iter()
        .map(|k| Arc::new(ComputerTool::new(k, shared.clone())) as Arc<dyn Tool>)
        .collect()
}
