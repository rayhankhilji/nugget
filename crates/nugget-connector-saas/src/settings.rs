//! Source settings shared by the four kinds. Each kind's own fields live next to its connector.

use nugget_core::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) const MAX_WINDOW_DAYS: u32 = 3650;

pub(crate) fn parse<T: for<'de> Deserialize<'de>>(settings: &Value) -> Result<T> {
    let v = if settings.is_null() {
        json!({})
    } else {
        settings.clone()
    };
    serde_json::from_value(v).map_err(|e| Error::Invalid(format!("settings: {e}")))
}

/// `token_env`: an environment variable name (`[A-Za-z_][A-Za-z0-9_]*`, at most 128 chars).
pub(crate) fn check_token_env(v: Option<&str>) -> Result<()> {
    if let Some(v) = v {
        let ok = !v.is_empty()
            && v.len() <= 128
            && !v.as_bytes()[0].is_ascii_digit()
            && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !ok {
            return Err(Error::Invalid(
                "settings: token_env must be an environment variable name".into(),
            ));
        }
    }
    Ok(())
}

/// An optional OAuth `client_id`: non-empty, no whitespace or control characters.
pub(crate) fn check_client_id(v: Option<&str>) -> Result<()> {
    if let Some(id) = v {
        let id = id.trim();
        if id.is_empty()
            || id.len() > 512
            || id.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(Error::Invalid("settings: client_id is malformed".into()));
        }
    }
    Ok(())
}

pub(crate) fn check_range(name: &str, v: u32, min: u32, max: u32) -> Result<()> {
    if v < min || v > max {
        return Err(Error::Invalid(format!(
            "settings: {name} must be between {min} and {max}"
        )));
    }
    Ok(())
}

/// A list of short names or ids: at most `max_len` entries, each 1..=200 printable chars.
pub(crate) fn check_list(name: &str, v: Option<&[String]>, max_len: usize) -> Result<()> {
    if let Some(items) = v {
        if items.len() > max_len {
            return Err(Error::Invalid(format!("settings: too many {name}")));
        }
        if items
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 200 || s.chars().any(char::is_control))
        {
            return Err(Error::Invalid(format!(
                "settings: {name} contains a bad entry"
            )));
        }
    }
    Ok(())
}

/// The schema skeleton every kind starts from: `token_env` plus the kind's own properties.
pub(crate) fn schema(token_help: &str, props: Value) -> Value {
    let mut properties = json!({
        "token_env": {
            "type": "string", "pattern": "^[A-Za-z_][A-Za-z0-9_]*$", "title": "Token environment variable",
            "description": format!("Read the token from this environment variable instead of the keychain. Normally leave empty and paste the token: {token_help}")
        }
    });
    if let (Some(dst), Value::Object(src)) = (properties.as_object_mut(), props) {
        dst.extend(src);
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": properties
    })
}

pub(crate) fn window_prop(default: u32, what: &str) -> Value {
    json!({
        "type": "integer", "minimum": 1, "maximum": MAX_WINDOW_DAYS, "default": default,
        "title": "Backfill window (days)",
        "description": format!("How far back to index {what} on first sync.")
    })
}

pub(crate) fn client_id_prop(help: &str) -> Value {
    json!({
        "type": "string", "minLength": 1, "title": "OAuth client ID (optional)",
        "description": help
    })
}

#[cfg(test)]
mod unit {
    /// `true` when `T: Debug` (inherent method wins), `false` otherwise (trait fallback).
    struct Probe<T>(std::marker::PhantomData<T>);
    trait NotDebug {
        fn is_debug(&self) -> bool {
            false
        }
    }
    impl<T> NotDebug for Probe<T> {}
    impl<T: std::fmt::Debug> Probe<T> {
        #[allow(dead_code)]
        fn is_debug(&self) -> bool {
            true
        }
    }

    /// Settings carry the OAuth `client_secret`: they must not be `Debug`, so no `{:?}` or
    /// `tracing` field can print it.
    #[test]
    fn settings_holding_secrets_are_not_debug() {
        use std::marker::PhantomData;
        assert!(!Probe::<crate::github::GitHubSettings>(PhantomData).is_debug());
        assert!(!Probe::<crate::linear::LinearSettings>(PhantomData).is_debug());
        assert!(Probe::<String>(PhantomData).is_debug());
    }
}
