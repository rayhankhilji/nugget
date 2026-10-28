//! The general pasteboard: save every item with every type, write plain text, restore.

use nugget_core::{Error, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

use crate::clipboard::{Clipboard, ClipboardFormat, ClipboardSnapshot, MAX_SNAPSHOT_BYTES};

/// `NSPasteboardTypeString`.
const UTF8_PLAIN_TEXT: &str = "public.utf8-plain-text";
/// nspasteboard.org markers: clipboard managers skip transient and concealed items.
const TRANSIENT: &str = "org.nspasteboard.TransientType";
const CONCEALED: &str = "org.nspasteboard.ConcealedType";

pub struct Pasteboard(Retained<NSPasteboard>);

impl Pasteboard {
    pub fn general() -> Self {
        Pasteboard(NSPasteboard::generalPasteboard())
    }
}

impl Clipboard for Pasteboard {
    fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
        let Some(items) = self.0.pasteboardItems() else {
            return Ok(ClipboardSnapshot::default());
        };
        let mut out = Vec::new();
        let mut total = 0usize;
        for item in items.iter() {
            let mut formats = Vec::new();
            for ty in item.types().iter() {
                let Some(data) = item.dataForType(&ty) else {
                    // Promised data the owner could not provide; nothing to restore.
                    continue;
                };
                let bytes = data.to_vec();
                total += bytes.len();
                if total > MAX_SNAPSHOT_BYTES {
                    return Err(Error::Unsupported("pasteboard too large to save".into()));
                }
                formats.push(ClipboardFormat {
                    format: ty.to_string(),
                    data: bytes,
                });
            }
            out.push(formats);
        }
        Ok(ClipboardSnapshot { items: out })
    }

    fn set_text(&mut self, text: &str) -> Result<()> {
        self.0.clearContents();
        // One item: the text, marked transient and concealed so clipboard history tools skip it.
        let item = NSPasteboardItem::new();
        let mut ok = item.setString_forType(
            &NSString::from_str(text),
            &NSString::from_str(UTF8_PLAIN_TEXT),
        );
        let empty = NSData::new();
        for marker in [TRANSIENT, CONCEALED] {
            ok &= item.setData_forType(&empty, &NSString::from_str(marker));
        }
        let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> =
            vec![ProtocolObject::from_retained(item)];
        ok = ok && self.0.writeObjects(&NSArray::from_retained_slice(&items));
        if ok {
            Ok(())
        } else {
            Err(Error::Other("could not write to the pasteboard".into()))
        }
    }

    fn restore(&mut self, snapshot: &ClipboardSnapshot) -> Result<()> {
        self.0.clearContents();
        let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = snapshot
            .items
            .iter()
            .filter(|formats| !formats.is_empty())
            .map(|formats| {
                let item = NSPasteboardItem::new();
                for f in formats {
                    let data = NSData::with_bytes(&f.data);
                    item.setData_forType(&data, &NSString::from_str(&f.format));
                }
                ProtocolObject::from_retained(item)
            })
            .collect();
        if items.is_empty() {
            return Ok(());
        }
        let array = NSArray::from_retained_slice(&items);
        if self.0.writeObjects(&array) {
            Ok(())
        } else {
            Err(Error::Other("could not restore the pasteboard".into()))
        }
    }
}
