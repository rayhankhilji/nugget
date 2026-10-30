//! Google Drive content parsing (`nugget-connector-google`): the mime/extension decision, Docs,
//! Slides and CSV export clean-up, Sheets metadata and values, Slides JSON, and `files.list`
//! entries, on bytes and mime types a hostile shared file or a broken server could send.
//!
//! Input: one byte picking the text cap, then `<mime type>\0<file name>\0<response body>`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use url::Url;

/// Text caps tried, small ones so truncation runs on every input.
const CAPS: [usize; 6] = [0, 1, 7, 64, 4096, 1 << 20];

fuzz_target!(|data: &[u8]| {
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    let cap = CAPS[usize::from(pick) % CAPS.len()];
    let mut parts = rest.splitn(3, |b| *b == 0);
    let mime = String::from_utf8_lossy(parts.next().unwrap_or_default());
    let name = String::from_utf8_lossy(parts.next().unwrap_or_default());
    let body = parts.next().unwrap_or_default();
    for (what, out, max) in nugget_connector_google::fuzz_export(&mime, &name, body, cap) {
        assert!(
            out.len() <= max,
            "{what}: {} bytes over the {max} cap",
            out.len()
        );
        match what {
            "text" => assert!(
                !out.chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
            ),
            "csv" => assert!(
                !out.chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
            ),
            "line" => assert!(!out.chars().any(char::is_control), "{what}: {out:?}"),
            "version" => assert!(!out.chars().any(|c| c.is_control() || c == '|'), "{out:?}"),
            "json" => assert!(serde_json_object(&out), "{out}"),
            "url" => {
                // A sheet title must stay one path segment on the Sheets host: no dot segments,
                // no `/`, `?` or `#` escaping into the path, query or fragment.
                let u = Url::parse(&out).expect("a valid URL");
                assert_eq!(u.host_str(), Some("sheets.googleapis.com"));
                assert_eq!(u.path_segments().map(Iterator::count), Some(5), "{out}");
                assert!(u.path().starts_with("/v4/spreadsheets/s1/values/"), "{out}");
                assert_eq!(u.query(), Some("majorDimension=ROWS"), "{out}");
                assert_eq!(u.fragment(), None);
            }
            other => panic!("unknown output kind {other}"),
        }
    }
});

/// `{...}` at the top level (the metadata is built with `serde_json::json!`).
fn serde_json_object(s: &str) -> bool {
    s.starts_with('{') && s.ends_with('}')
}
