//! Wayland clipboard through `wl-clipboard-rs` (the wlr/ext data-control protocols, supported
//! by wlroots compositors and KDE). Where the compositor lacks data control (GNOME), creating
//! the clipboard fails and the X11 path (XWayland) is used instead.

use std::io::Read;

use nugget_core::{Error, Result};
use wl_clipboard_rs::copy::{self, MimeSource, Source};
use wl_clipboard_rs::paste::{self, ClipboardType, Seat};

use crate::clipboard::{Clipboard, ClipboardFormat, ClipboardSnapshot, MAX_SNAPSHOT_BYTES};

fn werr(e: impl std::fmt::Display) -> Error {
    Error::Other(format!("Wayland clipboard: {e}"))
}

/// The text, plus the hint that keeps clipboard managers from recording a draft we paste
/// (`x-kde-passwordManagerHint: secret`, honoured by Klipper and others). Not used on restore.
fn text_sources(text: &str) -> Vec<MimeSource> {
    vec![
        MimeSource {
            source: Source::Bytes(text.as_bytes().into()),
            mime_type: copy::MimeType::Text,
        },
        MimeSource {
            source: Source::Bytes(super::x11::PASSWORD_HINT_DATA.into()),
            mime_type: copy::MimeType::Specific(super::x11::PASSWORD_HINT_TARGET.into()),
        },
    ]
}

pub struct WaylandClipboard;

impl WaylandClipboard {
    pub fn new() -> Result<Self> {
        // Probe for data-control support; an empty clipboard is fine.
        match paste::get_mime_types(ClipboardType::Regular, Seat::Unspecified) {
            Ok(_)
            | Err(paste::Error::ClipboardEmpty)
            | Err(paste::Error::NoSeats)
            | Err(paste::Error::NoMimeType) => Ok(WaylandClipboard),
            Err(e) => Err(werr(e)),
        }
    }
}

impl Clipboard for WaylandClipboard {
    fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
        let types = match paste::get_mime_types(ClipboardType::Regular, Seat::Unspecified) {
            Ok(t) => t,
            Err(paste::Error::ClipboardEmpty | paste::Error::NoSeats) => {
                return Ok(ClipboardSnapshot::default());
            }
            Err(e) => return Err(werr(e)),
        };
        let mut types: Vec<String> = types.into_iter().collect();
        types.sort();
        let mut formats = Vec::new();
        let mut total = 0usize;
        for mime in types {
            let (mut pipe, _) = match paste::get_contents(
                ClipboardType::Regular,
                Seat::Unspecified,
                paste::MimeType::Specific(&mime),
            ) {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!(%mime, error = %e, "could not read clipboard type");
                    continue;
                }
            };
            let mut data = Vec::new();
            pipe.by_ref()
                .take((MAX_SNAPSHOT_BYTES + 1 - total) as u64)
                .read_to_end(&mut data)
                .map_err(werr)?;
            total += data.len();
            if total > MAX_SNAPSHOT_BYTES {
                return Err(Error::Unsupported("clipboard too large to save".into()));
            }
            formats.push(ClipboardFormat { format: mime, data });
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
        // Served from a background thread until something else is copied.
        copy::Options::new()
            .copy_multi(text_sources(text))
            .map_err(werr)
    }

    fn restore(&mut self, snapshot: &ClipboardSnapshot) -> Result<()> {
        let sources: Vec<MimeSource> = snapshot
            .items
            .iter()
            .flatten()
            .map(|f| MimeSource {
                source: Source::Bytes(f.data.clone().into_boxed_slice()),
                mime_type: copy::MimeType::Specific(f.format.clone()),
            })
            .collect();
        if sources.is_empty() {
            return copy::clear(copy::ClipboardType::Regular, copy::Seat::All).map_err(werr);
        }
        copy::Options::new().copy_multi(sources).map_err(werr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_text_offers_the_password_manager_hint() {
        let s = text_sources("draft");
        assert!(matches!(s[0].mime_type, copy::MimeType::Text));
        assert!(matches!(
            &s[1].mime_type,
            copy::MimeType::Specific(m) if m == "x-kde-passwordManagerHint"
        ));
        assert!(matches!(&s[1].source, Source::Bytes(b) if &b[..] == b"secret"));
    }
}
