# GG6 area A4: desktop shell lifecycle (tray, updates, autostart, permissions, uninstall)

Explorer report, commit 5f1998e. Evidence is **code reading** with file:line references unless marked otherwise.
The Tauri shell does not build in the cloud container (`pkg-config --exists webkit2gtk-4.1` returns 1), so its
unit tests were not run here. Items marked "(Tauri, verify)" rely on Tauri 2.11 / tauri-plugin-updater 2.12
behaviour that is not vendored in the repo; the real-OS runs in `qa/GG6/real-os/` check what they can.

## Findings

| # | Sev | Finding | Evidence |
|---|---|---|---|
| F1 | High | Quitting Nugget never stops the daemon. "Quit Nugget" is `app.exit(0)`; nuggetd is spawned detached to outlive the app; there is no shutdown IPC and no `nuggetd stop`. The user is not told; nuggetd and any llama-server keep running (and holding memory) until logout. | `apps/desktop/src-tauri/src/tray.rs:87`, `daemon.rs:351-371`, `crates/nuggetd/src/main.rs:1-7`; `packaging/scripts/smoke-macos.sh:298` asserts it |
| F2 | High | Version skew after an update on macOS/Linux: `Daemon::start` accepts whatever daemon already answers; only `PROTOCOL_VERSION` is compared, never the app vs daemon version; the update path never restarts nuggetd, so the old daemon serves the new UI until reboot. (NSIS is fine: its hook kills the daemon.) | `daemon.rs:227-231`, `src/pages/Status.tsx:72,80`, `updater.rs:317-329`, `main.rs:218`, `docs/release.md:457-459` |
| F3 | High (verify) | The MSI has no hooks (`"wix": {}`) yet the updater feed carries an MSI entry: nothing stops `nuggetd.exe` in Program Files (files-in-use / reboot on update); uninstall leaves the "Nugget Daemon" task, the Run key and the running daemon. | `tauri.conf.json:84`, `packaging/scripts/updater_manifest.py:35` |
| F4 | High | Uninstall silently leaves several GB of models plus the encrypted store, config, keychain entry and `~/Nugget Vault` on every OS. NSIS hooks only remove the service and kill nuggetd; Tauri's "Delete app data" covers `dev.nugget.desktop` dirs, not `%LOCALAPPDATA%\Nugget\Nugget` (Tauri, verify); deb/rpm have no maintainer scripts. The user doc's Uninstall section omits the app dirs (incl. unencrypted `chat-history.json`), the vault, autostart entries, the AppImage `.desktop` and stopping nuggetd first. | `packaging/windows/installer-hooks.nsh:23-31`, `docs/book/src/install.md:120-128`, `history.rs:1-5` |
| F5 | Medium | macOS Accessibility cannot be recovered from inside the app once denied: the button only re-calls the AX prompt (macOS shows it once); `x-apple.systempreferences:` is rejected by the URL allowlist so there is no deep link; Input Monitoring always reads "unknown"; the hint `doubletap.hint.macos_permissions` is never used; a read failure says only "Couldn't read the text field." | `src-tauri/src/doubletap/commands.rs:100-118`, `open.rs:311`, `permissions.rs:159-165`, `en.json:1652`, `errors.rs:99` |
| F6 | Medium | No tray on stock GNOME: closing the window quits the app (daemon keeps running), yet docs and hints send users to tray-only "Stop all agents" / "Open Nugget"; the autostart hint says the window stays hidden, which is false without a tray. | `tray.rs:177-201`, `main.rs:160,201-204`, `docs/book/src/safety.md:4,86`, `troubleshooting.md:106`, `en.json:549,1653` |
| F7 | Medium (verify) | NSIS installs into `%LOCALAPPDATA%\Nugget`, which also contains the data dir `%LOCALAPPDATA%\Nugget\Nugget\{data,cache}`: a recursive delete of `$INSTDIR` would wipe store and models, and after uninstall the folder looks like a broken install. | `docs/release.md:47`, `crates/nugget-core/src/paths.rs:18-24` |
| F8 | Medium | Login entries outlive the app: NSIS leaves `HKCU\…\Run\NuggetDesktop`; macOS drag-to-Trash leaves the KeepAlive LaunchAgent `dev.nugget.nuggetd` and `dev.nugget.desktop.autostart.plist`; deb/rpm removal leaves `~/.config/systemd/user/nuggetd.service` and the XDG autostart file pointing at deleted binaries. | `autostart.rs:174-175`, `crates/nuggetd/src/service.rs:85-125` |
| F9 | Medium | Update checks contact the project's GitHub by default (Stable channel, first check 60 s after launch) through the shell's own HTTP client, outside the egress ledger, contradicting the privacy page ("Nothing is sent to the Nugget project, ever"). | `updater.rs:33-39,370-387`, `docs/book/src/privacy.md:7-8,19-20` |
| F10 | Low | The "Start at login" (daemon service) checkbox shows the stored answer, not the real launchd/systemd/schtasks state (autostart, by contrast, reconciles). | `updater.rs:452-477`, `UpdateSettings.tsx:170-178`, `autostart.rs:266-282` |
| F11 | Low | Two near-identical "at login" toggles, buried in Status → Updates, never offered in onboarding. | `en.json:546,548`, `UpdateSettings.tsx:156-180` |
| F12 | Low | The installed shell writes no log file (stdout only, and `windows_subsystem` hides it); the spawn error says "Check its log" with no path. | `main.rs:4,32-36`, `daemon.rs:254` |
| F13 | Low | If the daemon dies while the app is open, the Offline screen replaces every page and only polls to reconnect; it never respawns and never says why (crash vs stopped). | `App.tsx:137-142`, `Offline.tsx:29-42` |
| F14 | Low (test gap) | All installer smoke tests run with `NUGGET_TRAY=off`, so hide-on-close, tray Quit and install-at-quit are never exercised. | `smoke-windows.ps1:40`, `smoke-macos.sh:45` |

Fix directions: a daemon shutdown method and `nuggetd stop`, used by Quit; compare app/daemon versions on
connect and restart the daemon around updates; WiX custom action or ship NSIS only; an NSIS post-uninstall
step that removes the nuggetd data dirs when "Delete app data" is ticked plus the Run value, and a per-OS
"uninstall completely" doc; an allowlisted command that opens the macOS Privacy panes; expose `has_tray` to the
UI and put "Stop all agents" in the window; move the data root out of the NSIS install folder; document update
checks on the privacy page; reconcile the service state at launch; a "Background & startup" settings section;
a rolling shell log file.

## Per-OS table

| | macOS | Windows NSIS | Windows MSI | deb/rpm | AppImage |
|---|---|---|---|---|---|
| Install | ad-hoc signed without Apple credentials (`release.md:167`) | `%LOCALAPPDATA%\Nugget`; hook `taskkill`s nuggetd | `Program Files\Nugget`; no hooks | `/usr/bin`; portal `.desktop` is NoDisplay, Tauri's `Nugget.desktop` is the launcher | temporary mount |
| First launch | window hidden, placed, shown (`main.rs:155-164`); sidecar spawned detached | same | same | same | same |
| Close window | hides silently; Dock click reopens | hides to tray silently | same | hides with a StatusNotifier host; **quits on GNOME** | same |
| Quit | `app.exit`; **nuggetd keeps running** | same; staged update installs at quit | same | same | same |
| Autostart (app) | off by default (`settings.rs:41`); LaunchAgent | `HKCU Run\NuggetDesktop` | same | XDG autostart file | uses `$APPIMAGE` |
| Login service (nuggetd) | Status page; KeepAlive LaunchAgent; refused from DMG/translocation | task "Nugget Daemon" | same | systemd user unit | refused (`login.rs:15-19`) |
| Updates | notify-only, Stable, first check 60 s after launch; old daemon persists | passive install; hook kills daemon | msiexec; file lock (verify) | elevated install (verify); old daemon persists | file replaced |
| Unkeyed (dry-run) build | "can't update itself"; no background checks (`updater.rs:372`) | same | same | same | same |
| Permissions | explained before the OS prompt (`steps.tsx:202-276`); no deep link | none needed (UIA) | same | AT-SPI and portal hints | same |
| Uninstall leftovers | everything: data, models, keychain item, both LaunchAgents, running daemon, vault | data under the install parent, Run key, keychain | everything | everything per-user | everything, plus `.desktop` |

## Friction log

| Moment | Who |
|---|---|
| First close: the window vanishes, no "still running in the tray" notice | most |
| Tray Quit: user believes Nugget stopped; daemon (and llama-server) keep running | all who quit |
| GNOME: docs say "use the tray", there is none | some |
| Start at login: hidden in Status → Updates, two similar checkboxes, not in setup | most |
| After an update on macOS/Linux, new features show "not available" until reboot | all updating users there |
| Denied the macOS AX prompt: a second click does nothing; Double Tap error gives no reason | some |
| Wants the models' disk space back: no uninstaller option; two confusing dirs (`dev.Nugget.Nugget` vs `dev.nugget.desktop`) | most uninstallers |

## What works well

- Single instance: relaunch focuses the window; `--minimized` relaunch is a no-op; `--double-tap` is forwarded (`main.rs:44-50`).
- Tray fallbacks: the AppIndicator panic is caught and a missing StatusNotifier host is detected (`tray.rs:39-55,177-201`); the window is always shown when there is no tray (`main.rs:160`).
- Autostart reconciles with the OS entry and re-points moved apps (`autostart.rs:266-282`), with escaping tests.
- The login service refuses transient locations (DMG, App Translocation, AppImage) with a clear message (`login.rs:14-26`).
- Updater: the signed feed is verified before parsing (`feed.rs:71-113`); no downgrades (`updater.rs:230`); install pinned to the version shown (`updater.rs:304-309`); Off never touches the network; unkeyed builds say they can't update; the webview has no updater permission.
- NSIS hooks keep the user's login-service choice across updates (`$UpdateMode`).
- Permissions are explained before any OS prompt, with specific Wayland hints.
- Data dirs are documented per OS (`docs/book/src/reference/data-dir.md`).

## Checklist for the real-OS runners (used by `.github/workflows/qa-GG6.yml`)

- All: close the window, Quit, then check whether nuggetd / llama-server still run; kill nuggetd with the app open and watch the Offline UI.
- macOS: `ls ~/Library/LaunchAgents | grep nugget`; after drag-to-Trash: `pgrep nuggetd`, `ls ~/Library/Application\ Support/{dev.Nugget.Nugget,dev.nugget.desktop}`, `security find-generic-password -s dev.nugget.Nugget`, `du -sh` of the models dir.
- Windows NSIS: before/after uninstall `Get-ItemProperty HKCU:\Software\Microsoft\Windows\CurrentVersion\Run`, `schtasks /Query /TN "Nugget Daemon"`, `Test-Path` on `%LOCALAPPDATA%\Nugget`, `%LOCALAPPDATA%\Nugget\Nugget\data\models`, `%APPDATA%\dev.nugget.desktop`, `%LOCALAPPDATA%\dev.nugget.desktop`; `cmdkey /list | findstr nugget`; uninstall while nuggetd still runs after Quit.
- Windows MSI: install, run, uninstall from msiexec: daemon still running? task / Run key left?
- Linux deb: `apt remove` then `apt purge`, then list `~/.local/share/nugget ~/.config/nugget ~/.cache/nugget ~/.config/dev.nugget.desktop ~/.local/share/dev.nugget.desktop ~/Nugget\ Vault`; `pgrep nuggetd`.
