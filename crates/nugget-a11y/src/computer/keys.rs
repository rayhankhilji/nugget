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
