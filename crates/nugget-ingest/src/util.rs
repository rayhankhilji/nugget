//! Small helpers shared across the crate.

/// Longest error message kept in `last_error`, item errors, progress events and logs, in chars.
pub const MAX_ERROR_CHARS: usize = 300;

/// An error message fit for status fields and logs: control characters (a parser error can
/// quote raw input, e.g. bytes of a mail message) become spaces, and anything beyond
/// [`MAX_ERROR_CHARS`] characters is cut with an ellipsis.
pub fn cap_error(message: impl AsRef<str>) -> String {
    let message = message.as_ref();
    let mut out = String::with_capacity(message.len().min(MAX_ERROR_CHARS * 4));
    for (n, c) in message.chars().enumerate() {
        if n + 1 == MAX_ERROR_CHARS && message.chars().nth(MAX_ERROR_CHARS).is_some() {
            out.push('…');
            return out;
        }
        out.push(if c.is_control() { ' ' } else { c });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_capped_with_an_ellipsis_and_flattened() {
        assert_eq!(cap_error("short"), "short");
        let exact = "x".repeat(MAX_ERROR_CHARS);
        assert_eq!(
            cap_error(&exact),
            exact,
            "a message at the cap is kept whole"
        );
        let long = format!("parse error near: {}", "é\r\n\0".repeat(5_000));
        let capped = cap_error(&long);
        assert_eq!(capped.chars().count(), MAX_ERROR_CHARS);
        assert!(capped.ends_with('…'));
        assert!(capped.starts_with("parse error near: é  "));
        assert!(!capped.chars().any(char::is_control));
    }
}
