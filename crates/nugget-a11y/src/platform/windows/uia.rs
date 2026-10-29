//! Thin safe wrappers over the UI Automation COM API and the few Win32 calls the backend
//! needs. COM calls are `unsafe` in the `windows` crate because they take raw interface
//! pointers; every wrapper here passes only interfaces it owns.
#![allow(unsafe_code)]

use std::cell::RefCell;

use nugget_core::{Error, Result};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_I4, VariantClear};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationElement,
    IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern, IUIAutomationRangeValuePattern,
    IUIAutomationScrollItemPattern, IUIAutomationScrollPattern, IUIAutomationSelectionItemPattern,
    IUIAutomationTextPattern, IUIAutomationTextRange, IUIAutomationTogglePattern,
    IUIAutomationValuePattern, ScrollAmount, TextPatternRangeEndpoint_End,
    TextPatternRangeEndpoint_Start, TextUnit_Character, UIA_AriaPropertiesPropertyId,
    UIA_AriaRolePropertyId, UIA_AutomationIdPropertyId, UIA_BoundingRectanglePropertyId,
    UIA_ClassNamePropertyId, UIA_ControlTypePropertyId,
    UIA_ExpandCollapseExpandCollapseStatePropertyId, UIA_ExpandCollapsePatternId,
    UIA_HasKeyboardFocusPropertyId, UIA_HelpTextPropertyId, UIA_InvokePatternId,
    UIA_IsEnabledPropertyId, UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_IsInvokePatternAvailablePropertyId, UIA_IsKeyboardFocusablePropertyId,
    UIA_IsOffscreenPropertyId, UIA_IsPasswordPropertyId,
    UIA_IsRangeValuePatternAvailablePropertyId, UIA_IsScrollItemPatternAvailablePropertyId,
    UIA_IsScrollPatternAvailablePropertyId, UIA_IsSelectionItemPatternAvailablePropertyId,
    UIA_IsTogglePatternAvailablePropertyId, UIA_IsValuePatternAvailablePropertyId,
    UIA_NamePropertyId, UIA_PATTERN_ID, UIA_PROPERTY_ID, UIA_ProcessIdPropertyId,
    UIA_RangeValueIsReadOnlyPropertyId, UIA_RangeValuePatternId, UIA_ScrollItemPatternId,
    UIA_ScrollPatternId, UIA_SelectionItemIsSelectedPropertyId, UIA_SelectionItemPatternId,
    UIA_TextPatternId, UIA_TogglePatternId, UIA_ToggleToggleStatePropertyId,
    UIA_ValueIsReadOnlyPropertyId, UIA_ValuePatternId,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
};
use windows::core::BSTR;

use crate::Rect;

/// Edit-control messages (`CommCtrl.h`), for edit controls without a TextPattern.
const EM_GETSEL: u32 = 0x00B0;
const EM_SETSEL: u32 = 0x00B1;

/// `SendMessageTimeoutW` that gives up on a hung window; `None` on failure or timeout.
fn send_timeout(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> Option<usize> {
    let mut out = 0usize;
    // SAFETY: plain value messages (no pointers are passed); `out` is a valid out-pointer.
    let r = unsafe {
        SendMessageTimeoutW(
            hwnd,
            msg,
            WPARAM(wparam),
            LPARAM(lparam),
            SMTO_ABORTIFHUNG,
            250,
            Some(&mut out),
        )
    };
    (r.0 != 0).then_some(out)
}

fn werr(e: windows::core::Error) -> Error {
    Error::Other(format!("UI Automation: {e}"))
}

thread_local! {
    static AUTOMATION: RefCell<Option<IUIAutomation>> = const { RefCell::new(None) };
    static CACHE: RefCell<Option<IUIAutomationCacheRequest>> = const { RefCell::new(None) };
}

/// Properties fetched with every element in one round trip. The value is deliberately not
/// cached: it is requested separately, and never for password fields.
const CACHED: &[UIA_PROPERTY_ID] = &[
    UIA_ControlTypePropertyId,
    UIA_NamePropertyId,
    UIA_HelpTextPropertyId,
    UIA_ClassNamePropertyId,
    UIA_AutomationIdPropertyId,
    UIA_AriaRolePropertyId,
    UIA_AriaPropertiesPropertyId,
    UIA_IsPasswordPropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_IsKeyboardFocusablePropertyId,
    UIA_IsOffscreenPropertyId,
    UIA_ProcessIdPropertyId,
    UIA_BoundingRectanglePropertyId,
    // Computer use: state and the patterns behind each action.
    UIA_IsEnabledPropertyId,
    UIA_IsInvokePatternAvailablePropertyId,
    UIA_IsTogglePatternAvailablePropertyId,
    UIA_ToggleToggleStatePropertyId,
    UIA_IsValuePatternAvailablePropertyId,
    UIA_ValueIsReadOnlyPropertyId,
    UIA_IsRangeValuePatternAvailablePropertyId,
    UIA_RangeValueIsReadOnlyPropertyId,
    UIA_IsSelectionItemPatternAvailablePropertyId,
    UIA_SelectionItemIsSelectedPropertyId,
    UIA_IsExpandCollapsePatternAvailablePropertyId,
    UIA_ExpandCollapseExpandCollapseStatePropertyId,
    UIA_IsScrollPatternAvailablePropertyId,
    UIA_IsScrollItemPatternAvailablePropertyId,
];

fn cache_request() -> Result<IUIAutomationCacheRequest> {
    CACHE.with(|cell| {
        if let Some(c) = cell.borrow().as_ref() {
            return Ok(c.clone());
        }
        let a = automation()?;
        // SAFETY: valid automation object; the request is owned and only gains properties.
        let req = unsafe {
            let req = a.CreateCacheRequest().map_err(werr)?;
            for &p in CACHED {
                req.AddProperty(p).map_err(werr)?;
            }
            req
        };
        *cell.borrow_mut() = Some(req.clone());
        Ok(req)
    })
}

/// The per-thread `IUIAutomation`, initialising COM (multithreaded) on first use.
fn automation() -> Result<IUIAutomation> {
    AUTOMATION.with(|cell| {
        if let Some(a) = cell.borrow().as_ref() {
            return Ok(a.clone());
        }
        // SAFETY: initialising COM for this thread; S_FALSE / RPC_E_CHANGED_MODE (already
        // initialised, possibly as STA) are fine for an in-process UIA client.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // SAFETY: CUIAutomation is the documented CLSID for IUIAutomation.
        let a: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .map_err(werr)?;
        *cell.borrow_mut() = Some(a.clone());
        Ok(a)
    })
}

/// The foreground window and its process.
pub struct Foreground {
    pub pid: i32,
    pub exe: Option<String>,
    pub title: String,
}

pub fn foreground() -> Result<Option<Foreground>> {
    // SAFETY: no preconditions.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return Ok(None);
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out-pointer.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    // SAFETY: no preconditions; a length, 0 when untitled.
    let len = unsafe { GetWindowTextLengthW(hwnd) }.max(0) as usize;
    let mut buf = vec![0u16; len + 1];
    // SAFETY: `buf` has room for `len` characters and the terminator.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
    let title = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    Ok(Some(Foreground {
        pid: pid as i32,
        exe: exe_name(pid),
        title,
    }))
}

/// The executable file name of a process (`chrome.exe`).
pub fn exe_name(pid: u32) -> Option<String> {
    // SAFETY: a query-only handle, closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` UTF-16 units; `len` is updated to the written length.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: `h` is the handle opened above.
    let _ = unsafe { CloseHandle(h) };
    ok.ok()?;
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit(['\\', '/']).next().map(str::to_string)
}

/// Physical-to-logical scale of the foreground window (1.0 at 96 DPI).
pub fn foreground_scale() -> f64 {
    // SAFETY: no preconditions.
    window_scale(unsafe { GetForegroundWindow() })
}

/// Physical-to-logical scale of a window (1.0 at 96 DPI, and for an invalid handle).
pub fn window_scale(hwnd: HWND) -> f64 {
    if hwnd.is_invalid() {
        return 1.0;
    }
    // SAFETY: a window handle; returns 0 for an invalid one.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 1.0 } else { f64::from(dpi) / 96.0 }
}

fn bstr(r: windows::core::Result<BSTR>) -> String {
    r.map(|b| b.to_string()).unwrap_or_default()
}

/// Properties read for every node.
pub struct Info {
    pub control_type: i32,
    pub name: String,
    pub help_text: String,
    pub class_name: String,
    pub automation_id: String,
    pub aria_role: Option<String>,
    pub aria_properties: String,
    pub is_password: bool,
    pub has_focus: bool,
    pub is_keyboard_focusable: bool,
    pub offscreen: bool,
    pub process_id: i32,
    /// Physical pixels.
    pub bounds: Option<Rect>,
}

fn rect(r: RECT) -> Option<Rect> {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    (w > 0 && h > 0).then(|| Rect {
        x: f64::from(r.left),
        y: f64::from(r.top),
        width: f64::from(w),
        height: f64::from(h),
    })
}

/// What an element can do, from its pattern-availability properties (computer use).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub enabled: bool,
    pub invoke: bool,
    pub toggle: bool,
    /// `ToggleState`: 0 off, 1 on, 2 indeterminate.
    pub toggle_state: Option<i32>,
    pub value: bool,
    pub value_read_only: bool,
    pub range_value: bool,
    pub range_read_only: bool,
    pub selection_item: bool,
    pub selected: bool,
    pub expand_collapse: bool,
    /// `ExpandCollapseState`: 0 collapsed, 1 expanded, 2 partially expanded, 3 leaf.
    pub expand_state: Option<i32>,
    pub scroll: bool,
    pub scroll_item: bool,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            enabled: true,
            invoke: false,
            toggle: false,
            toggle_state: None,
            value: false,
            value_read_only: false,
            range_value: false,
            range_read_only: false,
            selection_item: false,
            selected: false,
            expand_collapse: false,
            expand_state: None,
            scroll: false,
            scroll_item: false,
        }
    }
}

enum Prop {
    Bool(bool),
    I32(i32),
}

/// A bool or i32 out of a property VARIANT, which is cleared. Anything else (including UIA's
/// "not supported" sentinel, an IUnknown) is `None`.
fn variant_prop(mut v: VARIANT) -> Option<Prop> {
    // SAFETY: `v` is an initialised VARIANT owned here; the union member read matches `vt`,
    // and VariantClear releases whatever it holds exactly once.
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        let out = if inner.vt == VT_BOOL {
            Some(Prop::Bool(inner.Anonymous.boolVal.0 != 0))
        } else if inner.vt == VT_I4 {
            Some(Prop::I32(inner.Anonymous.lVal))
        } else {
            None
        };
        let _ = VariantClear(&mut v);
        out
    }
}

/// A live UI Automation element.
#[derive(Clone)]
pub struct UiaEl {
    el: IUIAutomationElement,
    /// `el` was fetched with [`CACHED`] properties.
    cached: bool,
}

impl UiaEl {
    /// The element of a top-level window, with the cached properties.
    pub fn from_hwnd(hwnd: HWND) -> Result<UiaEl> {
        let a = automation()?;
        let req = cache_request()?;
        // SAFETY: valid automation and cache request; a stale handle yields an error.
        let el = unsafe { a.ElementFromHandleBuildCache(hwnd, &req) }.map_err(werr)?;
        Ok(UiaEl { el, cached: true })
    }

    fn prop(&self, id: UIA_PROPERTY_ID) -> Option<Prop> {
        // SAFETY: valid element; the VARIANT is owned and cleared by `variant_prop`.
        let v = unsafe {
            if self.cached {
                self.el.GetCachedPropertyValue(id)
            } else {
                self.el.GetCurrentPropertyValue(id)
            }
        };
        variant_prop(v.ok()?)
    }

    fn prop_bool(&self, id: UIA_PROPERTY_ID) -> Option<bool> {
        match self.prop(id)? {
            Prop::Bool(b) => Some(b),
            Prop::I32(_) => None,
        }
    }

    fn prop_i32(&self, id: UIA_PROPERTY_ID) -> Option<i32> {
        match self.prop(id)? {
            Prop::I32(i) => Some(i),
            Prop::Bool(_) => None,
        }
    }

    pub fn caps(&self) -> Caps {
        let b = |id| self.prop_bool(id).unwrap_or(false);
        let toggle = b(UIA_IsTogglePatternAvailablePropertyId);
        let expand_collapse = b(UIA_IsExpandCollapsePatternAvailablePropertyId);
        let selection_item = b(UIA_IsSelectionItemPatternAvailablePropertyId);
        let value = b(UIA_IsValuePatternAvailablePropertyId);
        let range_value = b(UIA_IsRangeValuePatternAvailablePropertyId);
        Caps {
            enabled: self.prop_bool(UIA_IsEnabledPropertyId).unwrap_or(true),
            invoke: b(UIA_IsInvokePatternAvailablePropertyId),
            toggle,
            toggle_state: toggle
                .then(|| self.prop_i32(UIA_ToggleToggleStatePropertyId))
                .flatten(),
            value,
            value_read_only: value && b(UIA_ValueIsReadOnlyPropertyId),
            range_value,
            range_read_only: range_value && b(UIA_RangeValueIsReadOnlyPropertyId),
            selection_item,
            selected: selection_item && b(UIA_SelectionItemIsSelectedPropertyId),
            expand_collapse,
            expand_state: expand_collapse
                .then(|| self.prop_i32(UIA_ExpandCollapseExpandCollapseStatePropertyId))
                .flatten(),
            scroll: b(UIA_IsScrollPatternAvailablePropertyId),
            scroll_item: b(UIA_IsScrollItemPatternAvailablePropertyId),
        }
    }

    fn pattern<T: windows::core::Interface>(&self, id: UIA_PATTERN_ID) -> Result<T> {
        // SAFETY: valid element; the pattern interface is owned.
        unsafe { self.el.GetCurrentPatternAs::<T>(id) }
            .map_err(|_| Error::Unsupported("the element does not support this action".into()))
    }

    pub fn invoke(&self) -> Result<()> {
        let p: IUIAutomationInvokePattern = self.pattern(UIA_InvokePatternId)?;
        // SAFETY: valid pattern interface.
        unsafe { p.Invoke() }.map_err(werr)
    }

    pub fn toggle(&self) -> Result<()> {
        let p: IUIAutomationTogglePattern = self.pattern(UIA_TogglePatternId)?;
        // SAFETY: valid pattern interface.
        unsafe { p.Toggle() }.map_err(werr)
    }

    pub fn select(&self) -> Result<()> {
        let p: IUIAutomationSelectionItemPattern = self.pattern(UIA_SelectionItemPatternId)?;
        // SAFETY: valid pattern interface.
        unsafe { p.Select() }.map_err(werr)
    }

    pub fn set_focus(&self) -> Result<()> {
        // SAFETY: valid element.
        unsafe { self.el.SetFocus() }.map_err(werr)
    }

    pub fn set_range_value(&self, v: f64) -> Result<()> {
        let p: IUIAutomationRangeValuePattern = self.pattern(UIA_RangeValuePatternId)?;
        // SAFETY: valid pattern interface.
        unsafe { p.SetValue(v) }.map_err(werr)
    }

    pub fn scroll_into_view(&self) -> Result<()> {
        let p: IUIAutomationScrollItemPattern = self.pattern(UIA_ScrollItemPatternId)?;
        // SAFETY: valid pattern interface.
        unsafe { p.ScrollIntoView() }.map_err(werr)
    }

    /// One `ScrollPattern.Scroll` step (horizontal, vertical).
    pub fn scroll(&self, horizontal: ScrollAmount, vertical: ScrollAmount) -> Result<()> {
        let p: IUIAutomationScrollPattern = self.pattern(UIA_ScrollPatternId)?;
        // SAFETY: valid pattern interface.
        unsafe { p.Scroll(horizontal, vertical) }.map_err(werr)
    }

    pub fn expand_collapse(&self, expand: bool) -> Result<()> {
        let p: IUIAutomationExpandCollapsePattern = self.pattern(UIA_ExpandCollapsePatternId)?;
        // SAFETY: valid pattern interface.
        unsafe { if expand { p.Expand() } else { p.Collapse() } }.map_err(werr)
    }

    pub fn focused() -> Result<Option<UiaEl>> {
        let a = automation()?;
        // SAFETY: `a` is a valid IUIAutomation.
        match unsafe { a.GetFocusedElement() } {
            Ok(el) => Ok(Some(UiaEl { el, cached: false })),
            Err(_) => Ok(None),
        }
    }

    pub fn info(&self) -> Info {
        let e = &self.el;
        // SAFETY (all calls below): `e` is a valid element; each call returns an owned value
        // or an error, which falls back to a default. Elements from the walker carry the
        // cached properties; the focused element and fallbacks read them live.
        unsafe {
            macro_rules! get {
                ($cached:ident, $current:ident) => {
                    if self.cached {
                        e.$cached()
                    } else {
                        e.$current()
                    }
                };
            }
            let aria = bstr(get!(CachedAriaRole, CurrentAriaRole));
            Info {
                control_type: get!(CachedControlType, CurrentControlType)
                    .map(|c| c.0)
                    .unwrap_or(0),
                name: bstr(get!(CachedName, CurrentName)),
                help_text: bstr(get!(CachedHelpText, CurrentHelpText)),
                class_name: bstr(get!(CachedClassName, CurrentClassName)),
                automation_id: bstr(get!(CachedAutomationId, CurrentAutomationId)),
                aria_role: (!aria.is_empty()).then_some(aria),
                aria_properties: bstr(get!(CachedAriaProperties, CurrentAriaProperties)),
                is_password: get!(CachedIsPassword, CurrentIsPassword).is_ok_and(|b| b.as_bool()),
                has_focus: get!(CachedHasKeyboardFocus, CurrentHasKeyboardFocus)
                    .is_ok_and(|b| b.as_bool()),
                is_keyboard_focusable: get!(CachedIsKeyboardFocusable, CurrentIsKeyboardFocusable)
                    .is_ok_and(|b| b.as_bool()),
                offscreen: get!(CachedIsOffscreen, CurrentIsOffscreen).is_ok_and(|b| b.as_bool()),
                process_id: get!(CachedProcessId, CurrentProcessId).unwrap_or(0),
                bounds: get!(CachedBoundingRectangle, CurrentBoundingRectangle)
                    .ok()
                    .and_then(rect),
            }
        }
    }

    fn value(&self) -> Option<IUIAutomationValuePattern> {
        // SAFETY: valid element; the pattern interface is owned.
        unsafe {
            self.el
                .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .ok()
        }
    }

    fn text(&self) -> Option<IUIAutomationTextPattern> {
        // SAFETY: as above.
        unsafe {
            self.el
                .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                .ok()
        }
    }

    /// `(value, read_only)` from the ValuePattern. Never called for password fields.
    pub fn value_pattern(&self) -> Option<(String, bool)> {
        let v = self.value()?;
        // SAFETY: valid pattern interface.
        unsafe {
            let value = v.CurrentValue().ok()?.to_string();
            let ro = v.CurrentIsReadOnly().is_ok_and(|b| b.as_bool());
            Some((value, ro))
        }
    }

    /// The text of the whole document range, capped.
    pub fn document_text(&self, max_bytes: usize) -> Option<String> {
        let t = self.text()?;
        let max = i32::try_from(max_bytes).unwrap_or(i32::MAX);
        // SAFETY: valid pattern and range interfaces.
        unsafe { Some(t.DocumentRange().ok()?.GetText(max).ok()?.to_string()) }
    }

    pub fn set_value(&self, value: &str) -> Result<()> {
        let v = self
            .value()
            .ok_or_else(|| Error::Unsupported("no ValuePattern".into()))?;
        // SAFETY: valid pattern; the BSTR is owned for the call.
        unsafe { v.SetValue(&BSTR::from(value)) }.map_err(werr)
    }

    /// The window of an edit-class control (Win32 Edit, WinForms `WindowsForms10.EDIT`,
    /// RichEdit) whose provider exposes no TextPattern: its selection and caret are read with
    /// edit messages instead.
    fn edit_hwnd(&self) -> Option<HWND> {
        // SAFETY: valid element; the handle is only used as a message target.
        let hwnd = unsafe { self.el.CurrentNativeWindowHandle() }.ok()?;
        if hwnd.is_invalid() || !self.info().class_name.to_ascii_lowercase().contains("edit") {
            return None;
        }
        Some(hwnd)
    }

    /// `EM_GETSEL` as character offsets into the value (UTF-16 units from the control).
    fn edit_selection(&self) -> Option<(u32, u32)> {
        let hwnd = self.edit_hwnd()?;
        let (text, _) = self.value_pattern()?;
        let units: Vec<u16> = text.encode_utf16().collect();
        // The packed return value holds 16-bit offsets.
        if units.len() > 0xFFFF {
            return None;
        }
        let r = send_timeout(hwnd, EM_GETSEL, 0, 0)?;
        let (s, e) = (r & 0xFFFF, (r >> 16) & 0xFFFF);
        if s > units.len() || e > units.len() {
            return None;
        }
        let chars = |n: usize| String::from_utf16_lossy(&units[..n]).chars().count() as u32;
        Some((chars(s), chars(e)))
    }

    /// The caret of an edit control from its GUI thread (physical pixels).
    fn edit_caret(&self) -> Option<Rect> {
        let hwnd = self.edit_hwnd()?;
        // SAFETY: a window handle; returns 0 for an invalid one.
        let tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
        if tid == 0 {
            return None;
        }
        let mut gi = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `gi` is a valid GUITHREADINFO with cbSize set.
        unsafe { GetGUIThreadInfo(tid, &mut gi) }.ok()?;
        if gi.hwndCaret != hwnd {
            return None;
        }
        let r = gi.rcCaret;
        let mut tl = POINT {
            x: r.left,
            y: r.top,
        };
        let mut br = POINT {
            x: r.right.max(r.left + 1),
            y: r.bottom,
        };
        // SAFETY: valid points; the conversion fails for a destroyed window.
        unsafe {
            if !ClientToScreen(hwnd, &mut tl).as_bool() || !ClientToScreen(hwnd, &mut br).as_bool()
            {
                return None;
            }
        }
        rect(RECT {
            left: tl.x,
            top: tl.y,
            right: br.x,
            bottom: br.y,
        })
    }

    /// Selection as character offsets: the text length from the document start to each end.
    /// Edit controls without a TextPattern answer `EM_GETSEL`.
    pub fn selection_offsets(&self) -> Option<(u32, u32)> {
        let Some(t) = self.text() else {
            return self.edit_selection();
        };
        // SAFETY: valid pattern/range interfaces, all owned.
        unsafe {
            let sel = t.GetSelection().ok()?;
            if sel.Length().ok()? < 1 {
                return None;
            }
            let range = sel.GetElement(0).ok()?;
            let doc = t.DocumentRange().ok()?;
            let measure = |endpoint| -> Option<u32> {
                let r: IUIAutomationTextRange = doc.Clone().ok()?;
                r.MoveEndpointByRange(TextPatternRangeEndpoint_End, &range, endpoint)
                    .ok()?;
                let s = r.GetText(-1).ok()?.to_string();
                Some(s.chars().count() as u32)
            };
            Some((
                measure(TextPatternRangeEndpoint_Start)?,
                measure(TextPatternRangeEndpoint_End)?,
            ))
        }
    }

    /// Bounds of the selection, or of the character at the caret (physical pixels).
    pub fn caret_rect(&self) -> Option<Rect> {
        let Some(t) = self.text() else {
            return self.edit_caret();
        };
        // SAFETY: valid interfaces; the SAFEARRAY is destroyed by `rects`.
        unsafe {
            let sel = t.GetSelection().ok()?;
            if sel.Length().ok()? < 1 {
                return None;
            }
            let range = sel.GetElement(0).ok()?;
            if let Some(r) = rects(range.GetBoundingRectangles().ok()?)
                .into_iter()
                .next()
            {
                return Some(r);
            }
            // A collapsed caret has no rectangle: measure the next character instead.
            let probe = range.Clone().ok()?;
            let moved = probe
                .MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, 1)
                .ok()?;
            if moved == 1
                && let Some(r) = rects(probe.GetBoundingRectangles().ok()?)
                    .into_iter()
                    .next()
            {
                return Some(Rect { width: 1.0, ..r });
            }
            let probe = range.Clone().ok()?;
            probe
                .MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -1)
                .ok()?;
            let r = rects(probe.GetBoundingRectangles().ok()?)
                .into_iter()
                .next()?;
            Some(Rect {
                x: r.x + r.width,
                width: 1.0,
                ..r
            })
        }
    }

    /// Put the caret at a character offset (best effort).
    pub fn set_caret(&self, offset: u32) {
        let Some(t) = self.text() else {
            if let (Some(hwnd), Some((text, _))) = (self.edit_hwnd(), self.value_pattern()) {
                let u: usize = text
                    .chars()
                    .take(offset as usize)
                    .map(char::len_utf16)
                    .sum();
                let _ = send_timeout(hwnd, EM_SETSEL, u, u as isize);
            }
            return;
        };
        // SAFETY: valid interfaces.
        unsafe {
            let Ok(r) = t.DocumentRange() else { return };
            let _ = r.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &r,
                TextPatternRangeEndpoint_Start,
            );
            let n = i32::try_from(offset).unwrap_or(i32::MAX);
            let _ = r.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, n);
            let _ = r.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, n);
            let _ = r.Select();
        }
    }

    pub fn labeled_by_name(&self) -> Option<String> {
        // SAFETY: valid element.
        unsafe {
            let l = self.el.CurrentLabeledBy().ok()?;
            let n = l.CurrentName().ok()?.to_string();
            (!n.trim().is_empty()).then_some(n)
        }
    }

    pub fn child_elements(&self, max: usize) -> Vec<UiaEl> {
        let (Ok(a), Ok(req)) = (automation(), cache_request()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // SAFETY: valid automation, walker, cache request and element interfaces.
        unsafe {
            let Ok(walker) = a.ControlViewWalker() else {
                return out;
            };
            let mut cur = walker.GetFirstChildElementBuildCache(&self.el, &req).ok();
            while let Some(el) = cur {
                if out.len() >= max {
                    break;
                }
                cur = walker.GetNextSiblingElementBuildCache(&el, &req).ok();
                out.push(UiaEl { el, cached: true });
            }
        }
        out
    }

    pub fn parent_element(&self) -> Option<UiaEl> {
        let a = automation().ok()?;
        let req = cache_request().ok()?;
        // SAFETY: valid interfaces.
        unsafe {
            let walker = a.ControlViewWalker().ok()?;
            let p = walker.GetParentElementBuildCache(&self.el, &req).ok()?;
            let root = a.GetRootElement().ok()?;
            if a.CompareElements(&p, &root).is_ok_and(|b| b.as_bool()) {
                return None;
            }
            Some(UiaEl {
                el: p,
                cached: true,
            })
        }
    }

    pub fn is_same(&self, other: &UiaEl) -> bool {
        let Ok(a) = automation() else { return false };
        // SAFETY: valid interfaces.
        unsafe { a.CompareElements(&self.el, &other.el) }.is_ok_and(|b| b.as_bool())
    }
}

/// Rectangles from a SAFEARRAY of doubles (left, top, width, height)*, which is destroyed.
///
/// # Safety
/// `sa` must be a SAFEARRAY of VT_R8 returned by UIA, owned by the caller.
unsafe fn rects(sa: *mut SAFEARRAY) -> Vec<Rect> {
    let mut out = Vec::new();
    if sa.is_null() {
        return out;
    }
    // SAFETY: `sa` is a valid one-dimensional SAFEARRAY of f64 (caller contract); the data
    // pointer is used only between Access and Unaccess, and the array is destroyed once.
    unsafe {
        if let (Ok(lo), Ok(hi)) = (SafeArrayGetLBound(sa, 1), SafeArrayGetUBound(sa, 1)) {
            let n = (hi - lo + 1).max(0) as usize;
            let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
            if SafeArrayAccessData(sa, &mut data).is_ok() && !data.is_null() {
                let vals = std::slice::from_raw_parts(data.cast::<f64>(), n);
                for c in vals.as_chunks::<4>().0 {
                    if c[2] > 0.0 || c[3] > 0.0 {
                        out.push(Rect {
                            x: c[0],
                            y: c[1],
                            width: c[2],
                            height: c[3],
                        });
                    }
                }
                let _ = SafeArrayUnaccessData(sa);
            }
        }
        let _ = SafeArrayDestroy(sa);
    }
    out
}

/// Ctrl+V through `SendInput`. Only Control and V: never Return.
pub fn send_ctrl_v() -> Result<()> {
    const VK_V: VIRTUAL_KEY = VIRTUAL_KEY(0x56);
    let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [
        key(VK_CONTROL, false),
        key(VK_V, false),
        key(VK_V, true),
        key(VK_CONTROL, true),
    ];
    // SAFETY: `inputs` is a valid array of keyboard INPUTs and its element size is passed.
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(Error::Other("SendInput was blocked".into()))
    }
}
