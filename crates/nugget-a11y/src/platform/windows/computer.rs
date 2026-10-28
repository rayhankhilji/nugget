//! Computer use on Windows (M4): apps and windows, element actions, key input and app launch.
//!
//! - Windows: `EnumWindows` (z-order) keeps visible, uncloaked, titled top-level windows that are
//!   not tool windows. The native id is the HWND. The app is the owning process (pid from
//!   `GetWindowThreadProcessId`, exe from `QueryFullProcessImageNameW`); frontmost is the
//!   process of `GetForegroundWindow`. The element is `ElementFromHandle`.
//! - Actions go through UIA patterns: Invoke/Toggle (press), Value/RangeValue (set value),
//!   SelectionItem (select), `SetFocus`, ScrollItem/Scroll (scroll), ExpandCollapse (expand).
//!   Which ones an element supports comes from its cached pattern-availability properties.
//! - Keys: `SendInput`. Text goes as `KEYEVENTF_UNICODE` units, never as shortcuts; named keys
//!   and chords use virtual-key codes (`VkKeyScanW` for characters beyond letters and digits).
//! - Launch: bring a running app's window to the front (`SetForegroundWindow`, attaching to
//!   the foreground thread's input when the foreground lock refuses); otherwise start
//!   `explorer.exe shell:AppsFolder\<AUMID>` for a packaged app id, or the exe from the
//!   registry's App Paths or `PATH`, without a shell.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::process::{Command, Stdio};

use nugget_core::{Error, Result};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::{
    ScrollAmount, ScrollAmount_NoAmount, ScrollAmount_SmallDecrement, ScrollAmount_SmallIncrement,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MapVirtualKeyW, SendInput, VIRTUAL_KEY,
    VkKeyScanW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GW_OWNER, GWL_EXSTYLE, GetForegroundWindow, GetWindow,
    GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE, SetForegroundWindow,
    ShowWindow, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, HSTRING, PCWSTR};

use super::uia::{self, Caps, UiaEl};
use super::scale_rect;
use crate::Rect;
use crate::computer::{
    AppEntry, Key, KeyChord, KeyInput, Modifiers, NamedKey, ScrollDirection, UiAction,
    UiActionKind,
};
use crate::engine::PlatformWindow;
use crate::tree::States;

/// Most `ScrollPattern` steps one scroll action takes.
const MAX_SCROLL_STEPS: u32 = 50;
/// Ancestors searched for a scrollable container.
const MAX_SCROLL_ANCESTORS: usize = 12;

// --- Element state and actions ---

/// Element states from the pattern properties.
pub(super) fn apply_caps(states: &mut States, caps: &Caps) {
    states.disabled = !caps.enabled;
    states.checked = if caps.toggle {
        caps.toggle_state.map(|s| s == 1)
    } else {
        None
    };
    states.expanded = match caps.expand_state {
        Some(0) => Some(false),
        Some(1 | 2) => Some(true),
        _ => None,
    };
    states.selected |= caps.selected;
}

/// The action kinds an element supports, from its patterns.
fn kinds_from_caps(caps: &Caps, focusable: bool, secure: bool) -> Vec<UiActionKind> {
    let mut k = Vec::new();
    if caps.invoke || caps.toggle {
        k.push(UiActionKind::Press);
    }
    if !secure
        && ((caps.value && !caps.value_read_only) || (caps.range_value && !caps.range_read_only))
    {
        k.push(UiActionKind::SetValue);
    }
    if caps.selection_item {
        k.push(UiActionKind::Select);
    }
    if focusable {
        k.push(UiActionKind::Focus);
    }
    if caps.scroll || caps.scroll_item {
        k.push(UiActionKind::Scroll);
    }
    if caps.expand_collapse && caps.expand_state != Some(3) {
        k.push(UiActionKind::Expand);
    }
    k
}

pub(super) fn actions(el: &UiaEl) -> Vec<UiActionKind> {
    let info = el.info();
    kinds_from_caps(&el.caps(), info.is_keyboard_focusable, info.is_password)
}

/// `(horizontal, vertical)` scroll amounts for one step.
fn scroll_step(direction: ScrollDirection) -> (ScrollAmount, ScrollAmount) {
    match direction {
        ScrollDirection::Up => (ScrollAmount_NoAmount, ScrollAmount_SmallDecrement),
        ScrollDirection::Down => (ScrollAmount_NoAmount, ScrollAmount_SmallIncrement),
        ScrollDirection::Left => (ScrollAmount_SmallDecrement, ScrollAmount_NoAmount),
        ScrollDirection::Right => (ScrollAmount_SmallIncrement, ScrollAmount_NoAmount),
    }
}

fn scroll(el: &UiaEl, direction: ScrollDirection, amount: u32) -> Result<()> {
    let mut cur = Some(el.clone());
    let mut target = None;
    for _ in 0..=MAX_SCROLL_ANCESTORS {
        let Some(e) = cur else { break };
        if e.caps().scroll {
            target = Some(e);
            break;
        }
        cur = e.parent_element();
    }
    let target = target
        .ok_or_else(|| Error::Unsupported("no scrollable container around the element".into()))?;
    let (h, v) = scroll_step(direction);
    for _ in 0..amount.min(MAX_SCROLL_STEPS) {
        target.scroll(h, v)?;
    }
    Ok(())
}

pub(super) fn perform(el: &UiaEl, action: &UiAction) -> Result<()> {
    let caps = el.caps();
    match action {
        UiAction::Press if caps.invoke => el.invoke(),
        UiAction::Press if caps.toggle => el.toggle(),
        UiAction::Press => Err(Error::Unsupported("the element cannot be pressed".into())),
        UiAction::SetValue { value } => {
            if el.info().is_password {
                return Err(Error::Invalid("refusing to set a secure field".into()));
            }
            if caps.value && !caps.value_read_only {
                el.set_value(value)
            } else if caps.range_value && !caps.range_read_only {
                let n: f64 = value
                    .trim()
                    .parse()
                    .map_err(|_| Error::Invalid(format!("{value:?} is not a number")))?;
                el.set_range_value(n)
            } else {
                Err(Error::Unsupported("the value is read-only".into()))
            }
        }
        UiAction::Select => el.select(),
        UiAction::Focus => el.set_focus(),
        UiAction::Scroll { amount: 0, .. } => el.scroll_into_view(),
        UiAction::Scroll { direction, amount } => scroll(el, *direction, *amount),
        UiAction::Expand { expand } => el.expand_collapse(*expand),
    }
}

// --- Apps and windows ---

struct TopWindow {
    hwnd: HWND,
    pid: u32,
    title: String,
    bounds: Option<Rect>,
    minimized: bool,
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Vec<HWND>` that `top_level_windows` passes to EnumWindows,
    // alive and not otherwise borrowed for the duration of the enumeration.
    let v = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    v.push(hwnd);
    BOOL(1)
}

fn window_text(hwnd: HWND) -> String {
    // SAFETY: a length, 0 when untitled or invalid.
    let len = unsafe { GetWindowTextLengthW(hwnd) }.max(0) as usize;
    if len == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len + 1];
    // SAFETY: `buf` has room for `len` characters and the terminator.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: `cloaked` is a valid 4-byte out buffer for DWMWA_CLOAKED.
    let r = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    r.is_ok() && cloaked != 0
}

/// A window the user can switch to: visible, not cloaked (other virtual desktops, suspended
/// UWP frames), titled, and not a tool window.
fn describe(hwnd: HWND) -> Option<TopWindow> {
    // SAFETY (all calls): `hwnd` came from EnumWindows; calls on a window destroyed since then
    // fail and are treated as "skip".
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0 {
            return None;
        }
        if is_cloaked(hwnd) {
            return None;
        }
        let title = window_text(hwnd);
        let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid());
        if title.trim().is_empty() && (owned || ex & WS_EX_APPWINDOW.0 == 0) {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let mut r = RECT::default();
        let bounds = GetWindowRect(hwnd, &mut r).ok().and_then(|()| {
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            (w > 0 && h > 0).then(|| {
                scale_rect(
                    Rect {
                        x: f64::from(r.left),
                        y: f64::from(r.top),
                        width: f64::from(w),
                        height: f64::from(h),
                    },
                    uia::window_scale(hwnd),
                )
            })
        });
        Some(TopWindow {
            hwnd,
            pid,
            title,
            bounds,
            minimized: IsIconic(hwnd).as_bool(),
        })
    }
}

/// Switchable top-level windows, front to back.
fn top_level_windows() -> Vec<TopWindow> {
    let mut hwnds: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `hwnds`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut hwnds as *mut Vec<HWND> as isize)) };
    hwnds.into_iter().filter_map(describe).collect()
}

fn foreground_pid() -> Option<u32> {
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out-pointer.
    unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

/// `chrome.exe` → `chrome`, as `focus()` names apps.
fn app_name(exe: &str) -> String {
    let lower = exe.to_ascii_lowercase();
    match lower.strip_suffix(".exe") {
        Some(_) => exe[..exe.len() - 4].to_string(),
        None => exe.to_string(),
    }
}

struct Listing {
    windows: Vec<TopWindow>,
    apps: HashMap<u32, AppEntry>,
}

fn listing() -> Listing {
    let windows = top_level_windows();
    let front = foreground_pid();
    let mut apps = HashMap::new();
    for w in &windows {
        apps.entry(w.pid).or_insert_with(|| {
            let exe = uia::exe_name(w.pid);
            AppEntry {
                name: exe.as_deref().map(app_name).unwrap_or_default(),
                id: exe,
                pid: Some(w.pid),
                frontmost: Some(w.pid) == front,
            }
        });
    }
    Listing { windows, apps }
}

pub(super) fn apps() -> Result<Vec<AppEntry>> {
    let l = listing();
    let mut seen = Vec::new();
    let mut out: Vec<AppEntry> = Vec::new();
    for w in &l.windows {
        if !seen.contains(&w.pid) {
            seen.push(w.pid);
            out.extend(l.apps.get(&w.pid).cloned());
        }
    }
    out.sort_by_key(|a| !a.frontmost);
    Ok(out)
}

pub(super) fn windows() -> Result<Vec<PlatformWindow<UiaEl>>> {
    let l = listing();
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    let mut out = Vec::new();
    for w in l.windows {
        let Some(app) = l.apps.get(&w.pid).cloned() else {
            continue;
        };
        let Ok(element) = UiaEl::from_hwnd(w.hwnd) else {
            continue;
        };
        out.push(PlatformWindow {
            app,
            native_id: format!("hwnd:{:x}", w.hwnd.0 as usize),
            title: (!w.title.is_empty()).then_some(w.title),
            bounds: w.bounds,
            focused: w.hwnd == fg,
            minimized: w.minimized,
            element,
        });
    }
    out.sort_by_key(|w| !w.focused);
    Ok(out)
}

// --- Key input ---

const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_DELETE: u16 = 0x2E;
const VK_LWIN: u16 = 0x5B;
const VK_F1: u16 = 0x70;

fn named_vk(k: NamedKey) -> Option<u16> {
    Some(match k {
        NamedKey::Enter => VK_RETURN,
        NamedKey::Tab => VK_TAB,
        NamedKey::Escape => VK_ESCAPE,
        NamedKey::Backspace => VK_BACK,
        NamedKey::Delete => VK_DELETE,
        NamedKey::Space => VK_SPACE,
        NamedKey::PageUp => 0x21,
        NamedKey::PageDown => 0x22,
        NamedKey::End => 0x23,
        NamedKey::Home => 0x24,
        NamedKey::Left => 0x25,
        NamedKey::Up => 0x26,
        NamedKey::Right => 0x27,
        NamedKey::Down => 0x28,
        NamedKey::F(n @ 1..=12) => VK_F1 + u16::from(n - 1),
        NamedKey::F(_) => return None,
    })
}

/// Letters, digits and space, which have layout-independent virtual keys; and whether Shift is
/// needed.
fn ascii_vk(c: char) -> Option<(u16, bool)> {
    match c {
        'a'..='z' => Some((c.to_ascii_uppercase() as u16, false)),
        'A'..='Z' | '0'..='9' => Some((c as u16, c.is_ascii_uppercase())),
        ' ' => Some((VK_SPACE, false)),
        _ => None,
    }
}

/// The virtual key and the modifiers the current keyboard layout needs for `c`.
fn char_vk(c: char) -> Option<(u16, Modifiers)> {
    if let Some((vk, shift)) = ascii_vk(c) {
        return Some((
            vk,
            Modifiers {
                shift,
                ..Default::default()
            },
        ));
    }
    let unit = u16::try_from(u32::from(c)).ok()?;
    // SAFETY: no preconditions; -1 when the layout has no key for the character.
    let r = unsafe { VkKeyScanW(unit) };
    if r == -1 {
        return None;
    }
    let (vk, state) = ((r & 0xff) as u16, (r >> 8) & 0xff);
    Some((
        vk,
        Modifiers {
            shift: state & 1 != 0,
            ctrl: state & 2 != 0,
            alt: state & 4 != 0,
            meta: false,
        },
    ))
}

/// Keys that need `KEYEVENTF_EXTENDEDKEY` (navigation cluster, Delete, Windows key).
fn is_extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | VK_DELETE | VK_LWIN)
}

/// Modifier keys to hold, in press order.
fn modifier_vks(m: Modifiers) -> Vec<u16> {
    let mut v = Vec::new();
    if m.ctrl {
        v.push(VK_CONTROL);
    }
    if m.alt {
        v.push(VK_MENU);
    }
    if m.shift {
        v.push(VK_SHIFT);
    }
    if m.meta {
        v.push(VK_LWIN);
    }
    v
}

fn kb(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn vk_event(vk: u16, down: bool) -> INPUT {
    // SAFETY: no preconditions; 0 when the key has no scan code.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16;
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    kb(vk, scan, flags)
}

fn send(inputs: &[INPUT]) -> Result<()> {
    for chunk in inputs.chunks(128) {
        // SAFETY: `chunk` is a valid array of keyboard INPUTs and its element size is passed.
        let sent = unsafe { SendInput(chunk, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != chunk.len() {
            return Err(Error::Other(
                "SendInput was blocked (the target may run elevated)".into(),
            ));
        }
    }
    Ok(())
}

fn text_inputs(text: &str) -> Vec<INPUT> {
    let mut out = Vec::new();
    for c in text.chars() {
        match c {
            '\n' | '\t' => {
                let vk = if c == '\n' { VK_RETURN } else { VK_TAB };
                out.push(vk_event(vk, true));
                out.push(vk_event(vk, false));
            }
            c if c.is_control() => {}
            c => {
                let mut tmp = [0u16; 2];
                for &u in c.encode_utf16(&mut tmp).iter() {
                    out.push(kb(0, u, KEYEVENTF_UNICODE));
                    out.push(kb(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
            }
        }
    }
    out
}

fn chord_inputs(chord: &KeyChord) -> Result<Vec<INPUT>> {
    let (vk, extra) = match chord.key {
        Key::Named(k) => (named_vk(k), Modifiers::default()),
        Key::Char(c) => match char_vk(c) {
            Some((vk, m)) => (Some(vk), m),
            None => (None, Modifiers::default()),
        },
    };
    let vk = vk.ok_or_else(|| Error::Invalid(format!("no key code for {:?}", chord.key)))?;
    let m = chord.modifiers;
    let held = modifier_vks(Modifiers {
        ctrl: m.ctrl || extra.ctrl,
        alt: m.alt || extra.alt,
        shift: m.shift || extra.shift,
        meta: m.meta || extra.meta,
    });
    let mut out: Vec<INPUT> = held.iter().map(|&k| vk_event(k, true)).collect();
    out.push(vk_event(vk, true));
    out.push(vk_event(vk, false));
    out.extend(held.iter().rev().map(|&k| vk_event(k, false)));
    Ok(out)
}

pub(super) fn key_input(input: &KeyInput) -> Result<()> {
    match input {
        KeyInput::Text(t) => send(&text_inputs(t)),
        KeyInput::Keys(chords) => {
            for c in chords {
                send(&chord_inputs(c)?)?;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(())
        }
    }
}

// --- Launch and activate ---

/// App names we launch: a plain name, exe name or packaged app id; no paths or options.
fn valid_app_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name.starts_with('-')
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | '/' | ':' | '"' | '<' | '>' | '|' | '*' | '?'))
}

fn app_matches(e: &AppEntry, name: &str) -> bool {
    e.name.eq_ignore_ascii_case(name)
        || e.id.as_deref().is_some_and(|id| id.eq_ignore_ascii_case(name))
}

/// Bring `hwnd` to the front. Windows only lets the foreground process move the foreground
/// ("foreground lock"); a background caller first attaches its input queue to the foreground
/// thread, the documented way for a process acting on the user's behalf.
fn activate(hwnd: HWND) -> Result<()> {
    // SAFETY (all calls): `hwnd` is a top-level window from EnumWindows; the thread ids come
    // from the system, and the input attachment is undone before returning.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd).as_bool() {
            return Ok(());
        }
        let fg_thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let me = GetCurrentThreadId();
        let attached =
            fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let ok = SetForegroundWindow(hwnd).as_bool();
        if attached {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
        if ok {
            Ok(())
        } else {
            Err(Error::Other(
                "Windows refused to bring the window to the front (foreground lock)".into(),
            ))
        }
    }
}

/// The registered path of `exe` under `App Paths` (how Run and the shell find `chrome.exe`).
fn app_path(exe: &str) -> Option<String> {
    let key = HSTRING::from(format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{exe}"
    ));
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        if let Some(p) = reg_default_string(root, &key) {
            let p = p.trim().trim_matches('"').to_string();
            if !p.is_empty() {
                return Some(p);
            }
        }
    }
    None
}

fn reg_default_string(root: HKEY, key: &HSTRING) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `size` bytes; RegGetValueW writes at most that and updates `size`.
    let err = unsafe {
        RegGetValueW(
            root,
            key,
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if err.0 != 0 {
        return None;
    }
    let n = (size as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    Some(s.trim_end_matches('\0').to_string())
}

fn spawn(program: &str, arg: Option<&str>) -> Result<()> {
    let mut cmd = Command::new(program);
    if let Some(a) = arg {
        cmd.arg(a);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| Error::NotFound(format!("could not start {program:?}: {e}")))
}

fn launch(name: &str) -> Result<()> {
    if name.contains('!') {
        // A packaged app's AppUserModelID (`Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`).
        let explorer = std::env::var("WINDIR")
            .map(|w| format!("{w}\\explorer.exe"))
            .unwrap_or_else(|_| "explorer.exe".into());
        return spawn(&explorer, Some(&format!("shell:AppsFolder\\{name}")));
    }
    let exe = if name.to_ascii_lowercase().ends_with(".exe") {
        name.to_string()
    } else {
        format!("{name}.exe")
    };
    match app_path(&exe) {
        Some(path) => spawn(&path, None),
        None => spawn(&exe, None),
    }
}

pub(super) fn open_app(name: &str) -> Result<()> {
    let name = name.trim();
    if !valid_app_name(name) {
        return Err(Error::Invalid(format!("not an app name: {name:?}")));
    }
    let l = listing();
    let running = l.windows.iter().find(|w| {
        l.apps
            .get(&w.pid)
            .is_some_and(|a| app_matches(a, name) || app_matches(a, &app_name(name)))
    });
    match running {
        Some(w) => activate(w.hwnd),
        None => launch(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_codes() {
        assert_eq!(named_vk(NamedKey::Enter), Some(0x0D));
        assert_eq!(named_vk(NamedKey::F(1)), Some(0x70));
        assert_eq!(named_vk(NamedKey::F(12)), Some(0x7B));
        assert_eq!(named_vk(NamedKey::F(0)), None);
        assert_eq!(ascii_vk('a'), Some((0x41, false)));
        assert_eq!(ascii_vk('Z'), Some((0x5A, true)));
        assert_eq!(ascii_vk('7'), Some((0x37, false)));
        assert_eq!(ascii_vk('?'), None);
        assert!(is_extended(0x25));
        assert!(!is_extended(0x41));
        let m = Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(modifier_vks(m), vec![VK_CONTROL, VK_SHIFT]);
    }

    #[test]
    fn action_mapping() {
        let caps = Caps {
            invoke: true,
            ..Default::default()
        };
        assert_eq!(kinds_from_caps(&caps, true, false), vec![
            UiActionKind::Press,
            UiActionKind::Focus
        ]);
        let caps = Caps {
            value: true,
            scroll_item: true,
            expand_collapse: true,
            expand_state: Some(0),
            ..Default::default()
        };
        assert_eq!(kinds_from_caps(&caps, false, false), vec![
            UiActionKind::SetValue,
            UiActionKind::Scroll,
            UiActionKind::Expand
        ]);
        // Secure fields never advertise SetValue; read-only values and leaf nodes neither.
        assert!(kinds_from_caps(&caps, false, true)
            .iter()
            .all(|k| *k != UiActionKind::SetValue));
        let caps = Caps {
            value: true,
            value_read_only: true,
            expand_collapse: true,
            expand_state: Some(3),
            selection_item: true,
            ..Default::default()
        };
        assert_eq!(kinds_from_caps(&caps, false, false), vec![UiActionKind::Select]);
    }

    #[test]
    fn states_from_caps() {
        let mut s = States::default();
        apply_caps(
            &mut s,
            &Caps {
                enabled: false,
                toggle: true,
                toggle_state: Some(1),
                expand_collapse: true,
                expand_state: Some(0),
                selection_item: true,
                selected: true,
                ..Default::default()
            },
        );
        assert!(s.disabled);
        assert_eq!(s.checked, Some(true));
        assert_eq!(s.expanded, Some(false));
        assert!(s.selected);
    }

    #[test]
    fn names() {
        assert_eq!(app_name("chrome.exe"), "chrome");
        assert_eq!(app_name("Code.EXE"), "Code");
        assert!(valid_app_name("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"));
        assert!(valid_app_name("notepad"));
        assert!(!valid_app_name("C:\\Windows\\notepad.exe"));
        assert!(!valid_app_name("-x"));
        assert_eq!(scroll_step(ScrollDirection::Down).1, ScrollAmount_SmallIncrement);
    }
}
