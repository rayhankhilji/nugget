//! Platform-independent detectors driven by abstract input events.
//!
//! Backends translate OS events into [`InputEvent`]s through [`InputEvent::from_key`], which is
//! where every key identity other than a modifier (and Escape, when a kill switch asks for it)
//! is dropped. Nothing downstream ever sees, stores or logs which other key was pressed.

use crate::Modifier;

/// What a backend knows about a key, after classification. [`KeyClass::Other`] carries no
/// identity on purpose: a backend maps every non-modifier key to it before anything else runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyClass {
    Modifier(Modifier),
    Escape,
    Other,
}

/// One abstract input event. Timestamps are passed alongside, in milliseconds on a monotonic
/// clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputEvent {
    ModifierDown(Modifier),
    ModifierUp(Modifier),
    /// Escape pressed. Only produced when the consumer observes Escape (the kill switch).
    EscapeDown,
    EscapeUp,
    /// Some other key was pressed. Which key is deliberately not represented.
    OtherKey,
    /// A mouse button was pressed (scroll wheels are not buttons).
    MouseButton,
}

impl InputEvent {
    /// Turn a classified key transition into an event, discarding everything a detector does not
    /// need. Escape is reported as Escape only when `observe_escape` is set; otherwise it is just
    /// another key. Releases of other keys are dropped: only presses can spoil a tap.
    pub fn from_key(key: KeyClass, down: bool, observe_escape: bool) -> Option<Self> {
        match (key, down) {
            (KeyClass::Modifier(m), true) => Some(Self::ModifierDown(m)),
            (KeyClass::Modifier(m), false) => Some(Self::ModifierUp(m)),
            (KeyClass::Escape, true) if observe_escape => Some(Self::EscapeDown),
            (KeyClass::Escape, false) if observe_escape => Some(Self::EscapeUp),
            (KeyClass::Escape | KeyClass::Other, true) => Some(Self::OtherKey),
            (KeyClass::Escape | KeyClass::Other, false) => None,
        }
    }
}

const fn bit(m: Modifier) -> u8 {
    1 << (m as u8)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TapState {
    Idle,
    /// The target went down cleanly at `at`.
    FirstDown {
        at: u64,
    },
    /// One clean tap finished at `at`.
    FirstUp {
        at: u64,
    },
    /// The second press of a candidate double tap went down at `at`.
    SecondDown {
        at: u64,
    },
    /// Just fired; taps continuing the same burst (last activity at `last`) are swallowed.
    Cooldown {
        last: u64,
    },
}

/// Detects a double tap of one modifier.
///
/// A **tap** is a press and release of the target modifier, held at most `window_ms`, with no
/// other key, modifier or mouse button pressed from the press to the release, and no other
/// modifier already held at the press. A **double tap** is two taps where the second press comes
/// at most `window_ms` after the first release. It fires on the second release, so a modifier
/// that turns out to be part of a shortcut never fires.
///
/// After firing, further taps in the same burst (each within `window_ms` of the previous one) are
/// swallowed, so a triple or quadruple tap fires once. Auto-repeated presses while the key is held
/// are ignored. Left and right modifiers are different keys.
#[derive(Debug, Clone)]
pub struct TapDetector {
    target: Modifier,
    window_ms: u64,
    max_hold_ms: u64,
    state: TapState,
    target_down: bool,
    others_down: u8,
}

impl TapDetector {
    pub fn new(target: Modifier, window_ms: u32) -> Self {
        Self {
            target,
            window_ms: u64::from(window_ms),
            max_hold_ms: u64::from(window_ms),
            state: TapState::Idle,
            target_down: false,
            others_down: 0,
        }
    }

    /// Feed one event at `now_ms`; returns `true` when the double tap fires.
    pub fn on_event(&mut self, event: InputEvent, now_ms: u64) -> bool {
        match event {
            InputEvent::ModifierDown(m) if m == self.target => self.target_pressed(now_ms),
            InputEvent::ModifierUp(m) if m == self.target => return self.target_released(now_ms),
            InputEvent::ModifierDown(m) => {
                self.others_down |= bit(m);
                self.state = TapState::Idle;
            }
            InputEvent::ModifierUp(m) => self.others_down &= !bit(m),
            InputEvent::EscapeDown | InputEvent::OtherKey | InputEvent::MouseButton => {
                self.state = TapState::Idle;
            }
            InputEvent::EscapeUp => {}
        }
        false
    }

    fn target_pressed(&mut self, now: u64) {
        if self.target_down {
            return; // auto-repeat while held
        }
        self.target_down = true;
        if self.others_down != 0 {
            self.state = TapState::Idle;
            return;
        }
        self.state = match self.state {
            TapState::FirstUp { at } if now.saturating_sub(at) <= self.window_ms => {
                TapState::SecondDown { at: now }
            }
            TapState::Cooldown { last } if now.saturating_sub(last) <= self.window_ms => {
                TapState::Cooldown { last: now }
            }
            _ => TapState::FirstDown { at: now },
        };
    }

    fn target_released(&mut self, now: u64) -> bool {
        if !self.target_down {
            return false; // a release we never saw go down
        }
        self.target_down = false;
        let held_ok = |at: u64| now.saturating_sub(at) <= self.max_hold_ms;
        match self.state {
            TapState::FirstDown { at } if held_ok(at) => self.state = TapState::FirstUp { at: now },
            TapState::SecondDown { at } if held_ok(at) => {
                self.state = TapState::Cooldown { last: now };
                return true;
            }
            TapState::Cooldown { .. } => self.state = TapState::Cooldown { last: now },
            _ => self.state = TapState::Idle,
        }
        false
    }
}

/// Detects Escape pressed `presses` times, each press at most `window_ms` after the previous
/// one, with no other key or mouse button in between. Modifiers are ignored. Fires on the last
/// press (a kill switch should not wait for the release); auto-repeat does not count.
#[derive(Debug, Clone)]
pub struct EscapeRepeatDetector {
    presses: u8,
    window_ms: u64,
    count: u8,
    last: u64,
    down: bool,
}

impl EscapeRepeatDetector {
    pub fn new(presses: u8, window_ms: u32) -> Self {
        Self {
            presses: presses.max(1),
            window_ms: u64::from(window_ms),
            count: 0,
            last: 0,
            down: false,
        }
    }

    /// Feed one event at `now_ms`; returns `true` when the sequence completes.
    pub fn on_event(&mut self, event: InputEvent, now_ms: u64) -> bool {
        match event {
            InputEvent::EscapeDown => {
                if self.down {
                    return false;
                }
                self.down = true;
                if self.count > 0 && now_ms.saturating_sub(self.last) <= self.window_ms {
                    self.count += 1;
                } else {
                    self.count = 1;
                }
                self.last = now_ms;
                if self.count >= self.presses {
                    self.count = 0;
                    return true;
                }
            }
            InputEvent::EscapeUp => self.down = false,
            InputEvent::OtherKey | InputEvent::MouseButton => self.count = 0,
            InputEvent::ModifierDown(_) | InputEvent::ModifierUp(_) => {}
        }
        false
    }
}

/// Either detector, as a backend runs it.
#[derive(Debug, Clone)]
pub(crate) enum Detector {
    Tap(TapDetector),
    Escape(EscapeRepeatDetector),
}

impl Detector {
    pub(crate) fn observes_escape(&self) -> bool {
        matches!(self, Self::Escape(_))
    }

    pub(crate) fn on_event(&mut self, event: InputEvent, now_ms: u64) -> bool {
        match self {
            Self::Tap(d) => d.on_event(event, now_ms),
            Self::Escape(d) => d.on_event(event, now_ms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use InputEvent::*;
    use Modifier::*;

    const W: u32 = 350;

    fn modifier(name: &str) -> Modifier {
        match name {
            "LCmd" => LeftCommand,
            "RCmd" => RightCommand,
            "LCtl" => LeftControl,
            "RCtl" => RightControl,
            "LOpt" => LeftOption,
            "ROpt" => RightOption,
            "LSh" => LeftShift,
            "RSh" => RightShift,
            _ => panic!("unknown modifier {name}"),
        }
    }

    /// Events from a spec such as `RCmd@0 key@40 LSh+100 LSh-160`: `<key>+<ms>` presses,
    /// `<key>-<ms>` releases, `<key>@<ms>` taps (released 60 ms later, Escape 10 ms later);
    /// `key` is some other key and `click` a mouse button.
    fn events(spec: &str) -> Vec<(u64, InputEvent)> {
        let mut out = Vec::new();
        for tok in spec.split_whitespace() {
            let (what, rest) = tok.split_at(tok.find(['@', '+', '-']).expect(tok));
            let t: u64 = rest[1..].parse().expect(tok);
            match (what, &rest[..1]) {
                ("key", _) => out.push((t, OtherKey)),
                ("click", _) => out.push((t, MouseButton)),
                ("esc", "+") => out.push((t, EscapeDown)),
                ("esc", "-") => out.push((t, EscapeUp)),
                ("esc", _) => out.extend([(t, EscapeDown), (t + 10, EscapeUp)]),
                (m, "+") => out.push((t, ModifierDown(modifier(m)))),
                (m, "-") => out.push((t, ModifierUp(modifier(m)))),
                (m, _) => out.extend([
                    (t, ModifierDown(modifier(m))),
                    (t + 60, ModifierUp(modifier(m))),
                ]),
            }
        }
        out
    }

    /// `target: events => fires`.
    fn check_tap(case: &str) {
        let (target, rest) = case.split_once(':').unwrap();
        let (spec, want) = rest.split_once("=>").unwrap();
        let mut d = TapDetector::new(modifier(target.trim()), W);
        let got = events(spec)
            .iter()
            .filter(|(t, e)| d.on_event(*e, *t))
            .count();
        assert_eq!(got, want.trim().parse::<usize>().unwrap(), "{case}");
    }

    #[test]
    fn tap_detector_cases() {
        for case in [
            "RCmd: RCmd@0 RCmd@200 => 1",
            // First release at 60: a second press at 60 + 350 is in, one ms later is out.
            "LSh: LSh@0 LSh@410 => 1",
            "LSh: LSh@0 LSh@411 => 0",
            "RCmd: RCmd@0 RCmd@1000 => 0",
            // A late tap becomes the first of a new pair.
            "RCmd: RCmd@0 RCmd@1000 RCmd@1200 => 1",
            // Part of a shortcut (Cmd+C), on either press, or a key in between.
            "LCmd: LCmd+0 key@40 LCmd-90 LCmd@150 => 0",
            "LCmd: LCmd@0 LCmd+150 key@180 LCmd-210 => 0",
            "LCmd: LCmd@0 key@100 LCmd@150 => 0",
            // A mouse click (Cmd-click) during or between the taps.
            "LCmd: LCmd+0 click@30 LCmd-60 LCmd@150 => 0",
            "LCmd: LCmd@0 click@100 LCmd@150 => 0",
            // Escape is just another key for a tap.
            "LSh: LSh@0 esc@100 LSh@150 => 0",
            // Ctrl+Shift twice is a combination, not a tap of Shift; so is another modifier
            // pressed during the tap. Once it is released, taps work again.
            "LSh: LCtl+0 LSh@10 LSh@100 LCtl-170 => 0",
            "LSh: LSh+0 LOpt+20 LOpt-40 LSh-60 LSh@150 => 0",
            "LSh: LCtl@0 LSh@100 LSh@250 => 1",
            // Held too long, first or second press.
            "ROpt: ROpt+0 ROpt-600 ROpt@700 => 0",
            "ROpt: ROpt@0 ROpt+150 ROpt-900 => 0",
            // An auto-repeated press inside a quick tap still counts as one press.
            "RCtl: RCtl+0 RCtl@30 RCtl@150 => 1",
            // Triple and quadruple taps fire once; a later double fires again.
            "RCmd: RCmd@0 RCmd@150 RCmd@300 => 1",
            "RCmd: RCmd@0 RCmd@150 RCmd@300 RCmd@450 RCmd@2000 RCmd@2150 => 2",
            // Left and right are different keys.
            "RCmd: LCmd@0 RCmd@150 => 0",
            "LCmd: LCmd@0 RCmd@150 => 0",
            // A release whose press was never seen (the listener started mid-hold).
            "LSh: LSh-0 LSh@50 LSh@200 => 1",
            "LSh: LSh@0 LSh-80 LSh@150 => 1",
            // The clock going backwards does not panic.
            "LSh: LSh+1000 LSh-900 LSh+800 LSh-700 => 1",
        ] {
            check_tap(case);
        }
        // Windows and X11 repeat modifier presses while held: still one long hold.
        let repeats: String = (1..20).map(|i| format!(" RCtl+{}", i * 30)).collect();
        check_tap(&format!("RCtl: RCtl+0{repeats} RCtl-800 RCtl@900 => 0"));
        // Every modifier is detected only on its own side.
        for target in Modifier::ALL {
            for pressed in Modifier::ALL {
                let mut d = TapDetector::new(target, W);
                let taps = [(0, true), (60, false), (150, true), (210, false)];
                let got = taps
                    .iter()
                    .filter(|&&(t, down)| {
                        let e = if down {
                            ModifierDown(pressed)
                        } else {
                            ModifierUp(pressed)
                        };
                        d.on_event(e, t)
                    })
                    .count();
                assert_eq!(
                    got,
                    usize::from(target == pressed),
                    "{target:?} {pressed:?}"
                );
            }
        }
    }

    #[test]
    fn fires_on_second_release_not_press() {
        let mut d = TapDetector::new(RightCommand, W);
        assert!(!d.on_event(ModifierDown(RightCommand), 0));
        assert!(!d.on_event(ModifierUp(RightCommand), 60));
        assert!(!d.on_event(ModifierDown(RightCommand), 150));
        assert!(d.on_event(ModifierUp(RightCommand), 210));
    }

    #[test]
    fn from_key_discards_non_modifier_key_identities() {
        // Every non-modifier key maps to the same identity-free event, whatever it was.
        for class in [KeyClass::Other, KeyClass::Escape] {
            assert_eq!(InputEvent::from_key(class, true, false), Some(OtherKey));
            assert_eq!(InputEvent::from_key(class, false, false), None);
        }
        let k = InputEvent::from_key;
        assert_eq!(k(KeyClass::Escape, true, true), Some(EscapeDown));
        assert_eq!(k(KeyClass::Escape, false, true), Some(EscapeUp));
        assert_eq!(k(KeyClass::Other, true, true), Some(OtherKey));
        let shift = KeyClass::Modifier(LeftShift);
        assert_eq!(k(shift, true, false), Some(ModifierDown(LeftShift)));
    }

    #[test]
    fn detector_state_holds_no_key_identity() {
        // Two runs that differ only in which other keys were typed leave identical state, so
        // nothing about those keys was kept.
        let drive = |skip| {
            let mut d = TapDetector::new(LeftShift, W);
            let mut t = 0;
            let keys = crate::keymap::tests::all_windows_non_modifier_vks()
                .skip(skip)
                .take(40)
                .filter_map(|vk| crate::keymap::windows_vk(vk, 0));
            for k in keys {
                for down in [true, false] {
                    if let Some(e) = InputEvent::from_key(k, down, false) {
                        d.on_event(e, t);
                    }
                    t += 5;
                }
            }
            format!("{d:?}")
        };
        assert_eq!(drive(0), drive(40));
    }

    #[test]
    fn escape_repeat_cases() {
        for (spec, want) in [
            ("esc@0 esc@200 esc@400", 1),
            ("esc@0 esc@100 esc@200 esc@300 esc@400 esc@500", 2),
            ("esc@0 esc@700 esc@800", 0),
            ("esc@0 esc@100 key@150 esc@200", 0),
            // Auto-repeat while held does not count.
            ("esc+0 esc+30 esc+60 esc+90", 0),
            // Modifiers are ignored.
            ("esc@0 LSh+20 esc@100 LSh-120 esc@200", 1),
        ] {
            let mut d = EscapeRepeatDetector::new(3, 500);
            let got = events(spec)
                .iter()
                .filter(|(t, e)| d.on_event(*e, *t))
                .count();
            assert_eq!(got, want, "{spec}");
        }
        // It fires on the third press, before the release.
        let mut d = EscapeRepeatDetector::new(3, 500);
        assert!(!d.on_event(EscapeDown, 0));
        d.on_event(EscapeUp, 10);
        assert!(!d.on_event(EscapeDown, 100));
        d.on_event(EscapeUp, 110);
        assert!(d.on_event(EscapeDown, 200));
    }
}
