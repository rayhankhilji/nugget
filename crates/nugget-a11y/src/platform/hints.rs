//! Pure per-OS decisions about apps and elements (which apps need their web accessibility
//! switched on, which focused elements are worth a second look, which editors are rich web
//! content). No OS calls, so the tests run on every platform.

// Each backend uses the helpers its OS needs; the others are only exercised by the tests.
#![cfg_attr(not(test), allow(dead_code))]

/// Chromium-based browsers and Electron/CEF apps on macOS, by bundle id. They build their web
/// accessibility tree only after an assistive client sets `AXManualAccessibility` on the app.
/// Firefox (enables on the first AX query) and Safari (always on) are not in the list.
pub fn macos_needs_manual_accessibility(bundle_id: &str) -> bool {
    const PREFIXES: &[&str] = &[
        // Browsers, including their beta/dev/canary channels (`com.google.Chrome.canary`).
        "com.google.Chrome",
        "com.microsoft.edgemac",
        "com.brave.Browser",
        "company.thebrowser.", // Arc and Dia
        "com.vivaldi.Vivaldi",
        "com.operasoftware.Opera",
        "org.chromium.Chromium",
        "ru.yandex.desktop.yandex-browser",
        "com.naver.Whale",
        "net.imput.helium",
        // Electron itself and apps built on it or on CEF.
        "com.github.Electron",
        "com.electron.",
        "com.tinyspeck.slackmacgap",
        "com.hnc.Discord",
        "com.microsoft.VSCode",
        "com.todesktop.", // Cursor, Linear and other ToDesktop builds
        "notion.id",
        "md.obsidian",
        "com.spotify.client",
        "com.figma.Desktop",
        "com.linear",
        "com.anthropic.claudefordesktop",
        "com.openai.chat",
        "com.microsoft.teams", // classic Teams (Electron)
        "com.skype.skype",
        "com.signal.Signal",
        "org.whispersystems.signal-desktop",
        "net.whatsapp.WhatsApp.Electron",
        "com.github.GitHubClient",
        "com.postmanlabs.mac",
        "com.1password.1password",
        "com.bitwarden.desktop",
        "com.loom.desktop",
        "com.clickup.desktop-app",
        "com.asana.app",
        "com.superhuman.electron",
        "com.readdle.spark",
        "com.hey.app",
        "com.mongodb.compass",
    ];
    PREFIXES.iter().any(|p| {
        bundle_id.starts_with(p)
            && (p.ends_with('.')
                || bundle_id.len() == p.len()
                || bundle_id[p.len()..].starts_with(['.', '-']))
    })
}

/// Gecko browsers on macOS, by bundle id. Gecko builds its accessibility tree for VoiceOver or
/// for a client that sets `AXEnhancedUserInterface` on the app, which is set for these apps
/// only (elsewhere it slows window animations).
pub fn macos_is_gecko(bundle_id: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "org.mozilla.", // Firefox, Developer Edition, Nightly, Thunderbird
        "net.waterfox.",
        "io.gitlab.librewolf-community",
        "app.zen-browser.",
        "org.torproject.",
    ];
    PREFIXES.iter().any(|p| bundle_id.starts_with(p))
}

/// Whether an app bundle at `bundle_path` embeds Chromium (Electron, CEF, or a Chromium
/// browser framework), for apps not in [`macos_needs_manual_accessibility`]'s list. Checks
/// for the framework directories only.
pub fn macos_bundle_embeds_chromium(exists: impl Fn(&str) -> bool) -> bool {
    [
        "Contents/Frameworks/Electron Framework.framework",
        "Contents/Frameworks/Chromium Embedded Framework.framework",
        "Contents/Frameworks/Google Chrome Framework.framework",
        "Contents/Frameworks/Microsoft Edge Framework.framework",
        "Contents/Frameworks/Brave Browser Framework.framework",
    ]
    .iter()
    .any(|p| exists(p))
}

/// The focused element a Chromium-based app reports before its web tree is ready: the window,
/// a group, or the web area itself with no editable descendant focused. Worth one more read
/// after a short wait.
pub fn macos_focus_is_placeholder(role: &str, editable: bool) -> bool {
    !editable
        && matches!(
            role,
            "AXWindow" | "AXGroup" | "AXWebArea" | "AXScrollArea" | ""
        )
}

/// A multi-line editable web element on macOS (contenteditable, rich editors, and textareas,
/// which cannot be told apart reliably): its newlines go through the clipboard, because
/// `AXSelectedText` inserts them as one `insertText` that rich editor frameworks handle
/// inconsistently.
pub fn macos_web_multiline_editor(role: &str, in_web_area: bool) -> bool {
    in_web_area
        && matches!(
            role,
            "AXTextArea" | "AXGroup" | "AXWebArea" | "AXUnknown" | ""
        )
}

/// UIA: a focused element that looks like a browser before its web tree is built (a document
/// or pane without a text pattern).
pub fn uia_focus_is_placeholder(control_type: i32, has_text_pattern: bool) -> bool {
    use crate::role::uia;
    !has_text_pattern && matches!(control_type, uia::DOCUMENT | uia::PANE)
}

/// UIA: a focused element that is a container (document, pane, window) without a text
/// pattern; the element with keyboard focus may be one of its descendants.
pub fn uia_focus_is_container(control_type: i32, has_text_pattern: bool) -> bool {
    use crate::role::uia;
    !has_text_pattern && matches!(control_type, uia::DOCUMENT | uia::PANE | uia::WINDOW)
}

/// AT-SPI: an editable element in web content that is not a native text control
/// (`<input>`/`<textarea>`), i.e. a contenteditable region or a rich editor.
///
/// `tag` is the `tag` object attribute (Firefox, Chromium); without it, a native `entry` with
/// no ARIA role is taken as native.
pub fn atspi_web_rich_editable(
    in_web_document: bool,
    role_name: &str,
    tag: Option<&str>,
    xml_roles: Option<&str>,
) -> bool {
    if !in_web_document {
        return false;
    }
    if let Some(tag) = tag {
        return !matches!(tag.to_ascii_lowercase().as_str(), "input" | "textarea");
    }
    !(matches!(role_name, "entry" | "password text" | "spin button")
        && xml_roles.is_none_or(|r| r.trim().is_empty()))
}

/// AT-SPI roles of web document roots.
pub fn atspi_web_document_role(role_name: &str) -> bool {
    matches!(
        role_name,
        "document web" | "document frame" | "document email"
    )
}

/// AT-SPI: a paragraph-like child of an office document (LibreOffice Writer focuses the
/// paragraph, not the document).
pub fn atspi_office_paragraph(role_name: &str, parent_role_name: &str) -> bool {
    matches!(role_name, "paragraph" | "heading" | "list item")
        && matches!(parent_role_name, "document text")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_bundle_ids() {
        for id in [
            "com.google.Chrome",
            "com.google.Chrome.canary",
            "com.google.Chrome.beta",
            "com.microsoft.edgemac",
            "com.microsoft.edgemac.Dev",
            "com.brave.Browser",
            "com.brave.Browser.nightly",
            "company.thebrowser.Browser",
            "org.chromium.Chromium",
            "com.github.Electron",
            "com.tinyspeck.slackmacgap",
            "com.microsoft.VSCode",
            "com.hnc.Discord",
        ] {
            let ok = macos_needs_manual_accessibility(id);
            assert!(ok, "{id}");
        }
        for id in [
            "com.apple.Safari",
            "org.mozilla.firefox",
            "com.apple.TextEdit",
            "com.apple.Notes",
            "com.google.Chromecast",
            "com.microsoft.edgemacx",
            "",
        ] {
            assert!(!macos_needs_manual_accessibility(id), "{id}");
        }
    }

    #[test]
    fn gecko_bundle_ids() {
        for id in [
            "org.mozilla.firefox",
            "org.mozilla.firefoxdeveloperedition",
            "org.mozilla.nightly",
            "io.gitlab.librewolf-community",
            "app.zen-browser.zen",
        ] {
            assert!(macos_is_gecko(id), "{id}");
            assert!(!macos_needs_manual_accessibility(id), "{id}");
        }
        for id in ["com.google.Chrome", "com.apple.Safari", "org.mozillax", ""] {
            assert!(!macos_is_gecko(id), "{id}");
        }
        assert!(macos_needs_manual_accessibility("com.google.Chrome"));
        assert!(macos_needs_manual_accessibility("com.microsoft.edgemac"));
    }

    #[test]
    fn bundle_framework_probe() {
        assert!(macos_bundle_embeds_chromium(|p| p.contains("Electron")));
        assert!(!macos_bundle_embeds_chromium(|_| false));
    }

    #[test]
    fn placeholder_focus() {
        assert!(macos_focus_is_placeholder("AXWebArea", false));
        assert!(macos_focus_is_placeholder("AXWindow", false));
        assert!(!macos_focus_is_placeholder("AXWebArea", true));
        assert!(!macos_focus_is_placeholder("AXTextArea", false));
        assert!(uia_focus_is_placeholder(crate::role::uia::DOCUMENT, false));
        assert!(uia_focus_is_placeholder(crate::role::uia::PANE, false));
        assert!(!uia_focus_is_placeholder(crate::role::uia::DOCUMENT, true));
        assert!(!uia_focus_is_placeholder(crate::role::uia::EDIT, false));
        assert!(uia_focus_is_container(crate::role::uia::WINDOW, false));
        assert!(uia_focus_is_container(crate::role::uia::DOCUMENT, false));
        assert!(!uia_focus_is_container(crate::role::uia::DOCUMENT, true));
        assert!(!uia_focus_is_container(crate::role::uia::EDIT, false));
        assert!(!uia_focus_is_container(crate::role::uia::GROUP, false));
    }

    #[test]
    fn web_editors() {
        assert!(macos_web_multiline_editor("AXTextArea", true));
        assert!(!macos_web_multiline_editor("AXTextArea", false));
        assert!(!macos_web_multiline_editor("AXTextField", true));

        assert!(atspi_web_rich_editable(true, "section", Some("div"), None));
        assert!(atspi_web_rich_editable(
            true,
            "entry",
            Some("div"),
            Some("textbox")
        ));
        assert!(!atspi_web_rich_editable(
            true,
            "entry",
            Some("textarea"),
            None
        ));
        assert!(!atspi_web_rich_editable(true, "entry", Some("INPUT"), None));
        assert!(!atspi_web_rich_editable(true, "entry", None, None));
        assert!(atspi_web_rich_editable(
            true,
            "entry",
            None,
            Some("textbox")
        ));
        assert!(atspi_web_rich_editable(true, "paragraph", None, None));
        assert!(!atspi_web_rich_editable(
            false,
            "section",
            Some("div"),
            None
        ));
        assert!(atspi_web_document_role("document web"));
        assert!(!atspi_web_document_role("document text"));
    }

    #[test]
    fn hint_boundaries() {
        // Prefix ids need a separator after them unless the prefix ends in a dot.
        assert!(!macos_needs_manual_accessibility("com.google.Chromex"));
        assert!(!macos_needs_manual_accessibility("com.google.Chrom"));
        assert!(macos_needs_manual_accessibility("com.brave.Browser-beta"));
        assert!(macos_needs_manual_accessibility("com.electron.myapp"));
        assert!(macos_needs_manual_accessibility(
            "com.todesktop.230313mzl4w4u92"
        ));
        assert!(!macos_needs_manual_accessibility("com.electron"));
        // Every framework path counts on its own.
        for fw in [
            "Electron Framework",
            "Chromium Embedded Framework",
            "Google Chrome Framework",
            "Microsoft Edge Framework",
            "Brave Browser Framework",
        ] {
            let want = format!("Contents/Frameworks/{fw}.framework");
            assert!(macos_bundle_embeds_chromium(|p| p == want), "{fw}");
        }
        // Native secure and spin entries without an ARIA role, and blank `xml-roles`.
        assert!(!atspi_web_rich_editable(true, "password text", None, None));
        assert!(!atspi_web_rich_editable(
            true,
            "spin button",
            None,
            Some("  ")
        ));
        assert!(!atspi_web_rich_editable(true, "entry", None, Some("")));
        assert!(atspi_web_rich_editable(
            true,
            "password text",
            None,
            Some("textbox")
        ));
        assert!(atspi_web_document_role("document frame"));
        assert!(atspi_web_document_role("document email"));
    }

    #[test]
    fn office_paragraphs() {
        assert!(atspi_office_paragraph("heading", "document text"));
        assert!(atspi_office_paragraph("list item", "document text"));
        assert!(!atspi_office_paragraph("paragraph", "document web"));
        assert!(atspi_office_paragraph("paragraph", "document text"));
        assert!(!atspi_office_paragraph("paragraph", "section"));
        assert!(!atspi_office_paragraph("push button", "document text"));
    }
}
