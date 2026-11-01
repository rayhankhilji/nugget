//! Messages between the host and the extension (and, relayed unchanged, the daemon).
//!
//! Host to extension: `{"id": 7, "type": "snapshot", ...}`. Extension to host: a `reply` with the
//! same id carrying `result` or `error`, or an unsolicited `event` (the user pressed Stop, a site
//! grant changed).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::action::BrowserAction;
use crate::snapshot::{PageSnapshot, SnapshotDiff};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SnapshotOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_text: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_hidden: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostRequest {
    Hello {
        protocol: u32,
    },
    Snapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<SnapshotOptions>,
    },
    Act {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<i64>,
        snapshot_id: String,
        action: BrowserAction,
        /// A confirmed submit: the extension digests the form again right before dispatching
        /// and refuses (`form_changed`) if it differs. Older extensions ignore it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_form: Option<FormExpectation>,
        /// A confirmed send outside a native form submit: the same, over the dialog or region
        /// its content lives in (`form` names that container).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_scope: Option<FormExpectation>,
    },
    Overlay {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_id: Option<i64>,
        active: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Halt,
    /// The user resumed agents from the daemon (the desktop's Resume): lift a halt the host
    /// sent. An extension that does not know it answers `bad_request`; that is harmless.
    Resume,
}

/// The form a confirmed action may only submit while it has exactly this digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormExpectation {
    pub form: String,
    pub digest: String,
}

/// What the extension must still see right before dispatching a confirmed action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expectations {
    pub form: Option<FormExpectation>,
    pub scope: Option<FormExpectation>,
}

/// A request with its correlation id, as framed to the extension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostMessage {
    pub id: u64,
    #[serde(flatten)]
    pub request: HostRequest,
}

/// Error codes the extension (or the bridge) reports.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    TooLarge,
    NoTab,
    PermissionRequired,
    StaleSnapshot,
    UnknownElement,
    NotVisible,
    Disabled,
    NotEditable,
    SensitiveField,
    BadOption,
    BadUrl,
    Timeout,
    Halted,
    /// The form changed after the user confirmed submitting it; nothing was sent.
    FormChanged,
    Internal,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum BrowserEvent {
    /// The user pressed Stop in the page overlay or the toolbar popup: engage the kill switch.
    UserStop {
        reason: String,
    },
    PermissionChanged {
        origins: Vec<String>,
        granted: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionMessage {
    Reply {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ReplyError>,
    },
    Event(BrowserEvent),
}

impl ExtensionMessage {
    pub fn error_reply(id: u64, code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Reply {
            id,
            result: None,
            error: Some(ReplyError {
                code,
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol: u32,
    pub extension_version: String,
    pub browser: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActResult {
    /// Empty when there was no earlier snapshot of the tab (a navigation from a blank tab).
    pub before_snapshot_id: String,
    pub snapshot: PageSnapshot,
    pub diff: SnapshotDiff,
}
