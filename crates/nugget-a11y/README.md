# nugget-a11y

Cross-platform accessibility for Double Tap: read the focused text field and the conversation
around it, and insert text at the caret with read-back verification. M4 (computer use) builds on
the same element tree.

```rust
let a11y = nugget_a11y::system();            // or system_with(A11yConfig { .. })
if a11y.permission() != Permission::Granted {
    a11y.request_permission()?;              // macOS prompt / enable AT-SPI on Linux
}
if let Some(f) = a11y.focused()? {           // app, field, caret, thread, recipients, subject
    let out = a11y.insert("Sounds good!")?;  // InsertOutcome { method, verified }
}
```

Call these from a blocking context (`spawn_blocking` under tokio): every call talks to other
processes and is bounded by timeouts, not instant.

## How it works

| Layer | Module | What it does |
|---|---|---|
| Contract | `lib.rs` | `Accessibility`, `Focused`, `Rect`, `Permission`, `InsertOutcome`, `system()` (frozen) |
| Policy | `engine` | `Backend<P: Platform>`: role normalisation, secure fields, deny list, thread walk, verification, clipboard fallback |
| Elements | `tree` | `Element` trait over live objects, serde `Node`/`Snapshot`, a bounded nearest-first walk around the focus, the editor root and `flatten_text` for rich editors |
| Roles | `role` | AX / UIA / AT-SPI roles to one `Role`; the field role (`text_area`, `text_field`, `combo_box`, `document`, `web_area`, `search_field`) |
| Thread | `thread` | Finds the message list around the composer and parses messages, per app family |
| Verify | `verify` | Expected value after inserting over the selection; exact, then loose (rich editor) comparison |
| Clipboard | `clipboard` | `Clipboard` trait, save, paste, verify, restore on every path |
| Safety | `deny`, `sanitize` | Excluded apps; control/bidi stripping and size caps on every string |
| OS | `platform/{linux,macos,windows}` | `Platform` implementations |

**Rich editors.** In a browser the focus can land on a paragraph or text run inside a
contenteditable (Firefox always, Chromium sometimes after a click). The backend then climbs to the
topmost editable ancestor below the page (`tree::editor_root`) and reads that as the field: value,
role, label and the thread walk all start there. The climb relies on the platform marking
elements inside a contenteditable editable (AT-SPI `State::Editable`; on macOS
`AXEditableAncestor`), ends at an element whose `editable_root` attr is `true`
(`AXHighestEditableAncestor`), and never takes an editable wrapper that also holds a
message-like log or list. Writes (accessibility insertion, paste, caret
bounds) still go to the focused element, in its own offsets. When a value holds U+FFFC
embedded-object characters (AT-SPI stands one in for each child paragraph), it is rebuilt from the
children in order, one `\n` between blocks (`tree::flatten_text`), and the selection is mapped into
it from the caret's element, or dropped when it cannot be placed.

**Insertion.** The backend first inserts through accessibility: `AXSelectedText` on macOS,
`ValuePattern` on Windows (plain edits only), `EditableText.InsertText` on Linux. It then polls the
field and compares it with the expected value. If the field did not change, it falls back to the
clipboard: it saves every format, writes the text, synthesises a paste (Cmd+V, Ctrl+V or AT-SPI
`PasteText`), verifies the result, then restores the saved formats. A paste that has not shown up
after 1.2 s is looked for during a further 1.5 s grace period before the clipboard is restored.

Rich web editors (ProseMirror/TipTap, Quill, Draft.js, Lexical, CKEditor, mail compose bodies) get
the clipboard first when the text has a line break or at least 1000 characters
(`A11yConfig::rich_paste`): they handle a paste as one undo step with paragraphs intact, while
accessibility insertion loses the breaks or is reverted. Accessibility insertion follows only when
the paste never reached the app (no clipboard, `set_text` failed). Once the paste keystroke was
sent, an unverified paste (it may still land) or an error is reported as `Clipboard`,
`verified: false`, and a failure waits out the grace period before the clipboard is restored, so
a late paste cannot insert the old contents. Whichever method runs first, if the field changed
but not as expected, or a read after it fails or is truncated, the backend reports
`verified: false` and does not insert a second copy. It never
synthesises Return, and it strips `\r` and control characters from the inserted text.

**Verification.** The expected value (the text over the selection) must match the field after
normalising line endings, U+2028/2029, NBSP, zero-width characters, U+FFFC, bidi marks (read back
as spaces) and trailing newlines. Failing that, rich editors are compared loosely: blank lines
dropped and spaces collapsed on both sides, so `\n` against `\n\n` between paragraphs, a lost
empty paragraph or a dropped bidi mark still verify, but the whole value must match. With a known
selection and complete reads those are the only checks. When the selection is unknown or a value
was truncated, the text must appear once more than before (and, with a known selection, the text
before the caret must survive). Partial insertions, insertions elsewhere, a replaced field and
second copies fail.

**Thread heuristics.** These are platform-neutral and run on snapshots:

- The backend walks the composer's ancestors (from the editor root), nearest first, and scores
  containers in the sibling subtrees. A container scores higher for more message-like children,
  for a `log` or `list` role, and for a name like "Messages" or "Conversation". It also scores
  higher when it sits in the composer's column and above it. Sidebars, navigation landmarks,
  chat lists, channel lists and lists of other conversations (each item one link) score lower;
  table rows are never a thread.
- Each item is split into author, timestamp and body.
- App families (Mail, Gmail, Outlook web, Slack, Teams, iMessage, WhatsApp, generic) add their own
  rules:
  - summaries such as "Your iMessage, …" or "Alice, text, 10:41 AM";
  - consecutive messages grouped under one author (Slack, Teams; elsewhere when the body is known
    to be in the same column);
  - "You" or "me" taken as the user;
  - quoted mail ("On … wrote:", Outlook `From:/Sent:` blocks), cut from each message of a thread;
  - To/Cc/Bcc recipients, and the subject from the Subject field or a reply-style window title.
    Other apps get the mail rules (and recipients) only from an editable field labelled exactly
    "To", "Cc", "Subject"... within three levels around the composer.
- `from_me` comes from the author, the item summary, `ThreadLimits::self_names`, or (in chat apps)
  right-aligned bubbles. When one author is "You" and the user's messages have a column of their
  own (or the app is known to label the user "You"), the other named authors are not the user.
- The result keeps the newest 30 messages, up to 20 KB.

**Safety.**

- Only the frontmost app's focused window is read.
- The value of a secure field (`AXSecureTextField`, UIA `IsPassword`, AT-SPI `password text`) is
  never requested. The field is reported with `secure: true`, an empty value, no selection and no
  thread, and insertion into it is refused.
- In a denied app, `focused()` returns no thread, recipients, subject or value, and `insert()`
  refuses. `DenyList::defaults()` covers password managers, keychains, system settings and
  banking/payment apps and sites. Users add rules with `DenyList::with_user_rules(["id:…",
  "name:…", "name~…", "host:…", "host~…"])`.
- Every string read from another app is sanitised and capped: C0/C1 controls, bidi overrides and
  isolates, zero-width characters and BOMs are removed. URLs lose their userinfo, and only
  `http(s)`/`file` URLs are kept.
- On Windows, the temporary text and the restored clipboard are excluded from clipboard history,
  cloud clipboard and clipboard monitors.
- On macOS, the temporary text is marked `org.nspasteboard.TransientType` and
  `org.nspasteboard.ConcealedType`, so clipboard managers skip it.
- On Linux (X11 and Wayland), the temporary text is offered with `x-kde-passwordManagerHint:
  secret`, so clipboard managers that honour it (Klipper and others) do not record it. Restored
  clipboards are served as they were.

## Platform support

| | Linux (AT-SPI) | macOS (AX) | Windows (UIA) |
|---|---|---|---|
| Frontmost app, window title | active toplevel; desktop-file id from the process | `AXFocusedApplication`, bundle id; `AXManualAccessibility` set once per Chromium/Electron pid | foreground window, exe name; one bounded focus retry for a fresh browser window |
| Browser URL | `Document` `DocURL` | `AXURL` on `AXWebArea` | document `ValuePattern` (Chromium) |
| Field role, label, placeholder | ✓ | ✓ | ✓ (placeholder from HelpText) |
| Value, selection | `Text`; U+FFFC embedded objects (web contenteditable) replaced by their text, caret mapped; a LibreOffice paragraph reads its whole document | `AXValue`, `AXSelectedTextRange` (UTF-16 converted), else text markers for rich web editors | `TextPattern` document text, else `ValuePattern`; `\r\n`/`\r` normalised to `\n` in value and offsets |
| Caret bounds | `GetCharacterExtents` | `AXBoundsForRange` | `TextPattern` rectangles, DPI-scaled |
| Secure fields | `password text` role | `AXSecureTextField` subrole | `IsPassword` |
| Thread walk | ✓ | ✓ (batched attribute reads) | ✓ (cache requests) |
| A11y insertion | `EditableText` (multi-line text into web contenteditable or LibreOffice: clipboard) | `AXSelectedText` (multi-line text into web editors: clipboard) | `ValuePattern.SetValue` (single-line plain edits) |
| Clipboard fallback | X11 selections (INCR) or Wayland data-control; XTest Ctrl+V, `PasteText` | NSPasteboard items × types; CGEvent Cmd+V | all memory formats + EMF; `SendInput` Ctrl+V |
| Permission | `org.a11y.Status.IsEnabled` (request enables it) | `AXIsProcessTrustedWithOptions` | not required |
| **Status** | **run and verified** (integration tests) | compile-checked (`aarch64-apple-darwin`) | compile-checked (`x86_64-pc-windows-msvc`) |

### Computer use on Linux (M4)

| | Linux (AT-SPI) |
|---|---|
| Apps | registry desktop children with a visible or minimised window; pid from `GetConnectionUnixProcessID`; id from the desktop file, Flatpak or Snap id, else the executable; frontmost = owns the `active` window |
| Windows | `frame`, `window`, `dialog`, `alert`, `file chooser` children of each app; focused = `active`, minimised = `iconified`; bounds from `Component.GetExtents` |
| Window native id | bus name + object path |
| States | `disabled` (interactive and neither `enabled` nor `sensitive`), `checked` (`checked`, or `pressed` for toggle buttons, on checkable roles), `expanded` (`expandable`/`expanded`) |
| Press | the `click`/`press`/`activate`/`toggle`/`jump`/`open` action, else action 0; selectable items without actions are selected |
| Set value | `EditableText.SetTextContents` (caret moved to the end), else `Value.CurrentValue` for sliders, spin buttons, scroll bars |
| Select | the combo box's `Selection` for its menu items; menu items are activated; else the parent's `Selection.SelectChild`, else a `select` action |
| Focus | `Component.GrabFocus` |
| Scroll | amount 0: `Component.ScrollTo(anywhere)`; else the nearest scroll bar's `Value` (of the element or an ancestor) moved by its step |
| Expand | no-op when already in the wanted state; else the `expand or contract` (or `expand`/`collapse`, press) action |
| Key input | text into an editable focused field: `EditableText.InsertText` at the caret over the selection; otherwise, and for keys and chords: XTest on X11 (keysyms; a spare keycode is remapped for characters the keymap lacks), else the registry's `DeviceEventController.GenerateKeyboardEvent` |
| Open app | a running app: EWMH `_NET_ACTIVE_WINDOW` for its pid, and `GrabFocus` on its window or a focusable element in it; else `gtk-launch <desktop id>` (by file name or `Name=`) or a program of that name on `PATH` |
| Screenshot | X11 only: `GetImage` of the window's extents as an uncompressed PNG (24/32-bit true colour) |

Secure fields (`password text`) never advertise `set_value` and `perform` refuses it. Launch names
must be plain names or ids (no paths, no leading `-`); processes are started without a shell.

### Computer use on macOS and Windows (M4)

| | macOS (AX) | Windows (UIA) |
|---|---|---|
| Apps | window server list (`CGWindowListCopyWindowInfo`, layer 0) → `NSRunningApplication`, regular apps only; frontmost from `AXFocusedApplication` | `EnumWindows` owners; exe from `QueryFullProcessImageNameW`; frontmost from `GetForegroundWindow` |
| Windows | `AXWindows` per app: title, `AXPosition`/`AXSize`, `AXMinimized`, focused = `AXFocusedWindow` | visible, uncloaked, titled, non-tool top-level windows; `GetWindowRect` (DPI-scaled), `IsIconic`; element from `ElementFromHandle` |
| Window native id | `cg:<CGWindowID>` (matched by pid and frame), else `ax:<pid>:<index>` | `hwnd:<HWND>` |
| States | `disabled` (`AXEnabled`), `checked` (numeric `AXValue` of check box, radio, switch, toggle), `expanded` (`AXExpanded`/`AXDisclosing`, disclosure triangles) | `disabled` (`IsEnabled`), `checked` (`ToggleState`), `expanded` (`ExpandCollapseState`), `selected` (`SelectionItem.IsSelected`), all cached |
| Press | `AXPress` (else `AXPick`, `AXConfirm`, `AXOpen`) | `Invoke`, else `Toggle` |
| Set value | settable `AXValue` (string, or number for sliders/steppers) | `ValuePattern.SetValue`, else `RangeValue.SetValue` |
| Select | `AXSelected = true`, else the parent's `AXSelectedRows` | `SelectionItem.Select` |
| Focus | `AXFocused = true` | `SetFocus` |
| Scroll | amount 0: `AXScrollToVisible`; else a line-unit scroll wheel event at the element's centre | amount 0: `ScrollItem.ScrollIntoView`; else `ScrollPattern.Scroll` small steps on the nearest scrollable ancestor |
| Expand | set `AXExpanded`/`AXDisclosing`, press a disclosure triangle, or `AXShowMenu`/`AXCancel` on pop-ups and menus | `ExpandCollapse.Expand`/`Collapse` |
| Key input | CGEvents at the HID tap: text as Unicode strings (20 UTF-16 units per event, no modifier flags); keys and chords as ANSI virtual key codes | `SendInput`: text as `KEYEVENTF_UNICODE`; keys as VK codes (`VkKeyScanW` for punctuation), extended flag for the navigation cluster |
| Open app | activate a running app (`activateWithOptions`, falling back to `open -b`); else `/usr/bin/open -b <bundle id>` or `-a <name>` | `SetForegroundWindow` on its window (attaching to the foreground thread's input when the foreground lock refuses); else `explorer.exe shell:AppsFolder\<AUMID>`, or the exe from App Paths or `PATH` |
| Screenshot | not implemented (`None`) | not implemented (`None`) |

Neither platform reads or sets a secure field's value: secure fields never advertise `set_value`,
and `perform` refuses it. App names given to `open_app` must be plain names or ids (no paths, no
leading `-`); processes are started without a shell.

## Tests

```sh
cargo test -p nugget-a11y                 # unit tests: roles, thread fixtures, verify, clipboard, deny, engine
cargo clippy -p nugget-a11y --all-targets -- -D warnings
# cross-checks (rustup target add aarch64-apple-darwin x86_64-pc-windows-msvc)
cargo clippy -p nugget-a11y --target aarch64-apple-darwin --all-targets -- -D warnings
cargo clippy -p nugget-a11y --target x86_64-pc-windows-msvc --all-targets -- -D warnings
```

The thread heuristics are tested on trees in `tests/fixtures/trees/`:

- `gtk_recorded_atspi.json` was recorded from a real app.
- The others are modelled on how Slack, Messages, WhatsApp, Gmail and Apple Mail expose their
  trees. Replace them with real recordings as they become available: `Backend::snapshot()` →
  `Snapshot::to_json()`, after redacting personal content.

### Linux integration tests (`NUGGET_A11Y_IT=1`)

The tests do the following:

1. They start Xvfb and a private `dbus-daemon`. The accessibility bus is started by D-Bus
   activation.
2. They re-run the test binary with that environment.
3. They drive the small GTK 3 app in `tests/fixtures/gtk_test_app.py`. The app prints its own
   buffer contents, which gives the tests an oracle independent of AT-SPI.

The scenarios are:

- the permission (enable AT-SPI);
- reading the composer and its thread;
- accessibility insertion of non-ASCII, multi-line text, verified;
- clipboard insertion through XTest Ctrl+V, with every saved format restored (including a 300 KB
  format sent with INCR);
- secure-field handling;
- placeholder and search-field detection;
- the deny list;
- a real snapshot.

`tests/linux_atspi_computer.rs` drives `tests/fixtures/gtk_computer_app.py` (button, check box,
entries, combo box, lists, a scrolled list, an expander) through `system_computer_with`:
window enumeration, snapshots with stable ids and no password value, a press that changes a label,
check box toggle, set value, typed text, `ctrl+a backspace` and XTest keys, list and combo
selection, scrolling, expanding, focus, activating the running app, and a screenshot.

Setup on Debian/Ubuntu:

```sh
sudo apt-get install -y xvfb dbus dbus-x11 at-spi2-core python3-gi gir1.2-gtk-3.0
NUGGET_A11Y_IT=1 cargo test -p nugget-a11y --test linux_atspi --test linux_atspi_computer -- --nocapture
```

- If `python3` is not the interpreter that has PyGObject (for example, a newer Python sits ahead of
  the system one in `PATH`), set `NUGGET_A11Y_IT_PYTHON=/usr/bin/python3.12` or whichever version
  has `gi`.
- Without `at-spi2-core` the accessibility bus is never activated; without `gir1.2-gtk-3.0`
  `gi.require_version('Gtk', '3.0')` fails.
- Set `NUGGET_A11Y_RECORD=<dir>` to write the recorded tree to `<dir>/gtk_recorded.json`.

## Known gaps

- macOS and Windows have not been run on a real machine yet. Their logic is shared with Linux
  through `engine`, but the OS calls are compile-checked only.
- On macOS, rich web editors without `AXValue`/`AXSelectedTextRange` are read through text
  markers; when the selection cannot be placed in the element's marker text it is left unknown
  and verification falls back to counting occurrences.
- On Windows, accessibility insertion is limited to single-line plain edit controls.
  Multi-line edits (Notepad), rich edits, documents and web content use the clipboard.
- On Linux, LibreOffice exposes only the paragraphs it has laid out as children of the
  document, so a long document may read in part. The read is marked truncated when a scroll
  bar next to the document has a range or a read budget runs out; a document without such a
  scroll bar is assumed to be exposed in full (not checked on a real LibreOffice yet). Rich web
  text is flattened within a budget (400 objects, 500 ms) and only for editable elements in a
  web document.
- On macOS and Windows, computer-use text input still types `\n` as Return when insertion is
  refused; on Linux it pastes text with line breaks instead (clipboard saved and restored).
- Computer use on macOS and Windows is compile-checked only. In particular, unverified on real
  machines:
  - macOS: scroll wheel events are posted at a location without moving the pointer, and their
    direction under "natural scrolling" has not been checked. Chord keys use US ANSI key
    positions, so on other layouts (AZERTY, Dvorak) `cmd+a` may press the key labelled
    differently. macOS 14+ may refuse `activateWithOptions` from a background process; the
    fallback goes through `open -b`.
  - macOS: window titles in `AXWindows` need only the Accessibility permission, but matching
    to CGWindowIDs relies on frames, so two windows with identical frames may swap ids.
  - Windows: `SetForegroundWindow` can still be refused by the foreground lock. `SendInput`
    cannot reach elevated windows (UIPI). Packaged (UWP) apps show up as
    `ApplicationFrameHost`; their AUMID is not read, so `open_app` needs the AUMID itself to
    launch them.
  - The pure helpers (key tables, action mapping) have unit tests in `platform/{macos,windows}`
    that only run on their own OS; on Linux they are type-checked by the cross-target clippy.
- On Wayland, the clipboard needs a compositor with data-control (wlroots, KDE). On GNOME
  Wayland, the X11 path through XWayland is used, and key synthesis falls back to AT-SPI
  `PasteText`.
- The AT-SPI tree walk reads each node with several D-Bus calls. The `Cache.GetItems` bulk read is
  a possible optimisation.
- The thread heuristics for apps other than the GTK test app are tested on modelled trees, not on
  recordings.
