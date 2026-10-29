//! Linux backend: AT-SPI 2 over D-Bus (`atspi` proxies on `zbus`, pure Rust), with an X11 or
//! Wayland clipboard for the paste fallback.
//!
//! - The accessibility bus address comes from `org.a11y.Bus` on the session bus; whether
//!   toolkits expose their trees is the `org.a11y.Status.IsEnabled` property, which
//!   [`request_permission`](crate::Accessibility::request_permission) turns on.
//! - Frontmost window: the toplevel with the `active` state. Focused element: a `Collection`
//!   match for the `focused` state (Firefox, Chromium), else a bounded breadth-first search
//!   (GTK's collection support returns nothing).
//! - Insertion: `EditableText.DeleteText` + `InsertText`, then the caret is moved after the
//!   text. Fallback: clipboard + `EditableText.PasteText` (works on Wayland too), else an XTest
//!   Ctrl+V on X11.
//! - Computer use (`computer`): apps and windows from the registry, element actions through the
//!   AT-SPI interfaces, key input (`input`: XTest, else the device event controller).

mod computer;
mod input;
mod wayland;
mod x11;

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use atspi::proxy::accessible::AccessibleProxyBlocking;
use atspi::proxy::bus::{BusProxyBlocking, StatusProxyBlocking};
use atspi::proxy::collection::CollectionProxyBlocking;
use atspi::proxy::component::ComponentProxyBlocking;
use atspi::proxy::document::DocumentProxyBlocking;
use atspi::proxy::editable_text::EditableTextProxyBlocking;
use atspi::proxy::text::TextProxyBlocking;
use atspi::{
    CoordType, Interface, InterfaceSet, MatchType, ObjectMatchRule, ObjectRefOwned, RelationType,
    SortOrder, State, StateSet,
};
use nugget_core::ipc::AppInfo;
use nugget_core::{Error, Result};
use zbus::blocking::Connection;
use zbus::proxy::CacheProperties;

use crate::clipboard::Clipboard;
use crate::engine::{FieldRead, Platform, Target};
use crate::role::{Role, role_from_atspi};
use crate::sanitize;
use crate::tree::{Element, Node, States};
use crate::verify::FieldState;
use crate::{Permission, Rect};

const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
const NULL_PATH: &str = "/org/a11y/atspi/null";
/// Per-call timeout: a hung app must not hang the caller.
const CALL_TIMEOUT: Duration = Duration::from_millis(1500);
/// Budget for the breadth-first focus search.
const FOCUS_SEARCH_NODES: usize = 6000;
const FOCUS_SEARCH_TIME: Duration = Duration::from_millis(800);
/// Text read for non-field nodes (labels, paragraphs) in snapshots, in characters.
const NODE_TEXT_CHARS: i32 = 4096;

fn zerr(e: zbus::Error) -> Error {
    Error::Other(format!("AT-SPI: {e}"))
}

fn fdo_err(e: zbus::fdo::Error) -> Error {
    Error::Other(format!("AT-SPI: {e}"))
}

/// AT-SPI platform.
pub struct AtspiPlatform {
    conn: Mutex<Option<Connection>>,
}

impl Default for AtspiPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl AtspiPlatform {
    pub fn new() -> Self {
        AtspiPlatform {
            conn: Mutex::new(None),
        }
    }

    fn session() -> Result<Connection> {
        zbus::blocking::connection::Builder::session()
            .and_then(|b| b.method_timeout(CALL_TIMEOUT).build())
            .map_err(zerr)
    }

    /// The accessibility bus, connected on first use and cached.
    fn bus(&self) -> Result<Connection> {
        let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        let session = Self::session()?;
        let addr = BusProxyBlocking::new(&session)
            .and_then(|p| p.get_address())
            .map_err(zerr)?;
        let conn = zbus::blocking::connection::Builder::address(addr.as_str())
            .and_then(|b| b.method_timeout(CALL_TIMEOUT).build())
            .map_err(zerr)?;
        *guard = Some(conn.clone());
        Ok(conn)
    }

    fn reset(&self) {
        *self.conn.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn status(session: &Connection) -> Result<StatusProxyBlocking<'static>> {
        StatusProxyBlocking::builder(session)
            .cache_properties(CacheProperties::No)
            .build()
            .map_err(zerr)
    }

    fn focus_inner(&self) -> Result<Option<Target<AtspiEl>>> {
        let conn = self.bus()?;
        let root = AtspiEl::new(&conn, REGISTRY, ROOT_PATH);
        let mut active = None;
        for app in root.child_refs(512) {
            let app_el = AtspiEl::from_ref(&conn, &app);
            let Some(app_el) = app_el else { continue };
            for win in app_el.child_refs(64) {
                let Some(win) = AtspiEl::from_ref(&conn, &win) else {
                    continue;
                };
                let Ok(states) = win.states() else { continue };
                if states.contains(State::Active) {
                    active = Some((app_el.clone(), win));
                    break;
                }
            }
            if active.is_some() {
                break;
            }
        }
        let Some((app_el, win)) = active else {
            return Ok(None);
        };
        let Some(focused) = find_focused(&win) else {
            return Ok(None);
        };
        let name = app_el.acc().name().unwrap_or_default();
        let app = AppInfo {
            id: app_id(&conn, &app_el.dest).or_else(|| (!name.is_empty()).then(|| name.clone())),
            name,
            window_title: win.acc().name().ok().filter(|t| !t.is_empty()),
            url: None,
        };
        Ok(Some(Target {
            app,
            element: focused,
        }))
    }
}

/// The AT-SPI role name (`GetRoleName`: "password text", "text", "push button"...).
///
/// GTK 4 answers `GetRoleName` with its own (ARIA) role names where its numeric role is the
/// AT-SPI one: "text box" for entries, text views and password entries alike (numeric
/// `PasswordText` marks a `GtkPasswordEntry`), and "application" for a `GtkApplicationWindow`
/// (numeric `Frame`). For those two names the numeric role decides.
fn role_name(acc: &AccessibleProxyBlocking<'_>) -> String {
    let name = acc.get_role_name().unwrap_or_default();
    if !matches!(name.as_str(), "text box" | "application") {
        return name;
    }
    match acc.get_role() {
        Ok(atspi::Role::PasswordText) => "password text".into(),
        Ok(atspi::Role::Frame) => "frame".into(),
        Ok(atspi::Role::Window) => "window".into(),
        Ok(atspi::Role::Dialog) => "dialog".into(),
        _ => name,
    }
}

/// The end offset for `Text.GetText(0, end)`: the character count, capped. `-1` ("to the end")
/// only when the count is unknown: GTK 4's entries answer `GetText(0, -1)` with "".
fn text_end(count: i32, max_chars: i32) -> i32 {
    if count > 0 { count.min(max_chars) } else { -1 }
}

/// A live AT-SPI object: bus name and object path on the accessibility bus.
#[derive(Clone)]
pub struct AtspiEl {
    conn: Connection,
    dest: String,
    path: String,
}

impl AtspiEl {
    fn new(conn: &Connection, dest: &str, path: &str) -> Self {
        AtspiEl {
            conn: conn.clone(),
            dest: dest.to_string(),
            path: path.to_string(),
        }
    }

    fn from_ref(conn: &Connection, r: &ObjectRefOwned) -> Option<Self> {
        if r.is_null() || r.path_as_str() == NULL_PATH {
            return None;
        }
        let name = r.name_as_str()?;
        Some(Self::new(conn, name, r.path_as_str()))
    }

    fn acc(&self) -> AccessibleProxyBlocking<'static> {
        // Building a proxy with property caching off makes no D-Bus call.
        AccessibleProxyBlocking::builder(&self.conn)
            .destination(self.dest.clone())
            .and_then(|b| b.path(self.path.clone()))
            .map(|b| b.cache_properties(CacheProperties::No))
            .and_then(|b| b.build())
            .expect("valid bus name and object path from the registry")
    }

    fn text(&self) -> Option<TextProxyBlocking<'static>> {
        TextProxyBlocking::builder(&self.conn)
            .destination(self.dest.clone())
            .ok()?
            .path(self.path.clone())
            .ok()?
            .cache_properties(CacheProperties::No)
            .build()
            .ok()
    }

    fn editable(&self) -> Option<EditableTextProxyBlocking<'static>> {
        EditableTextProxyBlocking::builder(&self.conn)
            .destination(self.dest.clone())
            .ok()?
            .path(self.path.clone())
            .ok()?
            .cache_properties(CacheProperties::No)
            .build()
            .ok()
    }

    fn component(&self) -> Option<ComponentProxyBlocking<'static>> {
        ComponentProxyBlocking::builder(&self.conn)
            .destination(self.dest.clone())
            .ok()?
            .path(self.path.clone())
            .ok()?
            .cache_properties(CacheProperties::No)
            .build()
            .ok()
    }

    fn document(&self) -> Option<DocumentProxyBlocking<'static>> {
        DocumentProxyBlocking::builder(&self.conn)
            .destination(self.dest.clone())
            .ok()?
            .path(self.path.clone())
            .ok()?
            .cache_properties(CacheProperties::No)
            .build()
            .ok()
    }

    fn collection(&self) -> Option<CollectionProxyBlocking<'static>> {
        CollectionProxyBlocking::builder(&self.conn)
            .destination(self.dest.clone())
            .ok()?
            .path(self.path.clone())
            .ok()?
            .cache_properties(CacheProperties::No)
            .build()
            .ok()
    }

    fn states(&self) -> Result<StateSet> {
        self.acc().get_state().map_err(zerr)
    }

    fn interfaces(&self) -> InterfaceSet {
        self.acc()
            .get_interfaces()
            .unwrap_or_else(|_| InterfaceSet::new(Interface::Accessible))
    }

    fn child_refs(&self, max: usize) -> Vec<ObjectRefOwned> {
        let mut v = self.acc().get_children().unwrap_or_default();
        v.truncate(max);
        v
    }

    fn bounds(&self) -> Option<Rect> {
        let (x, y, w, h) = self.component()?.get_extents(CoordType::Screen).ok()?;
        rect(x, y, w, h)
    }

    /// Text interface contents, up to `max_chars`, with the character count.
    fn text_contents(&self, max_chars: i32) -> Option<(String, i32)> {
        let t = self.text()?;
        let count = t.character_count().ok()?;
        let end = text_end(count, max_chars);
        let s = t.get_text(0, end).ok()?;
        Some((s, count))
    }

    fn labelled_by(&self) -> Option<String> {
        let rels = self.acc().get_relation_set().ok()?;
        for (kind, targets) in rels {
            if kind == RelationType::LabelledBy {
                for t in targets {
                    if let Some(el) = AtspiEl::from_ref(&self.conn, &t)
                        && let Ok(n) = el.acc().name()
                        && !n.trim().is_empty()
                    {
                        return Some(n);
                    }
                }
            }
        }
        None
    }

    fn read_node(&self, with_text: bool) -> (Node, InterfaceSet, HashMap<String, String>) {
        let acc = self.acc();
        let role_name = role_name(&acc);
        let st = acc.get_state().unwrap_or_else(|_| StateSet::empty());
        let attrs = acc.get_attributes().unwrap_or_default();
        let ifaces = self.interfaces();
        let role = role_from_atspi(&role_name, attrs.get("xml-roles").map(String::as_str));
        let secure = role_name == "password text";
        let multiline = if st.contains(State::MultiLine) {
            Some(true)
        } else if st.contains(State::SingleLine) {
            Some(false)
        } else {
            None
        };
        let (disabled, checked, expanded) = computer::extra_states(&role_name, &st);
        let states = States {
            focused: st.contains(State::Focused),
            editable: st.contains(State::Editable) && !st.contains(State::ReadOnly),
            secure,
            multiline,
            selected: st.contains(State::Selected),
            offscreen: !st.contains(State::Showing),
            disabled,
            checked,
            expanded,
        };
        let name = acc.name().ok().filter(|s| !s.trim().is_empty());
        let description = if name.is_none() {
            acc.description().ok().filter(|s| !s.trim().is_empty())
        } else {
            None
        };
        let mut node = Node {
            role,
            raw_role: role_name,
            name,
            description,
            states,
            bounds: if ifaces.contains(Interface::Component) {
                self.bounds()
            } else {
                None
            },
            ..Default::default()
        };
        node.placeholder = attrs
            .get("placeholder-text")
            .or_else(|| attrs.get("placeholder"))
            .cloned();
        if let Some(x) = attrs.get("xml-roles") {
            node.attrs.insert("xml_roles".into(), x.clone());
        }
        if let Some(id) = attrs.get("id") {
            node.attrs.insert("identifier".into(), id.clone());
        }
        if (role == Role::WebArea || role == Role::Document)
            && let Some(url) = self
                .document()
                .and_then(|d| d.get_attribute_value("DocURL").ok())
                .filter(|u| !u.is_empty())
        {
            node.attrs.insert("url".into(), url);
        }
        // Text for leaves and text roles; never for secure fields.
        let wants_text = !secure
            && ifaces.contains(Interface::Text)
            && (with_text
                || role.is_text()
                || role.is_text_input()
                || acc.child_count().unwrap_or(1) == 0);
        if wants_text && let Some((t, _)) = self.text_contents(NODE_TEXT_CHARS) {
            let t: String = t.chars().filter(|&c| c != '\u{FFFC}').collect();
            // Labels repeat their name as text; keep the value only when it adds something
            // (always for inputs).
            let repeats_name = Some(t.trim()) == node.name.as_deref().map(str::trim);
            if !t.trim().is_empty() && (role.is_text_input() || !repeats_name) {
                node.value = Some(t);
            }
        }
        (node, ifaces, attrs)
    }
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Option<Rect> {
    // Toolkits report (-1, -1, -1, -1) or (0, 0, 0, 0) for unknown extents.
    if w <= 0 || h <= 0 || (x == -1 && y == -1) {
        return None;
    }
    Some(Rect {
        x: f64::from(x),
        y: f64::from(y),
        width: f64::from(w),
        height: f64::from(h),
    })
}

impl Element for AtspiEl {
    fn node(&self) -> Node {
        self.read_node(false).0
    }

    fn children(&self, max: usize) -> Vec<Self> {
        self.child_refs(max)
            .iter()
            .filter_map(|r| AtspiEl::from_ref(&self.conn, r))
            .collect()
    }

    fn parent(&self) -> Option<Self> {
        let r = self.acc().parent().ok()?;
        AtspiEl::from_ref(&self.conn, &r)
    }

    fn same(&self, other: &Self) -> bool {
        self.dest == other.dest && self.path == other.path
    }

    fn actions(&self) -> Vec<crate::computer::UiActionKind> {
        self.ui_actions()
    }

    fn perform(&self, action: &crate::computer::UiAction) -> Result<()> {
        self.ui_perform(action)
    }
}

/// The focused element inside `win`: a collection match, else a bounded breadth-first search
/// that skips subtrees which are not showing.
fn find_focused(win: &AtspiEl) -> Option<AtspiEl> {
    if let Some(coll) = win.collection() {
        let rule = ObjectMatchRule::builder()
            .states([State::Focused], MatchType::All)
            .build();
        if let Ok(found) = coll.get_matches(rule, SortOrder::Canonical, 1, true)
            && let Some(el) = found.first().and_then(|r| AtspiEl::from_ref(&win.conn, r))
        {
            return Some(el);
        }
    }
    let deadline = Instant::now() + FOCUS_SEARCH_TIME;
    let mut queue = VecDeque::from([win.clone()]);
    let mut seen = 0usize;
    while let Some(el) = queue.pop_front() {
        seen += 1;
        if seen > FOCUS_SEARCH_NODES || Instant::now() > deadline {
            break;
        }
        let Ok(st) = el.states() else { continue };
        if st.contains(State::Focused) && seen > 1 {
            return Some(el);
        }
        if seen > 1 && !st.contains(State::Showing) {
            continue;
        }
        // Lists that manage their descendants can be huge (and hold no text fields).
        if st.contains(State::ManagesDescendants) && !st.contains(State::Editable) {
            continue;
        }
        queue.extend(el.children(512));
    }
    None
}

/// A desktop-file id for the app owning `bus_name`, from its process: the launcher's
/// `GIO_LAUNCHED_DESKTOP_FILE`, a Flatpak app id, a Snap name, else the executable name.
fn app_id(conn: &Connection, bus_name: &str) -> Option<String> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn).ok()?;
    let name = zbus::names::BusName::try_from(bus_name.to_string()).ok()?;
    let pid = dbus
        .get_connection_unix_process_id(name)
        .map_err(fdo_err)
        .ok()?;
    let proc = format!("/proc/{pid}");
    if let Ok(env) = std::fs::read(format!("{proc}/environ")) {
        for var in env.split(|&b| b == 0) {
            let var = String::from_utf8_lossy(var);
            if let Some(f) = var.strip_prefix("GIO_LAUNCHED_DESKTOP_FILE=")
                && let Some(stem) = std::path::Path::new(f).file_stem()
            {
                return Some(stem.to_string_lossy().into_owned());
            }
            if let Some(s) = var.strip_prefix("FLATPAK_ID=") {
                return Some(s.to_string());
            }
            if let Some(s) = var.strip_prefix("SNAP_NAME=") {
                return Some(s.to_string());
            }
        }
    }
    if let Ok(exe) = std::fs::read_link(format!("{proc}/exe")) {
        let base = exe.file_name()?.to_string_lossy().into_owned();
        // Interpreters say little; prefer the script or the process name.
        if !matches!(
            base.as_str(),
            "python3" | "python" | "node" | "electron" | "java"
        ) && !base.starts_with("python3.")
        {
            return Some(base);
        }
    }
    let comm = std::fs::read_to_string(format!("{proc}/comm")).ok()?;
    let comm = comm.trim();
    (!comm.is_empty()).then(|| comm.to_string())
}

/// Byte length for GTK/Gecko/Chromium (ATK passes it to `gtk_editable_insert_text` and kin);
/// Qt counts UTF-16 units.
fn insert_length(text: &str, toolkit: Option<&str>) -> i32 {
    let n = match toolkit {
        Some(t) if t.eq_ignore_ascii_case("qt") => text.encode_utf16().count(),
        _ => text.len(),
    };
    i32::try_from(n).unwrap_or(i32::MAX)
}

impl AtspiPlatform {
    fn read_state(el: &AtspiEl, max_value_bytes: usize, secure: bool) -> FieldState {
        if secure {
            return FieldState::default();
        }
        let Some(t) = el.text() else {
            return FieldState::default();
        };
        let count = t.character_count().unwrap_or(0).max(0);
        // Read up to max_value_bytes characters; the byte cap is applied after cleaning.
        let max_chars = i32::try_from(max_value_bytes).unwrap_or(i32::MAX);
        let end = text_end(count, max_chars);
        let raw = t.get_text(0, end).unwrap_or_default();
        let value = sanitize::clean_value(&raw, max_value_bytes);
        let truncated = count > max_chars || value.len() < raw.len();
        let selection = match t.get_n_selections() {
            Ok(n) if n > 0 => t
                .get_selection(0)
                .ok()
                .map(|(s, e)| (s.max(0) as u32, e.max(0) as u32)),
            _ => None,
        }
        .or_else(|| {
            t.caret_offset()
                .ok()
                .filter(|&c| c >= 0)
                .map(|c| (c as u32, c as u32))
        });
        FieldState {
            value,
            selection,
            truncated,
        }
    }
}

impl Platform for AtspiPlatform {
    type Element = AtspiEl;

    fn permission(&self) -> Permission {
        let Ok(session) = Self::session() else {
            return Permission::Denied;
        };
        match Self::status(&session).and_then(|s| s.is_enabled().map_err(zerr)) {
            Ok(true) => Permission::Granted,
            _ => Permission::Denied,
        }
    }

    fn request_permission(&self) -> Result<()> {
        let session = Self::session()?;
        Self::status(&session)?.set_is_enabled(true).map_err(zerr)?;
        // Toolkits started before this only expose their trees after a restart.
        self.reset();
        Ok(())
    }

    fn focus(&self) -> Result<Option<Target<AtspiEl>>> {
        match self.focus_inner() {
            Ok(t) => Ok(t),
            Err(e) => {
                // A stale bus connection (registry restarted): reconnect once.
                self.reset();
                tracing::debug!(error = %e, "AT-SPI focus lookup failed; reconnecting");
                self.focus_inner()
            }
        }
    }

    fn read_field(&self, el: &AtspiEl, max_value_bytes: usize) -> Result<FieldRead> {
        let (node, _ifaces, _attrs) = el.read_node(false);
        let label = if node.name.is_none() {
            el.labelled_by()
        } else {
            None
        };
        let state = Self::read_state(el, max_value_bytes, node.states.secure);
        Ok(FieldRead { node, label, state })
    }

    fn caret_bounds(&self, el: &AtspiEl, state: &FieldState) -> Option<Rect> {
        let t = el.text()?;
        let (start, end) = state.selection?;
        if start != end {
            let (x, y, w, h) = t
                .get_range_extents(start as i32, end as i32, CoordType::Screen)
                .ok()?;
            return rect(x, y, w, h);
        }
        let count = t.character_count().ok()?;
        let caret = start as i32;
        if caret < count
            && let Ok((x, y, w, h)) = t.get_character_extents(caret, CoordType::Screen)
            && let Some(r) = rect(x, y, w.max(1), h)
        {
            return Some(Rect { width: 1.0, ..r });
        }
        if caret > 0 {
            // At the end of the text: just after the previous character.
            let (x, y, w, h) = t.get_character_extents(caret - 1, CoordType::Screen).ok()?;
            let r = rect(x, y, w.max(1), h)?;
            return Some(Rect {
                x: r.x + r.width,
                width: 1.0,
                ..r
            });
        }
        None
    }

    fn insert_text(&self, el: &AtspiEl, text: &str, before: &FieldState) -> Result<()> {
        let (_, ifaces, attrs) = el.read_node(false);
        if !ifaces.contains(Interface::EditableText) {
            return Err(Error::Unsupported("element has no EditableText".into()));
        }
        let ed = el
            .editable()
            .ok_or_else(|| Error::Unsupported("EditableText proxy".into()))?;
        let (start, end) = before.selection.unwrap_or_else(|| {
            let n = before.value.chars().count() as u32;
            (n, n)
        });
        let (start, end) = (start.min(end) as i32, start.max(end) as i32);
        if end > start && !ed.delete_text(start, end).map_err(zerr)? {
            return Err(Error::Other("DeleteText refused".into()));
        }
        let len = insert_length(text, attrs.get("toolkit").map(String::as_str));
        if !ed.insert_text(start, text, len).map_err(zerr)? {
            return Err(Error::Other("InsertText refused".into()));
        }
        if let Some(t) = el.text() {
            let _ = t.set_caret_offset(start + text.chars().count() as i32);
        }
        Ok(())
    }

    fn clipboard(&self) -> Result<Box<dyn Clipboard>> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            match wayland::WaylandClipboard::new() {
                Ok(c) => return Ok(Box::new(c)),
                Err(e) => tracing::debug!(error = %e, "Wayland clipboard unavailable"),
            }
        }
        Ok(Box::new(x11::X11Clipboard::new()?))
    }

    fn paste(&self, el: &AtspiEl) -> Result<()> {
        // A real Ctrl+V on X11 behaves exactly like the user pasting. GTK 3's
        // `EditableText.PasteText` on text views corrupts state (it pastes asynchronously into a
        // freed buffer), so it is only the fallback, for Wayland sessions without XTest.
        let x11_session =
            std::env::var_os("DISPLAY").is_some() && std::env::var_os("WAYLAND_DISPLAY").is_none();
        if x11_session {
            match x11::paste_ctrl_v() {
                Ok(()) => return Ok(()),
                Err(e) => tracing::debug!(error = %e, "XTest paste failed; trying PasteText"),
            }
        }
        let ed = el
            .editable()
            .ok_or_else(|| Error::Unsupported("no way to paste into this element".into()))?;
        let pos = el
            .text()
            .and_then(|t| t.caret_offset().ok())
            .unwrap_or(0)
            .max(0);
        if ed.paste_text(pos).map_err(zerr)? {
            Ok(())
        } else {
            Err(Error::Unsupported("PasteText refused".into()))
        }
    }

    fn apps(&self) -> Result<Vec<crate::computer::AppEntry>> {
        self.cu_apps()
    }

    fn windows(&self) -> Result<Vec<crate::engine::PlatformWindow<AtspiEl>>> {
        self.cu_windows()
    }

    fn key_input(&self, input: &crate::computer::KeyInput) -> Result<()> {
        self.cu_key_input(input)
    }

    fn open_app(&self, name: &str) -> Result<()> {
        self.cu_open_app(name)
    }

    fn screenshot(&self, window: &AtspiEl) -> Result<Option<Vec<u8>>> {
        self.cu_screenshot(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_length_by_toolkit() {
        assert_eq!(
            insert_length("héllo ✓", Some("gtk")),
            "héllo ✓".len() as i32
        );
        assert_eq!(insert_length("héllo ✓", None), 10);
        assert_eq!(insert_length("a😀", Some("Qt")), 3);
    }

    #[test]
    fn unknown_extents_are_none() {
        assert_eq!(rect(-1, -1, -1, -1), None);
        assert_eq!(rect(0, 0, 0, 0), None);
        assert!(rect(10, 20, 5, 14).is_some());
    }
}
