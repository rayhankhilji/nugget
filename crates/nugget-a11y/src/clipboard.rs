//! Clipboard fallback for insertion: save every format on the clipboard, put the text there,
//! synthesise a paste, verify by reading the field back, then restore what was there before.
//!
//! The OS specifics sit behind [`Clipboard`] and the paste closure, so the sequencing (and the
//! guarantee that the previous contents come back on every path) is unit-tested here.

use std::time::{Duration, Instant};

use nugget_core::{Error, Result};

/// Refuse the clipboard fallback rather than risk losing more than this much clipboard data.
pub const MAX_SNAPSHOT_BYTES: usize = 256 * 1024 * 1024;

/// One clipboard format and its bytes. `format` is the platform's name: a UTI on macOS
/// (`public.utf8-plain-text`), a registered name or `#<id>` for a standard format on Windows,
/// a MIME type or X11 target on Linux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardFormat {
    pub format: String,
    pub data: Vec<u8>,
}

/// Everything that was on the clipboard. macOS pasteboards hold several items, each with its
/// own formats; the other platforms have exactly one item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    pub items: Vec<Vec<ClipboardFormat>>,
}

impl ClipboardSnapshot {
    pub fn is_empty(&self) -> bool {
        self.items.iter().all(Vec::is_empty)
    }

    pub fn total_bytes(&self) -> usize {
        self.items.iter().flatten().map(|f| f.data.len()).sum()
    }
}

/// Platform clipboard access.
pub trait Clipboard {
    /// Every item and format currently on the clipboard.
    fn snapshot(&mut self) -> Result<ClipboardSnapshot>;
    /// Replace the clipboard with plain text.
    fn set_text(&mut self, text: &str) -> Result<()>;
    /// Replace the clipboard with `snapshot` (clearing it when the snapshot is empty).
    fn restore(&mut self, snapshot: &ClipboardSnapshot) -> Result<()>;
}

/// Timing of the paste sequence.
#[derive(Debug, Clone, Copy)]
pub struct PasteTiming {
    /// Wait after setting the clipboard before pasting.
    pub before_paste: Duration,
    /// Poll interval while waiting for the paste to land.
    pub poll: Duration,
    /// Give up verifying after this long.
    pub verify_timeout: Duration,
    /// Wait after a successful verification before restoring, for apps that read the
    /// clipboard lazily.
    pub before_restore: Duration,
    /// When the paste was sent but never showed up, wait this long (and check once more) before
    /// restoring, so a slow app does not read the restored old contents and paste those.
    pub unverified_grace: Duration,
}

impl Default for PasteTiming {
    fn default() -> Self {
        PasteTiming {
            before_paste: Duration::from_millis(30),
            poll: Duration::from_millis(40),
            verify_timeout: Duration::from_millis(1200),
            before_restore: Duration::from_millis(80),
            unverified_grace: Duration::from_millis(1500),
        }
    }
}

/// Result of [`paste_with_restore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteResult {
    pub verified: bool,
    /// The previous clipboard contents were put back.
    pub restored: bool,
}

/// Paste `text` through the clipboard and put the previous contents back.
///
/// `paste` synthesises the paste (Cmd/Ctrl+V, or a toolkit paste action); it never sends
/// Return. `verify` reads the field back and reports whether the text arrived; it is polled
/// until it returns `true` or the timeout passes. The previous clipboard contents are restored
/// on every path after they were saved, including errors from `set_text`, `paste` or `verify`.
pub fn paste_with_restore(
    clipboard: &mut dyn Clipboard,
    text: &str,
    paste: &mut dyn FnMut() -> Result<()>,
    verify: &mut dyn FnMut() -> Result<bool>,
    timing: PasteTiming,
) -> Result<PasteResult> {
    let saved = clipboard.snapshot()?;
    if saved.total_bytes() > MAX_SNAPSHOT_BYTES {
        return Err(Error::Unsupported(
            "clipboard holds too much data to save and restore safely".into(),
        ));
    }
    let outcome = (|| -> Result<bool> {
        clipboard.set_text(text)?;
        sleep(timing.before_paste);
        paste()?;
        let deadline = Instant::now() + timing.verify_timeout;
        loop {
            sleep(timing.poll);
            if verify()? {
                sleep(timing.before_restore);
                return Ok(true);
            }
            if Instant::now() >= deadline {
                // The paste may still be in flight: restoring now could make the app paste
                // the old clipboard instead. Wait out the grace period, then look once more.
                sleep(timing.unverified_grace);
                return verify();
            }
        }
    })();
    let restored = match clipboard.restore(&saved) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "could not restore the clipboard after pasting");
            false
        }
    };
    let verified = outcome?;
    Ok(PasteResult { verified, restored })
}

fn sleep(d: Duration) {
    if !d.is_zero() {
        std::thread::sleep(d);
    }
}

/// An in-memory clipboard, for tests and for platforms without one.
#[derive(Debug, Default, Clone)]
pub struct MemoryClipboard {
    pub contents: ClipboardSnapshot,
    /// Fail the next `set_text`.
    pub fail_set: bool,
    pub restores: usize,
}

impl Clipboard for MemoryClipboard {
    fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
        Ok(self.contents.clone())
    }
    fn set_text(&mut self, text: &str) -> Result<()> {
        if self.fail_set {
            return Err(Error::Other("set failed".into()));
        }
        self.contents = ClipboardSnapshot {
            items: vec![vec![ClipboardFormat {
                format: "text/plain;charset=utf-8".into(),
                data: text.as_bytes().to_vec(),
            }]],
        };
        Ok(())
    }
    fn restore(&mut self, snapshot: &ClipboardSnapshot) -> Result<()> {
        self.contents = snapshot.clone();
        self.restores += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast() -> PasteTiming {
        PasteTiming {
            before_paste: Duration::ZERO,
            poll: Duration::from_millis(1),
            verify_timeout: Duration::from_millis(30),
            before_restore: Duration::ZERO,
            unverified_grace: Duration::from_millis(5),
        }
    }

    fn rich() -> ClipboardSnapshot {
        ClipboardSnapshot {
            items: vec![
                vec![
                    ClipboardFormat {
                        format: "public.png".into(),
                        data: vec![0x89, b'P', b'N', b'G', 0, 1, 2],
                    },
                    ClipboardFormat {
                        format: "public.html".into(),
                        data: b"<b>hi</b>".to_vec(),
                    },
                ],
                vec![ClipboardFormat {
                    format: "com.example.private".into(),
                    data: vec![1, 2, 3],
                }],
            ],
        }
    }

    /// A clipboard whose contents the paste closure can see, like a real one.
    struct Shared(std::rc::Rc<std::cell::RefCell<MemoryClipboard>>);

    impl Clipboard for Shared {
        fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
            self.0.borrow_mut().snapshot()
        }
        fn set_text(&mut self, text: &str) -> Result<()> {
            self.0.borrow_mut().set_text(text)
        }
        fn restore(&mut self, s: &ClipboardSnapshot) -> Result<()> {
            self.0.borrow_mut().restore(s)
        }
    }

    #[test]
    fn pastes_text_then_restores_every_format() {
        let state = std::rc::Rc::new(std::cell::RefCell::new(MemoryClipboard {
            contents: rich(),
            ..Default::default()
        }));
        let mut cb = Shared(state.clone());
        let field = std::cell::RefCell::new(String::from("Hi "));
        let r = paste_with_restore(
            &mut cb,
            "there",
            &mut || {
                // The target app reads whatever is on the clipboard when the paste lands.
                let st = state.borrow();
                let data = &st.contents.items[0][0].data;
                field
                    .borrow_mut()
                    .push_str(std::str::from_utf8(data).unwrap());
                Ok(())
            },
            &mut || Ok(field.borrow().as_str() == "Hi there"),
            fast(),
        )
        .unwrap();
        assert_eq!(
            r,
            PasteResult {
                verified: true,
                restored: true
            }
        );
        assert_eq!(state.borrow().contents, rich());
        assert_eq!(state.borrow().restores, 1);
    }

    #[test]
    fn slow_paste_is_not_cut_off_by_the_restore() {
        let state = std::rc::Rc::new(std::cell::RefCell::new(MemoryClipboard {
            contents: rich(),
            ..Default::default()
        }));
        let mut cb = Shared(state.clone());
        let started = std::cell::Cell::new(None::<Instant>);
        let landed = std::cell::Cell::new(false);
        let mut timing = fast();
        timing.unverified_grace = Duration::from_millis(60);
        let r = paste_with_restore(
            &mut cb,
            "there",
            &mut || {
                started.set(Some(Instant::now()));
                Ok(())
            },
            &mut || {
                // The app reads the clipboard 45 ms after the paste: after the verify timeout,
                // inside the grace period. The restore must not have happened yet.
                let late = started.get().unwrap().elapsed() >= Duration::from_millis(45);
                if late && !landed.get() {
                    assert_eq!(
                        state.borrow().restores,
                        0,
                        "restored before the paste landed"
                    );
                    landed.set(true);
                }
                Ok(landed.get())
            },
            timing,
        )
        .unwrap();
        assert!(r.verified && r.restored);
        assert_eq!(state.borrow().contents, rich());
    }

    #[test]
    fn restores_when_unverified() {
        let mut cb = MemoryClipboard {
            contents: rich(),
            ..Default::default()
        };
        let r =
            paste_with_restore(&mut cb, "x", &mut || Ok(()), &mut || Ok(false), fast()).unwrap();
        assert!(!r.verified);
        assert!(r.restored);
        assert_eq!(cb.contents, rich());
    }

    #[test]
    fn restores_when_paste_fails() {
        let mut cb = MemoryClipboard {
            contents: rich(),
            ..Default::default()
        };
        let err = paste_with_restore(
            &mut cb,
            "x",
            &mut || Err(Error::Unsupported("no paste".into())),
            &mut || Ok(true),
            fast(),
        );
        assert!(err.is_err());
        assert_eq!(cb.contents, rich());
        assert_eq!(cb.restores, 1);
    }

    #[test]
    fn restores_when_set_fails_and_when_verify_errors() {
        let mut cb = MemoryClipboard {
            contents: rich(),
            fail_set: true,
            ..Default::default()
        };
        assert!(
            paste_with_restore(&mut cb, "x", &mut || Ok(()), &mut || Ok(true), fast()).is_err()
        );
        assert_eq!(cb.restores, 1);

        let mut cb = MemoryClipboard {
            contents: rich(),
            ..Default::default()
        };
        assert!(
            paste_with_restore(
                &mut cb,
                "x",
                &mut || Ok(()),
                &mut || Err(Error::Other("gone".into())),
                fast()
            )
            .is_err()
        );
        assert_eq!(cb.contents, rich());
    }

    #[test]
    fn empty_clipboard_is_restored_empty() {
        let mut cb = MemoryClipboard::default();
        paste_with_restore(&mut cb, "x", &mut || Ok(()), &mut || Ok(true), fast()).unwrap();
        assert!(cb.contents.is_empty());
    }

    #[test]
    fn refuses_oversized_snapshot_without_touching_clipboard() {
        struct Huge;
        impl Clipboard for Huge {
            fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
                Ok(ClipboardSnapshot {
                    items: vec![vec![ClipboardFormat {
                        format: "big".into(),
                        data: vec![0; MAX_SNAPSHOT_BYTES + 1],
                    }]],
                })
            }
            fn set_text(&mut self, _: &str) -> Result<()> {
                panic!("must not overwrite")
            }
            fn restore(&mut self, _: &ClipboardSnapshot) -> Result<()> {
                panic!("must not restore")
            }
        }
        assert!(
            paste_with_restore(&mut Huge, "x", &mut || Ok(()), &mut || Ok(true), fast()).is_err()
        );
    }
}
