//! The Win32 clipboard: save every format (HGLOBAL data, and enhanced metafiles through their
//! bits), write Unicode text, restore. Our temporary text and the restore are marked
//! `ExcludeClipboardContentFromMonitorProcessing` and `CanIncludeInClipboardHistory = 0`, so
//! drafts never land in clipboard history or cloud clipboard.
#![allow(unsafe_code)]

use nugget_core::{Error, Result};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{GetEnhMetaFileBits, HENHMETAFILE, SetEnhMetaFileBits};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::{HSTRING, w};

use crate::clipboard::{Clipboard, ClipboardFormat, ClipboardSnapshot, MAX_SNAPSHOT_BYTES};

const CF_BITMAP: u32 = 2;
const CF_METAFILEPICT: u32 = 3;
const CF_PALETTE: u32 = 9;
const CF_UNICODETEXT: u32 = 13;
const CF_ENHMETAFILE: u32 = 14;
const CF_OWNERDISPLAY: u32 = 0x80;
const CF_DSPBITMAP: u32 = 0x82;
const CF_DSPMETAFILEPICT: u32 = 0x83;
const CF_DSPENHMETAFILE: u32 = 0x8E;
const CF_PRIVATEFIRST: u32 = 0x200;
const CF_GDIOBJLAST: u32 = 0x3FF;

const EXCLUDE_FROM_MONITORS: &str = "ExcludeClipboardContentFromMonitorProcessing";
const HISTORY: &str = "CanIncludeInClipboardHistory";
const CLOUD: &str = "CanUploadToCloudClipboard";

fn werr(e: windows::core::Error) -> Error {
    Error::Other(format!("clipboard: {e}"))
}

/// Formats whose data is a GDI handle (or owner-drawn) rather than memory; the clipboard
/// synthesises the bitmap ones from the DIB formats we do save.
fn is_handle_format(f: u32) -> bool {
    matches!(
        f,
        CF_BITMAP
            | CF_METAFILEPICT
            | CF_PALETTE
            | CF_OWNERDISPLAY
            | CF_DSPBITMAP
            | CF_DSPMETAFILEPICT
            | CF_DSPENHMETAFILE
    ) || (CF_PRIVATEFIRST..=CF_GDIOBJLAST).contains(&f)
}

fn format_name(f: u32) -> String {
    if f >= 0xC000 {
        let mut buf = [0u16; 256];
        // SAFETY: `buf` is a valid buffer; returns the number of characters written.
        let n = unsafe { GetClipboardFormatNameW(f, &mut buf) }.max(0) as usize;
        if n > 0 {
            return String::from_utf16_lossy(&buf[..n]);
        }
    }
    format!("#{f}")
}

fn format_id(name: &str) -> Option<u32> {
    if let Some(n) = name.strip_prefix('#') {
        return n.parse().ok();
    }
    // SAFETY: a valid, NUL-terminated format name.
    let id = unsafe { RegisterClipboardFormatW(&HSTRING::from(name)) };
    (id != 0).then_some(id)
}

/// A message-only window to own the clipboard (SetClipboardData fails without an owner).
pub struct Win32Clipboard {
    hwnd: HWND,
}

impl Win32Clipboard {
    pub fn new() -> Result<Self> {
        // SAFETY: creates a message-only window of the predefined STATIC class; destroyed in
        // Drop.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("nugget-a11y clipboard"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        }
        .map_err(werr)?;
        Ok(Win32Clipboard { hwnd })
    }

    /// Open the clipboard, retrying while another process holds it.
    fn open(&self) -> Result<Guard> {
        for _ in 0..50 {
            // SAFETY: `hwnd` is our window.
            if unsafe { OpenClipboard(Some(self.hwnd)) }.is_ok() {
                return Ok(Guard);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err(Error::Other("the clipboard is busy".into()))
    }
}

impl Drop for Win32Clipboard {
    fn drop(&mut self) {
        // SAFETY: the window created in `new`.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

/// Closes the clipboard when dropped.
struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: the clipboard was opened by this thread.
        let _ = unsafe { CloseClipboard() };
    }
}

fn read_global(h: HANDLE) -> Option<Vec<u8>> {
    let g = HGLOBAL(h.0);
    // SAFETY: `g` is clipboard-owned memory; we only read it while locked and never free it.
    unsafe {
        let size = GlobalSize(g);
        let p = GlobalLock(g);
        if p.is_null() {
            return None;
        }
        let data = std::slice::from_raw_parts(p.cast::<u8>(), size).to_vec();
        let _ = GlobalUnlock(g);
        Some(data)
    }
}

/// Hand `data` to the clipboard as `format`. The clipboard owns the memory on success.
fn put(format: u32, data: &[u8]) -> Result<()> {
    if format == CF_ENHMETAFILE {
        // SAFETY: `data` is the bits of an enhanced metafile we saved.
        let hemf = unsafe { SetEnhMetaFileBits(data) };
        if hemf.is_invalid() {
            return Err(Error::Other(
                "could not rebuild an enhanced metafile".into(),
            ));
        }
        // SAFETY: the clipboard takes ownership of the metafile handle.
        unsafe { SetClipboardData(format, Some(HANDLE(hemf.0))) }.map_err(werr)?;
        return Ok(());
    }
    // SAFETY: allocate, fill and hand over a movable global block; freed on failure.
    unsafe {
        let g = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1)).map_err(werr)?;
        let p = GlobalLock(g);
        if p.is_null() {
            let _ = GlobalFree(Some(g));
            return Err(Error::Other("GlobalLock failed".into()));
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), p.cast::<u8>(), data.len());
        let _ = GlobalUnlock(g);
        if let Err(e) = SetClipboardData(format, Some(HANDLE(g.0))) {
            let _ = GlobalFree(Some(g));
            return Err(werr(e));
        }
    }
    Ok(())
}

fn mark_private() {
    for (name, value) in [
        (EXCLUDE_FROM_MONITORS, None),
        (HISTORY, Some(0u32)),
        (CLOUD, Some(0u32)),
    ] {
        if let Some(id) = format_id(name) {
            let bytes = value.map(u32::to_ne_bytes).unwrap_or_default();
            let _ = put(id, &bytes);
        }
    }
}

impl Clipboard for Win32Clipboard {
    fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
        let _g = self.open()?;
        let mut formats = Vec::new();
        let mut total = 0usize;
        let mut f = 0u32;
        loop {
            // SAFETY: the clipboard is open; enumerates the next format id.
            f = unsafe { EnumClipboardFormats(f) };
            if f == 0 {
                break;
            }
            if is_handle_format(f) {
                continue;
            }
            // SAFETY: the clipboard is open; the handle stays owned by the clipboard.
            let Ok(h) = (unsafe { GetClipboardData(f) }) else {
                continue;
            };
            let data = if f == CF_ENHMETAFILE {
                let hemf = HENHMETAFILE(h.0);
                // SAFETY: a valid metafile handle; first call gets the size.
                let n = unsafe { GetEnhMetaFileBits(hemf, None) } as usize;
                let mut buf = vec![0u8; n];
                // SAFETY: `buf` has `n` bytes.
                unsafe { GetEnhMetaFileBits(hemf, Some(&mut buf)) };
                Some(buf)
            } else {
                read_global(h)
            };
            let Some(data) = data else { continue };
            total += data.len();
            if total > MAX_SNAPSHOT_BYTES {
                return Err(Error::Unsupported("clipboard too large to save".into()));
            }
            formats.push(ClipboardFormat {
                format: format_name(f),
                data,
            });
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
        let _g = self.open()?;
        // SAFETY: the clipboard is open and owned by our window after emptying.
        unsafe { EmptyClipboard() }.map_err(werr)?;
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        let bytes: Vec<u8> = wide.iter().flat_map(|u| u.to_ne_bytes()).collect();
        put(CF_UNICODETEXT, &bytes)?;
        mark_private();
        Ok(())
    }

    fn restore(&mut self, snapshot: &ClipboardSnapshot) -> Result<()> {
        let _g = self.open()?;
        // SAFETY: as above.
        unsafe { EmptyClipboard() }.map_err(werr)?;
        let mut failed = 0;
        for f in snapshot.items.iter().flatten() {
            if [EXCLUDE_FROM_MONITORS, HISTORY, CLOUD].contains(&f.format.as_str()) {
                continue;
            }
            match format_id(&f.format) {
                Some(id) => {
                    if put(id, &f.data).is_err() {
                        failed += 1;
                    }
                }
                None => failed += 1,
            }
        }
        // The original copy is already in history; do not record the restore again.
        mark_private();
        if failed > 0 {
            return Err(Error::Other(format!(
                "{failed} clipboard formats could not be restored"
            )));
        }
        Ok(())
    }
}
