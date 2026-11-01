//! `nugget://` deep links and `.nuggetplugin` file associations.
//!
//! | Link | Does |
//! |---|---|
//! | `nugget://open/<page>` | shows that page (`today`, `chat`, `search`, ...) |
//! | `nugget://search?q=<text>` | runs a search (read-only, on this device) |
//! | `nugget://item?source=<id>&key=<key>` | asks to open that item in its own app |
//! | `nugget://ask?q=<text>` | opens the quick-ask box with the question typed, not sent |
//! | a `.nuggetplugin` file | asks to install it, then the usual approval of what it may do |
//!
//! Everything is validated here ([`parse_url`], [`parse_plugin_path`]) before the UI sees it:
//! unknown hosts, pages and parameters, credentials, ports, control characters and over-long
//! values are refused. A link never acts by itself: a search is read-only, a question is only
//! typed in, and opening an item or installing a plugin asks the user first (in the main window).
//!
//! How links arrive:
//! - Windows and Linux: as a command-line argument, to the first instance or, through the
//!   single-instance plugin, to the running one ([`from_args`]).
//! - macOS: Apple events, through `tauri-plugin-deep-link`'s `on_open_url` (links and opened
//!   files alike).
//!
//! Registration: the bundles declare the scheme and the file type (`tauri.conf.json`:
//! `plugins.deep-link` and `bundle.fileAssociations`; Info.plist on macOS, the registry from the
//! installers on Windows, the desktop file's `MimeType` on Linux). An AppImage, which installs
//! nothing, registers the scheme for the current user at launch.
//!
//! The main window takes a link with `deeplink_take` (at startup and on each `nugget:deeplink`
//! event, which carries no payload), so a link that starts Nugget is not lost before the page has
//! loaded.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

pub const SCHEME: &str = "nugget";
pub const PLUGIN_EXTENSION: &str = "nuggetplugin";
/// Emitted to the main window when a link is waiting (no payload; see module docs).
pub const EVENT: &str = "nugget:deeplink";

const MAX_URL_LEN: usize = 2048;
const MAX_TEXT_LEN: usize = 500;
const MAX_REF_LEN: usize = 512;
/// Plugin packages are small; anything larger is not one.
const MAX_PLUGIN_BYTES: u64 = 64 * 1024 * 1024;

/// Pages a link may open (the frontend's `PageId`s).
pub const PAGES: &[&str] = &[
    "today",
    "chat",
    "search",
    "knowledge",
    "sources",
    "apps",
    "plugins",
    "agent",
    "safety",
    "doubletap",
    "models",
    "privacy",
    "status",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Link {
    Page { page: String },
    Search { query: String },
    Item { source: String, key: String },
    Ask { question: String },
    Plugin { path: String },
}

/// Characters that reorder or hide text (bidi overrides and isolates, zero-width marks): a link
/// could use them to make a name read as something else in a confirmation.
pub fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

/// The program's arguments, never panicking on one that isn't valid Unicode (it is kept, lossily,
/// and then fails validation).
pub fn launch_args() -> Vec<String> {
    std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// One line of user-visible text: control characters become spaces, invisible ones are dropped,
/// trimmed, non-empty.
fn text(value: &str, max: usize) -> Result<String, String> {
    if value.chars().count() > max {
        return Err(format!("longer than {max} characters"));
    }
    let clean: String = value
        .chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return Err("empty".into());
    }
    Ok(clean.to_owned())
}

/// An identifier as sent: no control characters at all (they are never part of a real one).
fn reference(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > MAX_REF_LEN
        || value.chars().any(|c| c.is_control() || is_invisible(c))
    {
        return Err("not a valid reference".into());
    }
    Ok(value.to_owned())
}

/// Parse and validate a `nugget://` URL.
pub fn parse_url(raw: &str) -> Result<Link, String> {
    if raw.len() > MAX_URL_LEN {
        return Err("link too long".into());
    }
    let url = tauri::Url::parse(raw).map_err(|e| format!("not a link: {e}"))?;
    if url.scheme() != SCHEME {
        return Err("not a nugget:// link".into());
    }
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return Err("unexpected parts in the link".into());
    }
    let host = url.host_str().unwrap_or_default();
    let segments: Vec<String> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default();
    let mut params = std::collections::BTreeMap::new();
    for (k, v) in url.query_pairs() {
        if params.insert(k.into_owned(), v.into_owned()).is_some() {
            return Err("a parameter is repeated".into());
        }
    }
    let only = |allowed: &[&str]| -> Result<(), String> {
        match params.keys().find(|k| !allowed.contains(&k.as_str())) {
            Some(k) => Err(format!("unknown parameter `{k}`")),
            None => Ok(()),
        }
    };
    let no_path = || {
        if segments.is_empty() {
            Ok(())
        } else {
            Err("unexpected path in the link".to_owned())
        }
    };
    let param = |name: &str| params.get(name).map(String::as_str).unwrap_or_default();
    match host {
        "open" => {
            only(&[])?;
            match segments.as_slice() {
                [page] if PAGES.contains(&page.as_str()) => Ok(Link::Page { page: page.clone() }),
                _ => Err("unknown page".into()),
            }
        }
        "search" => {
            only(&["q"])?;
            no_path()?;
            Ok(Link::Search {
                query: text(param("q"), MAX_TEXT_LEN).map_err(|e| format!("query: {e}"))?,
            })
        }
        "ask" => {
            only(&["q"])?;
            no_path()?;
            Ok(Link::Ask {
                question: text(param("q"), MAX_TEXT_LEN).map_err(|e| format!("question: {e}"))?,
            })
        }
        "item" => {
            only(&["source", "key"])?;
            no_path()?;
            Ok(Link::Item {
                source: reference(param("source")).map_err(|e| format!("source: {e}"))?,
                key: reference(param("key")).map_err(|e| format!("key: {e}"))?,
            })
        }
        _ => Err("unknown link".into()),
    }
}

/// Validate a file the OS asked Nugget to open: an existing `.nuggetplugin` regular file, of a
/// plausible size, as an absolute canonical path.
pub fn parse_plugin_path(path: &Path) -> Result<Link, String> {
    let ext_ok = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(PLUGIN_EXTENSION));
    if !ext_ok {
        return Err("not a .nuggetplugin file".into());
    }
    let canonical = std::fs::canonicalize(path).map_err(|e| format!("can't open the file: {e}"))?;
    // A link named `x.nuggetplugin` must point at a plugin package too.
    let target_ok = canonical
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(PLUGIN_EXTENSION));
    if !target_ok {
        return Err("not a .nuggetplugin file".into());
    }
    let meta = std::fs::metadata(&canonical).map_err(|e| format!("can't open the file: {e}"))?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() > MAX_PLUGIN_BYTES {
        return Err("too large to be a plugin".into());
    }
    let path = canonical
        .into_os_string()
        .into_string()
        .map_err(|_| "file paths must be valid Unicode".to_owned())?;
    // Windows' canonical form is a verbatim path (`\\?\C:\...`); the daemon takes the plain one.
    #[cfg(windows)]
    let path = match path.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => path,
    };
    Ok(Link::Plugin { path })
}

/// One argument (Windows/Linux launch, or a URL the OS opened on macOS): a link, a file, or
/// nothing for us. `Some(Err)` is a link or plugin file that failed validation.
pub fn parse_arg(arg: &str) -> Option<Result<Link, String>> {
    let lower = arg.get(..SCHEME.len() + 1).map(str::to_ascii_lowercase);
    if lower.as_deref() == Some("nugget:") {
        return Some(parse_url(arg));
    }
    if let Some(rest) = arg.strip_prefix("file://") {
        let path = tauri::Url::parse(arg)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .unwrap_or_else(|| PathBuf::from(rest));
        return Some(parse_plugin_path(&path));
    }
    let path = Path::new(arg);
    let is_plugin = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(PLUGIN_EXTENSION));
    is_plugin.then(|| parse_plugin_path(path))
}

/// The links among a launch's arguments (program name first), validated.
pub fn from_args(argv: &[String]) -> Vec<Result<Link, String>> {
    argv.iter().skip(1).filter_map(|a| parse_arg(a)).collect()
}

/// The link waiting for the main window, newest wins.
#[derive(Default)]
pub struct Pending(Mutex<Option<Link>>);

/// Act on links: a question goes to the quick-ask box; anything else waits for the main window.
pub fn dispatch(app: &AppHandle, links: Vec<Result<Link, String>>) {
    for link in links {
        match link {
            Err(e) => tracing::warn!(error = %e, "ignored a link"),
            Ok(Link::Ask { question }) => crate::quickask::show_with(app, Some(question)),
            Ok(link) => {
                tracing::info!(kind = kind(&link), "opening a link");
                if let Some(p) = app.try_state::<Pending>() {
                    *p.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(link);
                }
                crate::tray::show_main(app, None);
                let _ = app.emit_to("main", EVENT, ());
            }
        }
    }
}

/// For logs: never the query, path or reference itself.
fn kind(link: &Link) -> &'static str {
    match link {
        Link::Page { .. } => "page",
        Link::Search { .. } => "search",
        Link::Item { .. } => "item",
        Link::Ask { .. } => "ask",
        Link::Plugin { .. } => "plugin",
    }
}

/// The waiting link, once. Only the main window may take it.
#[tauri::command]
pub fn deeplink_take(app: AppHandle, window: tauri::Window) -> Option<Link> {
    if window.label() != "main" {
        return None;
    }
    app.try_state::<Pending>()
        .and_then(|p| p.0.lock().unwrap_or_else(PoisonError::into_inner).take())
}

/// Manage state; macOS: follow Apple events; AppImage: register the scheme for this user.
pub fn setup(app: &tauri::App) {
    app.manage(Pending::default());
    #[cfg(target_os = "macos")]
    {
        use tauri_plugin_deep_link::DeepLinkExt;
        let h = app.handle().clone();
        app.deep_link().on_open_url(move |e| {
            let links = e
                .urls()
                .iter()
                .filter_map(|u| parse_arg(u.as_str()))
                .collect();
            dispatch(&h, links);
        });
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("APPIMAGE").is_some() {
        use tauri_plugin_deep_link::DeepLinkExt;
        if let Err(e) = app.deep_link().register_all() {
            tracing::warn!(error = %e, "couldn't register nugget:// links");
        }
    }
    // The first launch's own arguments (a link or file that started Nugget).
    let links = from_args(&launch_args());
    if !links.is_empty() {
        let h = app.handle().clone();
        // After setup, so the quick-ask window and the tray exist.
        tauri::async_runtime::spawn(async move { dispatch(&h, links) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_search_ask_and_items() {
        assert_eq!(
            parse_url("nugget://open/today"),
            Ok(Link::Page {
                page: "today".into()
            })
        );
        assert_eq!(
            parse_url("nugget://open/status/"),
            Ok(Link::Page {
                page: "status".into()
            })
        );
        assert_eq!(
            parse_url("nugget://search?q=quarterly%20report"),
            Ok(Link::Search {
                query: "quarterly report".into()
            })
        );
        assert_eq!(
            parse_url("nugget://ask?q=When%20is%20my%20flight%3F"),
            Ok(Link::Ask {
                question: "When is my flight?".into()
            })
        );
        assert_eq!(
            parse_url("nugget://item?source=notes&key=a%2Fb.md"),
            Ok(Link::Item {
                source: "notes".into(),
                key: "a/b.md".into()
            })
        );
    }

    #[test]
    fn everything_else_is_refused() {
        for bad in [
            "https://open/today",
            "nugget://open/settings",
            "nugget://open/today/extra",
            "nugget://open/today?x=1",
            "nugget://run?q=rm",
            "nugget://search",
            "nugget://search?q=%20%20",
            "nugget://search/x?q=a",
            "nugget://search?q=a&q=b",
            "nugget://search?q=a&then=delete",
            "nugget://user:pw@search?q=a",
            "nugget://search:8080?q=a",
            "nugget://item?source=notes",
            "nugget://item?source=notes&key=a%00b",
            "not a url",
        ] {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
        let long = format!("nugget://search?q={}", "a".repeat(MAX_TEXT_LEN + 1));
        assert!(parse_url(&long).is_err());
        let huge = format!("nugget://search?q={}", "a".repeat(MAX_URL_LEN));
        assert!(parse_url(&huge).is_err());
    }

    #[test]
    fn invisible_characters_are_dropped_or_refused() {
        assert_eq!(
            parse_url("nugget://search?q=in%E2%80%AEvoice%E2%80%8B"),
            Ok(Link::Search {
                query: "invoice".into()
            })
        );
        assert!(parse_url("nugget://item?source=s&key=a%E2%80%AEfdp.exe").is_err());
        assert!(parse_url("nugget://item?source=s%E2%81%A6&key=a").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_plugin_named_link_to_another_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("nugget-deeplink-ln-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let secret = dir.join("id_key");
        std::fs::write(&secret, b"secret").unwrap();
        let link = dir.join("x.nuggetplugin");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        assert!(parse_plugin_path(&link).is_err());
        let real = dir.join("real.nuggetplugin");
        std::fs::write(&real, b"pkg").unwrap();
        let alias = dir.join("alias.nuggetplugin");
        let _ = std::fs::remove_file(&alias);
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        assert!(
            parse_plugin_path(&alias).is_ok(),
            "a link to a package is fine"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn text_is_one_clean_line() {
        assert_eq!(
            parse_url("nugget://search?q=a%0Ab%09c"),
            Ok(Link::Search {
                query: "a b c".into()
            })
        );
    }

    #[test]
    fn every_link_page_is_a_frontend_page() {
        let api = include_str!("../../src/api.ts");
        let line = api
            .lines()
            .find(|l| l.starts_with("export type PageId ="))
            .expect("PageId in api.ts");
        for p in PAGES {
            assert!(line.contains(&format!("\"{p}\"")), "{p}");
        }
        assert_eq!(line.matches('|').count() + 1, PAGES.len(), "{line}");
    }

    #[test]
    fn plugin_files_are_checked() {
        let dir = std::env::temp_dir().join(format!("nugget-deeplink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hello.NuggetPlugin");
        std::fs::write(&file, b"pkg").unwrap();
        let Ok(Link::Plugin { path }) = parse_plugin_path(&file) else {
            panic!("a plugin file");
        };
        assert!(Path::new(&path).is_absolute());
        assert!(path.ends_with("hello.NuggetPlugin"), "{path}");
        assert!(parse_plugin_path(&dir.join("missing.nuggetplugin")).is_err());
        let other = dir.join("notes.txt");
        std::fs::write(&other, b"x").unwrap();
        assert!(parse_plugin_path(&other).is_err());
        let folder = dir.join("dir.nuggetplugin");
        std::fs::create_dir_all(&folder).unwrap();
        assert!(parse_plugin_path(&folder).is_err());

        let argv: Vec<String> = [
            "nugget-desktop",
            "--minimized",
            file.to_str().unwrap(),
            "nugget://open/chat",
            "/tmp/readme.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let links = from_args(&argv);
        assert_eq!(links.len(), 2, "{links:?}");
        assert!(matches!(links[0], Ok(Link::Plugin { .. })));
        assert_eq!(
            links[1],
            Ok(Link::Page {
                page: "chat".into()
            })
        );
        let file_url = tauri::Url::from_file_path(&file).unwrap();
        assert!(matches!(
            parse_arg(file_url.as_str()),
            Some(Ok(Link::Plugin { .. }))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn links_serialize_for_the_frontend() {
        let v = serde_json::to_value(Link::Search { query: "q".into() }).unwrap();
        assert_eq!(v, serde_json::json!({"kind": "search", "query": "q"}));
    }
}
