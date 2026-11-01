# The desktop app on your computer

Nugget keeps running in the background so your sources stay indexed. This page covers the parts
that live outside the main window: the tray or menu bar icon, opening at login, notifications,
the quick-ask box, `nugget://` links and plugin files, and what happens when you quit or your
computer sleeps.

## Tray and menu bar

| System | Where the icon is | Clicking it |
|---|---|---|
| macOS | The menu bar. The icon is a template image, so it matches light and dark mode. | Left click opens Nugget. Right click opens the menu. |
| Windows | The notification area (it may be under the ^ arrow). | Left click opens Nugget. Right click opens the menu. |
| Linux | The system tray, through StatusNotifierItem (AppIndicator). | Every click opens the menu. |

At the top, the menu shows status lines:

- Whether Nugget is running.
- Whether indexing is busy, up to date or paused.
- Whether a model is ready.
- Whether an agent is running or waiting for you.

The status lines are refreshed every 20 seconds, soon after anything changes, and when your
computer wakes up. While the background service isn't running, only the first line is shown.

Below the status lines are these items:

- **Ask…** opens the [quick-ask box](#quick-ask).
- **Double Tap** starts a draft in the field that has focus, the same as your
  [Double Tap trigger](double-tap.md).
- **Pause indexing** turns off every source that is on, and **Resume indexing** turns exactly
  those back on. A pause lasts across restarts, until you resume.
- **Stop all agents** is the [kill switch](safety.md). It changes to **Resume agents**
  while agents are stopped.
- **Open Nugget**, **Status**, **Updates…** and **Quit Nugget**.

The menu uses the same language as the app.

**Linux:**

- GNOME shows tray icons only with the *AppIndicator and KStatusNotifierItem Support*
  extension. KDE Plasma, Xfce, Cinnamon, MATE, Budgie and sway with waybar show them without
  anything extra.
- When there is nowhere to show the icon, Nugget runs without one (see below).
- `NUGGET_TRAY=on` forces the icon, for trays that only support XEmbed. `NUGGET_TRAY=off`
  turns it off on any system.

With a tray icon, closing the window only hides it. Use **Quit Nugget** to quit.

**Without a tray icon** (stock GNOME, or `NUGGET_TRAY=off`):

- Closing the window quits Nugget, exactly like **Quit Nugget**: what happens to the background
  service follows [your quit choice](#quitting-sleep-and-a-second-launch).
- Launching Nugget again brings the window back.
- The tray's controls are in the window instead: **Status → Background controls** has **Pause
  indexing** / **Resume indexing**, and the kill switch (**Stop all agents**) is on the **Safety**
  page.

## Open at login and start hidden

Choose **Open Nugget in the tray when I log in** on the last onboarding step, or later on
**Status → Updates → Desktop app**. It is off until you turn it on. When Nugget opens at login,
the window stays hidden until you open it from the tray. Nugget registers the login entry the
usual way for each system:

| System | Login entry |
|---|---|
| macOS | `~/Library/LaunchAgents/dev.nugget.desktop.autostart.plist` |
| Windows | The `NuggetDesktop` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |
| Linux | `~/.config/autostart/dev.nugget.desktop.desktop` (XDG autostart). An AppImage is started from its own file. |

If you remove the entry in your system's own settings, Nugget turns this setting off. This is
separate from the background service's "start at login" option, which keeps indexing running
even when the app is closed.

## Notifications

Notifications are off until you turn them on, on the last onboarding step or on the Status page.
Once they are on, you choose which kinds you get:

- **Morning brief and reminders.** Clicking one opens Today.
- **Agents waiting for you**, when an agent needs a confirmation or an answer. Clicking one opens
  Agent. You only get this notification while the Nugget window isn't in front, and at most
  once every two minutes for the same task.
- **Quiet hours**, for example 22:00–07:00. Nothing is shown during these hours, and nothing is
  saved up to show later.

A notification never includes the text of your messages, files or reminders, or what an agent
wants to do. Lock screens, notification history and phones that mirror notifications would show
that text to anyone looking. A notification shows a count or a fixed line, and the details stay
in the app.

To stop notifications completely, you can also turn off Nugget in your system's notification
settings.

## Quick ask

Quick ask is a small ask box that floats above your other windows, separate from Double Tap.

- **To open it:** press the shortcut, which by default is **Ctrl+Shift+Space** (**⇧⌘Space** on
  macOS), or choose **Ask…** in the tray.
- **To use it:** type a question and press Enter. The answer is built from your own data, with
  `[n]` citations. Choose a citation or a source to open it.
- **To close it:** press Escape or click elsewhere.

**Open in Chat** and **Show in Search** switch to the main window and take your question along:
Search runs it, and Chat starts a new conversation with the question and, once it has finished,
the answer and its sources, so you can ask a follow-up. You can change or turn off
the shortcut on the Status page.

On Wayland, apps can't register global shortcuts this way. Bind a key in your desktop's keyboard
settings to this command instead:

```sh
nugget-desktop --quick-ask
```

On sway, for example: `bindsym Ctrl+Shift+space exec nugget-desktop --quick-ask`.

## Links: `nugget://`

Other apps, scripts and notes can link into Nugget:

| Link | What it does |
|---|---|
| `nugget://open/today` | Opens a page. The pages are `today`, `chat`, `search`, `knowledge`, `sources`, `apps`, `plugins`, `agent`, `safety`, `doubletap`, `models`, `privacy` and `status`. |
| `nugget://search?q=quarterly%20report` | Runs a search on this device. |
| `nugget://ask?q=When%20is%20my%20flight%3F` | Opens quick ask with the question typed in. It isn't sent until you press Enter. |
| `nugget://item?source=<source id>&key=<item key>` | Asks whether to open that item in its own app. |

A link can't do anything without you:

- Pages and searches only show you things.
- A question is only typed in, not sent.
- Opening an item asks you first.

Nugget refuses links with unknown parts, very long text or control characters, and doesn't act
on them.

## Plugin files

Opening a `.nuggetplugin` file, by double-clicking it or with **Open with → Nugget**, asks
whether to install it. If you continue, the usual [plugin approval](plugins.md) shows its
signature and what it asks to do before anything is installed.

## Quitting, sleep and a second launch

- **Quit:** the first time you quit, Nugget asks: *When you quit Nugget, also stop background
  indexing and the local AI?* **Stop everything** or **Keep running in the background**. Until you
  answer, quitting stops them, because quitting is expected to quit and the local AI can hold
  gigabytes of memory. Change your answer on **Status → Updates → Desktop app**.
  - Every way to quit counts: **Quit Nugget** in the tray, **Quit Nugget** in the macOS app menu
    or ⌘Q, and closing the window when there is no tray icon. Quitting from the Dock or by logging
    out doesn't ask; it uses your answer, or stops them if you haven't answered.
  - Only the background service the app started itself is stopped. A service set to start at
    login, or a `nuggetd` you started yourself, is never stopped. If the app can't prove it
    started the running service (for example one left over from a much older version), it leaves
    it running and says so in its log.
  - Stopping is clean: the service finishes in-flight work, stops the local model server and
    closes its store, on every system. If it doesn't stop within about 15 seconds, it is ended,
    and its model server goes with it (apps an agent opened keep running). The store is
    crash-safe, and an interrupted sync continues from where it stopped.
  - If the window can't show the question (it hasn't loaded, for example), quitting goes ahead
    after two seconds as if you hadn't answered.
- **Updates:** the app checks that the background service runs its own version every time it
  connects. After an update it replaces an older service it started, and restarts an older
  service that starts at login. A service newer than the app (an older copy of the app was
  opened) is never replaced with an older one. Otherwise, the Status page says what to do (see
  [Troubleshooting](troubleshooting.md#the-status-page-says-the-background-service-is-another-version)).
- **Sleep:** after your computer wakes up, Nugget reconnects to the service, refreshes the tray,
  and registers the Double Tap and quick-ask shortcuts again.
- **A second launch:** launching Nugget while it is already running brings up the window that's
  already open. Links and plugin files opened this way go to the running app.

## What is tested where

These run automatically:

- Link and file checks, tray status, notification rules, sleep detection and the settings files
  are covered by unit tests on Linux, macOS and Windows.
- The real login entry is written and removed on all three systems (`real-os.yml`, job
  `platform`).
- Clicking a notification is tested on real Windows and macOS (jobs `notify-*`).

These are checked by hand before a release:

- The tray icon and menu, on macOS (light and dark), Windows 11 and KDE or GNOME with the
  AppIndicator extension.
- The quick-ask shortcut.
- A `nugget://` link opened from a browser.
- Opening a `.nuggetplugin` file from Finder, Explorer and Files.
- Waking from real sleep.
