//! macOS for the `native` environment: the app windows are Google Chrome app windows, read and
//! driven over AX (`nugget_a11y::system_computer()`, the macOS backend). The matching,
//! presentation and actions are [`super::env_uia`]'s, which work on any [`ComputerUse`]; this
//! module holds what differs on macOS.
//!
//! - **The browser.** Google Chrome, preinstalled on GitHub's macOS images (Safari cannot open a
//!   bare app window from the command line, and keeps one process for every window). Each app
//!   gets its own Chrome process (`--app=<url>`, a throwaway profile, the mock keychain so no
//!   keychain prompt blocks it, renderer accessibility forced on), tiled as on Windows. Each
//!   process owns exactly one window, so windows map to apps by pid. `NUGGET_CUA_CHROME`
//!   overrides the binary (any Chromium build with `--app` works).
//! - **AX conventions** ([`normalize`]). Chromium's AX tree differs from UI Automation's in ways
//!   the matcher cares about, so each snapshot is first put in the form the matcher expects:
//!   static text carries its text in `AXValue`, not a name; an `aria-label` is an
//!   `AXDescription`; a `<select>` is an `AXPopUpButton` (a button to the role table) whose
//!   options are `AXMenuItem`s pressed with `AXPress`. A `<label for>` names its control through
//!   `AXTitleUIElement`, which the AX backend reads into the control's name. Only nodes whose
//!   raw role is an AX role are touched, so UIA trees pass through unchanged.
//!
//! [`ComputerUse`]: nugget_a11y::computer::ComputerUse

use std::path::{Path, PathBuf};

use nugget_a11y::computer::UiNode;
use nugget_a11y::role::Role as A11yRole;

/// Where Chrome (or another Chromium) usually lives on macOS, relative to an `Applications`
/// folder.
const CHROME_APPS: &[&str] = &[
    "Google Chrome.app/Contents/MacOS/Google Chrome",
    "Chromium.app/Contents/MacOS/Chromium",
];

/// `NUGGET_CUA_CHROME`, then Chrome or Chromium in `/Applications` or `~/Applications`.
pub fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NUGGET_CUA_CHROME") {
        return Some(PathBuf::from(p));
    }
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots
        .iter()
        .flat_map(|r| CHROME_APPS.iter().map(move |a| r.join(a)))
        .find(|p| p.is_file())
}

/// Command line for one app window: `url` in an app window at `pos` with `size`, its own
/// `profile`.
pub fn chrome_args(url: &str, profile: &Path, pos: (u32, u32), size: (u32, u32)) -> Vec<String> {
    vec![
        format!("--app={url}"),
        format!("--user-data-dir={}", profile.display()),
        format!("--window-position={},{}", pos.0, pos.1),
        format!("--window-size={},{}", size.0, size.1),
        "--force-renderer-accessibility".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        // No "Chrome Safe Storage" keychain prompt.
        "--use-mock-keychain".into(),
        "--password-store=basic".into(),
        "--disable-sync".into(),
        "--disable-extensions".into(),
        "--disable-component-update".into(),
        "--disable-background-networking".into(),
        "--disable-features=Translate,OptimizationHints,MediaRouter,DialMediaRouteProvider".into(),
        // Covered windows keep loading and polling (the pages reload when the world changes).
        "--disable-backgrounding-occluded-windows".into(),
        "--disable-renderer-backgrounding".into(),
        "--disable-background-timer-throttling".into(),
        "--no-proxy-server".into(),
    ]
}

/// Whether a window or app name is Chrome's (or Chromium's).
pub fn is_chrome(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("chrome") || n.contains("chromium")
}

fn blank(s: Option<&str>) -> bool {
    s.is_none_or(|s| s.trim().is_empty())
}

/// Put a Chromium AX tree in the conventions [`super::env_uia::match_tree`] expects (see the
/// module docs). Nodes without an AX raw role are left alone.
pub fn normalize(n: &mut UiNode) {
    if n.raw_role.starts_with("AX") {
        let raw = n.raw_role.split('/').next().unwrap_or("");
        if blank(n.name.as_deref()) {
            let from = if raw == "AXStaticText" {
                n.value.clone()
            } else {
                n.description.clone()
            };
            if !blank(from.as_deref()) {
                n.name = from;
            }
        }
        if raw == "AXPopUpButton" {
            n.role = A11yRole::ComboBox;
        }
    }
    for c in &mut n.children {
        normalize(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_a11y::computer::UiActionKind;

    fn ax(raw: &str, role: A11yRole) -> UiNode {
        UiNode {
            raw_role: raw.into(),
            role,
            ..UiNode::default()
        }
    }

    #[test]
    fn chromium_conventions_become_the_matchers() {
        let mut text = ax("AXStaticText", A11yRole::StaticText);
        text.value = Some("Hello".into());
        let mut list = ax("AXGroup", A11yRole::Group);
        list.description = Some("Inbox".into());
        let mut select = ax("AXPopUpButton", A11yRole::Button);
        select.value = Some("Germany".into());
        select.name = Some("Country".into());
        let mut option = ax("AXMenuItem", A11yRole::MenuItem);
        option.name = Some("France".into());
        option.actions = vec![UiActionKind::Press];
        let mut menu = ax("AXMenu", A11yRole::Menu);
        menu.children.push(option);
        select.children.push(menu);
        let mut field = ax("AXTextField", A11yRole::TextField);
        field.name = Some("Email".into());
        field.description = Some("not the name".into());
        field.value = Some("a@b.c".into());
        let mut root = ax("AXWindow", A11yRole::Window);
        root.children = vec![text, list, select, field];

        normalize(&mut root);
        let [text, list, select, field] = &root.children[..] else {
            panic!("children");
        };
        assert_eq!(text.name.as_deref(), Some("Hello"));
        assert_eq!(list.name.as_deref(), Some("Inbox"));
        assert_eq!(select.role, A11yRole::ComboBox);
        assert_eq!(select.name.as_deref(), Some("Country"));
        assert_eq!(select.value.as_deref(), Some("Germany"));
        assert_eq!(
            select.children[0].children[0].name.as_deref(),
            Some("France")
        );
        // A named control keeps its name; its value is never taken for one.
        assert_eq!(field.name.as_deref(), Some("Email"));
        assert_eq!(field.value.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn uia_trees_pass_through() {
        let mut n = UiNode {
            raw_role: "edit".into(),
            role: A11yRole::TextField,
            description: Some("help".into()),
            ..UiNode::default()
        };
        let before = n.clone();
        normalize(&mut n);
        assert_eq!(n, before);
        let mut t = UiNode {
            raw_role: "text".into(),
            role: A11yRole::StaticText,
            value: Some("x".into()),
            ..UiNode::default()
        };
        normalize(&mut t);
        assert_eq!(t.name, None);
    }

    #[test]
    fn app_window_command_line() {
        let a = chrome_args(
            "http://127.0.0.1:9/forms/",
            Path::new("profile"),
            (530, 0),
            (530, 490),
        );
        assert_eq!(a[0], "--app=http://127.0.0.1:9/forms/");
        assert!(a.contains(&"--window-position=530,0".to_owned()));
        assert!(a.contains(&"--window-size=530,490".to_owned()));
        assert!(a.contains(&"--force-renderer-accessibility".to_owned()));
        assert!(a.contains(&"--use-mock-keychain".to_owned()));
        assert!(a.iter().any(|x| x.starts_with("--user-data-dir=")));
        assert!(is_chrome("Google Chrome"));
        assert!(is_chrome("com.google.Chrome"));
        assert!(!is_chrome("msedge"));
    }
}
