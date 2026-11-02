//! Global trigger detection for Double Tap: a double tap of a modifier, or a chord. Only the
//! configured trigger is observed; no other keystrokes are recorded or stored.
//!
//! # Privacy
//!
//! This crate is not a keylogger and is built so it cannot become one by accident:
//! - A **double tap** listener observes modifier transitions (which modifier, up or down). For
//!   every other key it learns only that *some* key was pressed, which is what cancels a tap
//!   that was really part of a shortcut. Backends classify each OS event through
//!   [`keymap`] and [`InputEvent::from_key`] immediately; the key code of a non-modifier key is
//!   dropped there and never reaches a detector, a buffer, a channel or a log.
//! - A **chord** listener registers that one chord with the OS (`RegisterEventHotKey`,
//!   `RegisterHotKey`, `XGrabKey`, or the XDG GlobalShortcuts portal) and is told only when it
//!   is pressed.
//! - The **kill switch**'s Escape counter additionally recognises Escape, and nothing else.
//! - The **suggestion tap** ([`start_suggest_tap`], for ghost-text autocomplete) classifies
//!   every key event at the hook into one of five [`TapKey`] classes (Tab, Escape, Right arrow,
//!   "a modifier", "some other key") plus the four modifier flags, and drops the key code there.
//!   Only that class reaches the [`SuggestMachine`], and only a [`SuggestKey`]
//!   (`Accept`/`AcceptWord`/`Dismiss`/`Typed`/`Click`) reaches the callback. While no
//!   suggestion is visible the callback gets only `Typed` (some key was pressed) and `Click`,
//!   the same information a double-tap listener gets. Nothing is buffered: the machine's only
//!   state is which of Tab, Escape and Right is held after a consumed press.
//! - No key event, key code or text is ever logged, at any level.
//!
//! # Platforms
//!
//! | OS / session | Double tap | Chord | Escape ×3 kill switch | Suggestion keys |
//! |---|---|---|---|---|
//! | macOS | NSEvent `flagsChanged` monitors (no permission needed) | Carbon `RegisterEventHotKey` via `global-hotkey` | needs Accessibility (key-down monitor) | active `CGEventTap` (session, head insert) on key down/up and mouse down, own thread and run loop; needs Accessibility |
//! | Windows | `WH_KEYBOARD_LL` + `WH_MOUSE_LL` hooks | `RegisterHotKey` via `global-hotkey` | `WH_KEYBOARD_LL` | own `WH_KEYBOARD_LL` + `WH_MOUSE_LL` hooks, which swallow the consumed keys |
//! | Linux X11 | XInput2 raw events (`x11rb`) | `XGrabKey` via `global-hotkey` | XInput2 raw events | XInput2 (2.1+) raw events to decide; `XGrabKey` of Tab, Escape and Ctrl+Right only while active, to consume |
//! | Linux Wayland | **Unsupported** (use a chord) | XDG portal `GlobalShortcuts` (`ashpd`), stable shortcut ids, rebinds itself; see [`Listener::state`] | **Unsupported** (use a chord) | **Unsupported** |
//!
//! **Suggestion keys** ([`start_suggest_tap`]): while [`SuggestTap::set_active`] says a
//! suggestion is visible, Tab (no modifiers) → [`SuggestKey::Accept`], ⌥→ on macOS or Ctrl+→
//! elsewhere (exactly that modifier) → [`SuggestKey::AcceptWord`], and Escape (no modifiers) →
//! [`SuggestKey::Dismiss`] are **consumed**, along with their auto-repeats and their release
//! (even if the tap is deactivated before the key comes up). Any other key press, including
//! Shift+Tab or Cmd+→, passes through and reports [`SuggestKey::Typed`]; a mouse button press
//! passes through and reports [`SuggestKey::Click`]. While inactive, nothing is consumed (except
//! the tail of a key consumed earlier) and every key press, Tab and Escape included, reports
//! only [`SuggestKey::Typed`] (mouse presses [`SuggestKey::Click`]), so the app can debounce an
//! automatic suggestion trigger. Events this process injected itself
//! are ignored on macOS (event source pid) and Windows (every injected event is ignored); on
//! X11 nothing tells them apart, so this process's own XTest keys report `Typed` (and, while
//! active, a grabbed key would be swallowed): deactivate the tap and ignore `Typed` while
//! injecting.
//!
//! On Windows and Linux the `Command` modifiers are the Windows/Super keys and `Option` is Alt.
//! Use [`capabilities`] or [`check`] to ask before starting, and [`recommended_trigger`] for a
//! default that works on the current machine.
//!
//! # Threads
//!
//! `on_fire` always runs on a dedicated dispatcher thread, never on an OS hook or the main
//! thread, so it may block briefly or drop the listener.
//!
//! - **macOS:** NSEvent monitors and Carbon hot keys deliver through the **main run loop**, so
//!   the app's main thread must run an `NSApplication` event loop (Tauri's does). [`start`] on
//!   the main thread installs synchronously; from another thread it queues the installation on
//!   the main dispatch queue and returns at once (a chord the OS refuses is then only logged).
//!   Never block the main thread waiting for such a listener before the event loop runs.
//! - **Windows:** each listener owns a thread with its own message loop (low-level hooks and
//!   `RegisterHotKey` both deliver to the installing thread's queue).
//! - **Linux:** X11 listeners own a thread with their own X connection; the Wayland portal
//!   listener owns a thread with a small async runtime that waits for the desktop's consent,
//!   follows shortcut changes, and binds the shortcut again (with a capped backoff) if the
//!   desktop closes the session or the portal restarts.
//!
//! # Tauri integration
//!
//! Start the listeners in `tauri::Builder::setup` (the main thread, so macOS installs
//! synchronously and reports errors) and keep the handles in app state. To change the trigger,
//! drop the old listener and start a new one ([`check`] first: off the main thread macOS installs
//! asynchronously). Do not install another `global-hotkey` event handler (such as Tauri's
//! global-shortcut plugin) in the same process: it delivers to one process-wide handler, which
//! this crate installs.

use nugget_core::{Error, Result};

pub mod chord;
mod engine;
pub mod keymap;
pub mod suggest;
pub mod tap;

#[cfg(target_os = "linux")]
mod desktop_entry;
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
mod ghk;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
mod x11;

pub use chord::{Chord, ChordKey, ChordModifiers, Platform, parse_chord, parse_chord_for};
pub use suggest::{
    Mods, SuggestKey, SuggestMachine, SuggestTap, TapKey, Verdict, check_suggest_tap,
    start_suggest_tap,
};
pub use tap::{EscapeRepeatDetector, InputEvent, KeyClass, TapDetector};

use engine::Dispatcher;
use tap::Detector;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    LeftCommand,
    RightCommand,
    LeftControl,
    RightControl,
    LeftOption,
    RightOption,
    LeftShift,
    RightShift,
}

impl Modifier {
    pub const ALL: [Modifier; 8] = [
        Self::LeftCommand,
        Self::RightCommand,
        Self::LeftControl,
        Self::RightControl,
        Self::LeftOption,
        Self::RightOption,
        Self::LeftShift,
        Self::RightShift,
    ];

    /// The key's name as printed on that platform's keyboards, e.g. "Right ⌘ Command" on macOS
    /// and "Right Win" on Windows, for a settings page.
    pub fn label_for(self, platform: Platform) -> &'static str {
        use Modifier::*;
        match (platform, self) {
            (Platform::MacOs, LeftCommand) => "Left ⌘ Command",
            (Platform::MacOs, RightCommand) => "Right ⌘ Command",
            (Platform::MacOs, LeftControl) => "Left ⌃ Control",
            (Platform::MacOs, RightControl) => "Right ⌃ Control",
            (Platform::MacOs, LeftOption) => "Left ⌥ Option",
            (Platform::MacOs, RightOption) => "Right ⌥ Option",
            (Platform::MacOs, LeftShift) => "Left ⇧ Shift",
            (Platform::MacOs, RightShift) => "Right ⇧ Shift",
            (Platform::Windows, LeftCommand) => "Left Win",
            (Platform::Windows, RightCommand) => "Right Win",
            (Platform::Linux, LeftCommand) => "Left Super",
            (Platform::Linux, RightCommand) => "Right Super",
            (_, LeftControl) => "Left Ctrl",
            (_, RightControl) => "Right Ctrl",
            (_, LeftOption) => "Left Alt",
            (_, RightOption) => "Right Alt",
            (_, LeftShift) => "Left Shift",
            (_, RightShift) => "Right Shift",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Two presses of `modifier` alone within `window_ms`, with no other key in between.
    DoubleTap { modifier: Modifier, window_ms: u32 },
    /// A chord such as `Ctrl+Alt+Space`, in the platform's accelerator syntax.
    Chord(String),
}

impl Default for Trigger {
    fn default() -> Self {
        Self::DoubleTap {
            modifier: Modifier::RightCommand,
            window_ms: 350,
        }
    }
}

/// The shortest and longest double-tap windows [`start`] accepts.
pub const WINDOW_MS_RANGE: std::ops::RangeInclusive<u32> = 100..=2_000;

/// A running listener; dropping it stops listening.
pub trait Listener: Send {
    fn trigger(&self) -> &Trigger;

    /// Where the trigger stands now. Only a desktop-owned shortcut (the Wayland GlobalShortcuts
    /// portal) moves through the states; every other backend is active once [`start`] returns.
    fn state(&self) -> ShortcutState {
        ShortcutState::Active { trigger: None }
    }
}

/// The app id Nugget registers with the XDG desktop portal on Wayland. The portal (1.19+) accepts
/// it only when a desktop file named `dev.nugget.Nugget.desktop`, whose `Exec` program exists, is
/// installed in its `XDG_DATA_DIRS`; desktops key the shortcut's consent and settings by it.
///
/// Not the Tauri `identifier` (`dev.nugget.desktop`): KDE's kglobalaccel treats a component name
/// ending in `.desktop` as a desktop-file service whose shortcuts launch that file, so a portal
/// shortcut under that id is never registered and never reaches the app.
pub const APP_ID: &str = "dev.nugget.Nugget";

/// The portal shortcut ids, stable across runs so a desktop that remembers a consent (GNOME,
/// Plasma 6) binds a returning shortcut without asking again.
pub const DOUBLE_TAP_SHORTCUT_ID: &str = "double-tap";
/// See [`DOUBLE_TAP_SHORTCUT_ID`].
pub const KILL_SWITCH_SHORTCUT_ID: &str = "stop-agents";

/// Where a desktop-owned shortcut stands ([`Listener::state`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShortcutState {
    /// Listening. `trigger` is the key the desktop reports it bound (it may differ from the
    /// preferred one), when the backend knows it.
    Active { trigger: Option<String> },
    /// The bind request is open: the desktop is asking the user (GNOME, Plasma 6).
    AwaitingConsent,
    /// The desktop answered without assigning keys: the user declined, or must assign them in
    /// the desktop's keyboard shortcut settings. The session stays open, so an assignment made
    /// there later works.
    NotAssigned,
    /// The desktop closed the session or dropped the shortcut; Nugget is binding it again.
    Rebinding,
    /// The portal stopped working after [`start`] succeeded; the message says why.
    Failed(String),
}

/// Start listening; `on_fire` runs (on a background thread) each time the trigger fires.
///
/// Errors: [`Error::Invalid`] for a bad window or chord, [`Error::Unsupported`] when this OS or
/// session cannot observe the trigger (see the table in the crate docs), and [`Error::Other`] when
/// the OS refuses (for example, a chord another app has already registered).
pub fn start(trigger: Trigger, on_fire: Box<dyn Fn() + Send + Sync>) -> Result<Box<dyn Listener>> {
    let plan = Plan::for_trigger(&trigger)?;
    let dispatcher = Dispatcher::spawn(on_fire)?;
    let backend = plan.start(&dispatcher, Role::DoubleTap)?;
    Ok(Box::new(Running {
        trigger,
        backend,
        _dispatcher: dispatcher,
    }))
}

/// Check that `trigger` is valid and can be observed here, without starting anything.
pub fn check(trigger: &Trigger) -> Result<()> {
    Plan::for_trigger(trigger)?.supported()
}

struct Running {
    trigger: Trigger,
    // Fields drop in order: the backend stops before the dispatcher does.
    backend: Backend,
    _dispatcher: Dispatcher,
}

impl Listener for Running {
    fn trigger(&self) -> &Trigger {
        &self.trigger
    }

    fn state(&self) -> ShortcutState {
        self.backend.state()
    }
}

/// [`ShortcutState`] shared between a backend's thread and its listener handle.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
type SharedState = std::sync::Arc<std::sync::Mutex<ShortcutState>>;

/// A started backend: the handle whose drop stops it, and the state it reports, if it has one.
struct Backend {
    #[allow(dead_code)]
    handle: Box<dyn Send>,
    state: Option<SharedState>,
}

impl Backend {
    /// A backend that is active as soon as it starts.
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
        allow(dead_code)
    )]
    fn plain(handle: Box<dyn Send>) -> Self {
        Self {
            handle,
            state: None,
        }
    }

    fn state(&self) -> ShortcutState {
        match &self.state {
            Some(s) => s.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            None => ShortcutState::Active { trigger: None },
        }
    }
}

/// What a listener is for, which names its desktop-owned shortcut (Wayland portal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    DoubleTap,
    KillSwitch,
}

#[cfg(target_os = "linux")]
impl Role {
    fn shortcut_id(self) -> &'static str {
        match self {
            Self::DoubleTap => DOUBLE_TAP_SHORTCUT_ID,
            Self::KillSwitch => KILL_SWITCH_SHORTCUT_ID,
        }
    }

    /// The description the desktop shows next to the shortcut.
    fn description(self) -> &'static str {
        match self {
            Self::DoubleTap => "Nugget: draft a reply",
            Self::KillSwitch => "Nugget: stop all agents",
        }
    }
}

/// A second, independent trigger that stops whatever Nugget is doing (computer use).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillSwitch {
    /// Escape pressed `presses` times, each within `window_ms` of the previous one. Needs
    /// Accessibility permission on macOS; unsupported on Wayland.
    EscapeRepeat { presses: u8, window_ms: u32 },
    /// Any ordinary trigger, e.g. `Trigger::Chord("Ctrl+Alt+Esc".into())`.
    Trigger(Trigger),
}

impl Default for KillSwitch {
    fn default() -> Self {
        Self::EscapeRepeat {
            presses: 3,
            window_ms: 500,
        }
    }
}

/// A running kill switch; dropping it stops listening.
pub struct KillSwitchListener {
    kill_switch: KillSwitch,
    backend: Backend,
    _dispatcher: Dispatcher,
}

impl KillSwitchListener {
    /// Where the kill switch's shortcut stands; see [`Listener::state`].
    pub fn state(&self) -> ShortcutState {
        self.backend.state()
    }
}

impl std::fmt::Debug for KillSwitchListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KillSwitchListener")
            .field("kill_switch", &self.kill_switch)
            .finish()
    }
}

/// Start a kill switch. It runs its own backend instance, so it keeps working whatever the main
/// trigger is doing, and can run alongside it. `on_fire` runs on a background thread.
pub fn start_kill_switch(
    kill_switch: KillSwitch,
    on_fire: Box<dyn Fn() + Send + Sync>,
) -> Result<KillSwitchListener> {
    let plan = Plan::for_kill_switch(&kill_switch)?;
    let dispatcher = Dispatcher::spawn(on_fire)?;
    let backend = plan.start(&dispatcher, Role::KillSwitch)?;
    Ok(KillSwitchListener {
        kill_switch,
        backend,
        _dispatcher: dispatcher,
    })
}

/// [`check`] for a kill switch.
pub fn check_kill_switch(kill_switch: &KillSwitch) -> Result<()> {
    Plan::for_kill_switch(kill_switch)?.supported()
}

/// The desktop session the listeners would run in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Session {
    MacOs,
    Windows,
    X11,
    Wayland,
    /// Linux with no display server reachable (SSH, CI without Xvfb).
    Headless,
    /// An OS with no backend.
    Other,
}

/// What can be observed in the current session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub session: Session,
    pub double_tap: bool,
    pub chord: bool,
    pub escape_repeat: bool,
    /// The ghost-text suggestion keys ([`start_suggest_tap`]).
    pub suggest_tap: bool,
}

/// The current session. On Linux, `NUGGET_HOTKEY_SESSION=x11|wayland` overrides detection (for
/// example to use X11 under XWayland, or an Xvfb display inside a Wayland desktop).
pub fn session() -> Session {
    if cfg!(target_os = "macos") {
        Session::MacOs
    } else if cfg!(target_os = "windows") {
        Session::Windows
    } else if cfg!(target_os = "linux") {
        let var = |k: &str| std::env::var(k).ok();
        detect_linux_session(
            var("NUGGET_HOTKEY_SESSION").as_deref(),
            var("XDG_SESSION_TYPE").as_deref(),
            var("WAYLAND_DISPLAY").as_deref(),
            var("DISPLAY").as_deref(),
        )
    } else {
        Session::Other
    }
}

/// Session detection from the environment, as a pure function. A Wayland session wins over a
/// `DISPLAY` that only exists for XWayland: XWayland sees input only while an X11 window has
/// focus, so global double taps there would be unreliable.
pub fn detect_linux_session(
    forced: Option<&str>,
    xdg_session_type: Option<&str>,
    wayland_display: Option<&str>,
    display: Option<&str>,
) -> Session {
    let set = |v: Option<&str>| v.is_some_and(|v| !v.trim().is_empty());
    match forced.map(|f| f.trim().to_ascii_lowercase()).as_deref() {
        Some("x11") => return Session::X11,
        Some("wayland") => return Session::Wayland,
        _ => {}
    }
    match xdg_session_type
        .map(|t| t.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("wayland") => return Session::Wayland,
        Some("x11") if set(display) => return Session::X11,
        _ => {}
    }
    if set(wayland_display) {
        Session::Wayland
    } else if set(display) {
        Session::X11
    } else {
        Session::Headless
    }
}

pub fn capabilities() -> Capabilities {
    capabilities_for(session())
}

fn capabilities_for(session: Session) -> Capabilities {
    let (double_tap, chord, escape_repeat, suggest_tap) = match session {
        Session::MacOs | Session::Windows | Session::X11 => (true, true, true, true),
        Session::Wayland => (false, true, false, false),
        Session::Headless | Session::Other => (false, false, false, false),
    };
    Capabilities {
        session,
        double_tap,
        chord,
        escape_repeat,
        suggest_tap,
    }
}

/// A trigger that works on this machine: a right-Command double tap on macOS, a right-Ctrl double
/// tap on Windows and X11 (every PC keyboard has one; many laptops have no right Win key), and
/// `Ctrl+Alt+Space` where double taps are unsupported (Wayland).
pub fn recommended_trigger() -> Trigger {
    let caps = capabilities();
    match caps.session {
        Session::MacOs => Trigger::default(),
        _ if caps.double_tap => Trigger::DoubleTap {
            modifier: Modifier::RightControl,
            window_ms: 350,
        },
        _ => Trigger::Chord("Ctrl+Alt+Space".into()),
    }
}

enum Plan {
    Detect(Detector),
    Chord(Chord),
}

fn unsupported(what: &str, session: Session) -> Error {
    let hint = match session {
        Session::Wayland => {
            " Wayland does not let apps watch modifier keys without root; pick a chord such as \
             Ctrl+Alt+Space, which goes through the desktop's GlobalShortcuts portal."
        }
        Session::Headless => {
            " No X11 or Wayland display is reachable (DISPLAY/WAYLAND_DISPLAY unset)."
        }
        _ => "",
    };
    Error::Unsupported(format!(
        "{what} is not supported in this session ({session:?}).{hint}"
    ))
}

impl Plan {
    fn for_trigger(trigger: &Trigger) -> Result<Self> {
        match trigger {
            Trigger::DoubleTap {
                modifier,
                window_ms,
            } => {
                if !WINDOW_MS_RANGE.contains(window_ms) {
                    return Err(Error::Invalid(format!(
                        "double-tap window {window_ms} ms is outside {}..={} ms",
                        WINDOW_MS_RANGE.start(),
                        WINDOW_MS_RANGE.end()
                    )));
                }
                Ok(Self::Detect(Detector::Tap(TapDetector::new(
                    *modifier, *window_ms,
                ))))
            }
            Trigger::Chord(text) => Ok(Self::Chord(parse_chord(text)?)),
        }
    }

    fn for_kill_switch(kill_switch: &KillSwitch) -> Result<Self> {
        match kill_switch {
            KillSwitch::EscapeRepeat { presses, window_ms } => {
                if !(2..=10).contains(presses) {
                    return Err(Error::Invalid(format!(
                        "Escape presses {presses} is outside 2..=10"
                    )));
                }
                if !WINDOW_MS_RANGE.contains(window_ms) {
                    return Err(Error::Invalid(format!(
                        "Escape window {window_ms} ms is outside {}..={} ms",
                        WINDOW_MS_RANGE.start(),
                        WINDOW_MS_RANGE.end()
                    )));
                }
                Ok(Self::Detect(Detector::Escape(EscapeRepeatDetector::new(
                    *presses, *window_ms,
                ))))
            }
            KillSwitch::Trigger(t) => Self::for_trigger(t),
        }
    }

    fn describe(&self) -> &'static str {
        match self {
            Self::Detect(Detector::Tap(_)) => "a modifier double tap",
            Self::Detect(Detector::Escape(_)) => "an Escape repeat",
            Self::Chord(_) => "a chord",
        }
    }

    fn supported(&self) -> Result<()> {
        let caps = capabilities();
        let ok = match self {
            Self::Detect(Detector::Tap(_)) => caps.double_tap,
            Self::Detect(Detector::Escape(_)) => caps.escape_repeat,
            Self::Chord(_) => caps.chord,
        };
        if ok {
            Ok(())
        } else {
            Err(unsupported(self.describe(), caps.session))
        }
    }

    fn start(self, dispatcher: &Dispatcher, role: Role) -> Result<Backend> {
        self.supported()?;
        let fire = dispatcher.fire_handle();
        #[cfg(target_os = "macos")]
        {
            let _ = role;
            match self {
                Self::Detect(d) => macos::start_detector(d, fire),
                Self::Chord(c) => ghk::start_chord(c, fire),
            }
            .map(Backend::plain)
        }
        #[cfg(target_os = "windows")]
        {
            let _ = role;
            match self {
                Self::Detect(d) => windows::start(windows::Mode::Detect(d), fire),
                Self::Chord(c) => windows::start(windows::Mode::Chord(c), fire),
            }
            .map(Backend::plain)
        }
        #[cfg(target_os = "linux")]
        {
            match (self, session()) {
                (Self::Detect(d), Session::X11) => x11::start_detector(d, fire).map(Backend::plain),
                (Self::Chord(c), Session::X11) => ghk::start_chord(c, fire).map(Backend::plain),
                (Self::Chord(c), Session::Wayland) => wayland::start_chord(
                    c,
                    fire,
                    wayland::Shortcut {
                        id: role.shortcut_id(),
                        description: role.description(),
                    },
                ),
                (plan, s) => Err(unsupported(plan.describe(), s)),
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            let _ = (fire, role);
            Err(unsupported(self.describe(), Session::Other))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_triggers_are_rejected_before_any_backend_runs() {
        let tap = |window_ms| Trigger::DoubleTap {
            modifier: Modifier::LeftShift,
            window_ms,
        };
        for t in [tap(99), tap(2_001), Trigger::Chord("Ctrl+".into())] {
            assert!(matches!(check(&t), Err(Error::Invalid(_))), "{t:?}");
            let started = start(t.clone(), Box::new(|| {}));
            assert!(matches!(started, Err(Error::Invalid(_))), "{t:?}");
        }
        let esc = |presses, window_ms| KillSwitch::EscapeRepeat { presses, window_ms };
        for k in [
            esc(1, 500),
            esc(11, 500),
            esc(3, 5),
            KillSwitch::Trigger(Trigger::Chord("Nope+X".into())),
        ] {
            assert!(
                matches!(check_kill_switch(&k), Err(Error::Invalid(_))),
                "{k:?}"
            );
            let started = start_kill_switch(k.clone(), Box::new(|| {}));
            assert!(matches!(started, Err(Error::Invalid(_))), "{k:?}");
        }
    }

    #[test]
    fn session_detection() {
        use Session::*;
        let d = detect_linux_session;
        assert_eq!(d(None, Some("x11"), None, Some(":0")), X11);
        assert_eq!(d(None, None, None, Some(":99")), X11);
        assert_eq!(d(None, None, Some("wayland-1"), None), Wayland);
        assert_eq!(d(None, Some("tty"), None, None), Headless);
        assert_eq!(d(None, None, Some(""), Some("")), Headless);
        // XWayland's DISPLAY does not make a Wayland session X11.
        assert_eq!(d(None, None, Some("wayland-0"), Some(":0")), Wayland);
        // Overrides.
        assert_eq!(d(Some("x11"), Some("wayland"), Some("w"), Some(":1")), X11);
        assert_eq!(d(Some("WAYLAND"), Some("x11"), None, Some(":0")), Wayland);
        assert_eq!(d(Some("bogus"), Some("x11"), None, Some(":0")), X11);
    }

    #[test]
    fn capabilities_per_session() {
        let c = |s| {
            let c = capabilities_for(s);
            (c.double_tap, c.chord, c.escape_repeat, c.suggest_tap)
        };
        assert_eq!(c(Session::Wayland), (false, true, false, false));
        assert_eq!(c(Session::Headless), (false, false, false, false));
        assert_eq!(c(Session::Other), (false, false, false, false));
        for s in [Session::MacOs, Session::Windows, Session::X11] {
            assert_eq!(c(s), (true, true, true, true), "{s:?}");
        }
        let msg = unsupported("a modifier double tap", Session::Wayland).to_string();
        assert!(
            msg.contains("chord") && msg.contains("GlobalShortcuts"),
            "{msg}"
        );
        if capabilities().double_tap || capabilities().chord {
            check(&recommended_trigger()).unwrap();
        }
    }

    #[test]
    fn modifier_labels() {
        assert_eq!(
            Modifier::RightCommand.label_for(Platform::Windows),
            "Right Win"
        );
        assert_eq!(
            Modifier::RightOption.label_for(Platform::MacOs),
            "Right ⌥ Option"
        );
        assert_eq!(Modifier::LeftOption.label_for(Platform::Linux), "Left Alt");
        for p in [Platform::MacOs, Platform::Windows, Platform::Linux] {
            let labels: std::collections::HashSet<_> =
                Modifier::ALL.iter().map(|m| m.label_for(p)).collect();
            assert_eq!(labels.len(), 8);
        }
    }
}
