# nugget-a11y

Cross-platform accessibility for Double Tap (read the focused field and its conversation, insert
at the caret with read-back verification) and for computer use (`ComputerUse`, used by
`nugget-computer`).

```rust
let a11y = nugget_a11y::system();            // or system_with(A11yConfig { .. })
if a11y.permission() != Permission::Granted {
    a11y.request_permission()?;              // macOS prompt / enable AT-SPI on Linux
}
if let Some(f) = a11y.focused()? {           // app, field, caret, thread, recipients, subject
    let out = a11y.insert("Sounds good!")?;  // InsertOutcome { method, verified }
}
```

Calls talk to other processes and are bounded by timeouts: run them in `spawn_blocking`.

| Module | What it does |
|---|---|
| `lib.rs` | Frozen contract: `Accessibility`, `Focused`, `Rect`, `Permission`, `InsertOutcome`, `system()` |
| `engine` | `Backend<P: Platform>`: roles, secure fields, deny list, thread walk, verification, clipboard fallback |
| `tree`, `role` | `Element` trait, serde `Snapshot`, bounded walk, `editor_root`, `flatten_text`; AX/UIA/AT-SPI roles → one `Role` |
| `thread` | Finds and parses the message list around the composer, per app family |
| `verify`, `clipboard` | Expected-value checks; clipboard save, paste, verify, restore |
| `deny`, `sanitize` | Excluded apps; control/bidi stripping and size caps |
| `platform/{linux,macos,windows}` | `Platform` implementations |

## Behaviour

- **Rich editors**: inside a contenteditable, the topmost editable ancestor (`tree::editor_root`)
  is read as the field; writes go to the focused element; U+FFFC objects are rebuilt
  (`tree::flatten_text`).
- **Insertion**: accessibility first, then a clipboard paste with every format saved and restored
  (rich web editors get the clipboard first for multi-line or ≥ 1000-character text,
  `A11yConfig::rich_paste`). Anything unconfirmed reports `verified: false` and is never retried.
  Return is never synthesised.
- **Verification**: exact match after normalising line endings, special spaces and invisible
  marks; loose (blank lines and spaces collapsed) for rich editors; an occurrence count when the
  selection is unknown or reads are truncated.
- **Threads**: scores containers near the composer, splits author, time and body, and applies
  app-family rules (Mail, Gmail, Outlook web, Slack, Teams, iMessage, WhatsApp) for authors,
  "You", quoted mail, recipients and subject. Keeps the newest 30 messages, up to 20 KB.
- **Safety**: only the frontmost focused window is read; secure fields' values are never
  requested and insertion is refused; denied apps (`DenyList::defaults()`: password managers,
  keychains, settings, banking; user rules `id:`, `name:`, `name~`, `host:`, `host~`) are neither
  read nor written; strings are sanitised and capped; temporary clipboard text is hidden from
  clipboard history and managers on all three OSes.

## Platform support

| | Linux (AT-SPI) | macOS (AX) | Windows (UIA) |
|---|---|---|---|
| Insertion | `EditableText` (multi-line web/LibreOffice: clipboard) | `AXSelectedText` (multi-line web: clipboard) | `ValuePattern` single-line plain edits only |
| Clipboard paste | X11 (INCR) or Wayland data-control; XTest Ctrl+V or `PasteText` | NSPasteboard; CGEvent Cmd+V | all formats + EMF; `SendInput` Ctrl+V |
| Permission | `org.a11y.Status.IsEnabled` | `AXIsProcessTrustedWithOptions` | not required |
| Double Tap status | integration tests + app matrix | real runners (Safari, TextEdit) | real runners (Chrome, Edge, Notepad, WordPad) |
| Computer use | apps, windows, press, set value, select, focus, scroll, expand, keys (XTest or AT-SPI), open app (`gtk-launch`), X11 screenshot | AX actions, CGEvent keys, `open -b`; no screenshot | UIA patterns, `SendInput`, `shell:AppsFolder`; no screenshot |

Secure fields never advertise `set_value`; launch names must be plain names or ids, started
without a shell.

## Tests

```sh
cargo test -p nugget-a11y
cargo clippy -p nugget-a11y --target aarch64-apple-darwin --all-targets -- -D warnings      # also x86_64-pc-windows-msvc
```

- Thread fixtures: `tests/fixtures/trees/` (`gtk_recorded_atspi.json` and
  `web_chrome_windows_mail.json` recorded; the native chat and mail trees modelled). Record with
  `Backend::snapshot()` → `Snapshot::to_json()`, redacted.
- Real-OS suites (`linux_wayland`, `macos_ax`, `windows_uia`) run in `real-os.yml`.

## Known gaps

- Browsers: Chrome on Linux needs `--force-renderer-accessibility`; Chrome, Edge and Firefox on
  macOS were never measured.
- Safari normalises pastes to NFC, so verification falls back to counting occurrences; likewise on
  macOS when a selection can't be placed in text-marker editors.
- LibreOffice exposes only laid-out paragraphs, so long documents may read partially (not checked
  on real LibreOffice). Rich web text is flattened within 400 objects / 500 ms.
- Computer-use text input types `\n` as Return on macOS and Windows when insertion is refused.
- **Computer use on macOS and Windows is tested on CI runners only** (real AX/UIA, scripted model,
  `real-os.yml`), not on physical machines. Unverified: scroll direction
  under natural scrolling and non-US key layouts (macOS); `activateWithOptions` from the
  background on macOS 14+; window id matching for identical frames; Windows foreground lock, UIPI
  for elevated windows, and UWP apps needing their AUMID.
- Wayland clipboard needs data-control (wlroots, KDE); GNOME Wayland uses XWayland and `PasteText`.
- The AT-SPI walk makes several D-Bus calls per node (`Cache.GetItems` could batch them).
