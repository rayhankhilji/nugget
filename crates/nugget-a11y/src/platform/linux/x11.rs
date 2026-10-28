//! X11 clipboard (the `CLIPBOARD` selection), an XTest Ctrl+V, EWMH window activation and a
//! screen capture for the screenshot fallback, over x11rb's pure-Rust protocol implementation.
//!
//! Saving reads `TARGETS` and then every target (with INCR for large transfers). Restoring, or
//! setting text, makes a background thread the selection owner; it serves requests (INCR for
//! large data) until another client takes the selection over, as any clipboard owner does.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nugget_core::{Error, Result};
use x11rb::connection::{Connection as _, RequestConnection as _};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt as _,
    CreateWindowAux, EventMask, ImageFormat, ImageOrder, PropMode, Property,
    SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, VisualClass, Window, WindowClass,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

use crate::clipboard::{Clipboard, ClipboardFormat, ClipboardSnapshot, MAX_SNAPSHOT_BYTES};

const CONVERT_TIMEOUT: Duration = Duration::from_millis(800);
/// Targets that describe the selection rather than hold data, or that name X resources only
/// valid while the original owner lives.
const META_TARGETS: &[&str] = &[
    "TARGETS",
    "MULTIPLE",
    "TIMESTAMP",
    "SAVE_TARGETS",
    "DELETE",
    "INSERT_SELECTION",
    "INSERT_PROPERTY",
    "PIXMAP",
    "BITMAP",
    "DRAWABLE",
    "WINDOW",
    "COLORMAP",
];
const TEXT_TARGETS: &[&str] = &[
    "UTF8_STRING",
    "text/plain;charset=utf-8",
    "text/plain",
    "STRING",
    "TEXT",
];

/// Target and data that ask clipboard managers (Klipper and others) not to record the selection.
/// Offered with text we paste, never on restore.
pub(super) const PASSWORD_HINT_TARGET: &str = "x-kde-passwordManagerHint";
pub(super) const PASSWORD_HINT_DATA: &[u8] = b"secret";

/// The formats offered for pasted text: every text target, plus the password-manager hint.
fn text_formats(text: &str) -> Vec<ClipboardFormat> {
    TEXT_TARGETS
        .iter()
        .map(|t| ClipboardFormat {
            format: (*t).to_string(),
            data: text.as_bytes().to_vec(),
        })
        .chain([ClipboardFormat {
            format: PASSWORD_HINT_TARGET.into(),
            data: PASSWORD_HINT_DATA.to_vec(),
        }])
        .collect()
}

pub(super) fn xerr(e: impl std::fmt::Display) -> Error {
    Error::Other(format!("X11: {e}"))
}

pub(super) fn connect() -> Result<(RustConnection, usize)> {
    if std::env::var_os("DISPLAY").is_none() {
        return Err(Error::Unsupported("no X11 display".into()));
    }
    RustConnection::connect(None).map_err(xerr)
}

pub(super) fn intern(conn: &RustConnection, name: &str) -> Result<Atom> {
    Ok(conn
        .intern_atom(false, name.as_bytes())
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?
        .atom)
}

fn atom_name(conn: &RustConnection, atom: Atom) -> Result<String> {
    let r = conn
        .get_atom_name(atom)
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?;
    Ok(String::from_utf8_lossy(&r.name).into_owned())
}

fn helper_window(conn: &RustConnection, screen: usize) -> Result<Window> {
    let root = conn.setup().roots[screen].root;
    let win = conn.generate_id().map_err(xerr)?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(xerr)?;
    conn.flush().map_err(xerr)?;
    Ok(win)
}

/// X11 clipboard access.
pub struct X11Clipboard {
    conn: RustConnection,
    win: Window,
    clipboard: Atom,
    targets: Atom,
    incr: Atom,
    prop: Atom,
}

impl X11Clipboard {
    pub fn new() -> Result<Self> {
        let (conn, screen) = connect()?;
        let win = helper_window(&conn, screen)?;
        Ok(X11Clipboard {
            clipboard: intern(&conn, "CLIPBOARD")?,
            targets: intern(&conn, "TARGETS")?,
            incr: intern(&conn, "INCR")?,
            prop: intern(&conn, "NUGGET_A11Y_SELECTION")?,
            conn,
            win,
        })
    }

    fn wait_event(&self, deadline: Instant, mut pred: impl FnMut(&Event) -> bool) -> Result<Event> {
        loop {
            while let Some(ev) = self.conn.poll_for_event().map_err(xerr)? {
                if pred(&ev) {
                    return Ok(ev);
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::Other("X11 selection transfer timed out".into()));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn read_prop(&self) -> Result<(Atom, Vec<u8>)> {
        let mut data = Vec::new();
        let mut offset = 0u32;
        loop {
            let r = self
                .conn
                .get_property(false, self.win, self.prop, AtomEnum::ANY, offset, 1 << 20)
                .map_err(xerr)?
                .reply()
                .map_err(xerr)?;
            data.extend_from_slice(&r.value);
            if data.len() > MAX_SNAPSHOT_BYTES {
                return Err(Error::Unsupported("clipboard format too large".into()));
            }
            if r.bytes_after == 0 {
                self.conn
                    .delete_property(self.win, self.prop)
                    .map_err(xerr)?;
                self.conn.flush().map_err(xerr)?;
                return Ok((r.type_, data));
            }
            offset += (r.value.len() / 4) as u32;
        }
    }

    /// Convert the clipboard to `target`; `None` when the owner refuses.
    fn convert(&self, target: Atom) -> Result<Option<(Atom, Vec<u8>)>> {
        self.conn
            .convert_selection(self.win, self.clipboard, target, self.prop, CURRENT_TIME)
            .map_err(xerr)?;
        self.conn.flush().map_err(xerr)?;
        let deadline = Instant::now() + CONVERT_TIMEOUT;
        let ev = self.wait_event(
            deadline,
            |e| matches!(e, Event::SelectionNotify(n) if n.requestor == self.win),
        )?;
        let Event::SelectionNotify(n) = ev else {
            unreachable!()
        };
        if n.property == NONE {
            return Ok(None);
        }
        let (ty, data) = self.read_prop()?;
        if ty != self.incr {
            return Ok(Some((ty, data)));
        }
        // INCR: the owner sends chunks each time we delete the property; an empty chunk ends.
        let mut out = Vec::new();
        loop {
            let deadline = Instant::now() + CONVERT_TIMEOUT;
            self.wait_event(deadline, |e| {
                matches!(e, Event::PropertyNotify(p)
                    if p.window == self.win && p.atom == self.prop && p.state == Property::NEW_VALUE)
            })?;
            let (ty, chunk) = self.read_prop()?;
            if chunk.is_empty() {
                return Ok(Some((ty, out)));
            }
            out.extend_from_slice(&chunk);
            if out.len() > MAX_SNAPSHOT_BYTES {
                return Err(Error::Unsupported("clipboard format too large".into()));
            }
        }
    }
}

impl Drop for X11Clipboard {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.win);
        let _ = self.conn.flush();
    }
}

impl Clipboard for X11Clipboard {
    fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
        let owner = self
            .conn
            .get_selection_owner(self.clipboard)
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?
            .owner;
        if owner == NONE {
            return Ok(ClipboardSnapshot::default());
        }
        let Some((_, raw)) = self.convert(self.targets)? else {
            return Ok(ClipboardSnapshot::default());
        };
        let mut atoms: Vec<Atom> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_ne_bytes(*c))
            .collect();
        atoms.dedup();
        let mut formats = Vec::new();
        let mut total = 0usize;
        for atom in atoms {
            let name = atom_name(&self.conn, atom)?;
            if META_TARGETS.contains(&name.as_str())
                || formats.iter().any(|f: &ClipboardFormat| f.format == name)
            {
                continue;
            }
            match self.convert(atom) {
                Ok(Some((_, data))) => {
                    total += data.len();
                    if total > MAX_SNAPSHOT_BYTES {
                        return Err(Error::Unsupported("clipboard too large to save".into()));
                    }
                    formats.push(ClipboardFormat { format: name, data });
                }
                Ok(None) => {}
                // One stubborn target must not lose the others; a missing one is logged.
                Err(e) => {
                    tracing::debug!(target = %name, error = %e, "could not read clipboard target")
                }
            }
        }
        Ok(ClipboardSnapshot {
            items: if formats.is_empty() {
                Vec::new()
            } else {
                vec![formats]
            },
        })
    }

    fn set_text(&mut self, text: &str) -> Result<()> {
        serve(text_formats(text))
    }

    fn restore(&mut self, snapshot: &ClipboardSnapshot) -> Result<()> {
        let formats: Vec<ClipboardFormat> = snapshot.items.iter().flatten().cloned().collect();
        if formats.is_empty() {
            // The clipboard was empty: give up ownership so it is empty again.
            self.conn
                .set_selection_owner(NONE, self.clipboard, CURRENT_TIME)
                .map_err(xerr)?;
            self.conn.flush().map_err(xerr)?;
            return Ok(());
        }
        serve(formats)
    }
}

struct Transfer {
    requestor: Window,
    property: Atom,
    ty: Atom,
    data: std::sync::Arc<Vec<u8>>,
    offset: usize,
}

/// Become the clipboard owner in a background thread serving `formats`, returning once the
/// ownership is confirmed. The thread exits when another client takes the selection.
fn serve(formats: Vec<ClipboardFormat>) -> Result<()> {
    let (tx, rx) = mpsc::channel::<Result<()>>();
    std::thread::Builder::new()
        .name("nugget-a11y-x11-clipboard".into())
        .spawn(move || {
            if let Err(e) = serve_loop(formats, &tx) {
                let _ = tx.send(Err(e));
            }
        })
        .map_err(xerr)?;
    rx.recv_timeout(Duration::from_secs(2))
        .map_err(|_| Error::Other("X11 clipboard owner did not start".into()))?
}

fn serve_loop(formats: Vec<ClipboardFormat>, ready: &mpsc::Sender<Result<()>>) -> Result<()> {
    let (conn, screen) = connect()?;
    let win = helper_window(&conn, screen)?;
    let clipboard = intern(&conn, "CLIPBOARD")?;
    let targets = intern(&conn, "TARGETS")?;
    let timestamp = intern(&conn, "TIMESTAMP")?;
    let incr = intern(&conn, "INCR")?;
    let utf8 = intern(&conn, "UTF8_STRING")?;
    let text_atom = intern(&conn, "TEXT")?;
    let mut table: HashMap<Atom, std::sync::Arc<Vec<u8>>> = HashMap::new();
    let mut offered = vec![targets, timestamp];
    for f in formats {
        let a = intern(&conn, &f.format)?;
        if let Entry::Vacant(e) = table.entry(a) {
            offered.push(a);
            e.insert(std::sync::Arc::new(f.data));
        }
    }
    let chunk = (conn.maximum_request_bytes() / 4).clamp(4096, 256 * 1024);

    conn.set_selection_owner(win, clipboard, CURRENT_TIME)
        .map_err(xerr)?;
    let owner = conn
        .get_selection_owner(clipboard)
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?
        .owner;
    if owner != win {
        return Err(Error::Other("could not take the X11 clipboard".into()));
    }
    let _ = ready.send(Ok(()));

    let mut transfers: Vec<Transfer> = Vec::new();
    loop {
        let ev = conn.wait_for_event().map_err(xerr)?;
        match ev {
            Event::SelectionClear(e) if e.selection == clipboard => {
                if transfers.is_empty() {
                    break;
                }
                // Finish INCR transfers in flight, then stop.
                table.clear();
            }
            Event::SelectionRequest(req) => {
                let property = if req.property == NONE {
                    req.target
                } else {
                    req.property
                };
                let mut ok = true;
                if req.target == targets {
                    conn.change_property32(
                        PropMode::REPLACE,
                        req.requestor,
                        property,
                        AtomEnum::ATOM,
                        &offered,
                    )
                    .map_err(xerr)?;
                } else if req.target == timestamp {
                    conn.change_property32(
                        PropMode::REPLACE,
                        req.requestor,
                        property,
                        AtomEnum::INTEGER,
                        &[req.time],
                    )
                    .map_err(xerr)?;
                } else if let Some(data) = table.get(&req.target) {
                    let ty = if req.target == text_atom {
                        utf8
                    } else {
                        req.target
                    };
                    if data.len() > chunk {
                        conn.change_window_attributes(
                            req.requestor,
                            &ChangeWindowAttributesAux::new()
                                .event_mask(EventMask::PROPERTY_CHANGE),
                        )
                        .map_err(xerr)?;
                        conn.change_property32(
                            PropMode::REPLACE,
                            req.requestor,
                            property,
                            incr,
                            &[u32::try_from(data.len()).unwrap_or(u32::MAX)],
                        )
                        .map_err(xerr)?;
                        transfers.push(Transfer {
                            requestor: req.requestor,
                            property,
                            ty,
                            data: data.clone(),
                            offset: 0,
                        });
                    } else {
                        conn.change_property8(PropMode::REPLACE, req.requestor, property, ty, data)
                            .map_err(xerr)?;
                    }
                } else {
                    ok = false;
                }
                let notify = SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: req.time,
                    requestor: req.requestor,
                    selection: req.selection,
                    target: req.target,
                    property: if ok { property } else { NONE },
                };
                conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify)
                    .map_err(xerr)?;
                conn.flush().map_err(xerr)?;
            }
            Event::PropertyNotify(p) if p.state == Property::DELETE => {
                if let Some(i) = transfers
                    .iter()
                    .position(|t| t.requestor == p.window && t.property == p.atom)
                {
                    let t = &mut transfers[i];
                    let end = (t.offset + chunk).min(t.data.len());
                    conn.change_property8(
                        PropMode::REPLACE,
                        t.requestor,
                        t.property,
                        t.ty,
                        &t.data[t.offset..end],
                    )
                    .map_err(xerr)?;
                    let finished = t.offset == end;
                    t.offset = end;
                    if finished {
                        transfers.remove(i);
                    }
                    conn.flush().map_err(xerr)?;
                    if transfers.is_empty() && table.is_empty() {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    let _ = conn.destroy_window(win);
    let _ = conn.flush();
    Ok(())
}

const XK_CONTROL_L: u32 = 0xffe3;
const XK_V: u32 = 0x0076;
pub(super) const KEY_PRESS: u8 = 2;
pub(super) const KEY_RELEASE: u8 = 3;

/// Synthesise Ctrl+V with XTest. Only Control and V are pressed: never Return.
pub fn paste_ctrl_v() -> Result<()> {
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
    let map = conn
        .get_keyboard_mapping(min, max - min + 1)
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?;
    let per = usize::from(map.keysyms_per_keycode.max(1));
    let find = |sym: u32| {
        map.keysyms
            .chunks(per)
            .position(|syms| syms.contains(&sym))
            .map(|i| min + i as u8)
    };
    let ctrl = find(XK_CONTROL_L).ok_or_else(|| Error::Unsupported("no Control key".into()))?;
    let v = find(XK_V).ok_or_else(|| Error::Unsupported("no V key".into()))?;
    let root = setup.roots[screen].root;
    for (ty, key) in [
        (KEY_PRESS, ctrl),
        (KEY_PRESS, v),
        (KEY_RELEASE, v),
        (KEY_RELEASE, ctrl),
    ] {
        conn.xtest_fake_input(ty, key, CURRENT_TIME, root, 0, 0, 0)
            .map_err(xerr)?;
    }
    conn.get_input_focus()
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?;
    Ok(())
}

/// Ask the window manager (EWMH `_NET_ACTIVE_WINDOW`) to activate the client windows of `pid`.
/// `Ok(false)` when there is no EWMH window manager or no window of that process.
pub fn activate_pid(pid: u32) -> Result<bool> {
    let (conn, screen) = connect()?;
    let root = conn.setup().roots[screen].root;
    let list = intern(&conn, "_NET_CLIENT_LIST")?;
    let wm_pid = intern(&conn, "_NET_WM_PID")?;
    let active = intern(&conn, "_NET_ACTIVE_WINDOW")?;
    let clients = conn
        .get_property(false, root, list, AtomEnum::WINDOW, 0, 4096)
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?;
    let Some(clients) = clients.value32() else {
        return Ok(false);
    };
    let mut sent = false;
    for w in clients {
        let owner = conn
            .get_property(false, w, wm_pid, AtomEnum::CARDINAL, 0, 1)
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?;
        if owner.value32().and_then(|mut v| v.next()) != Some(pid) {
            continue;
        }
        // Source 2: a pager-like tool acting for the user.
        let ev = ClientMessageEvent::new(32, w, active, [2, CURRENT_TIME, 0, 0, 0]);
        conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ev,
        )
        .map_err(xerr)?;
        sent = true;
    }
    conn.flush().map_err(xerr)?;
    Ok(sent)
}

/// Largest screenshot area, in pixels.
const MAX_CAPTURE_PIXELS: u64 = 4096 * 4096;

/// A PNG of the screen area `r` (clipped to the root window). `Ok(None)` for visuals other than
/// 24/32-bit true colour.
pub fn capture_png(r: crate::Rect) -> Result<Option<Vec<u8>>> {
    let (conn, screen) = connect()?;
    let setup = conn.setup();
    let scr = &setup.roots[screen];
    let x0 = (r.x.max(0.0) as i32).min(i32::from(scr.width_in_pixels));
    let y0 = (r.y.max(0.0) as i32).min(i32::from(scr.height_in_pixels));
    let x1 = ((r.x + r.width) as i32).min(i32::from(scr.width_in_pixels));
    let y1 = ((r.y + r.height) as i32).min(i32::from(scr.height_in_pixels));
    let (w, h) = ((x1 - x0).max(0) as u16, (y1 - y0).max(0) as u16);
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_CAPTURE_PIXELS {
        return Ok(None);
    }
    let img = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            scr.root,
            x0 as i16,
            y0 as i16,
            w,
            h,
            !0,
        )
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?;
    let Some(vis) = scr
        .allowed_depths
        .iter()
        .flat_map(|d| &d.visuals)
        .find(|v| v.visual_id == img.visual)
    else {
        return Ok(None);
    };
    let bpp = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == img.depth)
        .map(|f| f.bits_per_pixel);
    if bpp != Some(32) || vis.class != VisualClass::TRUE_COLOR {
        return Ok(None);
    }
    let lsb = setup.image_byte_order == ImageOrder::LSB_FIRST;
    let chan = |px: u32, mask: u32| -> u8 {
        if mask == 0 {
            return 0;
        }
        let v = (px & mask) >> mask.trailing_zeros();
        let max = mask >> mask.trailing_zeros();
        (u64::from(v) * 255 / u64::from(max)) as u8
    };
    let stride = usize::from(w) * 4;
    if img.data.len() < stride * usize::from(h) {
        return Ok(None);
    }
    let mut rgb = Vec::with_capacity((usize::from(w) * 3 + 1) * usize::from(h));
    for row in img.data.chunks_exact(stride).take(usize::from(h)) {
        rgb.push(0); // filter: none
        for &b in row.as_chunks::<4>().0 {
            let px = if lsb {
                u32::from_le_bytes(b)
            } else {
                u32::from_be_bytes(b)
            };
            rgb.extend([
                chan(px, vis.red_mask),
                chan(px, vis.green_mask),
                chan(px, vis.blue_mask),
            ]);
        }
    }
    Ok(Some(png_rgb(u32::from(w), u32::from(h), &rgb)))
}

/// A PNG from filtered RGB8 scanlines, with stored (uncompressed) deflate blocks: no codec
/// dependency, at the cost of size.
fn png_rgb(w: u32, h: u32, scanlines: &[u8]) -> Vec<u8> {
    fn crc32(parts: &[&[u8]]) -> u32 {
        let mut c = !0u32;
        for b in parts.iter().flat_map(|p| p.iter()) {
            c ^= u32::from(*b);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        out.extend(ty);
        out.extend(data);
        out.extend(crc32(&[ty, data]).to_be_bytes());
    }
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]); // 8-bit RGB, deflate, no filter method, no interlace
    let mut z = vec![0x78, 0x01];
    let mut blocks = scanlines.chunks(65535).peekable();
    if blocks.peek().is_none() {
        z.extend([1, 0, 0, 0xff, 0xff]);
    }
    while let Some(b) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        let len = b.len() as u16;
        z.extend(len.to_le_bytes());
        z.extend((!len).to_le_bytes());
        z.extend(b);
    }
    let (mut a, mut s) = (1u32, 0u32);
    for &byte in scanlines {
        a = (a + u32::from(byte)) % 65521;
        s = (s + a) % 65521;
    }
    z.extend(((s << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10];
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_text_carries_the_password_manager_hint() {
        let f = text_formats("draft");
        assert!(
            f.iter()
                .any(|f| f.format == "UTF8_STRING" && f.data == b"draft")
        );
        assert!(
            f.iter()
                .any(|f| f.format == "x-kde-passwordManagerHint" && f.data == b"secret")
        );
    }

    #[test]
    fn png_has_valid_structure() {
        let png = png_rgb(2, 1, &[0, 255, 0, 0, 0, 0, 255]);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 13, 10, 26, 10]);
        assert_eq!(&png[12..16], b"IHDR");
        // CRC of the IEND chunk is fixed.
        assert_eq!(&png[png.len() - 4..], &[0xae, 0x42, 0x60, 0x82]);
        // zlib adler32 of the scanlines ends the IDAT payload.
        let idat_end = png.len() - 12 - 4;
        assert_eq!(&png[idat_end - 4..idat_end], &[0x07, 0x00, 0x01, 0xff]);
    }
}
