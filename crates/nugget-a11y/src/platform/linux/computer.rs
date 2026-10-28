//! Computer use on AT-SPI (M4): apps and windows from the registry, element actions through the
//! `Action`, `EditableText`, `Value`, `Selection` and `Component` interfaces, key input with
//! XTest (or the registry's device event controller off X11), and app launch or activation.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use atspi::proxy::action::ActionProxyBlocking;
use atspi::proxy::device_event_controller::{DeviceEventControllerProxyBlocking, KeySynthType};
use atspi::proxy::selection::SelectionProxyBlocking;
use atspi::proxy::value::ValueProxyBlocking;
use atspi::{Interface, InterfaceSet, ScrollType, State, StateSet};
use nugget_core::{Error, Result};
use zbus::blocking::Connection;
use zbus::proxy::CacheProperties;

use super::input::{XKeyboard, chord_keysym};
use super::{AtspiEl, AtspiPlatform, REGISTRY, ROOT_PATH, x11, zerr};
use crate::computer::{AppEntry, KeyChord, KeyInput, ScrollDirection, UiAction, UiActionKind};
use crate::engine::{Platform as _, PlatformWindow};
use crate::tree::Element as _;

/// Top-level roles listed as windows.
const WINDOW_ROLES: &[&str] = &["frame", "window", "dialog", "alert", "file chooser"];
/// Action names that press, click or activate.
const PRESS_NAMES: &[&str] = &["click", "press", "activate", "toggle", "jump", "open"];
/// Action names that expand or collapse.
const EXPAND_NAMES: &[&str] = &[
    "expand or contract",
    "expand or collapse",
    "expand",
    "collapse",
];
/// Roles whose `Value` is user-settable.
const VALUE_ROLES: &[&str] = &["slider", "spin button", "scroll bar", "dial"];
/// Roles that scroll their content.
const SCROLL_ROLES: &[&str] = &[
    "scroll pane",
    "viewport",
    "list",
    "list box",
    "tree",
    "tree table",
    "table",
    "document web",
    "document frame",
    "document text",
];
/// Roles with an on/off state.
const CHECK_ROLES: &[&str] = &[
    "check box",
    "radio button",
    "toggle button",
    "check menu item",
    "radio menu item",
    "switch",
];
/// How far up the scroll action looks for a scroll bar.
const SCROLL_ANCESTORS: usize = 16;
/// Budget for the search of a focusable element to activate a window through.
const ACTIVATE_SEARCH_NODES: usize = 400;
const ACTIVATE_SEARCH_TIME: Duration = Duration::from_millis(500);

fn is_window_role(role: &str) -> bool {
    WINDOW_ROLES.contains(&role)
}

/// `disabled`, `checked` and `expanded` for [`States`](crate::tree::States).
pub(super) fn extra_states(role: &str, st: &StateSet) -> (bool, Option<bool>, Option<bool>) {
    let interactive = st.contains(State::Focusable)
        || st.contains(State::Checkable)
        || st.contains(State::Editable)
        || CHECK_ROLES.contains(&role)
        || matches!(role, "push button" | "combo box" | "menu item" | "link");
    let disabled = interactive && !st.contains(State::Enabled) && !st.contains(State::Sensitive);
    let checked = (CHECK_ROLES.contains(&role) || st.contains(State::Checkable)).then(|| {
        st.contains(State::Checked) || (role == "toggle button" && st.contains(State::Pressed))
    });
    let expanded = (st.contains(State::Expandable) || st.contains(State::Expanded))
        .then(|| st.contains(State::Expanded));
    (disabled, checked, expanded)
}

/// The actions an element supports, from its role, states, interfaces and action names.
pub(super) fn action_kinds(
    role: &str,
    st: &StateSet,
    ifaces: &InterfaceSet,
    names: &[String],
) -> Vec<UiActionKind> {
    let mut out = Vec::new();
    let named = |set: &[&str]| names.iter().any(|n| set.contains(&n.as_str()));
    if named(PRESS_NAMES) || (!names.is_empty() && !named(EXPAND_NAMES) && !named(&["select"])) {
        out.push(UiActionKind::Press);
    }
    let secure = role == "password text";
    if !secure
        && ((ifaces.contains(Interface::EditableText)
            && st.contains(State::Editable)
            && !st.contains(State::ReadOnly))
            || (ifaces.contains(Interface::Value) && VALUE_ROLES.contains(&role)))
    {
        out.push(UiActionKind::SetValue);
    }
    if st.contains(State::Selectable) || named(&["select"]) {
        out.push(UiActionKind::Select);
    }
    if st.contains(State::Focusable) && ifaces.contains(Interface::Component) {
        out.push(UiActionKind::Focus);
    }
    if ifaces.contains(Interface::Component)
        && (SCROLL_ROLES.contains(&role) || !st.contains(State::Showing))
    {
        out.push(UiActionKind::Scroll);
    }
    if named(EXPAND_NAMES) || (st.contains(State::Expandable) && !names.is_empty()) {
        out.push(UiActionKind::Expand);
    }
    out.sort();
    out.dedup();
    out
}

/// The index of the first action named in `wanted` (in that order of preference).
fn action_index(names: &[String], wanted: &[&str]) -> Option<i32> {
    wanted
        .iter()
        .find_map(|w| names.iter().position(|n| n == w))
        .and_then(|i| i32::try_from(i).ok())
}

fn refused(what: &str) -> Error {
    Error::Other(format!("AT-SPI: {what} refused"))
}

impl AtspiEl {
    /// A proxy for one of this object's interfaces; building one makes no D-Bus call.
    fn proxy<T>(&self) -> Option<T>
    where
        T: zbus::proxy::Defaults + From<zbus::Proxy<'static>>,
    {
        zbus::blocking::proxy::Builder::<'static, T>::new(&self.conn)
            .destination(self.dest.clone())
            .ok()?
            .path(self.path.clone())
            .ok()?
            .cache_properties(CacheProperties::No)
            .build()
            .ok()
    }

    fn role_name(&self) -> String {
        self.acc().get_role_name().unwrap_or_default()
    }

    /// Lowercased action names (empty without the `Action` interface).
    fn action_names(&self, ifaces: &InterfaceSet) -> Vec<String> {
        if !ifaces.contains(Interface::Action) {
            return Vec::new();
        }
        self.proxy::<ActionProxyBlocking>()
            .and_then(|a| a.get_actions().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.name.to_ascii_lowercase())
            .collect()
    }

    fn do_action(&self, index: i32) -> Result<()> {
        let a = self
            .proxy::<ActionProxyBlocking>()
            .ok_or_else(|| Error::Unsupported("no Action interface".into()))?;
        if a.do_action(index).map_err(zerr)? {
            Ok(())
        } else {
            Err(refused("action"))
        }
    }

    pub(super) fn ui_actions(&self) -> Vec<UiActionKind> {
        let role = self.role_name();
        let st = self.states().unwrap_or_else(|_| StateSet::empty());
        let ifaces = self.interfaces();
        action_kinds(&role, &st, &ifaces, &self.action_names(&ifaces))
    }

    pub(super) fn ui_perform(&self, action: &UiAction) -> Result<()> {
        let role = self.role_name();
        let st = self
            .states()
            .map_err(|_| Error::NotFound("element is gone".into()))?;
        let ifaces = self.interfaces();
        let names = self.action_names(&ifaces);
        match action {
            UiAction::Press => {
                if let Some(i) = action_index(&names, PRESS_NAMES) {
                    return self.do_action(i);
                }
                if !names.is_empty() {
                    return self.do_action(0);
                }
                if st.contains(State::Selectable) {
                    return self.select(&role, &names);
                }
                Err(Error::Unsupported(format!("{role} cannot be pressed")))
            }
            UiAction::SetValue { value } => self.set_value(&role, &st, &ifaces, value),
            UiAction::Select => self.select(&role, &names),
            UiAction::Focus => {
                let c = self
                    .component()
                    .ok_or_else(|| Error::Unsupported("no Component interface".into()))?;
                if c.grab_focus().map_err(zerr)? {
                    Ok(())
                } else {
                    Err(refused("focus"))
                }
            }
            UiAction::Scroll { direction, amount } => {
                if *amount == 0 {
                    let c = self
                        .component()
                        .ok_or_else(|| Error::Unsupported("no Component interface".into()))?;
                    return if c.scroll_to(ScrollType::Anywhere).map_err(zerr)? {
                        Ok(())
                    } else {
                        Err(Error::Unsupported("scrolling into view refused".into()))
                    };
                }
                self.scroll_by(*direction, *amount)
            }
            UiAction::Expand { expand } => {
                let known = st.contains(State::Expandable) || st.contains(State::Expanded);
                if known && st.contains(State::Expanded) == *expand {
                    return Ok(());
                }
                let exact: &[&str] = if *expand { &["expand"] } else { &["collapse"] };
                let i = action_index(&names, EXPAND_NAMES)
                    .or_else(|| action_index(&names, exact))
                    .or_else(|| action_index(&names, PRESS_NAMES))
                    .or_else(|| (!names.is_empty()).then_some(0))
                    .ok_or_else(|| Error::Unsupported(format!("{role} cannot expand")))?;
                self.do_action(i)
            }
        }
    }

    fn set_value(
        &self,
        role: &str,
        st: &StateSet,
        ifaces: &InterfaceSet,
        value: &str,
    ) -> Result<()> {
        if role == "password text" {
            return Err(Error::Invalid("refusing to set a secure field".into()));
        }
        if ifaces.contains(Interface::EditableText) && st.contains(State::Editable) {
            let ed = self
                .editable()
                .ok_or_else(|| Error::Unsupported("EditableText proxy".into()))?;
            return if ed.set_text_contents(value).map_err(zerr)? {
                // Leave the caret after the value, as typing it would (GTK puts it at 0).
                if let Some(t) = self.text() {
                    let _ = t.set_caret_offset(value.chars().count() as i32);
                }
                Ok(())
            } else {
                Err(refused("SetTextContents"))
            };
        }
        if ifaces.contains(Interface::Value) {
            let v: f64 = value
                .trim()
                .parse()
                .map_err(|_| Error::Invalid(format!("{role} takes a number")))?;
            let p = self
                .proxy::<ValueProxyBlocking>()
                .ok_or_else(|| Error::Unsupported("Value proxy".into()))?;
            return p.set_current_value(v).map_err(zerr);
        }
        Err(Error::Unsupported(format!("{role} has no settable value")))
    }

    fn selection_of(el: &AtspiEl) -> Option<SelectionProxyBlocking<'static>> {
        el.interfaces()
            .contains(Interface::Selection)
            .then(|| el.proxy::<SelectionProxyBlocking>())
            .flatten()
    }

    /// Select through the container's `Selection`; menu items are activated instead (selecting
    /// one only highlights it), except a combo box's items, which the combo box selects.
    fn select(&self, role: &str, names: &[String]) -> Result<()> {
        let parent = self.parent();
        let index = self.acc().get_index_in_parent().map_err(zerr)?;
        let combo = parent
            .as_ref()
            .filter(|p| p.role_name() == "menu")
            .and_then(|p| p.parent())
            .filter(|g| g.role_name() == "combo box");
        if let Some(sel) = combo.as_ref().and_then(Self::selection_of) {
            return if sel.select_child(index).map_err(zerr)? {
                Ok(())
            } else {
                Err(refused("SelectChild"))
            };
        }
        let menu_item = role.ends_with("menu item");
        if !menu_item
            && let Some(sel) = parent.as_ref().and_then(Self::selection_of)
            && sel.select_child(index).map_err(zerr)?
        {
            return Ok(());
        }
        if let Some(i) = action_index(names, &["select"])
            .or_else(|| action_index(names, PRESS_NAMES))
            .or_else(|| (menu_item && !names.is_empty()).then_some(0))
        {
            return self.do_action(i);
        }
        Err(Error::Unsupported(format!("{role} cannot be selected")))
    }

    /// Move the nearest scroll bar (of this element or an ancestor) by `amount` steps.
    fn scroll_by(&self, direction: ScrollDirection, amount: u32) -> Result<()> {
        let vertical = matches!(direction, ScrollDirection::Up | ScrollDirection::Down);
        let sign = if matches!(direction, ScrollDirection::Up | ScrollDirection::Left) {
            -1.0
        } else {
            1.0
        };
        let orientation = if vertical {
            State::Vertical
        } else {
            State::Horizontal
        };
        let mut cur = Some(self.clone());
        for _ in 0..SCROLL_ANCESTORS {
            let Some(el) = cur else { break };
            for child in el.children(64) {
                if child.role_name() != "scroll bar"
                    || !child.states().is_ok_and(|s| s.contains(orientation))
                {
                    continue;
                }
                let Some(v) = child.proxy::<ValueProxyBlocking>() else {
                    continue;
                };
                let (Ok(now), Ok(min), Ok(max)) =
                    (v.current_value(), v.minimum_value(), v.maximum_value())
                else {
                    continue;
                };
                let inc = v.minimum_increment().unwrap_or(0.0);
                let step = if inc > 0.0 { inc } else { (max - min) / 20.0 };
                let to = (now + sign * step * f64::from(amount)).clamp(min, max);
                return v.set_current_value(to).map_err(zerr);
            }
            cur = el.parent();
        }
        Err(Error::Unsupported("no scroll bar to scroll with".into()))
    }

    /// Bring this window to the front: focus it, or else the element that last had focus in it
    /// or its first focusable element (toolkits present the window when focusing inside it).
    fn activate_window(&self) -> Result<()> {
        if self.component().and_then(|c| c.grab_focus().ok()) == Some(true) {
            return Ok(());
        }
        let deadline = Instant::now() + ACTIVATE_SEARCH_TIME;
        let mut queue = std::collections::VecDeque::from(self.children(256));
        let mut first = None;
        let mut seen = 0;
        while let Some(el) = queue.pop_front() {
            seen += 1;
            if seen > ACTIVATE_SEARCH_NODES || Instant::now() > deadline {
                break;
            }
            let Ok(st) = el.states() else { continue };
            if !st.contains(State::Showing) {
                continue;
            }
            if st.contains(State::Focused) {
                first = Some(el);
                break;
            }
            if st.contains(State::Focusable) && first.is_none() {
                first = Some(el.clone());
            }
            queue.extend(el.children(256));
        }
        let el =
            first.ok_or_else(|| Error::Unsupported("nothing to focus in the window".into()))?;
        match el.component().map(|c| c.grab_focus()) {
            Some(Ok(true)) => Ok(()),
            Some(Err(e)) => Err(zerr(e)),
            _ => Err(refused("focus")),
        }
    }
}

/// One app from the registry with its windows.
struct AppScan {
    entry: AppEntry,
    windows: Vec<(AtspiEl, StateSet, Option<String>)>,
}

fn pid_of(conn: &Connection, bus_name: &str) -> Option<u32> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn).ok()?;
    let name = zbus::names::BusName::try_from(bus_name.to_string()).ok()?;
    dbus.get_connection_unix_process_id(name).ok()
}

impl AtspiPlatform {
    fn scan_inner(&self) -> Result<Vec<AppScan>> {
        let conn = self.bus()?;
        let root = AtspiEl::new(&conn, REGISTRY, ROOT_PATH);
        let refs = root.acc().get_children().map_err(zerr)?;
        let mut apps = Vec::new();
        for r in refs.iter().take(512) {
            let Some(app) = AtspiEl::from_ref(&conn, r) else {
                continue;
            };
            let mut windows = Vec::new();
            for w in app.children(64) {
                if !is_window_role(&w.role_name()) {
                    continue;
                }
                let Ok(st) = w.states() else { continue };
                if !st.contains(State::Visible) && !st.contains(State::Iconified) {
                    continue;
                }
                let title = w
                    .acc()
                    .name()
                    .ok()
                    .map(|t| crate::sanitize::clean_line(&t, crate::sanitize::MAX_SHORT))
                    .filter(|t| !t.is_empty());
                windows.push((w, st, title));
            }
            if windows.is_empty() {
                continue;
            }
            // Focused window first.
            windows.sort_by_key(|(_, st, _)| !st.contains(State::Active));
            let pid = pid_of(&conn, &app.dest);
            let id = super::app_id(&conn, &app.dest);
            let name = app.acc().name().unwrap_or_default();
            let name = crate::sanitize::clean_line(&name, crate::sanitize::MAX_SHORT);
            let name = if name.is_empty() {
                id.clone().unwrap_or_else(|| app.dest.clone())
            } else {
                name
            };
            let frontmost = windows[0].1.contains(State::Active);
            apps.push(AppScan {
                entry: AppEntry {
                    id,
                    name,
                    pid,
                    frontmost,
                },
                windows,
            });
        }
        apps.sort_by_key(|a| !a.entry.frontmost);
        Ok(apps)
    }

    /// Registry scan, reconnecting once when the bus connection went stale.
    fn scan(&self) -> Result<Vec<AppScan>> {
        self.scan_inner().or_else(|e| {
            tracing::debug!(error = %e, "AT-SPI app scan failed; reconnecting");
            self.reset();
            self.scan_inner()
        })
    }

    pub(super) fn cu_apps(&self) -> Result<Vec<AppEntry>> {
        Ok(self.scan()?.into_iter().map(|a| a.entry).collect())
    }

    pub(super) fn cu_windows(&self) -> Result<Vec<PlatformWindow<AtspiEl>>> {
        let mut out = Vec::new();
        for app in self.scan()? {
            for (el, st, title) in app.windows {
                out.push(PlatformWindow {
                    app: app.entry.clone(),
                    native_id: format!("{}{}", el.dest, el.path),
                    title,
                    bounds: el.bounds(),
                    focused: app.entry.frontmost && st.contains(State::Active),
                    minimized: st.contains(State::Iconified),
                    element: el,
                });
            }
        }
        // Focused window first; otherwise the scan's order (frontmost app first).
        out.sort_by_key(|w| !w.focused);
        Ok(out)
    }

    pub(super) fn cu_key_input(&self, input: &KeyInput) -> Result<()> {
        match input {
            KeyInput::Text(text) => {
                let text: String = text.chars().filter(|&c| c != '\r').collect();
                if text.is_empty() {
                    return Ok(());
                }
                // Into an editable focused field: insert at the caret over the selection, which
                // is exact for any script. Elsewhere: synthesised keys.
                if let Ok(Some(t)) = self.focus() {
                    let (node, ifaces, _) = t.element.read_node(false);
                    if !node.states.secure
                        && node.states.editable
                        && ifaces.contains(Interface::EditableText)
                    {
                        let before = Self::read_state(&t.element, 1 << 20, false);
                        if self.insert_text(&t.element, &text, &before).is_ok() {
                            return Ok(());
                        }
                    }
                }
                self.synth_text(&text)
            }
            KeyInput::Keys(chords) => self.synth_chords(chords),
        }
    }

    fn x11_session() -> bool {
        std::env::var_os("DISPLAY").is_some() && std::env::var_os("WAYLAND_DISPLAY").is_none()
    }

    fn dec(&self) -> Result<DeviceEventControllerProxyBlocking<'static>> {
        DeviceEventControllerProxyBlocking::new(&self.bus()?).map_err(zerr)
    }

    fn synth_text(&self, text: &str) -> Result<()> {
        if Self::x11_session() {
            match XKeyboard::new().and_then(|mut k| k.type_text(text)) {
                Ok(()) => return Ok(()),
                Err(e) => tracing::debug!(error = %e, "XTest typing failed; trying AT-SPI"),
            }
        }
        self.dec()?
            .generate_keyboard_event(0, text, KeySynthType::String)
            .map_err(zerr)
    }

    fn synth_chords(&self, chords: &[KeyChord]) -> Result<()> {
        if Self::x11_session() {
            match XKeyboard::new().and_then(|mut k| k.chords(chords)) {
                Ok(()) => return Ok(()),
                Err(e) => tracing::debug!(error = %e, "XTest keys failed; trying AT-SPI"),
            }
        }
        let dec = self.dec()?;
        for c in chords {
            let m = c.modifiers;
            // X modifier mask bits: Shift, Control, Mod1 (Alt), Mod4 (Super).
            let mask = i32::from(m.shift)
                | (i32::from(m.ctrl) << 2)
                | (i32::from(m.alt) << 3)
                | (i32::from(m.meta) << 6);
            if mask != 0 {
                dec.generate_keyboard_event(mask, "", KeySynthType::Lockmodifiers)
                    .map_err(zerr)?;
            }
            // Keysyms are at most 0x0110_ffff.
            let sym = chord_keysym(c.key) as i32;
            let r = dec
                .generate_keyboard_event(sym, "", KeySynthType::Sym)
                .map_err(zerr);
            if mask != 0 {
                dec.generate_keyboard_event(mask, "", KeySynthType::Unlockmodifiers)
                    .map_err(zerr)?;
            }
            r?;
        }
        Ok(())
    }

    pub(super) fn cu_open_app(&self, name: &str) -> Result<()> {
        let want = name.trim();
        if want.is_empty() {
            return Err(Error::Invalid("empty app name".into()));
        }
        let matches = |a: &AppEntry| {
            let stem = |s: &str| {
                Path::new(s)
                    .file_stem()
                    .map(|x| x.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            a.name.eq_ignore_ascii_case(want)
                || stem(&a.name).eq_ignore_ascii_case(want)
                || a.id.as_deref().is_some_and(|id| {
                    id.eq_ignore_ascii_case(want) || stem(id).eq_ignore_ascii_case(want)
                })
        };
        if let Some(app) = self.scan()?.into_iter().find(|a| matches(&a.entry)) {
            let mut done = false;
            if Self::x11_session()
                && let Some(pid) = app.entry.pid
            {
                done = x11::activate_pid(pid).unwrap_or(false);
            }
            let (win, _, _) = &app.windows[0];
            return match win.activate_window() {
                Err(_) if done => Ok(()),
                r => r,
            };
        }
        launch(want)
    }

    pub(super) fn cu_screenshot(&self, window: &AtspiEl) -> Result<Option<Vec<u8>>> {
        if !Self::x11_session() {
            return Ok(None);
        }
        match window.bounds() {
            Some(r) => x11::capture_png(r),
            None => Ok(None),
        }
    }
}

/// Whether `name` can name a desktop id or a program without being a path or an option.
fn plain_name(name: &str) -> bool {
    !name.starts_with('-')
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
}

fn which(prog: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(prog))
        .find(|f| {
            f.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// `applications/` directories, user first (XDG base directories).
fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) if !d.is_empty() => dirs.push(PathBuf::from(d)),
        _ => {
            if let Some(h) = std::env::var_os("HOME") {
                dirs.push(PathBuf::from(h).join(".local/share"));
            }
        }
    }
    let sys = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(std::env::split_paths(&sys));
    dirs.into_iter().map(|d| d.join("applications")).collect()
}

/// The `Name=` of a desktop entry's `[Desktop Entry]` group, when it is shown.
pub(super) fn desktop_entry_name(contents: &str) -> Option<String> {
    let mut in_main = false;
    let mut name = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        if let Some(v) = line.strip_prefix("Name=") {
            name = Some(v.trim().to_string());
        } else if matches!(line, "NoDisplay=true" | "Hidden=true") {
            return None;
        }
    }
    name
}

/// The desktop id for `want`: a desktop file named so, or one whose `Name=` matches.
fn find_desktop_id(want: &str) -> Option<String> {
    let id = want.strip_suffix(".desktop").unwrap_or(want);
    let dirs = application_dirs();
    for d in &dirs {
        if d.join(format!("{id}.desktop")).is_file() {
            return Some(id.to_string());
        }
    }
    for d in &dirs {
        let Ok(rd) = std::fs::read_dir(d) else {
            continue;
        };
        for f in rd.flatten().take(4096) {
            let p = f.path();
            if p.extension().is_none_or(|e| e != "desktop") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            if desktop_entry_name(&text).is_some_and(|n| n.eq_ignore_ascii_case(want)) {
                return p.file_stem().map(|s| s.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Launch an app that is not running: `gtk-launch <desktop id>`, else a program of that name on
/// `PATH`. No shell is involved and nothing but the name is passed.
fn launch(want: &str) -> Result<()> {
    let desktop = find_desktop_id(want).filter(|id| plain_name(id));
    let (prog, args): (PathBuf, Vec<String>) =
        match (desktop, which("gtk-launch"), plain_name(want)) {
            (Some(id), Some(gl), _) => (gl, vec![id]),
            (_, _, true) => match which(want) {
                Some(p) => (p, Vec::new()),
                None => return Err(Error::NotFound(format!("no app named {want:?}"))),
            },
            _ => return Err(Error::NotFound(format!("no app named {want:?}"))),
        };
    let mut child = Command::new(&prog)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Reap it whenever it exits so it does not linger as a zombie.
    std::thread::Builder::new()
        .name("nugget-a11y-launch".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(states: &[State]) -> StateSet {
        let mut s = StateSet::empty();
        for &st in states {
            s.insert(st);
        }
        s
    }

    #[test]
    fn states_for_check_boxes_and_expanders() {
        let st = set(&[
            State::Enabled,
            State::Sensitive,
            State::Focusable,
            State::Checked,
        ]);
        assert_eq!(extra_states("check box", &st), (false, Some(true), None));
        let st = set(&[State::Focusable]);
        assert_eq!(extra_states("check box", &st), (true, Some(false), None));
        let st = set(&[State::Enabled, State::Pressed]);
        assert_eq!(extra_states("toggle button", &st).1, Some(true));
        let st = set(&[State::Enabled, State::Expandable]);
        assert_eq!(extra_states("toggle button", &st).2, Some(false));
        let st = set(&[State::Expandable, State::Expanded]);
        assert_eq!(extra_states("filler", &st).2, Some(true));
        // A label is neither checkable nor disabled for lacking `enabled`.
        assert_eq!(extra_states("label", &set(&[])), (false, None, None));
    }

    #[test]
    fn actions_from_names_states_and_interfaces() {
        let comp = InterfaceSet::new(Interface::Component) | InterfaceSet::new(Interface::Action);
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let st = set(&[State::Focusable, State::Showing]);
        assert_eq!(
            action_kinds("push button", &st, &comp, &names(&["click"])),
            vec![UiActionKind::Press, UiActionKind::Focus]
        );
        let ed = comp | InterfaceSet::new(Interface::EditableText);
        let st = set(&[State::Focusable, State::Showing, State::Editable]);
        assert_eq!(
            action_kinds("text", &st, &ed, &names(&["activate"])),
            vec![
                UiActionKind::Press,
                UiActionKind::SetValue,
                UiActionKind::Focus
            ]
        );
        // Secure fields are never offered SetValue.
        assert!(!action_kinds("password text", &st, &ed, &[]).contains(&UiActionKind::SetValue));
        let st = set(&[State::Showing, State::Expandable]);
        assert_eq!(
            action_kinds("toggle button", &st, &comp, &names(&["activate"])),
            vec![UiActionKind::Press, UiActionKind::Expand]
        );
        let st = set(&[State::Showing, State::Selectable]);
        assert_eq!(
            action_kinds("list item", &st, &comp, &[]),
            vec![UiActionKind::Select]
        );
        let c = InterfaceSet::new(Interface::Component);
        assert_eq!(
            action_kinds("scroll pane", &set(&[State::Showing]), &c, &[]),
            vec![UiActionKind::Scroll]
        );
        assert_eq!(
            action_kinds("label", &set(&[]), &c, &[]),
            vec![UiActionKind::Scroll],
            "off-screen elements can be scrolled into view"
        );
        assert!(action_kinds("label", &set(&[State::Showing]), &c, &[]).is_empty());
    }

    #[test]
    fn action_preference() {
        let n: Vec<String> = ["expand or contract", "activate"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(action_index(&n, PRESS_NAMES), Some(1));
        assert_eq!(action_index(&n, EXPAND_NAMES), Some(0));
        assert_eq!(action_index(&n, &["select"]), None);
    }

    #[test]
    fn desktop_entries_and_names() {
        let e = "[Desktop Entry]\nType=Application\nName=Text Editor\nExec=gedit %U\n\
                 [Desktop Action new]\nName=New Window\n";
        assert_eq!(desktop_entry_name(e).as_deref(), Some("Text Editor"));
        assert_eq!(
            desktop_entry_name("[Desktop Entry]\nName=X\nNoDisplay=true\n"),
            None
        );
        assert!(plain_name("org.gnome.TextEditor"));
        assert!(plain_name("gtk_test_app"));
        assert!(!plain_name("/bin/sh"));
        assert!(!plain_name("--help"));
        assert!(!plain_name("a b"));
        assert!(!plain_name("x;rm"));
    }
}
