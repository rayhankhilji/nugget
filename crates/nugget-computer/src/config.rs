//! [`ComputerConfig`]: limits and switches for the computer-use tools, parsed from the daemon's
//! config section (every field has a default).

use std::time::Duration;

use nugget_a11y::TreeLimits;
use serde::{Deserialize, Serialize};

use crate::render::RenderOptions;

/// Extra words that raise a click (or a confirming button in a dialog) to a risk class. They are
/// added to the built-in lists in [`crate::risk`]; matching is by whole words, case-insensitive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskKeywords {
    pub send: Vec<String>,
    pub destroy: Vec<String>,
    pub spend: Vec<String>,
    pub security: Vec<String>,
}

/// Configuration of the computer-use tools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComputerConfig {
    /// Snapshot walk: most elements read from one window.
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_children: usize,
    /// Snapshot walk: wall-clock budget per snapshot.
    pub snapshot_deadline_ms: u64,
    /// Leave off-screen subtrees out of snapshots.
    pub skip_offscreen: bool,
    /// Rendering for the model: most bytes and element lines in one `observe` result.
    pub render_max_bytes: usize,
    pub render_max_nodes: usize,
    /// Longest name or value shown per element, in characters.
    pub render_max_text: usize,
    /// Return a PNG of the window when its tree has no actionable element. Off by default:
    /// pixels are a last resort and are never clicked by coordinates.
    pub screenshot_fallback: bool,
    /// `wait_for`: default timeout, the cap on any requested timeout, and the poll interval.
    pub wait_timeout_ms: u64,
    pub wait_max_timeout_ms: u64,
    pub wait_poll_ms: u64,
    /// Snapshots kept for `risk`/`describe` lookups (one per window, most recent first).
    pub cached_windows: usize,
    pub risk_keywords: RiskKeywords,
    /// Extra terminal emulators (app names or ids, matched by whole words, case-insensitive),
    /// added to [`crate::risk::TERMINAL_APPS`]. Typing or pressing keys in a terminal is
    /// Security risk: it can run anything.
    pub terminal_apps: Vec<String>,
}

impl Default for ComputerConfig {
    fn default() -> Self {
        let limits = TreeLimits::default();
        Self {
            max_nodes: limits.max_nodes,
            max_depth: limits.max_depth,
            max_children: limits.max_children,
            snapshot_deadline_ms: limits.deadline.as_millis() as u64,
            skip_offscreen: limits.skip_offscreen,
            render_max_bytes: 12_000,
            render_max_nodes: 300,
            render_max_text: 120,
            screenshot_fallback: false,
            wait_timeout_ms: 10_000,
            wait_max_timeout_ms: 60_000,
            wait_poll_ms: 250,
            cached_windows: 8,
            risk_keywords: RiskKeywords::default(),
            terminal_apps: Vec::new(),
        }
    }
}

impl ComputerConfig {
    pub fn tree_limits(&self) -> TreeLimits {
        TreeLimits {
            max_nodes: self.max_nodes.max(1),
            max_depth: self.max_depth.max(1),
            max_children: self.max_children.max(1),
            deadline: Duration::from_millis(self.snapshot_deadline_ms.max(1)),
            skip_offscreen: self.skip_offscreen,
        }
    }

    pub fn render_options(&self) -> RenderOptions {
        RenderOptions {
            max_bytes: self.render_max_bytes.max(256),
            max_nodes: self.render_max_nodes.max(1),
            max_text: self.render_max_text.max(8),
        }
    }

    /// The timeout for a `wait_for` call that asked for `requested` milliseconds.
    pub fn wait_timeout(&self, requested: Option<u64>) -> Duration {
        let ms = requested
            .unwrap_or(self.wait_timeout_ms)
            .min(self.wait_max_timeout_ms);
        Duration::from_millis(ms)
    }

    pub fn wait_poll(&self) -> Duration {
        Duration::from_millis(self.wait_poll_ms.clamp(10, 5_000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_sections() {
        let c: ComputerConfig = serde_json::from_value(serde_json::json!({
            "screenshot_fallback": true,
            "risk_keywords": {"send": ["dispatch"]}
        }))
        .unwrap();
        assert!(c.screenshot_fallback);
        assert_eq!(c.risk_keywords.send, vec!["dispatch"]);
        assert_eq!(c.render_max_nodes, 300);
        assert_eq!(c.tree_limits().max_nodes, TreeLimits::default().max_nodes);
    }

    #[test]
    fn wait_timeout_is_capped() {
        let c = ComputerConfig::default();
        assert_eq!(c.wait_timeout(None), Duration::from_secs(10));
        assert_eq!(c.wait_timeout(Some(1_000_000)), Duration::from_secs(60));
    }
}
