//! Plain-language sync health: turns a source's state and last error into a reason a person
//! understands and a fix they can act on.
//!
//! The ingest pipeline only knows `nugget_core::Error` rendered as text, so classification
//! works on the `Display` prefixes of its variants (`authorization required:`, `rate limited`,
//! `network error:` ...) plus a few well-known phrases connectors use in their messages.

use nugget_core::ipc::{SourceHealth, SourceState};

/// Machine codes of [`SourceHealth::code`].
pub mod code {
    pub const OK: &str = "ok";
    pub const RECONNECT: &str = "reconnect";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const OFFLINE: &str = "offline";
    pub const SERVER_ERROR: &str = "server_error";
    pub const TOO_LARGE: &str = "too_large";
    pub const CONFIG: &str = "config";
    pub const ITEM_ERRORS: &str = "item_errors";
    pub const ERROR: &str = "error";
    pub const DISABLED: &str = "disabled";
}

/// Describe a source's health.
///
/// - `state`/`last_error`: as reported by ingest (`last_error` is the last failed pass, cleared
///   by a successful one).
/// - `item_errors`/`last_item_error`: items skipped in recent passes (one bad item never stops a
///   sync, but the user should still learn about it).
pub fn diagnose(
    kind: &str,
    state: SourceState,
    last_error: Option<&str>,
    item_errors: u64,
    last_item_error: Option<&str>,
) -> SourceHealth {
    let service = service_name(kind);
    let mk = |code: &str, reason: String, fix: String| SourceHealth {
        code: code.into(),
        reason,
        fix,
        item_errors,
        last_item_error: last_item_error.map(str::to_owned),
    };

    if state == SourceState::Disabled {
        return mk(
            code::DISABLED,
            "Syncing is turned off for this source.".into(),
            "Turn the source on to resume; it picks up where it stopped.".into(),
        );
    }
    if state == SourceState::NeedsAuth {
        return mk(
            code::RECONNECT,
            format!("{service} stopped accepting Nugget's sign-in, so syncing is paused."),
            reconnect_fix(kind, &service),
        );
    }
    let failing = matches!(state, SourceState::Error) || last_error.is_some();
    if let (true, Some(err)) = (failing, last_error) {
        let (c, reason, fix) = classify(kind, &service, err);
        return mk(c, reason, fix);
    }
    if item_errors > 0 {
        let noun = if item_errors == 1 { "item" } else { "items" };
        return mk(
            code::ITEM_ERRORS,
            format!(
                "Syncing works, but {item_errors} {noun} could not be read and {} skipped.",
                if item_errors == 1 { "was" } else { "were" }
            ),
            "Nothing to do unless you need those items; they are retried on the next change."
                .into(),
        );
    }
    mk(code::OK, "Up to date.".into(), String::new())
}

fn classify(kind: &str, service: &str, err: &str) -> (&'static str, String, String) {
    let lower = err.to_ascii_lowercase();
    if lower.starts_with("authorization required")
        || lower.contains("invalid_grant")
        || lower.contains("reconnect")
    {
        return (
            code::RECONNECT,
            format!("{service} stopped accepting Nugget's sign-in, so syncing is paused."),
            reconnect_fix(kind, service),
        );
    }
    if lower.starts_with("rate limited") || lower.contains("quota") {
        return (
            code::RATE_LIMITED,
            format!("{service} asked Nugget to slow down (rate limit or daily quota)."),
            "Nothing to do: Nugget waits and resumes where it stopped.".into(),
        );
    }
    if lower.contains("exceeds") && (lower.contains("bytes") || lower.contains("size")) {
        return (
            code::TOO_LARGE,
            "A response or item was larger than Nugget's size limit.".into(),
            "Nothing to do for the source; very large items are skipped.".into(),
        );
    }
    if lower.starts_with("network error") {
        let server = ["http 5", "server error", "bad gateway", "unavailable"]
            .iter()
            .any(|p| lower.contains(p));
        if server {
            return (
                code::SERVER_ERROR,
                format!("{service} is having trouble right now (server error)."),
                "Nothing to do: Nugget retries with backoff and resumes where it stopped.".into(),
            );
        }
        return (
            code::OFFLINE,
            format!("Nugget could not reach {service}."),
            "Check your internet connection, VPN or proxy; Nugget retries automatically.".into(),
        );
    }
    if lower.starts_with("invalid input") || lower.starts_with("not found") {
        return (
            code::CONFIG,
            format!("{service} rejected this source's settings."),
            "Open the source's settings and check the account, folders or paths.".into(),
        );
    }
    (
        code::ERROR,
        format!("The last sync of {service} failed."),
        "Try \"Sync now\"; if it keeps failing, see Troubleshooting in the docs.".into(),
    )
}

fn reconnect_fix(kind: &str, service: &str) -> String {
    match kind {
        "imap" | "caldav" => {
            format!("Enter a new app password for {service} (the old one was revoked or changed).")
        }
        "slack" | "notion" | "github" | "linear" => {
            format!("Paste a new {service} token, or sign in again, from the source's settings.")
        }
        _ => format!("Click \"Reconnect\" and sign in to {service} again."),
    }
}

/// Human name of a connector kind (falls back to the kind itself).
pub fn service_name(kind: &str) -> String {
    match kind {
        "files" => "Files",
        "obsidian" => "Obsidian",
        "gmail" => "Gmail",
        "gcal" => "Google Calendar",
        "gdrive" => "Google Drive",
        "imap" => "the mail server",
        "caldav" => "the calendar server",
        "outlook" => "Outlook",
        "outlook-calendar" => "Outlook Calendar",
        "onedrive" => "OneDrive",
        "slack" => "Slack",
        "notion" => "Notion",
        "github" => "GitHub",
        "linear" => "Linear",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(kind: &str, state: SourceState, err: Option<&str>) -> SourceHealth {
        diagnose(kind, state, err, 0, None)
    }

    #[test]
    fn healthy_and_item_errors() {
        assert_eq!(d("gmail", SourceState::Idle, None).code, code::OK);
        assert_eq!(
            d("gmail", SourceState::Disabled, Some("network error: x")).code,
            code::DISABLED
        );
        let h = diagnose("gdrive", SourceState::Idle, None, 3, Some("bad pdf"));
        assert_eq!(h.code, code::ITEM_ERRORS);
        assert!(h.reason.contains("3 items"), "{}", h.reason);
        assert_eq!(h.last_item_error.as_deref(), Some("bad pdf"));
    }

    #[test]
    fn reconnect_is_never_silent() {
        let h = d("gmail", SourceState::NeedsAuth, Some("authorization required: x"));
        assert_eq!(h.code, code::RECONNECT);
        assert!(h.fix.contains("Reconnect"));
        let h = d("imap", SourceState::NeedsAuth, None);
        assert!(h.fix.contains("app password"));
        // A refresh failure reported as a plain error still asks to reconnect.
        let h = d("onedrive", SourceState::Error, Some("provider error: invalid_grant"));
        assert_eq!(h.code, code::RECONNECT);
    }

    #[test]
    fn classifies_transient_and_config_errors() {
        let cases = [
            ("rate limited (retry after 5000 ms)", code::RATE_LIMITED),
            ("network error: HTTP 503 Service Unavailable: x", code::SERVER_ERROR),
            ("network error: request failed: dns", code::OFFLINE),
            ("invalid input: response body exceeds 10 bytes", code::TOO_LARGE),
            ("invalid input: unknown folder", code::CONFIG),
            ("something odd", code::ERROR),
        ];
        for (err, want) in cases {
            assert_eq!(d("slack", SourceState::Error, Some(err)).code, want, "{err}");
        }
    }
}
