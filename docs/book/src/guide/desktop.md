# The desktop app on your computer

Heed keeps running in the background so your sources stay indexed.

## Tray and menu bar

| System | Where the icon is | Clicking it |
|---|---|---|
| macOS | The menu bar | Left click opens Heed, right click opens the menu. |
| Windows | The notification area (maybe under the ^ arrow) | Left click opens Heed, right click opens the menu. |
| Linux | The system tray (StatusNotifierItem / AppIndicator) | Every click opens the menu. |

The menu shows Heed's status, then:

- **Ask…** opens [quick ask](#quick-ask).
- **Double Tap** starts a draft in the focused field (see [Double Tap](double-tap.md)).
- **Pause indexing** / **Resume indexing** turns all enabled sources off and back on. A pause lasts
  across restarts.
- **Stop all agents** is the [kill switch](safety.md); it becomes **Resume agents** while stopped.
- **Open Heed**, **Status**, **Updates…** and **Quit Heed**.

With a tray icon, closing the window only hides it; use **Quit Heed** to quit.

**Linux:** GNOME needs the *AppIndicator and KStatusNotifierItem Support* extension; KDE Plasma,
Xfce, Cinnamon, MATE, Budgie and sway with waybar work as is. `NUGGET_TRAY=on` forces the icon (for
XEmbed-only trays); `NUGGET_TRAY=off` turns it off.

**Without a tray icon**, closing the window quits Heed (following your
[quit choice](#quitting-sleep-and-a-second-launch)), and launching it again brings the window back.
**Pause indexing** is under **Status → Background controls**, and **Stop all agents** is on the
**Safety** page.

## Open at login and start hidden

Turn on **Open Heed in the tray when I log in** on the last onboarding step or under **Status →
Updates → Desktop app**. Heed then starts hidden in the tray. The login entry is:

| System | Login entry |
|---|---|
| macOS | `~/Library/LaunchAgents/dev.nugget.desktop.autostart.plist` |
| Windows | The `NuggetDesktop` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |
| Linux | `~/.config/autostart/dev.nugget.desktop.desktop` (XDG autostart) |

This is separate from the background service's own "start at login", which keeps indexing while
the app is closed.

## Notifications

Off until you turn them on (last onboarding step or the Status page). You choose:

- **Morning brief and reminders** (clicking opens Today).
- **Agents waiting for you** (clicking opens Agent). Only while the window isn't in front, at most
  once every two minutes per task.
- **Quiet hours**, for example 22:00–07:00. Nothing is shown or saved for later.

Notifications never contain the text of your messages, files or agent actions.

## Quick ask

A small floating ask box. Open it with **Ctrl+Shift+Space** (**⇧⌘Space** on macOS) or **Ask…** in
the tray, type a question and press Enter. Answers come from your data with `[n]` citations. Escape
closes it. **Open in Chat** and **Show in Search** carry the question to the main window. Change or
turn off the shortcut on the Status page.

On Wayland, bind a key in your desktop settings to:

```sh
nugget-desktop --quick-ask
```

On sway, for example: `bindsym Ctrl+Shift+space exec nugget-desktop --quick-ask`.

## Links: `heed://`

| Link | What it does |
|---|---|
| `heed://open/today` | Opens a page: `today` (Home), `needs`, `chat`, `search`, `knowledge`, `sources`, `connections`, `apps`, `plugins`, `agent`, `safety`, `doubletap`, `models`, `privacy` or `status`. |
| `heed://search?q=quarterly%20report` | Opens Search with the text typed in, not run: press Enter to search. |
| `heed://ask?q=When%20is%20my%20flight%3F` | Opens quick ask with the question typed in, not sent. |
| `heed://item?source=<source id>&key=<item key>` | Asks whether to open that item in its own app. |

A link never acts without you, and malformed links are refused. Links shared before the rename to
Heed start with `nugget://`; they still work and do the same thing.

Opening a `.nuggetplugin` file shows the usual [plugin approval](plugins.md) before installing.

## Quitting, sleep and a second launch

- **Quit:** the first time, Heed asks whether to also stop background indexing and the local AI
  (**Stop everything** or **Keep running in the background**). Until you answer, quitting stops
  them. Change it under **Status → Updates → Desktop app**. Only a service the app started itself is
  stopped; a login service or a `nuggetd` you started is left running.
- **Updates:** after an update the app replaces an older service it started and restarts an older
  login service. It never replaces a newer service with an older one. Otherwise the Status page says
  what to do (see
  [Troubleshooting](troubleshooting.md#the-status-page-says-the-background-service-is-another-version)).
- **Sleep:** on wake, Heed reconnects, refreshes the tray and re-registers its shortcuts.
- **Second launch:** brings up the running window; links and plugin files go to the running app.
