//! `HEED_*` environment variables, with the pre-rename `NUGGET_*` names as fallbacks.
//!
//! The code reads `NUGGET_*` names throughout. Each binary calls [`promote`] first thing in
//! `main`, before it starts any thread: every `HEED_X` is copied over `NUGGET_X`, so `HEED_X`
//! wins when both are set, and child processes inherit the same values.

use crate::identity::{ENV_PREFIX, LEGACY_ENV_PREFIX};

/// The `NUGGET_*` assignments that `vars` (`HEED_*` taking precedence) imply.
pub fn promotions<I>(vars: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut out: Vec<(String, String)> = vars
        .into_iter()
        .filter_map(|(k, v)| {
            let rest = k.strip_prefix(ENV_PREFIX)?;
            (!rest.is_empty()).then(|| (format!("{LEGACY_ENV_PREFIX}{rest}"), v))
        })
        .collect();
    out.sort();
    out
}

/// Copies every `HEED_X` over `NUGGET_X`. Call it at the very start of `main`, before any other
/// thread exists (setting the environment is not thread-safe).
#[allow(unsafe_code)]
pub fn promote() {
    let vars = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
    for (k, v) in promotions(vars) {
        // SAFETY: called first thing in `main`, before the process starts other threads, so
        // nothing reads the environment concurrently.
        unsafe { std::env::set_var(k, v) };
    }
}

/// `HEED_<name>` if set, else `NUGGET_<name>` (for code that runs without [`promote`]).
pub fn var(name: &str) -> Option<String> {
    std::env::var(format!("{ENV_PREFIX}{name}"))
        .or_else(|_| std::env::var(format!("{LEGACY_ENV_PREFIX}{name}")))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::promotions;

    #[test]
    fn heed_names_map_onto_nugget_names_and_win() {
        let p = promotions([
            ("HEED_HOME".into(), "/h".into()),
            ("NUGGET_HOME".into(), "/n".into()),
            ("NUGGET_KEYCHAIN".into(), "off".into()),
            ("HEED_".into(), "x".into()),
            ("PATH".into(), "/bin".into()),
        ]);
        // Only HEED_HOME is promoted; NUGGET_KEYCHAIN (no HEED_ twin) is left alone.
        assert_eq!(p, vec![("NUGGET_HOME".to_owned(), "/h".to_owned())]);
    }
}
