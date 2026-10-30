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
    pub const SYNCING: &str = "syncing";
    pub const NOT_SET_UP: &str = "not_set_up";
    pub const DISK_FULL: &str = "disk_full";
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
        // Not every 401/403 is fixed by signing in again (a rejected OAuth client or an API not
        // enabled in the user's own cloud project is a settings problem).
        let (c, reason, fix) = match last_error {
            Some(err) => classify(kind, &service, err),
            None => reconnect(kind, &service),
        };
        let (c, reason, fix) = if matches!(c, code::CONFIG | code::NOT_SET_UP) {
            (c, reason, fix)
        } else {
            reconnect(kind, &service)
        };
        return mk(c, reason, fix);
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
    if state == SourceState::Syncing {
        return mk(code::SYNCING, "Syncing now.".into(), String::new());
    }
    mk(code::OK, "Up to date.".into(), String::new())
}

type Diagnosis = (&'static str, String, String);

fn reconnect(kind: &str, service: &str) -> Diagnosis {
    (
        code::RECONNECT,
        format!(
            "{} stopped accepting Nugget's sign-in, so syncing is paused.",
            cap(service)
        ),
        reconnect_fix(kind, service),
    )
}

fn classify(kind: &str, service: &str, err: &str) -> Diagnosis {
    let lower = err.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));

    // Settings problems that look like auth failures: re-signing in cannot fix them.
    if has(&[
        "invalid_client",
        "unauthorized_client",
        "accessnotconfigured",
        "has not been used in project",
        "admin_consent",
        "aadsts65001",
    ]) {
        return (
            code::CONFIG,
            format!(
                "{} rejected the app registration Nugget signs in with.",
                cap(service)
            ),
            "Check the client ID (and secret) in the source's settings, and that the API is \
             enabled for your app in the provider's console."
                .into(),
        );
    }
    if lower.starts_with("authorization required")
        && has(&["not connected", "authorize the source first"])
    {
        return (
            code::NOT_SET_UP,
            format!("This source is not signed in to {service} yet."),
            "Sign in from the source to finish setting it up.".into(),
        );
    }
    if lower.starts_with("authorization required") || has(&["invalid_grant"]) {
        return reconnect(kind, service);
    }
    if lower.starts_with("insufficient disk space")
        || has(&[
            "no space left",
            "disk quota exceeded",
            "os error 28",
            "os error 122",
        ])
    {
        return (
            code::DISK_FULL,
            "This computer is out of disk space, so Nugget cannot store more.".into(),
            "Free up disk space; syncing resumes on its own.".into(),
        );
    }
    if lower.starts_with("rate limited")
        || has(&[
            "ratelimitexceeded",
            "userratelimitexceeded",
            "dailylimitexceeded",
            "quotaexceeded\"",
        ])
    {
        return (
            code::RATE_LIMITED,
            format!(
                "{} asked Nugget to slow down (rate limit or daily quota).",
                cap(service)
            ),
            "Nothing to do: Nugget waits and resumes where it stopped.".into(),
        );
    }
    if lower.contains("exceeds") && has(&["bytes", "size"]) {
        // Oversized *items* are skipped by the connectors; a pass that fails on size hit the
        // cap on a listing or page, and retrying will not help.
        return (
            code::TOO_LARGE,
            format!("A response from {service} was larger than Nugget's size limit, so the sync is stuck."),
            "Please report this (Troubleshooting in the docs); removing the oversized item from the source also unblocks it."
                .into(),
        );
    }
    if lower.starts_with("network error") {
        if has(&[
            "http 5",
            "server error",
            "bad gateway",
            "service unavailable",
        ]) {
            return (
                code::SERVER_ERROR,
                format!(
                    "{} is having trouble right now (server error).",
                    cap(service)
                ),
                "Nothing to do: Nugget retries with backoff and resumes where it stopped.".into(),
            );
        }
        return (
            code::OFFLINE,
            format!("Nugget could not reach {service}."),
            "Check your internet connection, VPN or proxy; Nugget retries automatically.".into(),
        );
    }
    if lower.starts_with("not found")
        || has(&["http 404", "os error 2)", "os error 3)", "no such file"])
    {
        return (
            code::CONFIG,
            format!("Something this source points at no longer exists in {service}."),
            "Open the source's settings and check the account, folders or paths.".into(),
        );
    }
    if has(&[
        "permission denied",
        "os error 13)",
        "operation not permitted",
        "os error 1)",
    ]) {
        return (
            code::CONFIG,
            "Nugget is not allowed to read part of this source.".into(),
            "Give Nugget access (on macOS: System Settings → Privacy & Security → Full Disk \
             Access), or remove that folder from the source."
                .into(),
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
        _ => format!("Choose \"Sign in again\" on the source and sign in to {service}."),
    }
}

/// Capitalise the first letter (service names like "the mail server" start sentences).
fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
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
        let h = d(
            "gmail",
            SourceState::NeedsAuth,
            Some("authorization required: x"),
        );
        assert_eq!(h.code, code::RECONNECT);
        assert!(h.fix.contains("Sign in again"));
        let h = d("imap", SourceState::NeedsAuth, None);
        assert!(h.reason.starts_with("The mail server"), "{}", h.reason);
        assert!(h.fix.contains("app password"));
        // A refresh failure reported as a plain error still asks to reconnect.
        let h = d(
            "onedrive",
            SourceState::Error,
            Some("provider error: invalid_grant"),
        );
        assert_eq!(h.code, code::RECONNECT);
        // Signing in again cannot fix a rejected client or a disabled API.
        let h = d(
            "gmail",
            SourceState::NeedsAuth,
            Some("authorization required: the OAuth client was rejected (invalid_client)"),
        );
        assert_eq!(h.code, code::CONFIG);
        let h = d(
            "gdrive",
            SourceState::NeedsAuth,
            Some("authorization required: HTTP 403 accessNotConfigured"),
        );
        assert_eq!(h.code, code::CONFIG);
        let h = d(
            "gmail",
            SourceState::NeedsAuth,
            Some("authorization required: not connected to Google; authorize the source first"),
        );
        assert_eq!(h.code, code::NOT_SET_UP);
        assert_eq!(d("gmail", SourceState::Syncing, None).code, code::SYNCING);
    }

    #[test]
    fn classifies_transient_and_config_errors() {
        let cases = [
            ("rate limited (retry after 5000 ms)", code::RATE_LIMITED),
            (
                "network error: HTTP 503 Service Unavailable: x",
                code::SERVER_ERROR,
            ),
            ("network error: request failed: dns", code::OFFLINE),
            (
                "invalid input: response body exceeds 10 bytes",
                code::TOO_LARGE,
            ),
            ("not found: folder Work", code::CONFIG),
            ("HTTP 404: gone", code::CONFIG),
            ("io error: Permission denied (os error 13)", code::CONFIG),
            (
                "io error: No space left on device (os error 28)",
                code::DISK_FULL,
            ),
            (
                "io error: Disk quota exceeded (os error 122)",
                code::DISK_FULL,
            ),
            (
                "insufficient disk space: need 5 bytes, 1 available",
                code::DISK_FULL,
            ),
            (
                "network error: request failed: Resource temporarily unavailable (os error 11)",
                code::OFFLINE,
            ),
            ("network error: reconnecting after reset", code::OFFLINE),
            (
                "invalid input: token endpoint returned malformed JSON",
                code::ERROR,
            ),
            ("provider error: storageQuotaExceeded", code::ERROR),
            ("something odd", code::ERROR),
        ];
        for (err, want) in cases {
            assert_eq!(
                d("slack", SourceState::Error, Some(err)).code,
                want,
                "{err}"
            );
        }
    }
}
