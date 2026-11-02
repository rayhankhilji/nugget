//! The suggestion tap: the global keys that accept or dismiss a ghost-text suggestion.
//!
//! While a suggestion is visible ([`SuggestTap::set_active`]), Tab accepts it, the accept-word
//! chord (⌥→ on macOS, Ctrl+→ on Windows and Linux) accepts the next word, and Escape dismisses
//! it; those key presses are **consumed**, so the focused app never sees them. Every other key
//! press passes through and is reported only as [`SuggestKey::Typed`], with no identity; a mouse
//! button press passes through and is reported as [`SuggestKey::Click`]. While no suggestion is
//! visible nothing is consumed (except the tail of a key consumed earlier), and every key press,
//! Tab and Escape included, is reported only as [`SuggestKey::Typed`] (mouse presses as
//! [`SuggestKey::Click`]): identity-free "a key was pressed" pulses, the same information the
//! double-tap detector gets, which the desktop uses to debounce its automatic suggestion trigger.
//!
//! Backends classify each OS event at the hook into a [`TapKey`] (Tab, Escape, Right, a modifier
//! or "other") and the four modifier flags, and the pure [`SuggestMachine`] decides from that
//! alone. Key codes and characters never cross the hook.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nugget_core::{Error, Result};

use crate::chord::Platform;
use crate::engine::Dispatcher;
use crate::{Session, capabilities, unsupported};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
mod x11;

/// What the user did while a suggestion was visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuggestKey {
    /// Tab with no modifiers: accept the whole suggestion. The key was consumed.
    Accept,
    /// ⌥→ on macOS, Ctrl+→ on Windows and Linux: accept the next word. The key was consumed.
    AcceptWord,
    /// Escape with no modifiers: dismiss. The key was consumed.
    Dismiss,
    /// Some other key was pressed (which one is deliberately not known); it reached the app.
    Typed,
    /// A mouse button was pressed; it reached the app. The caret has probably moved.
    Click,
}

/// A key as the suggestion tap classifies it at the hook. [`TapKey::Other`] carries no identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TapKey {
    Tab,
    Escape,
    Right,
    /// Shift, Control, Option/Alt or Command/Win/Super (either side), Caps Lock or Fn.
    Modifier,
    Other,
}

/// The modifiers held when a key went down. Lock keys and keypad/Fn flags are not modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub shift: bool,
    pub control: bool,
    /// Option on macOS, Alt elsewhere.
    pub option: bool,
    /// Command on macOS, Win on Windows, Super on Linux.
    pub command: bool,
}

impl Mods {
    pub const NONE: Self = Self {
        shift: false,
        control: false,
        option: false,
        command: false,
    };

    /// Only the accept-word modifier: Option on macOS, Control elsewhere.
    pub fn accept_word(platform: Platform) -> Self {
        match platform {
            Platform::MacOs => Self {
                option: true,
                ..Self::NONE
            },
            Platform::Windows | Platform::Linux => Self {
                control: true,
                ..Self::NONE
            },
        }
    }
}

/// What a backend does with one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Verdict {
    /// Swallow the event: the focused app must not see it.
    pub consume: bool,
    /// What to report to the callback, if anything.
    pub emit: Option<SuggestKey>,
}

impl Verdict {
    const PASS: Self = Self {
        consume: false,
        emit: None,
    };

    const fn consume(emit: Option<SuggestKey>) -> Self {
        Self {
            consume: true,
            emit,
        }
    }

    const fn pass(emit: SuggestKey) -> Self {
        Self {
            consume: false,
            emit: Some(emit),
        }
    }
}

/// The suggestion tap's decisions, as a pure state machine. Its only state is which of Tab,
/// Escape and Right are held down after a consumed press, so their auto-repeats and release are
/// consumed too, even if the tap was deactivated in between (the app never sees an orphan
/// key-up or repeated Tabs after an accept).
#[derive(Debug, Clone)]
pub struct SuggestMachine {
    platform: Platform,
    /// Held-and-consumed flags for Tab, Escape and Right.
    consumed: [bool; 3],
}

fn slot(key: TapKey) -> Option<usize> {
    match key {
        TapKey::Tab => Some(0),
        TapKey::Escape => Some(1),
        TapKey::Right => Some(2),
        TapKey::Modifier | TapKey::Other => None,
    }
}

impl SuggestMachine {
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            consumed: [false; 3],
        }
    }

    /// One key transition. `repeat` marks an auto-repeat key-down (backends that cannot tell
    /// pass `false`: a key-down of a key already held after a consumed press is treated as a
    /// repeat either way). `mods` are the modifiers held at the time; `active` is whether a
    /// suggestion is visible.
    pub fn key(
        &mut self,
        key: TapKey,
        down: bool,
        repeat: bool,
        mods: Mods,
        active: bool,
    ) -> Verdict {
        if let Some(held) = slot(key).map(|i| &mut self.consumed[i])
            && *held
        {
            // Auto-repeats and the release of a consumed key are consumed, whatever `active`
            // says now. The release ends it.
            if !down {
                *held = false;
            }
            return Verdict::consume(None);
        }
        if !down || key == TapKey::Modifier {
            return Verdict::PASS;
        }
        if !active || repeat {
            // Inactive: every press is just "a key was pressed" (the desktop debounces its
            // automatic suggestion trigger on it). A repeat of a key held since before the
            // suggestion appeared is typing, never accepting.
            return Verdict::pass(SuggestKey::Typed);
        }
        let emit = match key {
            TapKey::Tab if mods == Mods::NONE => SuggestKey::Accept,
            TapKey::Escape if mods == Mods::NONE => SuggestKey::Dismiss,
            TapKey::Right if mods == Mods::accept_word(self.platform) => SuggestKey::AcceptWord,
            _ => return Verdict::pass(SuggestKey::Typed),
        };
        if let Some(i) = slot(key) {
            self.consumed[i] = true;
        }
        Verdict::consume(Some(emit))
    }

    /// A mouse button press: always reported as [`SuggestKey::Click`], never consumed.
    /// (`active` is taken for symmetry with [`SuggestMachine::key`].)
    pub fn mouse(&mut self, _active: bool) -> Verdict {
        Verdict::pass(SuggestKey::Click)
    }
}

/// The platform half of a running tap. Dropping it stops the tap and releases any hook or grab.
trait TapBackend: Send + Sync {
    /// Called by [`SuggestTap::set_active`] after the shared flag is updated.
    fn set_active(&self, _active: bool) {}
}

/// A running suggestion tap; dropping it stops the tap and releases any hook or grab.
pub struct SuggestTap {
    active: Arc<AtomicBool>,
    // Fields drop in order: the backend stops before the dispatcher does.
    backend: Box<dyn TapBackend>,
    _dispatcher: Dispatcher<SuggestKey>,
}

impl std::fmt::Debug for SuggestTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuggestTap")
            .field("active", &self.is_active())
            .finish()
    }
}

impl SuggestTap {
    /// Consume Tab, the accept-word chord and Escape only while active (a suggestion is
    /// visible). Cheap, callable from any thread. On X11 it grabs or ungrabs those keys
    /// synchronously (one round trip).
    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Release);
        self.backend.set_active(active);
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}

/// Start the suggestion tap, inactive. `on_key` runs on a dedicated dispatcher thread, never on
/// the OS hook thread, so it may block briefly or call [`SuggestTap::set_active`].
///
/// Errors: [`Error::Unsupported`] where no tap can run (see [`check_suggest_tap`]), and
/// [`Error::Other`] when the OS refuses (on macOS, when Accessibility is not granted).
pub fn start_suggest_tap(on_key: Box<dyn Fn(SuggestKey) + Send + Sync>) -> Result<SuggestTap> {
    check_suggest_tap()?;
    let dispatcher = Dispatcher::spawn_with(on_key)?;
    let active = Arc::new(AtomicBool::new(false));
    let backend = start_backend(&dispatcher, active.clone())?;
    Ok(SuggestTap {
        active,
        backend,
        _dispatcher: dispatcher,
    })
}

#[allow(unused_variables)]
fn start_backend(
    dispatcher: &Dispatcher<SuggestKey>,
    active: Arc<AtomicBool>,
) -> Result<Box<dyn TapBackend>> {
    let emit = dispatcher.fire_handle();
    #[cfg(target_os = "macos")]
    return macos::start(active, emit);
    #[cfg(target_os = "windows")]
    return windows::start(active, emit);
    #[cfg(target_os = "linux")]
    return x11::start(active, emit);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    Err(suggest_unsupported(Session::Other))
}

fn suggest_unsupported(session: Session) -> Error {
    match session {
        Session::Wayland => Error::Unsupported(
            "ghost-text suggestion keys are not supported on Wayland: Wayland does not let an \
             app observe or consume keys typed into other apps. Use an X11 session to get them."
                .into(),
        ),
        s => unsupported("the ghost-text suggestion tap", s),
    }
}

/// Whether a suggestion tap can run in this session: [`Error::Unsupported`] on Wayland, headless
/// Linux and other OSes. It does not check the macOS Accessibility permission.
pub fn check_suggest_tap() -> Result<()> {
    let caps = capabilities();
    if caps.suggest_tap {
        Ok(())
    } else {
        Err(suggest_unsupported(caps.session))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SuggestKey::*;

    const NONE: Mods = Mods::NONE;
    const SHIFT: Mods = Mods {
        shift: true,
        ..Mods::NONE
    };
    const CONTROL: Mods = Mods {
        control: true,
        ..Mods::NONE
    };
    const OPTION: Mods = Mods {
        option: true,
        ..Mods::NONE
    };
    const COMMAND: Mods = Mods {
        command: true,
        ..Mods::NONE
    };

    fn consumed(emit: Option<SuggestKey>) -> Verdict {
        Verdict::consume(emit)
    }

    fn passed(emit: Option<SuggestKey>) -> Verdict {
        Verdict {
            consume: false,
            emit,
        }
    }

    fn mac() -> SuggestMachine {
        SuggestMachine::new(Platform::MacOs)
    }

    #[test]
    fn inactive_consumes_nothing_and_reports_presses_only_as_typed() {
        for platform in [Platform::MacOs, Platform::Windows, Platform::Linux] {
            let mut m = SuggestMachine::new(platform);
            let word = Mods::accept_word(platform);
            for (key, mods) in [
                (TapKey::Tab, NONE),
                (TapKey::Escape, NONE),
                (TapKey::Right, word),
                (TapKey::Tab, SHIFT),
                (TapKey::Other, NONE),
                (TapKey::Other, COMMAND),
            ] {
                // Press, auto-repeat: passed, "a key was pressed". Release: nothing.
                assert_eq!(m.key(key, true, false, mods, false), passed(Some(Typed)));
                assert_eq!(m.key(key, true, true, mods, false), passed(Some(Typed)));
                assert_eq!(m.key(key, false, false, mods, false), passed(None));
            }
            assert_eq!(m.mouse(false), passed(Some(Click)));
        }
    }

    #[test]
    fn tab_active_is_consumed_with_its_repeats_and_release() {
        let mut m = mac();
        assert_eq!(
            m.key(TapKey::Tab, true, false, NONE, true),
            consumed(Some(Accept))
        );
        assert_eq!(m.key(TapKey::Tab, true, true, NONE, true), consumed(None));
        assert_eq!(m.key(TapKey::Tab, true, true, NONE, true), consumed(None));
        assert_eq!(m.key(TapKey::Tab, false, false, NONE, true), consumed(None));
        // Released: the next press is judged afresh.
        assert_eq!(
            m.key(TapKey::Tab, true, false, NONE, false),
            passed(Some(Typed))
        );
    }

    #[test]
    fn release_and_repeats_are_consumed_after_deactivation() {
        let mut m = mac();
        assert_eq!(
            m.key(TapKey::Tab, true, false, NONE, true),
            consumed(Some(Accept))
        );
        // The callback hides the suggestion while Tab is still held.
        assert_eq!(m.key(TapKey::Tab, true, true, NONE, false), consumed(None));
        // A backend that cannot flag repeats (Windows) sends a plain key-down: still consumed.
        assert_eq!(m.key(TapKey::Tab, true, false, NONE, false), consumed(None));
        assert_eq!(
            m.key(TapKey::Tab, false, false, NONE, false),
            consumed(None)
        );
        assert_eq!(m.key(TapKey::Tab, false, false, NONE, false), passed(None));
        // Escape and Right the same way.
        assert_eq!(
            m.key(TapKey::Escape, true, false, NONE, true),
            consumed(Some(Dismiss))
        );
        assert_eq!(
            m.key(TapKey::Escape, false, false, NONE, false),
            consumed(None)
        );
        assert_eq!(
            m.key(TapKey::Right, true, false, OPTION, true),
            consumed(Some(AcceptWord))
        );
        // Option released before Right: the release is still consumed.
        assert_eq!(
            m.key(TapKey::Right, false, false, NONE, false),
            consumed(None)
        );
    }

    #[test]
    fn modified_tab_passes_as_typed() {
        let mut m = mac();
        for mods in [SHIFT, CONTROL, OPTION, COMMAND] {
            assert_eq!(
                m.key(TapKey::Tab, true, false, mods, true),
                passed(Some(Typed)),
                "{mods:?}"
            );
            assert_eq!(
                m.key(TapKey::Tab, false, false, mods, true),
                passed(None),
                "{mods:?}"
            );
            assert_eq!(
                m.key(TapKey::Escape, true, false, mods, true),
                passed(Some(Typed))
            );
            assert_eq!(
                m.key(TapKey::Escape, false, false, mods, true),
                passed(None)
            );
        }
    }

    #[test]
    fn accept_word_is_option_right_on_mac_and_control_right_elsewhere() {
        let mut m = mac();
        assert_eq!(
            m.key(TapKey::Right, true, false, OPTION, true),
            consumed(Some(AcceptWord))
        );
        assert_eq!(
            m.key(TapKey::Right, false, false, OPTION, true),
            consumed(None)
        );
        for mods in [
            NONE,
            CONTROL,
            COMMAND,
            SHIFT,
            Mods {
                shift: true,
                ..OPTION
            },
        ] {
            assert_eq!(
                m.key(TapKey::Right, true, false, mods, true),
                passed(Some(Typed)),
                "{mods:?}"
            );
            assert_eq!(m.key(TapKey::Right, false, false, mods, true), passed(None));
        }
        for platform in [Platform::Windows, Platform::Linux] {
            let mut m = SuggestMachine::new(platform);
            assert_eq!(
                m.key(TapKey::Right, true, false, CONTROL, true),
                consumed(Some(AcceptWord))
            );
            assert_eq!(
                m.key(TapKey::Right, false, false, CONTROL, true),
                consumed(None)
            );
            for mods in [
                NONE,
                OPTION,
                COMMAND,
                Mods {
                    shift: true,
                    ..CONTROL
                },
            ] {
                assert_eq!(
                    m.key(TapKey::Right, true, false, mods, true),
                    passed(Some(Typed)),
                    "{platform:?} {mods:?}"
                );
            }
            // Inactive: Ctrl+→ is the app's, reported only as a key press.
            assert_eq!(
                m.key(TapKey::Right, true, false, CONTROL, false),
                passed(Some(Typed))
            );
        }
    }

    #[test]
    fn command_right_passes_as_typed() {
        let mut m = mac();
        assert_eq!(
            m.key(TapKey::Right, true, false, COMMAND, true),
            passed(Some(Typed))
        );
    }

    #[test]
    fn escape_dismisses() {
        let mut m = SuggestMachine::new(Platform::Windows);
        assert_eq!(
            m.key(TapKey::Escape, true, false, NONE, true),
            consumed(Some(Dismiss))
        );
        assert_eq!(
            m.key(TapKey::Escape, true, true, NONE, true),
            consumed(None)
        );
        assert_eq!(
            m.key(TapKey::Escape, false, false, NONE, true),
            consumed(None)
        );
    }

    #[test]
    fn other_keys_pass_as_typed_and_releases_report_nothing() {
        let mut m = mac();
        assert_eq!(
            m.key(TapKey::Other, true, false, NONE, true),
            passed(Some(Typed))
        );
        assert_eq!(
            m.key(TapKey::Other, true, true, NONE, true),
            passed(Some(Typed))
        );
        assert_eq!(
            m.key(TapKey::Other, true, false, COMMAND, true),
            passed(Some(Typed))
        );
        assert_eq!(m.key(TapKey::Other, false, false, NONE, true), passed(None));
        // A Tab held since before the suggestion appeared is typing, not accepting.
        assert_eq!(
            m.key(TapKey::Tab, true, true, NONE, true),
            passed(Some(Typed))
        );
    }

    #[test]
    fn modifiers_alone_emit_nothing() {
        let mut m = mac();
        for active in [true, false] {
            for down in [true, false] {
                assert_eq!(
                    m.key(TapKey::Modifier, down, false, NONE, active),
                    passed(None)
                );
                assert_eq!(
                    m.key(TapKey::Modifier, down, true, CONTROL, active),
                    passed(None)
                );
            }
        }
    }

    #[test]
    fn mouse_passes_and_reports_click_active_or_not() {
        let mut m = mac();
        assert_eq!(m.mouse(true), passed(Some(Click)));
        assert_eq!(m.mouse(false), passed(Some(Click)));
    }

    #[test]
    fn independent_keys_track_separately() {
        let mut m = mac();
        assert_eq!(
            m.key(TapKey::Tab, true, false, NONE, true),
            consumed(Some(Accept))
        );
        // Escape pressed while Tab is still held (Tab's consumption must survive it).
        assert_eq!(
            m.key(TapKey::Escape, true, false, NONE, true),
            consumed(Some(Dismiss))
        );
        assert_eq!(
            m.key(TapKey::Escape, false, false, NONE, true),
            consumed(None)
        );
        assert_eq!(m.key(TapKey::Tab, false, false, NONE, true), consumed(None));
    }

    #[test]
    fn check_matches_capabilities_and_explains_wayland() {
        let caps = capabilities().suggest_tap;
        match check_suggest_tap() {
            Ok(()) => assert!(caps),
            Err(e) => {
                assert!(!caps);
                assert!(matches!(e, Error::Unsupported(_)), "{e}");
            }
        }
        let msg = suggest_unsupported(Session::Wayland).to_string();
        assert!(msg.contains("Wayland"), "{msg}");
        let msg = suggest_unsupported(Session::Headless).to_string();
        assert!(msg.contains("DISPLAY"), "{msg}");
    }
}
