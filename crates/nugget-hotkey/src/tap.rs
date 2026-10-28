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

    pub fn target(&self) -> Modifier {
        self.target
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

    fn run(d: &mut TapDetector, events: &[(u64, InputEvent)]) -> usize {
        events.iter().filter(|(t, e)| d.on_event(*e, *t)).count()
    }

    fn tap(m: Modifier, at: u64) -> [(u64, InputEvent); 2] {
        [(at, ModifierDown(m)), (at + 60, ModifierUp(m))]
    }

    fn seq(parts: &[&[(u64, InputEvent)]]) -> Vec<(u64, InputEvent)> {
        parts.iter().flat_map(|p| p.iter().copied()).collect()
    }

    #[test]
    fn double_tap_within_window_fires() {
        let mut d = TapDetector::new(RightCommand, W);
        let events = seq(&[&tap(RightCommand, 0), &tap(RightCommand, 200)]);
        assert_eq!(run(&mut d, &events), 1);
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
    fn gap_exactly_at_window_fires_and_one_past_does_not() {
        let mut d = TapDetector::new(LeftShift, W);
        // First release at 60; second press at 60 + 350.
        let ok = seq(&[&tap(LeftShift, 0), &tap(LeftShift, 410)]);
        assert_eq!(run(&mut d, &ok), 1);
        let mut d = TapDetector::new(LeftShift, W);
        let late = seq(&[&tap(LeftShift, 0), &tap(LeftShift, 411)]);
        assert_eq!(run(&mut d, &late), 0);
    }

    #[test]
    fn slow_pair_does_not_fire() {
        let mut d = TapDetector::new(RightControl, W);
        let events = seq(&[&tap(RightControl, 0), &tap(RightControl, 1_000)]);
        assert_eq!(run(&mut d, &events), 0);
    }

    #[test]
    fn slow_pair_then_quick_tap_fires_from_the_second() {
        // The late tap becomes the first tap of a new pair.
        let mut d = TapDetector::new(RightControl, W);
        let events = seq(&[
            &tap(RightControl, 0),
            &tap(RightControl, 1_000),
            &tap(RightControl, 1_200),
        ]);
        assert_eq!(run(&mut d, &events), 1);
    }

    #[test]
    fn modifier_used_in_shortcut_does_not_fire() {
        // Cmd+C, then a clean tap: the first press was a shortcut, not a tap.
        let mut d = TapDetector::new(LeftCommand, W);
        let events = [
            (0, ModifierDown(LeftCommand)),
            (40, OtherKey),
            (90, ModifierUp(LeftCommand)),
            (150, ModifierDown(LeftCommand)),
            (210, ModifierUp(LeftCommand)),
        ];
        assert_eq!(run(&mut d, &events), 0);
    }

    #[test]
    fn shortcut_on_the_second_press_does_not_fire() {
        let mut d = TapDetector::new(LeftCommand, W);
        let events = [
            (0, ModifierDown(LeftCommand)),
            (60, ModifierUp(LeftCommand)),
            (150, ModifierDown(LeftCommand)),
            (180, OtherKey),
            (210, ModifierUp(LeftCommand)),
        ];
        assert_eq!(run(&mut d, &events), 0);
    }

    #[test]
    fn key_between_the_taps_does_not_fire() {
        let mut d = TapDetector::new(LeftCommand, W);
        let events = seq(&[
            &tap(LeftCommand, 0),
            &[(100, OtherKey)],
            &tap(LeftCommand, 150),
        ]);
        assert_eq!(run(&mut d, &events), 0);
    }

    #[test]
    fn mouse_click_spoils_the_tap() {
        // Cmd-click, for example.
        let mut d = TapDetector::new(LeftCommand, W);
        let events = [
            (0, ModifierDown(LeftCommand)),
            (30, MouseButton),
            (60, ModifierUp(LeftCommand)),
            (150, ModifierDown(LeftCommand)),
            (210, ModifierUp(LeftCommand)),
        ];
        assert_eq!(run(&mut d, &events), 0);
        let mut d = TapDetector::new(LeftCommand, W);
        let events = seq(&[
            &tap(LeftCommand, 0),
            &[(100, MouseButton)],
            &tap(LeftCommand, 150),
        ]);
        assert_eq!(run(&mut d, &events), 0);
    }

    #[test]
    fn modifier_combination_does_not_fire() {
        // Ctrl+Shift pressed together twice is a combination, not a tap of Shift.
        let mut d = TapDetector::new(LeftShift, W);
        let events = [
            (0, ModifierDown(LeftControl)),
            (10, ModifierDown(LeftShift)),
            (60, ModifierUp(LeftShift)),
            (100, ModifierDown(LeftShift)),
            (160, ModifierUp(LeftShift)),
            (170, ModifierUp(LeftControl)),
        ];
        assert_eq!(run(&mut d, &events), 0);
        // Another modifier pressed during the tap.
        let mut d = TapDetector::new(LeftShift, W);
        let events = [
            (0, ModifierDown(LeftShift)),
            (20, ModifierDown(LeftOption)),
            (40, ModifierUp(LeftOption)),
            (60, ModifierUp(LeftShift)),
            (150, ModifierDown(LeftShift)),
            (210, ModifierUp(LeftShift)),
        ];
        assert_eq!(run(&mut d, &events), 0);
    }

    #[test]
    fn works_again_after_another_modifier_is_released() {
        let mut d = TapDetector::new(LeftShift, W);
        let events = seq(&[
            &[
                (0, ModifierDown(LeftControl)),
                (50, ModifierUp(LeftControl)),
            ],
            &tap(LeftShift, 100),
            &tap(LeftShift, 250),
        ]);
        assert_eq!(run(&mut d, &events), 1);
    }

    #[test]
    fn holding_does_not_fire() {
        let mut d = TapDetector::new(RightOption, W);
        // Long first hold.
        let events = [
            (0, ModifierDown(RightOption)),
            (600, ModifierUp(RightOption)),
            (700, ModifierDown(RightOption)),
            (760, ModifierUp(RightOption)),
        ];
        assert_eq!(run(&mut d, &events), 0);
        // Clean first tap, long second hold.
        let mut d = TapDetector::new(RightOption, W);
        let events = [
            (0, ModifierDown(RightOption)),
            (60, ModifierUp(RightOption)),
            (150, ModifierDown(RightOption)),
            (900, ModifierUp(RightOption)),
        ];
        assert_eq!(run(&mut d, &events), 0);
    }

    #[test]
    fn auto_repeat_while_held_is_ignored() {
        // Windows and X11 repeat modifier presses while held; that is still one long hold.
        let mut d = TapDetector::new(RightControl, W);
        let mut events = vec![(0, ModifierDown(RightControl))];
        events.extend((1..20).map(|i| (i * 30, ModifierDown(RightControl))));
        events.push((800, ModifierUp(RightControl)));
        events.extend(tap(RightControl, 900));
        assert_eq!(run(&mut d, &events), 0);
        // A quick tap with one repeat in it still counts.
        let mut d = TapDetector::new(RightControl, W);
        let events = [
            (0, ModifierDown(RightControl)),
            (30, ModifierDown(RightControl)),
            (60, ModifierUp(RightControl)),
            (150, ModifierDown(RightControl)),
            (210, ModifierUp(RightControl)),
        ];
        assert_eq!(run(&mut d, &events), 1);
    }

    #[test]
    fn triple_tap_fires_once() {
        let mut d = TapDetector::new(RightCommand, W);
        let events = seq(&[
            &tap(RightCommand, 0),
            &tap(RightCommand, 150),
            &tap(RightCommand, 300),
        ]);
        assert_eq!(run(&mut d, &events), 1);
    }

    #[test]
    fn quadruple_tap_fires_once_and_a_later_double_fires_again() {
        let mut d = TapDetector::new(RightCommand, W);
        let events = seq(&[
            &tap(RightCommand, 0),
            &tap(RightCommand, 150),
            &tap(RightCommand, 300),
            &tap(RightCommand, 450),
            &tap(RightCommand, 2_000),
            &tap(RightCommand, 2_150),
        ]);
        assert_eq!(run(&mut d, &events), 2);
    }

    #[test]
    fn left_and_right_are_distinguished() {
        // Taps of the other side never count.
        let mut d = TapDetector::new(RightCommand, W);
        let events = seq(&[&tap(LeftCommand, 0), &tap(LeftCommand, 150)]);
        assert_eq!(run(&mut d, &events), 0);
        // Mixed left/right does not make a double tap of either.
        let mut d = TapDetector::new(RightCommand, W);
        let events = seq(&[&tap(LeftCommand, 0), &tap(RightCommand, 150)]);
        assert_eq!(run(&mut d, &events), 0);
        let mut d = TapDetector::new(LeftCommand, W);
        let events = seq(&[&tap(LeftCommand, 0), &tap(RightCommand, 150)]);
        assert_eq!(run(&mut d, &events), 0);
        // Every modifier is detected only on its own side.
        let all = [
            LeftCommand,
            RightCommand,
            LeftControl,
            RightControl,
            LeftOption,
            RightOption,
            LeftShift,
            RightShift,
        ];
        for target in all {
            for pressed in all {
                let mut d = TapDetector::new(target, W);
                let events = seq(&[&tap(pressed, 0), &tap(pressed, 150)]);
                assert_eq!(
                    run(&mut d, &events),
                    usize::from(target == pressed),
                    "{target:?} {pressed:?}"
                );
            }
        }
    }

    #[test]
    fn stray_release_is_ignored() {
        // A release whose press we never saw (the listener started mid-hold).
        let mut d = TapDetector::new(LeftShift, W);
        let events = seq(&[
            &[(0, ModifierUp(LeftShift))],
            &tap(LeftShift, 50),
            &tap(LeftShift, 200),
        ]);
        assert_eq!(run(&mut d, &events), 1);
        let mut d = TapDetector::new(LeftShift, W);
        let events = seq(&[
            &tap(LeftShift, 0),
            &[(80, ModifierUp(LeftShift))],
            &tap(LeftShift, 150),
        ]);
        assert_eq!(run(&mut d, &events), 1);
    }

    #[test]
    fn escape_counts_as_another_key_for_taps() {
        let mut d = TapDetector::new(LeftShift, W);
        let events = seq(&[
            &tap(LeftShift, 0),
            &[(100, EscapeDown), (120, EscapeUp)],
            &tap(LeftShift, 150),
        ]);
        assert_eq!(run(&mut d, &events), 0);
    }

    #[test]
    fn clock_going_backwards_does_not_panic() {
        let mut d = TapDetector::new(LeftShift, W);
        let events = [
            (1_000, ModifierDown(LeftShift)),
            (900, ModifierUp(LeftShift)),
            (800, ModifierDown(LeftShift)),
            (700, ModifierUp(LeftShift)),
        ];
        assert_eq!(run(&mut d, &events), 1);
    }

    #[test]
    fn from_key_discards_non_modifier_key_identities() {
        // Every non-modifier key maps to the same identity-free event, whatever it was.
        let classes = [KeyClass::Other, KeyClass::Escape];
        for class in classes {
            assert_eq!(InputEvent::from_key(class, true, false), Some(OtherKey));
            assert_eq!(InputEvent::from_key(class, false, false), None);
        }
        assert_eq!(
            InputEvent::from_key(KeyClass::Escape, true, true),
            Some(EscapeDown)
        );
        assert_eq!(
            InputEvent::from_key(KeyClass::Escape, false, true),
            Some(EscapeUp)
        );
        assert_eq!(
            InputEvent::from_key(KeyClass::Other, true, true),
            Some(OtherKey)
        );
        assert_eq!(
            InputEvent::from_key(KeyClass::Modifier(LeftShift), true, false),
            Some(ModifierDown(LeftShift))
        );
    }

    #[test]
    fn detector_state_holds_no_key_identity() {
        // Two runs that differ only in which other keys were typed leave identical state, so
        // nothing about those keys was kept.
        let typed_a: Vec<_> = crate::keymap::tests::all_windows_non_modifier_vks()
            .take(40)
            .map(|vk| crate::keymap::windows_vk(vk, 0))
            .collect();
        let typed_b: Vec<_> = crate::keymap::tests::all_windows_non_modifier_vks()
            .skip(40)
            .take(40)
            .map(|vk| crate::keymap::windows_vk(vk, 0))
            .collect();
        let drive = |keys: &[Option<KeyClass>]| {
            let mut d = TapDetector::new(LeftShift, W);
            let mut t = 0;
            for k in keys.iter().flatten() {
                for down in [true, false] {
                    if let Some(e) = InputEvent::from_key(*k, down, false) {
                        d.on_event(e, t);
                    }
                    t += 5;
                }
            }
            format!("{d:?}")
        };
        assert_eq!(drive(&typed_a), drive(&typed_b));
    }

    #[test]
    fn escape_three_times_fires() {
        let mut d = EscapeRepeatDetector::new(3, 500);
        let mut fired = 0;
        for i in 0..3 {
            fired += usize::from(d.on_event(EscapeDown, i * 200));
            d.on_event(EscapeUp, i * 200 + 50);
        }
        assert_eq!(fired, 1);
        // It fires on the third press, before the release.
        let mut d = EscapeRepeatDetector::new(3, 500);
        assert!(!d.on_event(EscapeDown, 0));
        d.on_event(EscapeUp, 10);
        assert!(!d.on_event(EscapeDown, 100));
        d.on_event(EscapeUp, 110);
        assert!(d.on_event(EscapeDown, 200));
    }

    #[test]
    fn escape_too_slow_or_interrupted_does_not_fire() {
        let mut d = EscapeRepeatDetector::new(3, 500);
        let slow = [
            (0, EscapeDown),
            (10, EscapeUp),
            (700, EscapeDown),
            (710, EscapeUp),
            (800, EscapeDown),
        ];
        assert_eq!(slow.iter().filter(|(t, e)| d.on_event(*e, *t)).count(), 0);
        let mut d = EscapeRepeatDetector::new(3, 500);
        let interrupted = [
            (0, EscapeDown),
            (10, EscapeUp),
            (100, EscapeDown),
            (110, EscapeUp),
            (150, OtherKey),
            (200, EscapeDown),
        ];
        assert_eq!(
            interrupted
                .iter()
                .filter(|(t, e)| d.on_event(*e, *t))
                .count(),
            0
        );
    }

    #[test]
    fn escape_held_does_not_count_repeats_and_modifiers_are_ignored() {
        let mut d = EscapeRepeatDetector::new(3, 500);
        let held = [
            (0, EscapeDown),
            (30, EscapeDown),
            (60, EscapeDown),
            (90, EscapeDown),
        ];
        assert_eq!(held.iter().filter(|(t, e)| d.on_event(*e, *t)).count(), 0);
        let mut d = EscapeRepeatDetector::new(3, 500);
        let with_mods = [
            (0, EscapeDown),
            (10, EscapeUp),
            (20, ModifierDown(LeftShift)),
            (100, EscapeDown),
            (110, EscapeUp),
            (120, ModifierUp(LeftShift)),
            (200, EscapeDown),
        ];
        assert_eq!(
            with_mods.iter().filter(|(t, e)| d.on_event(*e, *t)).count(),
            1
        );
    }

    #[test]
    fn six_escapes_fire_twice() {
        let mut d = EscapeRepeatDetector::new(3, 500);
        let mut fired = 0;
        for i in 0..6 {
            fired += usize::from(d.on_event(EscapeDown, i * 100));
            d.on_event(EscapeUp, i * 100 + 20);
        }
        assert_eq!(fired, 2);
    }
}
