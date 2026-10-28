//! Roles: one platform-neutral [`Role`] for every element, mapped from macOS AX roles, Windows UI
//! Automation control types and AT-SPI role names, plus the normalised [`FieldRole`] reported in
//! `FieldInfo::role`.

use serde::{Deserialize, Serialize};

/// Platform-neutral element role. Coarse on purpose: drafting and the thread heuristics (and
/// computer use in M4) need to tell containers, text, controls and editable fields apart, not
/// every toolkit nuance. The raw platform role is kept next to it in [`crate::tree::Node`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Application,
    Window,
    Dialog,
    /// Single-line editable text.
    TextField,
    /// Multi-line editable text.
    TextArea,
    SearchField,
    ComboBox,
    /// A rich document (word processor body, editable document).
    Document,
    /// A browser or Electron web content root.
    WebArea,
    StaticText,
    Paragraph,
    Heading,
    Link,
    Button,
    Image,
    List,
    ListItem,
    Table,
    Row,
    Cell,
    /// A chat log or feed (ARIA `log`/`feed`).
    Log,
    Article,
    Group,
    Section,
    ScrollArea,
    Toolbar,
    Menu,
    MenuItem,
    Tab,
    CheckBox,
    Separator,
    #[default]
    Unknown,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Application => "application",
            Role::Window => "window",
            Role::Dialog => "dialog",
            Role::TextField => "text_field",
            Role::TextArea => "text_area",
            Role::SearchField => "search_field",
            Role::ComboBox => "combo_box",
            Role::Document => "document",
            Role::WebArea => "web_area",
            Role::StaticText => "static_text",
            Role::Paragraph => "paragraph",
            Role::Heading => "heading",
            Role::Link => "link",
            Role::Button => "button",
            Role::Image => "image",
            Role::List => "list",
            Role::ListItem => "list_item",
            Role::Table => "table",
            Role::Row => "row",
            Role::Cell => "cell",
            Role::Log => "log",
            Role::Article => "article",
            Role::Group => "group",
            Role::Section => "section",
            Role::ScrollArea => "scroll_area",
            Role::Toolbar => "toolbar",
            Role::Menu => "menu",
            Role::MenuItem => "menu_item",
            Role::Tab => "tab",
            Role::CheckBox => "check_box",
            Role::Separator => "separator",
            Role::Unknown => "unknown",
        }
    }

    /// Roles that hold text a user would read as content.
    pub fn is_text(self) -> bool {
        matches!(
            self,
            Role::StaticText | Role::Paragraph | Role::Heading | Role::Link | Role::Cell
        )
    }

    /// Roles that are interface chrome rather than content (buttons, menus, toolbars...).
    pub fn is_chrome(self) -> bool {
        matches!(
            self,
            Role::Button
                | Role::Toolbar
                | Role::Menu
                | Role::MenuItem
                | Role::Tab
                | Role::CheckBox
                | Role::Separator
                | Role::Image
        )
    }

    /// Roles that are editable by nature (before looking at the editable state).
    pub fn is_text_input(self) -> bool {
        matches!(
            self,
            Role::TextField | Role::TextArea | Role::SearchField | Role::ComboBox
        )
    }
}

/// The normalised role of the focused editable element (`FieldInfo::role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldRole {
    TextArea,
    TextField,
    ComboBox,
    Document,
    WebArea,
    SearchField,
}

impl FieldRole {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldRole::TextArea => "text_area",
            FieldRole::TextField => "text_field",
            FieldRole::ComboBox => "combo_box",
            FieldRole::Document => "document",
            FieldRole::WebArea => "web_area",
            FieldRole::SearchField => "search_field",
        }
    }
}

impl std::fmt::Display for FieldRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What is known about an element when deciding its field role.
#[derive(Debug, Clone, Copy, Default)]
pub struct FieldHints<'a> {
    pub editable: bool,
    /// `Some(true)` multi-line, `Some(false)` single-line, `None` unknown.
    pub multiline: Option<bool>,
    pub placeholder: Option<&'a str>,
    pub label: Option<&'a str>,
}

/// Decide the field role of an element, or `None` when it is not an editable text target.
pub fn field_role(role: Role, hints: FieldHints<'_>) -> Option<FieldRole> {
    let searchy = |s: Option<&str>| {
        s.is_some_and(|s| {
            let s = s.to_lowercase();
            s.starts_with("search")
                || s.contains(" search")
                || s == "find"
                || s.starts_with("find ")
        })
    };
    let fr = match role {
        Role::SearchField => FieldRole::SearchField,
        Role::ComboBox if hints.editable => FieldRole::ComboBox,
        Role::TextField | Role::TextArea if !hints.editable => return None,
        Role::TextField if hints.multiline == Some(true) => FieldRole::TextArea,
        Role::TextField => FieldRole::TextField,
        Role::TextArea if hints.multiline == Some(false) => FieldRole::TextField,
        Role::TextArea => FieldRole::TextArea,
        Role::Document if hints.editable => FieldRole::Document,
        Role::WebArea if hints.editable => FieldRole::WebArea,
        // contenteditable regions surface as generic containers or paragraphs that are editable.
        Role::Group | Role::Section | Role::Paragraph | Role::Unknown | Role::Cell | Role::Log
            if hints.editable =>
        {
            if hints.multiline == Some(false) {
                FieldRole::TextField
            } else {
                FieldRole::TextArea
            }
        }
        _ => return None,
    };
    if fr == FieldRole::TextField && (searchy(hints.placeholder) || searchy(hints.label)) {
        return Some(FieldRole::SearchField);
    }
    Some(fr)
}

/// Map a macOS AX role and subrole (`AXTextArea`, `AXSecureTextField`...).
pub fn role_from_ax(role: &str, subrole: Option<&str>) -> Role {
    match subrole.unwrap_or("") {
        "AXSearchField" => return Role::SearchField,
        "AXSecureTextField" => return Role::TextField,
        "AXApplicationLog" => return Role::Log,
        "AXDocumentArticle" => return Role::Article,
        "AXContentList" | "AXDefinitionList" => return Role::List,
        "AXDialog" | "AXSystemDialog" | "AXFloatingWindow" => return Role::Dialog,
        "AXTextAttachment" => return Role::Image,
        _ => {}
    }
    match role {
        "AXApplication" => Role::Application,
        "AXWindow" | "AXDrawer" | "AXSheet" => Role::Window,
        "AXTextField" => Role::TextField,
        "AXTextArea" => Role::TextArea,
        "AXComboBox" => Role::ComboBox,
        "AXWebArea" => Role::WebArea,
        "AXStaticText" => Role::StaticText,
        "AXHeading" => Role::Heading,
        "AXLink" => Role::Link,
        "AXButton"
        | "AXPopUpButton"
        | "AXMenuButton"
        | "AXDisclosureTriangle"
        | "AXIncrementor" => Role::Button,
        "AXImage" => Role::Image,
        "AXList" | "AXOutline" => Role::List,
        "AXTable" | "AXGrid" => Role::Table,
        "AXRow" => Role::Row,
        "AXCell" => Role::Cell,
        "AXGroup" | "AXSplitGroup" | "AXLayoutArea" | "AXLayoutItem" | "AXRadioGroup" => {
            Role::Group
        }
        "AXScrollArea" | "AXBrowser" => Role::ScrollArea,
        "AXToolbar" | "AXTabGroup" => Role::Toolbar,
        "AXMenu" | "AXMenuBar" => Role::Menu,
        "AXMenuItem" | "AXMenuBarItem" => Role::MenuItem,
        "AXRadioButton" => Role::Tab,
        "AXCheckBox" => Role::CheckBox,
        "AXSplitter" => Role::Separator,
        "AXDocument" => Role::Document,
        _ => Role::Unknown,
    }
}

/// UI Automation control type ids (`UIA_*ControlTypeId`).
pub mod uia {
    pub const BUTTON: i32 = 50000;
    pub const CALENDAR: i32 = 50001;
    pub const CHECK_BOX: i32 = 50002;
    pub const COMBO_BOX: i32 = 50003;
    pub const EDIT: i32 = 50004;
    pub const HYPERLINK: i32 = 50005;
    pub const IMAGE: i32 = 50006;
    pub const LIST_ITEM: i32 = 50007;
    pub const LIST: i32 = 50008;
    pub const MENU: i32 = 50009;
    pub const MENU_BAR: i32 = 50010;
    pub const MENU_ITEM: i32 = 50011;
    pub const RADIO_BUTTON: i32 = 50013;
    pub const SPLIT_BUTTON: i32 = 50031;
    pub const TAB: i32 = 50018;
    pub const TAB_ITEM: i32 = 50019;
    pub const TEXT: i32 = 50020;
    pub const TOOL_BAR: i32 = 50021;
    pub const TREE: i32 = 50023;
    pub const TREE_ITEM: i32 = 50024;
    pub const GROUP: i32 = 50026;
    pub const DATA_GRID: i32 = 50028;
    pub const DATA_ITEM: i32 = 50029;
    pub const DOCUMENT: i32 = 50030;
    pub const WINDOW: i32 = 50032;
    pub const PANE: i32 = 50033;
    pub const HEADER: i32 = 50034;
    pub const HEADER_ITEM: i32 = 50035;
    pub const TABLE: i32 = 50036;
    pub const TITLE_BAR: i32 = 50037;
    pub const SEPARATOR: i32 = 50038;
    pub const CUSTOM: i32 = 50025;
}

/// Map a UI Automation control type. `aria_role` (Chromium/Edge/Firefox expose it) refines web
/// content: `searchbox`, `log`, `feed`, `article`, `textbox`...
pub fn role_from_uia(control_type: i32, aria_role: Option<&str>, class_name: Option<&str>) -> Role {
    match aria_role.unwrap_or("") {
        "searchbox" => return Role::SearchField,
        "log" | "feed" => return Role::Log,
        "article" => return Role::Article,
        "heading" => return Role::Heading,
        "paragraph" => return Role::Paragraph,
        "region" | "main" | "complementary" | "navigation" => return Role::Section,
        "textbox" if control_type == uia::EDIT || control_type == uia::GROUP => {
            return Role::TextArea;
        }
        _ => {}
    }
    match control_type {
        uia::BUTTON | uia::SPLIT_BUTTON => Role::Button,
        uia::CHECK_BOX => Role::CheckBox,
        uia::COMBO_BOX => Role::ComboBox,
        // Edit is single- or multi-line; the backend refines with the multi-line hint.
        uia::EDIT => Role::TextField,
        uia::HYPERLINK => Role::Link,
        uia::IMAGE => Role::Image,
        uia::LIST_ITEM | uia::TREE_ITEM | uia::DATA_ITEM => Role::ListItem,
        uia::LIST | uia::TREE => Role::List,
        uia::MENU | uia::MENU_BAR => Role::Menu,
        uia::MENU_ITEM => Role::MenuItem,
        uia::RADIO_BUTTON | uia::TAB_ITEM => Role::Tab,
        uia::TAB | uia::TOOL_BAR | uia::HEADER | uia::TITLE_BAR => Role::Toolbar,
        uia::TEXT => Role::StaticText,
        uia::GROUP | uia::CUSTOM | uia::CALENDAR => Role::Group,
        uia::DATA_GRID | uia::TABLE => Role::Table,
        uia::HEADER_ITEM => Role::Cell,
        uia::DOCUMENT => {
            let class = class_name.unwrap_or("");
            if class.starts_with("Chrome_RenderWidgetHostHWND")
                || class == "MozillaWindowClass"
                || class.contains("WebView")
            {
                Role::WebArea
            } else {
                Role::Document
            }
        }
        uia::WINDOW => Role::Window,
        uia::PANE => Role::ScrollArea,
        uia::SEPARATOR => Role::Separator,
        _ => Role::Unknown,
    }
}

/// Map an AT-SPI role name (`Accessible.GetRoleName`, e.g. `"password text"`). `xml_roles` is
/// the `xml-roles` object attribute browsers expose for ARIA roles.
pub fn role_from_atspi(role_name: &str, xml_roles: Option<&str>) -> Role {
    if let Some(x) = xml_roles {
        for r in x.split_whitespace() {
            match r {
                "searchbox" => return Role::SearchField,
                "log" | "feed" => return Role::Log,
                "article" => return Role::Article,
                _ => {}
            }
        }
    }
    match role_name {
        "application" => Role::Application,
        "frame" | "window" => Role::Window,
        "dialog" | "alert" | "file chooser" => Role::Dialog,
        "entry" | "password text" | "spin button" => Role::TextField,
        // GTK text views report "text"; single/multi-line comes from the states.
        "text" | "terminal" => Role::TextArea,
        "combo box" => Role::ComboBox,
        "document frame" | "document text" | "document spreadsheet" | "document presentation" => {
            Role::Document
        }
        "document web" | "document email" => Role::WebArea,
        "label" | "static" | "caption" | "description value" | "description term" => {
            Role::StaticText
        }
        "paragraph" | "block quote" => Role::Paragraph,
        "heading" => Role::Heading,
        "link" => Role::Link,
        "push button" | "toggle button" | "push button menu" => Role::Button,
        "image" | "icon" | "animation" => Role::Image,
        "list" | "list box" | "tree" | "description list" => Role::List,
        "list item" | "tree item" => Role::ListItem,
        "table" | "tree table" => Role::Table,
        "table row" => Role::Row,
        "table cell" | "column header" | "row header" => Role::Cell,
        "log" => Role::Log,
        "article" | "comment" => Role::Article,
        "panel" | "filler" | "grouping" | "form" | "landmark" | "embedded" | "internal frame"
        | "split pane" | "layered pane" | "root pane" | "option pane" | "viewport" => Role::Group,
        "section" | "block" | "footer" | "header" | "content deletion" | "content insertion"
        | "mark" => Role::Section,
        "scroll pane" | "page tab list" => Role::ScrollArea,
        "tool bar" | "status bar" | "menu bar" => Role::Toolbar,
        "menu" | "popup menu" => Role::Menu,
        "menu item" | "check menu item" | "radio menu item" | "tearoff menu item" => Role::MenuItem,
        "page tab" | "radio button" => Role::Tab,
        "check box" => Role::CheckBox,
        "separator" => Role::Separator,
        _ => Role::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editable(multiline: Option<bool>) -> FieldHints<'static> {
        FieldHints {
            editable: true,
            multiline,
            ..Default::default()
        }
    }

    #[test]
    fn ax_roles() {
        assert_eq!(role_from_ax("AXTextArea", None), Role::TextArea);
        assert_eq!(
            role_from_ax("AXTextField", Some("AXSecureTextField")),
            Role::TextField
        );
        assert_eq!(
            role_from_ax("AXTextField", Some("AXSearchField")),
            Role::SearchField
        );
        assert_eq!(role_from_ax("AXWebArea", None), Role::WebArea);
        assert_eq!(role_from_ax("AXGroup", Some("AXApplicationLog")), Role::Log);
        assert_eq!(role_from_ax("AXSomethingNew", None), Role::Unknown);
    }

    #[test]
    fn uia_roles() {
        assert_eq!(role_from_uia(uia::EDIT, None, None), Role::TextField);
        assert_eq!(
            role_from_uia(uia::EDIT, Some("searchbox"), None),
            Role::SearchField
        );
        assert_eq!(
            role_from_uia(uia::DOCUMENT, None, Some("Chrome_RenderWidgetHostHWND")),
            Role::WebArea
        );
        assert_eq!(
            role_from_uia(uia::DOCUMENT, None, Some("RichEditD2DPT")),
            Role::Document
        );
        assert_eq!(role_from_uia(uia::LIST_ITEM, None, None), Role::ListItem);
        assert_eq!(role_from_uia(uia::GROUP, Some("log"), None), Role::Log);
    }

    #[test]
    fn atspi_roles() {
        assert_eq!(role_from_atspi("password text", None), Role::TextField);
        assert_eq!(role_from_atspi("text", None), Role::TextArea);
        assert_eq!(
            role_from_atspi("entry", Some("searchbox")),
            Role::SearchField
        );
        assert_eq!(role_from_atspi("document web", None), Role::WebArea);
        assert_eq!(role_from_atspi("list box", None), Role::List);
        assert_eq!(role_from_atspi("section", Some("log")), Role::Log);
    }

    #[test]
    fn field_roles_from_roles_and_hints() {
        assert_eq!(
            field_role(Role::TextArea, editable(Some(false))),
            Some(FieldRole::TextField)
        );
        assert_eq!(
            field_role(Role::TextArea, editable(Some(true))),
            Some(FieldRole::TextArea)
        );
        assert_eq!(
            field_role(Role::TextField, editable(Some(true))),
            Some(FieldRole::TextArea)
        );
        assert_eq!(
            field_role(Role::WebArea, editable(None)),
            Some(FieldRole::WebArea)
        );
        assert_eq!(
            field_role(Role::Document, editable(None)),
            Some(FieldRole::Document)
        );
        assert_eq!(
            field_role(Role::ComboBox, editable(None)),
            Some(FieldRole::ComboBox)
        );
        // contenteditable div (Gmail body, Slack composer)
        assert_eq!(
            field_role(Role::Group, editable(None)),
            Some(FieldRole::TextArea)
        );
        // Read-only things are not fields.
        assert_eq!(field_role(Role::TextField, FieldHints::default()), None);
        assert_eq!(field_role(Role::WebArea, FieldHints::default()), None);
        assert_eq!(field_role(Role::Button, editable(None)), None);
    }

    #[test]
    fn search_detected_from_placeholder_or_label() {
        let hints = FieldHints {
            editable: true,
            multiline: Some(false),
            placeholder: Some("Search messages"),
            label: None,
        };
        assert_eq!(
            field_role(Role::TextField, hints),
            Some(FieldRole::SearchField)
        );
        let hints = FieldHints {
            editable: true,
            multiline: Some(false),
            placeholder: None,
            label: Some("Research notes"),
        };
        assert_eq!(
            field_role(Role::TextField, hints),
            Some(FieldRole::TextField)
        );
        assert_eq!(FieldRole::SearchField.as_str(), "search_field");
    }
}
