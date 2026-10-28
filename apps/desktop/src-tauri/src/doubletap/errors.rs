//! Mapping daemon and accessibility errors to the overlay's i18n keys.

use crate::daemon::CmdError;

pub const REFUSED_SECURE: &str = "doubletap.refused.secure";
pub const REFUSED_EXCLUDED: &str = "doubletap.refused.excluded";
pub const REFUSED_NOT_DRAFTABLE: &str = "doubletap.refused.not_draftable";
pub const REFUSED_TERMINAL: &str = "doubletap.refused.terminal";
pub const REFUSED_NO_FIELD: &str = "doubletap.refused.no_field";
pub const ERROR_NOT_RUNNING: &str = "doubletap.error.not_running";
pub const ERROR_DISCONNECTED: &str = "doubletap.error.disconnected";
pub const ERROR_UNAVAILABLE: &str = "doubletap.error.unavailable";
pub const ERROR_PERMISSION: &str = "doubletap.error.permission";
pub const ERROR_FOCUS_LOST: &str = "doubletap.error.focus_lost";
pub const ERROR_INSERT: &str = "doubletap.error.insert";

/// How a failure is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Drafting is not allowed here (overlay phase `refused`).
    Refused(&'static str),
    /// Something went wrong (phase `error`): an i18n key or raw text.
    Error(String),
}

/// Recognise the refusals `nugget-draft` and `nugget-a11y` report (their messages may arrive
/// prefixed by the error kind, e.g. `invalid input: ...`).
pub fn refusal_key(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    if t.contains("secure field") || t.contains("password and secure") {
        Some(REFUSED_SECURE)
    } else if t.contains("drafting is disabled in") || t.contains("excluded from drafting") {
        Some(REFUSED_EXCLUDED)
    } else if t.contains("nothing to draft") {
        Some(REFUSED_NOT_DRAFTABLE)
    } else if t.contains("no focused text field") || t.contains("not in an editable text field") {
        Some(REFUSED_NO_FIELD)
    } else {
        None
    }
}

pub fn classify_text(text: &str) -> Failure {
    match refusal_key(text) {
        Some(k) => Failure::Refused(k),
        None => Failure::Error(text.trim().to_owned()),
    }
}

/// A message for the overlay from error text (a stream's `chat.done` error).
pub fn message_for(text: &str) -> String {
    match classify_text(text) {
        Failure::Refused(k) => k.to_owned(),
        Failure::Error(e) => e,
    }
}

/// JSON-RPC "method not found".
const METHOD_NOT_FOUND: i64 = -32601;

pub fn classify_cmd(err: &CmdError) -> Failure {
    match err {
        CmdError::NotRunning(_) => Failure::Error(ERROR_NOT_RUNNING.into()),
        CmdError::Disconnected => Failure::Error(ERROR_DISCONNECTED.into()),
        CmdError::Rpc { code, .. } if *code == METHOD_NOT_FOUND => {
            Failure::Error(ERROR_UNAVAILABLE.into())
        }
        CmdError::Rpc { message, .. } => classify_text(message),
        other => Failure::Error(other.to_string()),
    }
}

/// An accessibility error while reading the field or inserting.
pub fn classify_a11y(err: &nugget_core::Error, inserting: bool) -> Failure {
    use nugget_core::Error as E;
    match err {
        E::Unauthorized(_) => Failure::Error(ERROR_PERMISSION.into()),
        E::NotFound(_) if !inserting => Failure::Refused(REFUSED_NO_FIELD),
        E::NotFound(_) => Failure::Error(ERROR_FOCUS_LOST.into()),
        E::Unsupported(_) if inserting => Failure::Error(ERROR_INSERT.into()),
        other => match classify_text(&other.to_string()) {
            Failure::Refused(k) if !inserting => Failure::Refused(k),
            Failure::Refused(k) => Failure::Error(k.into()),
            Failure::Error(e) => Failure::Error(e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_refusals_map_to_keys() {
        let rpc = |m: &str| CmdError::Rpc {
            code: -32602,
            message: m.into(),
        };
        assert_eq!(
            classify_cmd(&rpc(
                "invalid input: drafting is refused in password and secure fields"
            )),
            Failure::Refused(REFUSED_SECURE)
        );
        assert_eq!(
            classify_cmd(&rpc("invalid input: drafting is disabled in 1Password")),
            Failure::Refused(REFUSED_EXCLUDED)
        );
        assert_eq!(
            classify_cmd(&rpc(
                "capability unsupported: nothing to draft in a search field"
            )),
            Failure::Refused(REFUSED_NOT_DRAFTABLE)
        );
        assert_eq!(
            classify_cmd(&rpc("provider error: quota exceeded")),
            Failure::Error("provider error: quota exceeded".into())
        );
        assert_eq!(
            classify_cmd(&CmdError::Rpc {
                code: METHOD_NOT_FOUND,
                message: "method not found".into()
            }),
            Failure::Error(ERROR_UNAVAILABLE.into())
        );
        assert_eq!(
            classify_cmd(&CmdError::NotRunning("refused".into())),
            Failure::Error(ERROR_NOT_RUNNING.into())
        );
        assert_eq!(
            classify_cmd(&CmdError::Disconnected),
            Failure::Error(ERROR_DISCONNECTED.into())
        );
        assert_eq!(message_for("drafting is disabled in Bank"), REFUSED_EXCLUDED);
        assert_eq!(message_for(" timed out "), "timed out");
    }

    #[test]
    fn accessibility_errors() {
        use nugget_core::Error as E;
        assert_eq!(
            classify_a11y(&E::Unauthorized("ax".into()), false),
            Failure::Error(ERROR_PERMISSION.into())
        );
        assert_eq!(
            classify_a11y(&E::NotFound("no focused text field".into()), false),
            Failure::Refused(REFUSED_NO_FIELD)
        );
        assert_eq!(
            classify_a11y(&E::NotFound("no focused text field".into()), true),
            Failure::Error(ERROR_FOCUS_LOST.into())
        );
        assert_eq!(
            classify_a11y(
                &E::Invalid("refusing to insert into a secure field".into()),
                true
            ),
            Failure::Error(REFUSED_SECURE.into())
        );
        assert_eq!(
            classify_a11y(&E::Other("AT-SPI: timeout".into()), false),
            Failure::Error("AT-SPI: timeout".into())
        );
    }
}
