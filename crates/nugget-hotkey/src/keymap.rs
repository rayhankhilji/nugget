//! Pure key classification for every backend, so each OS mapping is unit-tested on any host.
//!
//! Each function answers only "which modifier is this, is it Escape, or is it some other key"
//! (the `*_tap_key` functions for the suggestion tap additionally recognise Tab and Right arrow).
//! The key code of an "other" key is never returned, stored or logged.

use crate::Modifier;
use crate::suggest::{Mods, TapKey};
use crate::tap::KeyClass;

/// Windows virtual-key code (from `KBDLLHOOKSTRUCT::vkCode`) to a key class. `scan_code` is used
/// only to drop the fake Left Ctrl that Windows injects before AltGr (scan code 0x21D), which
/// would otherwise make every AltGr press look like a Ctrl+Alt combination. `None` means "ignore
/// this event entirely".
pub fn windows_vk(vk: u32, scan_code: u32) -> Option<KeyClass> {
    const ALTGR_FAKE_LCTRL_SCAN: u32 = 0x21D;
    Some(match vk {
        0x5B => KeyClass::Modifier(Modifier::LeftCommand), // VK_LWIN
        0x5C => KeyClass::Modifier(Modifier::RightCommand), // VK_RWIN
        0xA0 => KeyClass::Modifier(Modifier::LeftShift),   // VK_LSHIFT
        0xA1 => KeyClass::Modifier(Modifier::RightShift),  // VK_RSHIFT
        0xA2 if scan_code == ALTGR_FAKE_LCTRL_SCAN => return None,
        0xA2 => KeyClass::Modifier(Modifier::LeftControl), // VK_LCONTROL
        0xA3 => KeyClass::Modifier(Modifier::RightControl), // VK_RCONTROL
        0xA4 => KeyClass::Modifier(Modifier::LeftOption),  // VK_LMENU
        0xA5 => KeyClass::Modifier(Modifier::RightOption), // VK_RMENU
        0x1B => KeyClass::Escape,                          // VK_ESCAPE
        _ => KeyClass::Other,
    })
}

/// macOS virtual key code (`NSEvent.keyCode`, `kVK_*`) to a key class.
pub fn mac_keycode(code: u16) -> KeyClass {
    match code {
        55 => KeyClass::Modifier(Modifier::LeftCommand),
        54 => KeyClass::Modifier(Modifier::RightCommand),
        56 => KeyClass::Modifier(Modifier::LeftShift),
        60 => KeyClass::Modifier(Modifier::RightShift),
        58 => KeyClass::Modifier(Modifier::LeftOption),
        61 => KeyClass::Modifier(Modifier::RightOption),
        59 => KeyClass::Modifier(Modifier::LeftControl),
        62 => KeyClass::Modifier(Modifier::RightControl),
        53 => KeyClass::Escape,
        _ => KeyClass::Other,
    }
}

/// Device-dependent modifier bits in `NSEvent.modifierFlags` (`NX_DEVICE*KEYMASK` in IOKit's
/// `IOLLEvent.h`). These are what tell left from right without Input Monitoring.
pub const MAC_DEVICE_BITS: [(u64, Modifier); 8] = [
    (0x0000_0001, Modifier::LeftControl),
    (0x0000_0002, Modifier::LeftShift),
    (0x0000_0004, Modifier::RightShift),
    (0x0000_0008, Modifier::LeftCommand),
    (0x0000_0010, Modifier::RightCommand),
    (0x0000_0020, Modifier::LeftOption),
    (0x0000_0040, Modifier::RightOption),
    (0x0000_2000, Modifier::RightControl),
];

/// The modifier transitions between two `flagsChanged` flag words, as `(modifier, is_down)`.
pub fn mac_flag_changes(previous: u64, current: u64) -> impl Iterator<Item = (Modifier, bool)> {
    MAC_DEVICE_BITS.into_iter().filter_map(move |(bit, m)| {
        let (was, is) = (previous & bit != 0, current & bit != 0);
        (was != is).then_some((m, is))
    })
}

/// X11 keysym (level 0 of a keycode) to a key class. Super is the Command key, and AltGr
/// (`ISO_Level3_Shift`, what the right Alt key produces on most non-US layouts) is right Option.
pub fn x11_keysym(keysym: u32) -> KeyClass {
    match keysym {
        0xffe1 => KeyClass::Modifier(Modifier::LeftShift), // Shift_L
        0xffe2 => KeyClass::Modifier(Modifier::RightShift), // Shift_R
        0xffe3 => KeyClass::Modifier(Modifier::LeftControl), // Control_L
        0xffe4 => KeyClass::Modifier(Modifier::RightControl), // Control_R
        0xffe9 => KeyClass::Modifier(Modifier::LeftOption), // Alt_L
        0xffea | 0xfe03 => KeyClass::Modifier(Modifier::RightOption), // Alt_R, ISO_Level3_Shift
        0xffeb => KeyClass::Modifier(Modifier::LeftCommand), // Super_L
        0xffec => KeyClass::Modifier(Modifier::RightCommand), // Super_R
        0xff1b => KeyClass::Escape,                        // Escape
        _ => KeyClass::Other,
    }
}

/// macOS virtual key code (`kVK_*`, from `kCGKeyboardEventKeycode`) to a suggestion-tap class.
pub fn mac_tap_key(code: u16) -> TapKey {
    match code {
        48 => TapKey::Tab,           // kVK_Tab
        53 => TapKey::Escape,        // kVK_Escape
        124 => TapKey::Right,        // kVK_RightArrow
        54..=63 => TapKey::Modifier, // Command, Shift, Caps Lock, Option, Control (both sides), Fn
        _ => TapKey::Other,
    }
}

/// The modifiers in a `CGEventFlags` word. Only Shift, Control, Option and Command count: Caps
/// Lock, Help, and the NumericPad and SecondaryFn bits (which macOS sets on every arrow key, so
/// also on ⌥→) are ignored, as are the device-dependent bits.
pub fn mac_mods(flags: u64) -> Mods {
    Mods {
        shift: flags & 0x0002_0000 != 0,   // kCGEventFlagMaskShift
        control: flags & 0x0004_0000 != 0, // kCGEventFlagMaskControl
        option: flags & 0x0008_0000 != 0,  // kCGEventFlagMaskAlternate
        command: flags & 0x0010_0000 != 0, // kCGEventFlagMaskCommand
    }
}

/// Windows virtual-key code (`KBDLLHOOKSTRUCT::vkCode`) to a suggestion-tap class. The generic
/// `VK_SHIFT`/`VK_CONTROL`/`VK_MENU` codes count as modifiers too.
pub fn windows_tap_key(vk: u32) -> TapKey {
    match vk {
        0x09 => TapKey::Tab,    // VK_TAB
        0x1B => TapKey::Escape, // VK_ESCAPE
        0x27 => TapKey::Right,  // VK_RIGHT
        0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5 => TapKey::Modifier,
        _ => TapKey::Other,
    }
}

/// X11 keysym (level 0 of a keycode) to a suggestion-tap class. `ISO_Left_Tab` (what some
/// keymaps put on Shift+Tab) is Tab; the Shift that produced it is tracked as a modifier, so
/// it still reads as Shift+Tab. `KP_Right` is not Right.
pub fn x11_tap_key(keysym: u32) -> TapKey {
    match keysym {
        0xff09 | 0xfe20 => TapKey::Tab,      // Tab, ISO_Left_Tab
        0xff1b => TapKey::Escape,            // Escape
        0xff53 => TapKey::Right,             // Right
        0xffe7 | 0xffe8 => TapKey::Modifier, // Meta_L, Meta_R
        _ if matches!(x11_keysym(keysym), KeyClass::Modifier(_)) => TapKey::Modifier,
        _ => TapKey::Other,
    }
}

/// The [`Mods`] held, from a set of held [`Modifier`]s (left or right).
pub fn mods_from_held(held: impl IntoIterator<Item = Modifier>) -> Mods {
    let mut mods = Mods::default();
    for m in held {
        match m {
            Modifier::LeftShift | Modifier::RightShift => mods.shift = true,
            Modifier::LeftControl | Modifier::RightControl => mods.control = true,
            Modifier::LeftOption | Modifier::RightOption => mods.option = true,
            Modifier::LeftCommand | Modifier::RightCommand => mods.command = true,
        }
    }
    mods
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const WIN_MODIFIER_VKS: [u32; 9] = [0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x1B];

    pub(crate) fn all_windows_non_modifier_vks() -> impl Iterator<Item = u32> {
        (0..=0xFFu32).filter(|vk| !WIN_MODIFIER_VKS.contains(vk))
    }

    #[test]
    fn windows_vks() {
        use Modifier::*;
        for (vk, scan, m) in [
            (0xA2, 0x1D, LeftControl),
            (0xA3, 0x1D, RightControl),
            (0xA4, 0x38, LeftOption),
            (0xA5, 0x38, RightOption),
            (0xA0, 0x2A, LeftShift),
            (0xA1, 0x36, RightShift),
            (0x5B, 0, LeftCommand),
            (0x5C, 0, RightCommand),
        ] {
            assert_eq!(windows_vk(vk, scan), Some(KeyClass::Modifier(m)), "{vk:#x}");
        }
        assert_eq!(windows_vk(0x1B, 1), Some(KeyClass::Escape));
        // AltGr's fake Left Ctrl is dropped.
        assert_eq!(windows_vk(0xA2, 0x21D), None);
        // Every other key carries no identity.
        for vk in all_windows_non_modifier_vks() {
            assert_eq!(windows_vk(vk, 0), Some(KeyClass::Other), "vk {vk:#x}");
        }
    }

    #[test]
    fn mac_keycodes() {
        assert_eq!(mac_keycode(54), KeyClass::Modifier(Modifier::RightCommand));
        assert_eq!(mac_keycode(55), KeyClass::Modifier(Modifier::LeftCommand));
        assert_eq!(mac_keycode(53), KeyClass::Escape);
        let special = [53, 54, 55, 56, 58, 59, 60, 61, 62];
        for code in (0..=u16::from(u8::MAX)).filter(|c| !special.contains(c)) {
            assert_eq!(mac_keycode(code), KeyClass::Other);
        }
    }

    #[test]
    fn mac_flag_diffs_tell_left_from_right() {
        let changes = |a, b| mac_flag_changes(a, b).collect::<Vec<_>>();
        // Right Command down: device bit 0x10 plus the device-independent Command bit.
        let down = 0x0010_0000 | 0x10;
        assert_eq!(changes(0, down), [(Modifier::RightCommand, true)]);
        assert_eq!(changes(down, 0), [(Modifier::RightCommand, false)]);
        // Left Command joins while right is held.
        assert_eq!(changes(down, down | 0x08), [(Modifier::LeftCommand, true)]);
        // Device-independent bits alone change nothing.
        assert!(changes(0, 0x0010_0000).is_empty());
        for (bit, m) in MAC_DEVICE_BITS {
            assert_eq!(changes(0, bit), [(m, true)]);
        }
    }

    #[test]
    fn x11_keysyms() {
        use Modifier::*;
        assert_eq!(x11_keysym(0xffe4), KeyClass::Modifier(RightControl));
        assert_eq!(x11_keysym(0xfe03), KeyClass::Modifier(RightOption)); // AltGr
        assert_eq!(x11_keysym(0xffec), KeyClass::Modifier(RightCommand));
        assert_eq!(x11_keysym(0xff1b), KeyClass::Escape);
        assert_eq!(x11_keysym(u32::from(b'a')), KeyClass::Other);
        assert_eq!(x11_keysym(0xff0d), KeyClass::Other); // Return
    }
    #[test]
    fn mac_tap_keys() {
        assert_eq!(mac_tap_key(48), TapKey::Tab);
        assert_eq!(mac_tap_key(53), TapKey::Escape);
        assert_eq!(mac_tap_key(124), TapKey::Right);
        for code in 54..=63 {
            assert_eq!(mac_tap_key(code), TapKey::Modifier, "{code}");
        }
        let special = [48, 53, 124, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63];
        for code in (0..=u16::from(u8::MAX)).filter(|c| !special.contains(c)) {
            assert_eq!(mac_tap_key(code), TapKey::Other, "{code}");
        }
    }

    #[test]
    fn mac_mods_ignore_lock_numpad_and_fn_bits() {
        assert_eq!(mac_mods(0), Mods::default());
        // ⌥→ as macOS reports it: Alternate plus NumericPad and SecondaryFn, and the
        // device-dependent left-Option bit.
        let option_right = 0x0008_0000 | 0x0020_0000 | 0x0080_0000 | 0x20;
        assert_eq!(
            mac_mods(option_right),
            Mods {
                option: true,
                ..Mods::default()
            }
        );
        // Caps Lock (AlphaShift), Help and NonCoalesced change nothing.
        assert_eq!(mac_mods(0x0001_0000 | 0x0040_0000 | 0x100), Mods::default());
        let all = mac_mods(0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000);
        assert!(all.shift && all.control && all.option && all.command);
        assert!(mac_mods(0x0002_0000).shift);
        assert!(mac_mods(0x0004_0000).control);
        assert!(mac_mods(0x0010_0000).command);
    }

    #[test]
    fn windows_tap_keys() {
        assert_eq!(windows_tap_key(0x09), TapKey::Tab);
        assert_eq!(windows_tap_key(0x1B), TapKey::Escape);
        assert_eq!(windows_tap_key(0x27), TapKey::Right);
        let modifiers = [
            0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5,
        ];
        for vk in modifiers {
            assert_eq!(windows_tap_key(vk), TapKey::Modifier, "{vk:#x}");
        }
        for vk in
            (0..=0xFFu32).filter(|vk| ![0x09, 0x1B, 0x27].contains(vk) && !modifiers.contains(vk))
        {
            assert_eq!(windows_tap_key(vk), TapKey::Other, "{vk:#x}");
        }
    }

    #[test]
    fn x11_tap_keys() {
        assert_eq!(x11_tap_key(0xff09), TapKey::Tab);
        assert_eq!(x11_tap_key(0xfe20), TapKey::Tab); // ISO_Left_Tab
        assert_eq!(x11_tap_key(0xff1b), TapKey::Escape);
        assert_eq!(x11_tap_key(0xff53), TapKey::Right);
        assert_eq!(x11_tap_key(0xff98), TapKey::Other); // KP_Right
        assert_eq!(x11_tap_key(0xff51), TapKey::Other); // Left
        assert_eq!(x11_tap_key(u32::from(b'a')), TapKey::Other);
        for sym in [
            0xffe1, 0xffe2, 0xffe3, 0xffe4, 0xffe7, 0xffe8, 0xffe9, 0xffea, 0xffeb, 0xffec, 0xfe03,
        ] {
            assert_eq!(x11_tap_key(sym), TapKey::Modifier, "{sym:#x}");
        }
    }

    #[test]
    fn mods_from_held_modifiers() {
        use Modifier::*;
        assert_eq!(mods_from_held([]), Mods::default());
        let m = mods_from_held([RightControl]);
        assert!(m.control && !m.shift && !m.option && !m.command);
        let m = mods_from_held([LeftShift, RightOption, LeftCommand]);
        assert!(m.shift && m.option && m.command && !m.control);
    }
}
