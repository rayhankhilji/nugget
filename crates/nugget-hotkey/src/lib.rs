//! Global trigger detection for Double Tap: a double tap of a modifier, or a chord.
//!
//! **Contract (M3):** [`Trigger`], [`start`], [`Listener`]. Work package M3-B implements the
//! platform backends. Only the configured trigger is observed; no other keystrokes are recorded
//! or stored.

use nugget_core::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    LeftCommand,
    RightCommand,
    LeftControl,
    RightControl,
    LeftOption,
    RightOption,
    LeftShift,
    RightShift,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Two presses of `modifier` alone within `window_ms`, with no other key in between.
    DoubleTap { modifier: Modifier, window_ms: u32 },
    /// A chord such as `Ctrl+Alt+Space`, in the platform's accelerator syntax.
    Chord(String),
}

impl Default for Trigger {
    fn default() -> Self {
        Self::DoubleTap {
            modifier: Modifier::RightCommand,
            window_ms: 350,
        }
    }
}

/// A running listener; dropping it stops listening.
pub trait Listener: Send {
    fn trigger(&self) -> &Trigger;
}

/// Start listening; `on_fire` runs (on a background thread) each time the trigger fires.
pub fn start(trigger: Trigger, on_fire: Box<dyn Fn() + Send + Sync>) -> Result<Box<dyn Listener>> {
    let _ = (trigger, on_fire);
    Err(nugget_core::Error::Unsupported(
        "global triggers are not implemented on this platform yet".into(),
    ))
}
