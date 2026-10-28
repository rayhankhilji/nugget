//! Named keys and chords for [`KeyInput::Keys`](super::KeyInput): `enter`, `ctrl+a`,
//! `shift+tab`, `cmd+shift+z`. Parsing is strict: unknown names are an error, never typed.

use serde::{Deserialize, Serialize};

/// Keys with a name. Platforms map each to their own key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedKey {
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Space,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// F1 to F12.
    F(u8),
}

/// A key: a named key or one printable character (for chords such as `ctrl+a`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Named(NamedKey),
    Char(char),
}

/// Modifier set. `meta` is Cmd on macOS and the Windows/Super key elsewhere.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

/// A key with modifiers held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyChord {
    pub key: Key,
    #[serde(default)]
    pub modifiers: Modifiers,
}

impl NamedKey {
    /// The canonical name, as [`KeyChord::parse`] reads it back.
    pub fn name(self) -> String {
        match self {
            NamedKey::Enter => "enter".into(),
            NamedKey::Tab => "tab".into(),
            NamedKey::Escape => "escape".into(),
            NamedKey::Backspace => "backspace".into(),
            NamedKey::Delete => "delete".into(),
            NamedKey::Space => "space".into(),
            NamedKey::Up => "up".into(),
            NamedKey::Down => "down".into(),
            NamedKey::Left => "left".into(),
            NamedKey::Right => "right".into(),
            NamedKey::Home => "home".into(),
            NamedKey::End => "end".into(),
            NamedKey::PageUp => "page_up".into(),
            NamedKey::PageDown => "page_down".into(),
            NamedKey::F(n) => format!("f{n}"),
        }
    }

    fn from_name(s: &str) -> Option<NamedKey> {
        Some(match s {
            "enter" | "return" => NamedKey::Enter,
            "tab" => NamedKey::Tab,
            "escape" | "esc" => NamedKey::Escape,
            "backspace" => NamedKey::Backspace,
            "delete" | "del" => NamedKey::Delete,
            "space" | "spacebar" => NamedKey::Space,
            "up" | "arrowup" | "arrow_up" => NamedKey::Up,
            "down" | "arrowdown" | "arrow_down" => NamedKey::Down,
            "left" | "arrowleft" | "arrow_left" => NamedKey::Left,
            "right" | "arrowright" | "arrow_right" => NamedKey::Right,
            "home" => NamedKey::Home,
            "end" => NamedKey::End,
            "pageup" | "page_up" | "pgup" => NamedKey::PageUp,
            "pagedown" | "page_down" | "pgdn" | "pgdown" => NamedKey::PageDown,
            f if f.len() >= 2 && f.starts_with('f') => {
                let n: u8 = f[1..].parse().ok()?;
                if !(1..=12).contains(&n) || f[1..].starts_with('0') {
                    return None;
                }
                NamedKey::F(n)
            }
            _ => return None,
        })
    }
}

/// Printable keys that have a name, so they can be written inside a chord or a list.
const CHAR_NAMES: &[(&str, char)] = &[
    ("plus", '+'),
    ("minus", '-'),
    ("comma", ','),
    ("period", '.'),
    ("slash", '/'),
];

impl Key {
    fn parse(s: &str) -> Option<Key> {
        if let Some(k) = NamedKey::from_name(s) {
            return Some(Key::Named(k));
        }
        if let Some(&(_, c)) = CHAR_NAMES.iter().find(|(n, _)| *n == s) {
            return Some(Key::Char(c));
        }
        let mut chars = s.chars();
        let c = chars.next()?;
        if chars.next().is_some() || c.is_whitespace() || c.is_control() {
            return None;
        }
        Some(Key::Char(c))
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Named(k) => f.write_str(&k.name()),
            Key::Char(c) => match CHAR_NAMES.iter().find(|(_, x)| x == c) {
                Some((n, _)) => f.write_str(n),
                None => write!(f, "{c}"),
            },
        }
    }
}

fn invalid(msg: String) -> nugget_core::Error {
    nugget_core::Error::Invalid(msg)
}

impl KeyChord {
    /// A key without modifiers.
    pub fn key(key: Key) -> KeyChord {
        KeyChord {
            key,
            modifiers: Modifiers::default(),
        }
    }

    /// Parse `enter`, `a`, `f5`, `ctrl+shift+tab`, `cmd+z`. Case-insensitive; modifiers in any
    /// order (`cmd`/`command`/`super`/`win` are `meta`, `option` is `alt`). Exactly one
    /// non-modifier key. Unknown names, repeated modifiers and bare modifiers are refused.
    pub fn parse(s: &str) -> nugget_core::Result<KeyChord> {
        let lower = s.trim().to_lowercase();
        if lower.is_empty() {
            return Err(invalid("empty key".into()));
        }
        // `ctrl++` and a bare `+` mean the plus key.
        let (mods_part, key_part) = if lower == "+" {
            ("", "+")
        } else if let Some(m) = lower.strip_suffix("++") {
            (m, "+")
        } else {
            match lower.rsplit_once('+') {
                Some((m, k)) => (m, k),
                None => ("", lower.as_str()),
            }
        };
        let mut modifiers = Modifiers::default();
        if !mods_part.is_empty() {
            for m in mods_part.split('+') {
                let slot = match m.trim() {
                    "ctrl" | "control" | "ctl" => &mut modifiers.ctrl,
                    "alt" | "option" | "opt" => &mut modifiers.alt,
                    "shift" => &mut modifiers.shift,
                    "meta" | "cmd" | "command" | "super" | "win" | "windows" => &mut modifiers.meta,
                    other => return Err(invalid(format!("unknown modifier {other:?} in {s:?}"))),
                };
                if *slot {
                    return Err(invalid(format!("repeated modifier in {s:?}")));
                }
                *slot = true;
            }
        }
        let key_part = key_part.trim();
        if matches!(
            key_part,
            "ctrl" | "control" | "alt" | "option" | "shift" | "meta" | "cmd" | "command" | "super"
        ) {
            return Err(invalid(format!("{s:?} has no key besides modifiers")));
        }
        let key = Key::parse(key_part).ok_or_else(|| invalid(format!("unknown key {s:?}")))?;
        Ok(KeyChord { key, modifiers })
    }

    /// Parse a list of chords separated by commas or whitespace: `"ctrl+a, backspace"`,
    /// `"tab tab enter"`. Empty input is refused.
    pub fn parse_list(s: &str) -> nugget_core::Result<Vec<KeyChord>> {
        let out = s
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .map(KeyChord::parse)
            .collect::<nugget_core::Result<Vec<_>>>()?;
        if out.is_empty() {
            return Err(invalid("no keys given".into()));
        }
        Ok(out)
    }
}

impl std::str::FromStr for KeyChord {
    type Err = nugget_core::Error;
    fn from_str(s: &str) -> nugget_core::Result<Self> {
        KeyChord::parse(s)
    }
}

impl std::fmt::Display for KeyChord {
    /// Canonical form: modifiers as `ctrl+alt+shift+meta`, then the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = self.modifiers;
        for (on, name) in [
            (m.ctrl, "ctrl+"),
            (m.alt, "alt+"),
            (m.shift, "shift+"),
            (m.meta, "meta+"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        write!(f, "{}", self.key)
    }
}
