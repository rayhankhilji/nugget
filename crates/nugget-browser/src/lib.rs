//! Browser control for Nugget (M4): a native-messaging host that bridges the Nugget browser
//! extension (`extensions/browser`) to the daemon, a DOM-snapshot protocol, and typed browser
//! actions executed in the user's own profile with explicit per-site consent.
//!
//! - [`framing`]: native-messaging framing (length-prefixed JSON over stdio) with size caps.
//! - [`protocol`], [`snapshot`], [`action`]: the wire types shared with the extension.
//! - [`controller`]: [`BrowserController`], which asks the [`nugget_policy::PolicyGate`] before
//!   every snapshot and action, validates against the latest snapshot and verifies the result.
//! - [`bridge`]: the [`BrowserBridge`] trait the daemon implements, and [`BridgeHub`], a ready
//!   implementation served over the daemon's IPC socket by [`HubHandler`].
//! - [`host`]: the relay run by the `nugget-browser-host` binary.
//! - [`tool`]: the actions as agent tools.
//! - [`install`]: native host manifests and registry keys for Chrome, Chromium, Edge and Firefox
//!   on macOS, Linux and Windows.

pub mod action;
pub mod bridge;
pub mod controller;
pub mod framing;
pub mod host;
pub mod install;
pub mod protocol;
pub mod snapshot;
pub mod tool;

pub use action::{BrowserAction, ValidationError, Verification, WaitCondition};
pub use bridge::{BridgeError, BridgeHub, BrowserBridge, HubHandler, KillSwitch};
pub use controller::{ActionOutcome, BrowserController, BrowserError, Confirmer, DeclineAll};
pub use protocol::{BrowserEvent, ExtensionMessage, HostMessage, HostRequest};
pub use snapshot::{ElementInfo, PageSnapshot, SnapshotDiff};
pub use tool::{AgentTool, ToolContext, ToolError, ToolSpec, browser_tools};
