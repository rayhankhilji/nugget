# Double Tap: drafting in your voice

> **Status:** Double Tap works in the desktop app: the trigger, the draft overlay at the caret and
> the **Double Tap** settings page. The daemon serves `draft.begin`, `draft.cancel`,
> `draft.feedback` and `voice.status`. Its latency targets are not yet measured in the shipped app
> (see [Latency](#latency)).

Put the cursor in a text field in any app, press the trigger, and Nugget drafts a reply in your own
writing style, grounded in passages from your own data. You see the draft before anything is
inserted.

## How it works

1. **The trigger.** By default a double tap of a modifier key: right Command on macOS, right Ctrl on
   Windows and Linux X11. On Wayland, where double taps cannot be observed, the default is the chord
   `Ctrl+Alt+Space`. You can choose any modifier for a double tap (with a window between 100 ms and
   2 s; 350 ms by default), or any chord.
2. **Reading the field.** The desktop app reads the focused field of the frontmost app through the
   OS accessibility API (macOS Accessibility, Windows UI Automation, Linux AT-SPI): the field's text,
   its role and label, and the conversation around it (the messages of a chat or the thread of an
   email, with subject and recipients). Only the focused window of the frontmost app is read.
3. **Deciding what kind of field it is.** Fixed rules, not the model, decide whether the field is an
   email body, a chat composer, a comment, a document, a search box or a form field. Search boxes and
   other non-draftable fields are refused.
4. **Context.** The daemon retrieves passages related to the thread, subject and recipients and
   packs them into a budget (1 500 tokens by default), with citations. It does not send your whole
   vault, and it does not silently truncate.
5. **Voice.** Style statistics (length, greetings, sign-offs, formality, punctuation habits) and a
   few short examples of messages you wrote, chosen by recipient, then channel, then topic.
6. **The draft streams into an overlay** next to the caret. **Enter** accepts, **Tab** lets you edit,
   **Esc** cancels.
7. **Insertion.** Nugget inserts at the caret through the accessibility API and reads the field back
   to check the text arrived. If that is not possible, it pastes through the clipboard, saving and
   restoring every clipboard format, not just text.

Whether you accepted, edited or cancelled is recorded as feedback, so the voice profile can learn
from your edits.

## Your voice profile

The profile is built from messages **you** sent: sent mail (Gmail's `SENT` label, an IMAP Sent
folder), chat messages marked as yours, your Markdown notes and your feedback on earlier drafts.
Quoted replies and signatures are stripped first. "You" are recognised by the addresses and names you
configure, plus the most frequent `From` address of your sent mail. The profile is refreshed
incrementally as new items are indexed.

The settings page shows its status (from `voice.status`): how many of your messages it learned from,
per channel.

## Settings

On the **Double Tap** settings page:

| Setting | What it does |
|---|---|
| Trigger | The modifier to double-tap, or a chord |
| Provider per app | Which model to use in a given app or site, for example a local model in your work mail and a cloud model in a chat app. Otherwise the `synthesis` tier is used, like Ask |
| Excluded apps | Apps and sites where Double Tap never reads or drafts |

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/screenshots/double-tap-dark.png">
  <img src="../images/screenshots/double-tap-light.png" alt="The Double Tap settings page: the on switch, the trigger (a double-tapped key or a key combination), the provider per app and the excluded apps" loading="lazy">
</picture>

## What it will not do

- **It never reads password or secure fields.** The accessibility layer reports such a field as
  secure without reading its value, and the drafting engine refuses it (password, one-time code and
  card number fields).
- **Excluded apps are never read.** By default this covers password managers and keychains
  (1Password, Bitwarden, LastPass, Dashlane, KeePass and others), banking, payment and brokerage apps
  and sites, and system settings. In an excluded app no thread, recipients, subject or field text is
  read, and nothing is inserted. You can add more.
- **It does not send by itself.** It inserts text into the field; you press Send.
- **It does not record keystrokes.** For a double tap only modifier presses are observed; for every
  other key the listener learns only that *some* key was pressed (which cancels a tap that was part of
  a shortcut). A chord is registered with the OS, which reports only that chord. No key, key code or
  text is ever logged.

## Privacy

What leaves the device depends on the provider used for the draft:

- **Local or local-network model:** nothing leaves your machine (a local-network request is still
  recorded in the [egress ledger](privacy.md)).
- **Cloud model:** the field text, the thread read around it, the subject and recipients, the
  retrieved passages and your voice examples are sent to that provider. **Sensitive items are never
  included**: they are left out of the passages and the examples, and the engine refuses outright if
  a sensitive passage would still reach a cloud model. Each request is recorded in the egress ledger
  with feature `draft` and the items whose content was included.
- **Screenshots** are used only when no text thread could be read, only with a vision-capable model,
  and only with a local one unless you allow cloud screenshots (off by default).

Text read from other apps is treated as untrusted data in the prompt, never as instructions. Every
draft is audited without its content.

## Permissions per OS

| OS | What to allow |
|---|---|
| macOS | Accessibility for the Nugget app (reading fields and inserting). A double tap needs no extra permission; the kill switch's Escape counter needs Accessibility too |
| Windows | Nothing extra |
| Linux | Accessibility (AT-SPI) enabled in your desktop. On Wayland, use a chord: the trigger goes through the desktop's global-shortcuts portal |

## Latency

The targets are a first inserted token in under 1.2 s (median) with a cloud provider and under 3 s
with a local 4B-class model on a 16 GB machine. To get there, Nugget starts warming the provider,
retrieval and the voice lookup as soon as the trigger fires. These targets are measured by the
`draft` benchmark suite (see [Benchmarks](../architecture/benchmarks.md)); they are not yet measured
in the shipped app.
