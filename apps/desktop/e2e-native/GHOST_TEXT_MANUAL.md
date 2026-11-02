# Ghost text: manual test on macOS (Intel Mac)

Nothing macOS-specific in ghost text has run on a real Mac yet. That covers the CGEventTap key tap,
the NSWindow shim, AX font and colour reads, and transparency. These steps check each part. Each
step says what you should see; if something else happens, note the step number.

## 0. Build and permissions

1. `pnpm -C apps/desktop install && cargo tauri build --features tauri/custom-protocol` (or
   `cargo tauri dev`). Start `nuggetd` with a local model. Ghost text works with a cloud provider
   too, but the speed target assumes a local model.
2. Launch Nugget. On the Double Tap page, **Accessibility** must be *granted*. The suggestion key
   tap is an active event tap, which uses Accessibility. If the page says inline suggestions
   aren't available, also grant **Input Monitoring** in System Settings → Privacy & Security, then
   quit and reopen Nugget.
3. On the Double Tap page, check that **Suggest as you type** is on, with no "inline suggestions
   aren't available" note below it.

## 1. Automatic suggestion (TextEdit, then Notes)

1. Open TextEdit with a new plain-text document and type `Thanks for the update, I'll send the`.
   Stop typing.
2. **Expect:** within about 0.5–1 s, faint grey text appears right after the caret. It sits on
   the same baseline, the same size as your text, at about 45% opacity, and words appear as they
   stream in.
3. TextEdit must keep the keyboard focus: its window title stays active, the caret keeps
   blinking, and Nugget does not come to the front or appear in ⌘-Tab.
4. Move the mouse over the ghost text and click. The click must go to TextEdit (the caret moves),
   and the suggestion disappears.

## 2. Accept, accept a word, dismiss

1. Get a suggestion as in step 1. Press **⌥→**. **Expect:** exactly one word (with its leading
   space) is inserted into TextEdit and the rest of the suggestion stays, now starting after it.
2. Press **Tab**. **Expect:** the rest is inserted and the ghost text disappears. TextEdit gets
   **no** tab character, and holding Tab down does not add tabs afterwards.
3. Get a new suggestion and press **Esc**. **Expect:** it disappears, TextEdit receives no Esc,
   and no new suggestion appears until you type something new.
4. With **no** suggestion visible, press **Tab** in TextEdit. **Expect:** a normal tab character.
   Do the same in a browser form: Tab moves to the next field as usual. Press Esc in an open
   Safari find bar: it still closes.

## 3. Typing over a suggestion

1. Get a suggestion. Type its first two characters exactly. **Expect:** the ghost text shrinks
   by those two characters and stays in place after the caret.
2. Type a character that differs from the suggestion. **Expect:** it disappears at once. After
   about 0.4 s idle a new one may appear.
3. Press ← while a suggestion shows. **Expect:** it disappears (the caret moved).

## 4. Follows the field, hides on focus change

1. In Notes or Mail, get a suggestion near the bottom of a long note, then scroll with the
   trackpad. **Expect:** within about 0.25 s the ghost text follows the caret, or disappears if
   the caret scrolled out of view.
2. Move the window with its title bar while a suggestion shows. **Expect:** the ghost text
   follows.
3. Press ⌘-Tab to another app. **Expect:** the suggestion disappears.
4. Switch to a full-screen app (for example Safari in full screen) and type in a text field.
   **Expect:** ghost text appears over the full-screen app too.

## 5. Real apps

Repeat steps 1–2 in Slack (desktop), Mail (reply), Messages, Chrome (Gmail compose and a
`<textarea>`) and Safari. For each app, note:

- ghost text position: on the caret, offset, or at the field's top-left;
- font size: matches, or too big or small;
- colour: matches the app's text colour, or mid grey;
- whether Tab inserted.

Where an app gives no caret position, automatic suggestions stay off there and Double Tap opens
the panel instead. That is expected.

## 6. Guards (nothing must appear)

1. A password field (Safari login page, or System Settings → Users & Groups → change password):
   type and pause. **Expect:** nothing.
2. A field labelled "One-time code" or "Verification code", and a credit-card number field on a
   checkout page. **Expect:** nothing.
3. Terminal and iTerm. **Expect:** nothing. With "Allow terminals" on, still nothing in the middle
   of a line.
4. 1Password, Bitwarden, a banking site. **Expect:** nothing.
5. Engage the kill switch (Esc ×3, or the tray item) while a suggestion is visible.
   **Expect:** it disappears, and nothing appears until you resume.
6. Turn **Suggest as you type** off. **Expect:** no automatic suggestions. The Double Tap trigger
   still shows a draft as ghost text.

## 7. Double Tap on demand

1. In a Slack reply box (empty), double-tap right ⌘. **Expect:** the whole reply appears as ghost
   text at the caret, wrapping inside the box from the caret's line. Tab inserts all of it.
2. In an app where the caret can't be placed, or in the Double Tap page's practice field,
   double-tap. **Expect:** the old panel opens, as before.

## 8. Speed

With a local model and read-ahead on, time from the typing pause to the first ghost characters.
**Target:** about 0.5 s after the 0.4 s pause on a warm model. Note the times you see in Slack and
TextEdit.

## What to send back

The step numbers that failed, the app names, and screenshots of any misplaced ghost text. Also
include the `nugget-desktop` log lines that contain `ghost` (in the logs folder linked from the
Status page).
