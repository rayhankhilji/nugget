//! Key synthesis for computer use: X keysyms for text and named keys, typed with XTest on X11.
//! A character the keymap lacks is typed by remapping a spare keycode for the moment, as other
//! X input tools do; the mapping is restored afterwards.

use std::time::Duration;

use nugget_core::{Error, Result};
use x11rb::CURRENT_TIME;
use x11rb::connection::{Connection as _, RequestConnection as _};
use x11rb::protocol::xproto::{ConnectionExt as _, Keycode, Window};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use super::x11::{KEY_PRESS, KEY_RELEASE, connect, xerr};
use crate::computer::{Key, KeyChord, Modifiers, NamedKey};

pub(super) const XK_SHIFT_L: u32 = 0xffe1;
pub(super) const XK_CONTROL_L: u32 = 0xffe3;
pub(super) const XK_ALT_L: u32 = 0xffe9;
pub(super) const XK_SUPER_L: u32 = 0xffeb;
/// Time for clients to pick up a keymap change before and after a remapped key.
const REMAP_SETTLE: Duration = Duration::from_millis(30);

/// The keysym that types `c`: Latin-1 directly, everything else as a Unicode keysym.
pub(super) fn char_keysym(c: char) -> u32 {
    match c {
        '\n' => named_keysym(NamedKey::Enter),
        '\t' => named_keysym(NamedKey::Tab),
        '\u{20}'..='\u{7e}' | '\u{a0}'..='\u{ff}' => c as u32,
        _ => 0x0100_0000 | c as u32,
    }
}

pub(super) fn named_keysym(k: NamedKey) -> u32 {
    match k {
        NamedKey::Enter => 0xff0d,
        NamedKey::Tab => 0xff09,
        NamedKey::Escape => 0xff1b,
        NamedKey::Backspace => 0xff08,
        NamedKey::Delete => 0xffff,
        NamedKey::Space => 0x0020,
        NamedKey::Left => 0xff51,
        NamedKey::Up => 0xff52,
        NamedKey::Right => 0xff53,
        NamedKey::Down => 0xff54,
        NamedKey::Home => 0xff50,
        NamedKey::End => 0xff57,
        NamedKey::PageUp => 0xff55,
        NamedKey::PageDown => 0xff56,
        NamedKey::F(n) => 0xffbe + u32::from(n.clamp(1, 12)) - 1,
    }
}

/// The keysym of a chord's key. Letters are lowercased: `ctrl+A` is Ctrl+A, not Ctrl+Shift+A.
pub(super) fn chord_keysym(key: Key) -> u32 {
    match key {
        Key::Named(n) => named_keysym(n),
        Key::Char(c) if c.is_ascii_alphabetic() => char_keysym(c.to_ascii_lowercase()),
        Key::Char(c) => char_keysym(c),
    }
}

/// Modifier keysyms in press order.
pub(super) fn modifier_keysyms(m: Modifiers) -> Vec<u32> {
    [
        (m.ctrl, XK_CONTROL_L),
        (m.alt, XK_ALT_L),
        (m.shift, XK_SHIFT_L),
        (m.meta, XK_SUPER_L),
    ]
    .into_iter()
    .filter_map(|(on, sym)| on.then_some(sym))
    .collect()
}

/// Where `sym` sits in a keymap of `per` keysyms per keycode starting at `min`: the keycode and
/// whether Shift selects it. Unshifted positions win.
pub(super) fn find_in_map(map: &[u32], per: usize, min: u8, sym: u32) -> Option<(u8, bool)> {
    let per = per.max(1);
    for col in 0..per.min(2) {
        if let Some(i) = map.chunks(per).position(|syms| syms[col] == sym) {
            return Some((min.wrapping_add(i as u8), col == 1));
        }
    }
    None
}

/// An XTest keyboard on the current display.
pub(super) struct XKeyboard {
    conn: RustConnection,
    root: Window,
    min: u8,
    per: usize,
    map: Vec<u32>,
    /// A keycode with no keysyms, borrowed for characters the keymap lacks.
    spare: Option<(Keycode, bool)>,
}

impl XKeyboard {
    pub(super) fn new() -> Result<Self> {
        let (conn, screen) = connect()?;
        if conn
            .extension_information(x11rb::protocol::xtest::X11_EXTENSION_NAME)
            .map_err(xerr)?
            .is_none()
        {
            return Err(Error::Unsupported("the X server has no XTest".into()));
        }
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let root = setup.roots[screen].root;
        let reply = conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?;
        let per = usize::from(reply.keysyms_per_keycode.max(1));
        let spare = reply
            .keysyms
            .chunks(per)
            .rposition(|syms| syms.iter().all(|&s| s == 0))
            .map(|i| (min + i as u8, false));
        Ok(XKeyboard {
            conn,
            root,
            min,
            per,
            map: reply.keysyms,
            spare,
        })
    }

    fn fake(&self, ty: u8, key: Keycode) -> Result<()> {
        self.conn
            .xtest_fake_input(ty, key, CURRENT_TIME, self.root, 0, 0, 0)
            .map(drop)
            .map_err(xerr)
    }

    /// Round-trip to the server so every queued event has been delivered.
    fn sync(&self) -> Result<()> {
        self.conn
            .get_input_focus()
            .map_err(xerr)?
            .reply()
            .map(drop)
            .map_err(xerr)
    }

    fn keycode(&self, sym: u32) -> Option<(Keycode, bool)> {
        find_in_map(&self.map, self.per, self.min, sym)
    }

    fn set_spare(&mut self, sym: u32) -> Result<Keycode> {
        let (code, _) = self
            .spare
            .ok_or_else(|| Error::Unsupported("no spare keycode to type this character".into()))?;
        let syms = vec![sym; self.per];
        self.conn
            .change_keyboard_mapping(1, code, self.per as u8, &syms)
            .map_err(xerr)?;
        let i = usize::from(code - self.min) * self.per;
        self.map[i..i + self.per].copy_from_slice(&syms);
        self.spare = Some((code, true));
        self.sync()?;
        std::thread::sleep(REMAP_SETTLE);
        Ok(code)
    }

    fn restore_spare(&mut self) {
        if let Some((code, true)) = self.spare {
            std::thread::sleep(REMAP_SETTLE);
            let zeros = vec![0u32; self.per];
            let _ = self
                .conn
                .change_keyboard_mapping(1, code, self.per as u8, &zeros);
            let i = usize::from(code - self.min) * self.per;
            self.map[i..i + self.per].copy_from_slice(&zeros);
            self.spare = Some((code, false));
            let _ = self.sync();
        }
    }

    fn modifier(&self, sym: u32) -> Result<Keycode> {
        self.keycode(sym)
            .map(|(k, _)| k)
            .ok_or_else(|| Error::Unsupported(format!("no keycode for modifier {sym:#x}")))
    }

    /// Press `sym` with `mods` held, then release in reverse order.
    fn tap(&mut self, sym: u32, mods: &[u32]) -> Result<()> {
        let (code, shift) = match self.keycode(sym) {
            Some(k) => k,
            None => (self.set_spare(sym)?, false),
        };
        let mut held: Vec<Keycode> = mods
            .iter()
            .map(|&m| self.modifier(m))
            .collect::<Result<_>>()?;
        if shift && !mods.contains(&XK_SHIFT_L) {
            held.push(self.modifier(XK_SHIFT_L)?);
        }
        for &m in &held {
            self.fake(KEY_PRESS, m)?;
        }
        self.fake(KEY_PRESS, code)?;
        self.fake(KEY_RELEASE, code)?;
        for &m in held.iter().rev() {
            self.fake(KEY_RELEASE, m)?;
        }
        Ok(())
    }

    /// Type `text` character by character (`\r` dropped).
    pub(super) fn type_text(&mut self, text: &str) -> Result<()> {
        let r = text
            .chars()
            .filter(|&c| c != '\r')
            .try_for_each(|c| self.tap(char_keysym(c), &[]));
        self.restore_spare();
        self.sync()?;
        r
    }

    /// Press each chord in order.
    pub(super) fn chords(&mut self, chords: &[KeyChord]) -> Result<()> {
        let r = chords
            .iter()
            .try_for_each(|c| self.tap(chord_keysym(c.key), &modifier_keysyms(c.modifiers)));
        self.restore_spare();
        self.sync()?;
        r
    }
}

impl Drop for XKeyboard {
    fn drop(&mut self) {
        self.restore_spare();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms() {
        assert_eq!(char_keysym('a'), 0x61);
        assert_eq!(char_keysym('é'), 0xe9);
        assert_eq!(char_keysym('✓'), 0x0100_2713);
        assert_eq!(char_keysym('\n'), 0xff0d);
        assert_eq!(named_keysym(NamedKey::F(1)), 0xffbe);
        assert_eq!(named_keysym(NamedKey::F(12)), 0xffc9);
        assert_eq!(chord_keysym(Key::Char('A')), 0x61);
        let m = Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(modifier_keysyms(m), vec![XK_CONTROL_L, XK_SHIFT_L]);
    }

    #[test]
    fn keymap_lookup_prefers_unshifted() {
        // keycodes 8..: [a A] [1 !] [! x] [0 0]
        let map = [0x61, 0x41, 0x31, 0x21, 0x21, 0x78, 0, 0];
        assert_eq!(find_in_map(&map, 2, 8, 0x61), Some((8, false)));
        assert_eq!(find_in_map(&map, 2, 8, 0x41), Some((8, true)));
        assert_eq!(find_in_map(&map, 2, 8, 0x21), Some((10, false)));
        assert_eq!(find_in_map(&map, 2, 8, 0x78), Some((10, true)));
        assert_eq!(find_in_map(&map, 2, 8, 0x7a), None);
    }
}
