//! Mapping daemon and accessibility errors to the overlay's i18n keys.
//!
//! The daemon marks refusals with a stable `data.refused` (`secure`, `excluded_app`,
//! `terminal`, `not_draftable`, `disabled`) on the JSON-RPC error; the UI has exactly the error
//! keys below, and anything else is shown as raw text.

use nugget_ipc::IpcError;

pub const REFUSED_SECURE: &str = "doubletap.refused.secure";
pub const REFUSED_EXCLUDED: &str = "doubletap.refused.excluded";
pub const REFUSED_TERMINAL: &str = "doubletap.refused.terminal";
pub const REFUSED_NOT_DRAFTABLE: &str = "doubletap.refused.not_draftable";
pub const REFUSED_NO_FIELD: &str = "doubletap.refused.no_field";
/// The daemon is not running or the connection dropped.
pub const ERROR_OFFLINE: &str = "doubletap.error.offline";
pub const ERROR_NO_PROVIDER: &str = "doubletap.error.no_provider";
/// The focused field could not be read (including a missing permission).
pub const ERROR_READ_FAILED: &str = "doubletap.error.read_failed";
/// Insertion failed, or focus did not come back to the target app.
pub const ERROR_INSERT_FAILED: &str = "doubletap.error.insert_failed";
pub const ERROR_DRAFT_FAILED: &str = "doubletap.error.draft_failed";

/// The `chat.done` error after `draft.cancel`.
pub const CANCELLED: &str = "cancelled";

/// How a failure is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Drafting is not allowed here (overlay phase `refused`).
    Refused(&'static str),
    /// Something went wrong (phase `error`): an i18n key or raw text.
    Error(String),
}

/// The key for a daemon `data.refused` reason.
pub fn refused_reason(reason: &str) -> Failure {
    match reason {
        "secure" => Failure::Refused(REFUSED_SECURE),
        "excluded_app" => Failure::Refused(REFUSED_EXCLUDED),
        "terminal" => Failure::Refused(REFUSED_TERMINAL),
        "not_draftable" => Failure::Refused(REFUSED_NOT_DRAFTABLE),
        _ => Failure::Error(ERROR_DRAFT_FAILED.into()),
    }
}

fn mentions_no_provider(text: &str) -> bool {
    text.to_lowercase().contains("no provider")
}

/// A message for the overlay from a stream's `chat.done` error text.
pub fn message_for(text: &str) -> String {
    let t = text.trim();
    if mentions_no_provider(t) {
        ERROR_NO_PROVIDER.into()
    } else if t.is_empty() {
        ERROR_DRAFT_FAILED.into()
    } else {
        t.to_owned()
    }
}

/// JSON-RPC "method not found": a daemon without the draft feature.
const METHOD_NOT_FOUND: i64 = -32601;

/// A failed daemon call (`draft.begin`).
pub fn classify_ipc(err: &IpcError) -> Failure {
    match err {
        IpcError::Connect(_) | IpcError::Disconnected | IpcError::Io(_) => {
            Failure::Error(ERROR_OFFLINE.into())
        }
        IpcError::Rpc(e) => {
            if let Some(reason) = e
                .data
                .as_ref()
                .and_then(|d| d.get("refused"))
                .and_then(|r| r.as_str())
            {
                return refused_reason(reason);
            }
            if e.code == METHOD_NOT_FOUND {
                Failure::Error(ERROR_DRAFT_FAILED.into())
            } else if mentions_no_provider(&e.message) {
                Failure::Error(ERROR_NO_PROVIDER.into())
            } else {
                Failure::Error(e.message.trim().to_owned())
            }
        }
        other => Failure::Error(other.to_string()),
    }
}

/// An accessibility error while reading the field (`inserting == false`) or inserting.
pub fn classify_a11y(err: &nugget_core::Error, inserting: bool) -> Failure {
    use nugget_core::Error as E;
    match (err, inserting) {
        (E::NotFound(_), false) => Failure::Refused(REFUSED_NO_FIELD),
        (_, false) => Failure::Error(ERROR_READ_FAILED.into()),
        (_, true) => Failure::Error(ERROR_INSERT_FAILED.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_ipc::RpcError;
    use serde_json::json;

    fn rpc(code: i64, msg: &str, data: Option<serde_json::Value>) -> IpcError {
        let mut e = RpcError::new(code, msg);
        e.data = data;
        IpcError::Rpc(e)
    }

    #[test]
    fn daemon_refusals_map_by_reason() {
        for (reason, want) in [
            ("secure", Failure::Refused(REFUSED_SECURE)),
            ("excluded_app", Failure::Refused(REFUSED_EXCLUDED)),
            ("terminal", Failure::Refused(REFUSED_TERMINAL)),
            ("not_draftable", Failure::Refused(REFUSED_NOT_DRAFTABLE)),
            ("disabled", Failure::Error(ERROR_DRAFT_FAILED.into())),
        ] {
            let e = rpc(
                -32602,
                "whatever the text says",
                Some(json!({"refused": reason, "field_kind": "search"})),
            );
            assert_eq!(classify_ipc(&e), want, "{reason}");
        }
    }

    #[test]
    fn other_daemon_errors() {
        assert_eq!(
            classify_ipc(&rpc(-32000, "provider error: quota exceeded", None)),
            Failure::Error("provider error: quota exceeded".into())
        );
        assert_eq!(
            classify_ipc(&rpc(
                -32000,
                "not found: no provider is available for the Synthesis tier",
                None
            )),
            Failure::Error(ERROR_NO_PROVIDER.into())
        );
        assert_eq!(
            classify_ipc(&rpc(METHOD_NOT_FOUND, "method not found", None)),
            Failure::Error(ERROR_DRAFT_FAILED.into())
        );
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        assert_eq!(
            classify_ipc(&IpcError::Connect(refused)),
            Failure::Error(ERROR_OFFLINE.into())
        );
        assert_eq!(
            classify_ipc(&IpcError::Disconnected),
            Failure::Error(ERROR_OFFLINE.into())
        );
        assert_eq!(message_for(" timed out "), "timed out");
        assert_eq!(message_for("no provider could draft"), ERROR_NO_PROVIDER);
        assert_eq!(message_for(""), ERROR_DRAFT_FAILED);
    }

    #[test]
    fn accessibility_errors() {
        use nugget_core::Error as E;
        assert_eq!(
            classify_a11y(&E::Unauthorized("ax".into()), false),
            Failure::Error(ERROR_READ_FAILED.into())
        );
        assert_eq!(
            classify_a11y(&E::NotFound("no focused text field".into()), false),
            Failure::Refused(REFUSED_NO_FIELD)
        );
        assert_eq!(
            classify_a11y(&E::NotFound("no focused text field".into()), true),
            Failure::Error(ERROR_INSERT_FAILED.into())
        );
        assert_eq!(
            classify_a11y(
                &E::Invalid("refusing to insert into a secure field".into()),
                true
            ),
            Failure::Error(ERROR_INSERT_FAILED.into())
        );
        assert_eq!(
            classify_a11y(&E::Other("AT-SPI: timeout".into()), false),
            Failure::Error(ERROR_READ_FAILED.into())
        );
    }
}
