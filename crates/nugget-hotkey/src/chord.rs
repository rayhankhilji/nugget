//! Accelerator strings such as `Ctrl+Alt+Space`: parsing, validation and per-platform names.

use std::fmt;

use nugget_core::{Error, Result};

/// The platforms a chord can be written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    /// The platform this build targets (other Unixes are treated as Linux).
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// The modifiers of a chord. Chords do not distinguish left from right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ChordModifiers {
    pub control: bool,
    /// Alt, or Option on macOS.
    pub alt: bool,
    pub shift: bool,
    /// Command on macOS, the Windows key on Windows, Super on Linux.
    pub super_key: bool,
}

impl ChordModifiers {
    pub fn is_empty(&self) -> bool {
        !(self.control || self.alt || self.shift || self.super_key)
    }
}

/// The non-modifier key of a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordKey {
    /// `A`–`Z`, stored upper case.
    Letter(char),
    /// `0`–`9`.
    Digit(u8),
    /// `F1`–`F24`.
    Function(u8),
    Space,
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    Minus,
    Equal,
    Comma,
    Period,
    Slash,
    Backslash,
    Semicolon,
    Quote,
    Backquote,
    BracketLeft,
    BracketRight,
}

const NAMED_KEYS: &[(ChordKey, &str, &str, &str)] = &[
    // (key, display name, W3C `KeyboardEvent.code`, XKB keysym name)
    (ChordKey::Space, "Space", "Space", "space"),
    (ChordKey::Enter, "Enter", "Enter", "Return"),
    (ChordKey::Tab, "Tab", "Tab", "Tab"),
    (ChordKey::Escape, "Esc", "Escape", "Escape"),
    (ChordKey::Backspace, "Backspace", "Backspace", "BackSpace"),
    (ChordKey::Delete, "Delete", "Delete", "Delete"),
    (ChordKey::Insert, "Insert", "Insert", "Insert"),
    (ChordKey::Home, "Home", "Home", "Home"),
    (ChordKey::End, "End", "End", "End"),
    (ChordKey::PageUp, "PageUp", "PageUp", "Page_Up"),
    (ChordKey::PageDown, "PageDown", "PageDown", "Page_Down"),
    (ChordKey::Up, "Up", "ArrowUp", "Up"),
    (ChordKey::Down, "Down", "ArrowDown", "Down"),
    (ChordKey::Left, "Left", "ArrowLeft", "Left"),
    (ChordKey::Right, "Right", "ArrowRight", "Right"),
    (ChordKey::Minus, "-", "Minus", "minus"),
    (ChordKey::Equal, "=", "Equal", "equal"),
    (ChordKey::Comma, ",", "Comma", "comma"),
    (ChordKey::Period, ".", "Period", "period"),
    (ChordKey::Slash, "/", "Slash", "slash"),
    (ChordKey::Backslash, "\\", "Backslash", "backslash"),
    (ChordKey::Semicolon, ";", "Semicolon", "semicolon"),
    (ChordKey::Quote, "'", "Quote", "apostrophe"),
    (ChordKey::Backquote, "`", "Backquote", "grave"),
    (ChordKey::BracketLeft, "[", "BracketLeft", "bracketleft"),
    (ChordKey::BracketRight, "]", "BracketRight", "bracketright"),
];

/// Extra spellings accepted when parsing (lower case).
const KEY_ALIASES: &[(&str, ChordKey)] = &[
    ("space", ChordKey::Space),
    ("spacebar", ChordKey::Space),
    ("enter", ChordKey::Enter),
    ("return", ChordKey::Enter),
    ("tab", ChordKey::Tab),
    ("esc", ChordKey::Escape),
    ("escape", ChordKey::Escape),
    ("backspace", ChordKey::Backspace),
    ("delete", ChordKey::Delete),
    ("del", ChordKey::Delete),
    ("insert", ChordKey::Insert),
    ("ins", ChordKey::Insert),
    ("home", ChordKey::Home),
    ("end", ChordKey::End),
    ("pageup", ChordKey::PageUp),
    ("pgup", ChordKey::PageUp),
    ("pagedown", ChordKey::PageDown),
    ("pgdn", ChordKey::PageDown),
    ("up", ChordKey::Up),
    ("arrowup", ChordKey::Up),
    ("down", ChordKey::Down),
    ("arrowdown", ChordKey::Down),
    ("left", ChordKey::Left),
    ("arrowleft", ChordKey::Left),
    ("right", ChordKey::Right),
    ("arrowright", ChordKey::Right),
    ("minus", ChordKey::Minus),
    ("equal", ChordKey::Equal),
    ("comma", ChordKey::Comma),
    ("period", ChordKey::Period),
    ("slash", ChordKey::Slash),
    ("backslash", ChordKey::Backslash),
    ("semicolon", ChordKey::Semicolon),
    ("quote", ChordKey::Quote),
    ("apostrophe", ChordKey::Quote),
    ("backquote", ChordKey::Backquote),
    ("grave", ChordKey::Backquote),
    ("bracketleft", ChordKey::BracketLeft),
    ("bracketright", ChordKey::BracketRight),
];

impl ChordKey {
    fn named(self) -> Option<&'static (ChordKey, &'static str, &'static str, &'static str)> {
        NAMED_KEYS.iter().find(|(k, ..)| *k == self)
    }

    /// The display name used in normalised accelerators (`A`, `5`, `F13`, `Space`, `/`).
    pub fn name(self) -> String {
        match self {
            Self::Letter(c) => c.to_string(),
            Self::Digit(d) => d.to_string(),
            Self::Function(n) => format!("F{n}"),
            other => other.named().map(|n| n.1).unwrap_or_default().to_string(),
        }
    }

    /// The W3C `KeyboardEvent.code` name (`KeyA`, `Digit5`, `F13`, `ArrowUp`), which is what the
    /// `global-hotkey` crate takes.
    pub fn w3c_code(self) -> String {
        match self {
            Self::Letter(c) => format!("Key{c}"),
            Self::Digit(d) => format!("Digit{d}"),
            Self::Function(n) => format!("F{n}"),
            other => other.named().map(|n| n.2).unwrap_or_default().to_string(),
        }
    }

    /// The XKB keysym name (`a`, `5`, `F13`, `space`), used by the XDG GlobalShortcuts portal.
    pub fn xkb_name(self) -> String {
        match self {
            Self::Letter(c) => c.to_ascii_lowercase().to_string(),
            Self::Digit(d) => d.to_string(),
            Self::Function(n) => format!("F{n}"),
            other => other.named().map(|n| n.3).unwrap_or_default().to_string(),
        }
    }

    fn parse(token: &str) -> Option<Self> {
        let lower = token.to_ascii_lowercase();
        let mut chars = token.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_alphabetic() {
                return Some(Self::Letter(c.to_ascii_uppercase()));
            }
            if let Some(d) = c.to_digit(10) {
                return Some(Self::Digit(d as u8));
            }
            return NAMED_KEYS.iter().find(|n| n.1 == token).map(|n| n.0);
        }
        // W3C code spellings: KeyA, Digit5.
        if let Some(rest) = lower.strip_prefix("key")
            && rest.len() == 1
        {
            return Self::parse(rest);
        }
        if let Some(rest) = lower.strip_prefix("digit")
            && rest.len() == 1
            && rest.as_bytes()[0].is_ascii_digit()
        {
            return Self::parse(rest);
        }
        if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            return (1..=24).contains(&n).then_some(Self::Function(n));
        }
        KEY_ALIASES
            .iter()
            .find(|(a, _)| *a == lower)
            .map(|(_, k)| *k)
    }
}

/// A validated chord: one or more modifiers plus exactly one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub modifiers: ChordModifiers,
    pub key: ChordKey,
}

#[derive(Clone, Copy)]
enum ModToken {
    Control,
    Alt,
    Shift,
    Super,
    /// Command on macOS, Control elsewhere.
    CmdOrCtrl,
}

fn parse_modifier(token: &str) -> Option<ModToken> {
    Some(match token.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "ctl" | "⌃" => ModToken::Control,
        "alt" | "option" | "opt" | "⌥" => ModToken::Alt,
        "shift" | "⇧" => ModToken::Shift,
        "cmd" | "command" | "⌘" | "super" | "win" | "windows" | "meta" | "logo" => {
            ModToken::Super
        }
        "cmdorctrl" | "cmdorcontrol" | "commandorcontrol" | "commandorctrl" => ModToken::CmdOrCtrl,
        _ => return None,
    })
}

/// Parse an accelerator such as `Ctrl+Alt+Space` for the current platform.
///
/// Accepted:
/// - Modifiers, case-insensitive: `Ctrl`/`Control`, `Alt`/`Option`/`Opt`, `Shift`,
///   `Cmd`/`Command`/`Super`/`Win`/`Meta`, the symbols `⌃ ⌥ ⇧ ⌘`, and `CmdOrCtrl` (Command on
///   macOS, Control elsewhere).
/// - One key: a letter, a digit, `F1`–`F24`, a named key (`Space`, `Enter`, `Tab`, `Esc`,
///   `Backspace`, `Delete`, `Insert`, `Home`, `End`, `PageUp`, `PageDown`, arrows), or
///   punctuation (`- = , . / \ ; ' \` [ ]` or their names).
///
/// Rejected: empty parts, unknown names, a repeated modifier, no key or more than one key, and
/// a chord that would steal ordinary typing (no modifier, or Shift alone) unless the key is a
/// function key.
pub fn parse_chord(accelerator: &str) -> Result<Chord> {
    parse_chord_for(accelerator, Platform::current())
}

/// [`parse_chord`] for a given platform (this only changes what `CmdOrCtrl` means).
pub fn parse_chord_for(accelerator: &str, platform: Platform) -> Result<Chord> {
    let invalid = |why: String| Error::Invalid(format!("chord {accelerator:?}: {why}"));
    let trimmed = accelerator.trim();
    if trimmed.is_empty() {
        return Err(invalid("is empty".into()));
    }
    let mut modifiers = ChordModifiers::default();
    let mut key = None;
    for raw in trimmed.split('+') {
        let token = raw.trim();
        if token.is_empty() {
            return Err(invalid(
                "has an empty part (use `=` or `Equal` for the plus key)".into(),
            ));
        }
        if let Some(m) = parse_modifier(token) {
            let slot = match (m, platform) {
                (ModToken::Control, _) => &mut modifiers.control,
                (ModToken::Alt, _) => &mut modifiers.alt,
                (ModToken::Shift, _) => &mut modifiers.shift,
                (ModToken::Super, _) | (ModToken::CmdOrCtrl, Platform::MacOs) => {
                    &mut modifiers.super_key
                }
                (ModToken::CmdOrCtrl, _) => &mut modifiers.control,
            };
            if std::mem::replace(slot, true) {
                return Err(invalid(format!("repeats the modifier {token:?}")));
            }
            continue;
        }
        let Some(k) = ChordKey::parse(token) else {
            return Err(invalid(format!("unknown key or modifier {token:?}")));
        };
        if key.replace(k).is_some() {
            return Err(invalid("has more than one non-modifier key".into()));
        }
    }
    let Some(key) = key else {
        return Err(invalid(
            "has no key (a modifier alone is a double tap, not a chord)".into(),
        ));
    };
    let steals_typing = !(modifiers.control || modifiers.alt || modifiers.super_key);
    if steals_typing && !matches!(key, ChordKey::Function(_)) {
        return Err(invalid(
            "needs Ctrl, Alt or Cmd/Super (without one it would swallow ordinary typing)".into(),
        ));
    }
    Ok(Chord { modifiers, key })
}

impl Chord {
    /// The accelerator in this platform's conventional names and order.
    pub fn to_accelerator(&self) -> String {
        self.accelerator_for(Platform::current())
    }

    /// The accelerator for `platform`: `Ctrl+Option+Shift+Cmd+K` on macOS (Apple's order),
    /// `Ctrl+Alt+Shift+Win+K` on Windows and `Ctrl+Alt+Shift+Super+K` on Linux. Parsing the
    /// result gives back the same chord.
    pub fn accelerator_for(&self, platform: Platform) -> String {
        let m = self.modifiers;
        let (alt, sup) = match platform {
            Platform::MacOs => ("Option", "Cmd"),
            Platform::Windows => ("Alt", "Win"),
            Platform::Linux => ("Alt", "Super"),
        };
        let mut parts: Vec<String> = [
            (m.control, "Ctrl"),
            (m.alt, alt),
            (m.shift, "Shift"),
            (m.super_key, sup),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| n.to_string())
        .collect();
        parts.push(self.key.name());
        parts.join("+")
    }

    /// The `global-hotkey` form (`control+alt+Space`, `super+KeyK`).
    pub(crate) fn global_hotkey_string(&self) -> String {
        let m = self.modifiers;
        let mut parts: Vec<String> = [
            (m.control, "control"),
            (m.alt, "alt"),
            (m.shift, "shift"),
            (m.super_key, "super"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| n.to_string())
        .collect();
        parts.push(self.key.w3c_code());
        parts.join("+")
    }

    /// The trigger string for the XDG GlobalShortcuts portal (`CTRL+ALT+space`), following the
    /// XDG "shortcuts" specification.
    pub fn portal_trigger(&self) -> String {
        let m = self.modifiers;
        let mut parts: Vec<String> = [
            (m.control, "CTRL"),
            (m.alt, "ALT"),
            (m.shift, "SHIFT"),
            (m.super_key, "LOGO"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| n.to_string())
        .collect();
        parts.push(self.key.xkb_name());
        parts.join("+")
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_accelerator())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_keys() -> Vec<ChordKey> {
        let mut keys: Vec<_> = ('A'..='Z').map(ChordKey::Letter).collect();
        keys.extend((0..=9).map(ChordKey::Digit));
        keys.extend((1..=24).map(ChordKey::Function));
        keys.extend(NAMED_KEYS.iter().map(|n| n.0));
        keys
    }

    #[test]
    fn parses_the_common_chord() {
        let c = parse_chord_for("Ctrl+Alt+Space", Platform::Linux).unwrap();
        assert_eq!(
            c,
            Chord {
                modifiers: ChordModifiers {
                    control: true,
                    alt: true,
                    ..Default::default()
                },
                key: ChordKey::Space,
            }
        );
        assert_eq!(c.accelerator_for(Platform::Linux), "Ctrl+Alt+Space");
        assert_eq!(c.accelerator_for(Platform::MacOs), "Ctrl+Option+Space");
        assert_eq!(c.accelerator_for(Platform::Windows), "Ctrl+Alt+Space");
        assert_eq!(c.portal_trigger(), "CTRL+ALT+space");
        assert_eq!(c.global_hotkey_string(), "control+alt+Space");
    }

    #[test]
    fn aliases_case_and_spacing_normalise() {
        let expect = parse_chord_for("Ctrl+Alt+Shift+Cmd+K", Platform::MacOs).unwrap();
        for s in [
            "control + option + shift + command + k",
            "CTRL+ALT+SHIFT+SUPER+K",
            "⌃+⌥+⇧+⌘+k",
            "ctl+opt+shift+win+KeyK",
            "Meta+Shift+Alt+Ctrl+k",
        ] {
            assert_eq!(parse_chord_for(s, Platform::MacOs).unwrap(), expect, "{s}");
        }
        assert_eq!(
            expect.accelerator_for(Platform::MacOs),
            "Ctrl+Option+Shift+Cmd+K"
        );
        assert_eq!(
            expect.accelerator_for(Platform::Windows),
            "Ctrl+Alt+Shift+Win+K"
        );
        assert_eq!(
            expect.accelerator_for(Platform::Linux),
            "Ctrl+Alt+Shift+Super+K"
        );
        assert_eq!(expect.portal_trigger(), "CTRL+ALT+SHIFT+LOGO+k");
    }

    #[test]
    fn cmd_or_ctrl_depends_on_platform() {
        let mac = parse_chord_for("CmdOrCtrl+Shift+D", Platform::MacOs).unwrap();
        assert!(mac.modifiers.super_key && !mac.modifiers.control);
        let win = parse_chord_for("CommandOrControl+Shift+D", Platform::Windows).unwrap();
        assert!(win.modifiers.control && !win.modifiers.super_key);
        assert!(parse_chord_for("CmdOrCtrl+Ctrl+D", Platform::Linux).is_err());
    }

    #[test]
    fn named_and_punctuation_keys() {
        let cases = [
            ("Ctrl+Enter", ChordKey::Enter),
            ("Ctrl+Return", ChordKey::Enter),
            ("Ctrl+Esc", ChordKey::Escape),
            ("Alt+PgDn", ChordKey::PageDown),
            ("Alt+ArrowUp", ChordKey::Up),
            ("Ctrl+/", ChordKey::Slash),
            ("Ctrl+=", ChordKey::Equal),
            ("Ctrl+Minus", ChordKey::Minus),
            ("Ctrl+\\", ChordKey::Backslash),
            ("Ctrl+`", ChordKey::Backquote),
            ("Ctrl+[", ChordKey::BracketLeft),
            ("Ctrl+Digit7", ChordKey::Digit(7)),
            ("Ctrl+7", ChordKey::Digit(7)),
            ("Ctrl+f12", ChordKey::Function(12)),
        ];
        for (s, key) in cases {
            assert_eq!(parse_chord_for(s, Platform::Linux).unwrap().key, key, "{s}");
        }
    }

    #[test]
    fn bare_function_keys_are_allowed() {
        let c = parse_chord_for("F13", Platform::Linux).unwrap();
        assert!(c.modifiers.is_empty());
        assert!(parse_chord_for("Shift+F5", Platform::Linux).is_ok());
    }

    #[test]
    fn rejects_invalid_chords() {
        for s in [
            "",
            "   ",
            "Ctrl+",
            "+Space",
            "Ctrl++Space",
            "Ctrl+Ctrl+Space",
            "Ctrl+Control+Space",
            "Ctrl+Alt",
            "Shift",
            "Ctrl+A+B",
            "Ctrl+Hyper+A",
            "Ctrl+F25",
            "Ctrl+F0",
            "Ctrl+Key",
            "Ctrl+KeyAB",
            "Ctrl+é",
            "A",
            "Space",
            "Shift+A",
        ] {
            let err = parse_chord_for(s, Platform::Linux).unwrap_err();
            assert!(matches!(err, Error::Invalid(_)), "{s}: {err}");
        }
    }

    #[test]
    fn every_key_round_trips_on_every_platform() {
        let mods = ChordModifiers {
            control: true,
            alt: true,
            shift: true,
            super_key: true,
        };
        for platform in [Platform::MacOs, Platform::Windows, Platform::Linux] {
            for key in all_keys() {
                let c = Chord {
                    modifiers: mods,
                    key,
                };
                let text = c.accelerator_for(platform);
                assert_eq!(parse_chord_for(&text, platform).unwrap(), c, "{text}");
            }
        }
    }

    #[test]
    fn every_key_has_codes_for_the_backends() {
        for key in all_keys() {
            assert!(!key.name().is_empty(), "{key:?}");
            assert!(!key.w3c_code().is_empty(), "{key:?}");
            assert!(!key.xkb_name().is_empty(), "{key:?}");
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    #[test]
    fn every_key_converts_to_a_global_hotkey() {
        use std::str::FromStr;
        let mods = ChordModifiers {
            control: true,
            shift: true,
            ..Default::default()
        };
        for key in all_keys() {
            let c = Chord {
                modifiers: mods,
                key,
            };
            global_hotkey::hotkey::HotKey::from_str(&c.global_hotkey_string())
                .unwrap_or_else(|e| panic!("{key:?}: {e}"));
        }
    }

    #[test]
    fn errors_name_the_input() {
        let err = parse_chord_for("Ctrl+Bogus", Platform::Linux)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Ctrl+Bogus") && err.contains("Bogus"), "{err}");
    }
}
