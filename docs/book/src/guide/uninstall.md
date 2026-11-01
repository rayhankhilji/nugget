# Uninstall

Removing the Nugget app does not remove your data. Models (often several GB), the encrypted store,
your settings, the logs, the saved keys in your OS keychain and your vault (`~/Nugget Vault`, your
notes) all stay, so a reinstall picks up where you left off. This page lists every location per
OS and how to remove it.

Only the Windows installer (`setup.exe`) asks what to do with them. A `.deb` or `.rpm` is removed
for the whole machine, as root, so it can't ask each user about their files; a `.dmg` app is just
dragged to the Trash; the `.msi` doesn't ask. For those, use the commands below or the
[cleanup script](#the-cleanup-script).

Want to keep a copy first? `nuggetd backup` makes an encrypted backup, and
`nuggetd export --to DIR` writes everything in open formats (see
[Backup, restore and export](backup.md)). Backups and exports are never removed by anything on
this page.

## 1. Stop the login service, then quit

While the app is still installed, turn off **Start the Nugget background service at login** and
**Open Nugget in the tray when I log in** on the Status page, then quit Nugget from the tray or
menu bar. Or, from a terminal:

```sh
nuggetd uninstall-service
```

The binary is `/Applications/Nugget.app/Contents/MacOS/nuggetd` on macOS, `/usr/bin/nuggetd` for
the `.deb` and `.rpm`, and `C:\Program Files\Nugget\nuggetd.exe` for the `.msi`. The Windows
`setup.exe` uninstaller does this step for you. If you already removed the app, see
[login entries left behind](#login-entries-left-behind).

## 2. Remove the app

- **macOS:** drag `/Applications/Nugget.app` to the Trash.
- **Windows:** **Settings → Apps → Installed apps → Nugget → Uninstall**. See [below](#windows) for
  what the `setup.exe` uninstaller offers.
- **Linux:** `sudo apt remove nugget`, `sudo dnf remove nugget` or `sudo zypper remove nugget`
  (`apt purge` removes nothing more); for an AppImage, delete the file.

## 3. Remove what stays

Remove **app data and models** and **your vault** separately: the vault is your notes, so the
commands below never delete it together with the rest. Deleting the store and its key cannot be
undone; a backup can still be restored later with your backup passphrase.

If you set `[vault] path` in [config.toml](../reference/config.md#vault), your vault is in that
folder instead of `~/Nugget Vault`; it is your folder, so nothing here deletes it. If you run
Nugget with `NUGGET_HOME` or `--home`, its data is in that directory instead (see
[Data directory](../reference/data-dir.md)).

### Windows

| What | Where |
|---|---|
| Models | `%LOCALAPPDATA%\Nugget\Nugget\data\models` |
| Encrypted store, search index, logs | `%LOCALAPPDATA%\Nugget\Nugget\data` |
| Cache | `%LOCALAPPDATA%\Nugget\Nugget\cache` |
| Settings (`config.toml`, `sources.toml`) | `%APPDATA%\Nugget\Nugget\config` |
| The app's settings, history and window state | `%APPDATA%\dev.nugget.desktop` |
| The app window's WebView data | `%LOCALAPPDATA%\dev.nugget.desktop` |
| Saved keys: the store's key, API keys, account sign-ins | Credential Manager, generic credentials whose names end in `.dev.nugget.Nugget` (for example `provider:openai.dev.nugget.Nugget`) |
| Your vault | `%USERPROFILE%\Nugget Vault` |

The `setup.exe` install lives in `%LOCALAPPDATA%\Nugget`, so the data folder is inside the install
folder; uninstalling removes only the app's own files from it.

**The `setup.exe` uninstaller** first shows a **Your Nugget data** page that lists what stays,
with sizes, and offers two boxes, both unticked:

- **Remove models and Nugget's data (index, settings, logs, saved keys)**: removes every row of
  the table except the vault, including the Credential Manager entries.
- **Also delete my Nugget Vault (your notes)**: available only once the first box is ticked, and
  asks again before it is accepted. A vault at a folder you chose is listed but never deleted.

The next page is the standard confirm page. Its **Delete the application data** box removes only
the two `dev.nugget.desktop` folders; it is implied when you ticked the first box on the data page.
If something can't be removed (a file in use), the uninstaller says what is left.

Other ways the uninstaller runs:

| Run as | What it removes |
|---|---|
| An update by the in-app updater, or a newer `setup.exe` run over the old one | Never any data. If you pick that installer's **Uninstall before installing**, the old version's uninstaller opens with the data page above, boxes unticked |
| `uninstall.exe /S` (silent) or `/P` (passive) | The app only; everything above stays |
| `uninstall.exe /S /REMOVEDATA` | The app, and everything above except the vault |
| `uninstall.exe /S /REMOVEDATA /REMOVEVAULT` | All of the above, including `%USERPROFILE%\Nugget Vault` |

`uninstall.exe` is in `%LOCALAPPDATA%\Nugget`.

**The `.msi`** (and the `setup.exe` uninstaller when you leave the boxes unticked) leaves
everything. To remove it by hand, quit Nugget, then in PowerShell, app data and models:

```powershell
cmd /c rmdir /s /q "%LOCALAPPDATA%\Nugget\Nugget"
cmd /c rmdir /s /q "%APPDATA%\Nugget\Nugget"
cmd /c rmdir /s /q "%APPDATA%\dev.nugget.desktop"
cmd /c rmdir /s /q "%LOCALAPPDATA%\dev.nugget.desktop"
```

Saved keys (or open **Credential Manager → Windows Credentials** and remove each generic
credential ending in `.dev.nugget.Nugget`):

```powershell
cmdkey /list | Select-String 'target=(.+\.dev\.nugget\.Nugget)\s*$' |
  ForEach-Object { cmdkey "/delete:$($_.Matches[0].Groups[1].Value)" }
```

Your vault, only if you no longer want your notes:

```powershell
cmd /c rmdir /s /q "%USERPROFILE%\Nugget Vault"
```

`rmdir /s` removes a link inside a folder without following it, so a models folder you moved to
another drive with a junction stays there; delete it yourself.

### macOS

| What | Where |
|---|---|
| Models | `~/Library/Application Support/dev.Nugget.Nugget/models` |
| Encrypted store, search index, settings, logs | `~/Library/Application Support/dev.Nugget.Nugget` |
| Cache | `~/Library/Caches/dev.Nugget.Nugget` |
| The app's settings, history and window state | `~/Library/Application Support/dev.nugget.desktop` |
| The app window's web data | `~/Library/WebKit/dev.nugget.desktop`, `~/Library/Caches/dev.nugget.desktop`, and, if present, `~/Library/HTTPStorages/dev.nugget.desktop`, `~/Library/Saved Application State/dev.nugget.desktop.savedState`, `~/Library/Preferences/dev.nugget.desktop.plist` |
| Saved keys | Your login keychain, items with service (Where) `dev.nugget.Nugget` |
| Your vault | `~/Nugget Vault` |

App data and models:

```sh
rm -rf ~/Library/Application\ Support/dev.Nugget.Nugget ~/Library/Caches/dev.Nugget.Nugget \
  ~/Library/Application\ Support/dev.nugget.desktop ~/Library/Caches/dev.nugget.desktop \
  ~/Library/WebKit/dev.nugget.desktop ~/Library/HTTPStorages/dev.nugget.desktop \
  ~/Library/HTTPStorages/dev.nugget.desktop.binarycookies \
  ~/Library/Saved\ Application\ State/dev.nugget.desktop.savedState \
  ~/Library/Preferences/dev.nugget.desktop.plist
```

Saved keys (each call removes one item; macOS may ask for your login password). Or search for
`dev.nugget.Nugget` in **Keychain Access** and delete the items:

```sh
while security delete-generic-password -s dev.nugget.Nugget >/dev/null 2>&1; do :; done
```

Your vault, only if you no longer want your notes:

```sh
rm -rf ~/Nugget\ Vault
```

### Linux (`.deb`, `.rpm`, AppImage)

The same for every package. `$XDG_DATA_HOME`, `$XDG_CONFIG_HOME` and `$XDG_CACHE_HOME` replace
`~/.local/share`, `~/.config` and `~/.cache` when you set them.

| What | Where |
|---|---|
| Models | `~/.local/share/nugget/models` |
| Encrypted store, search index, logs | `~/.local/share/nugget` |
| Cache | `~/.cache/nugget` |
| Settings | `~/.config/nugget` |
| The app's settings, history and window state | `~/.config/dev.nugget.desktop`, `~/.local/share/dev.nugget.desktop` |
| The app window's web data | `~/.local/share/dev.nugget.desktop`, `~/.cache/dev.nugget.desktop` |
| Saved keys | The Secret Service (GNOME Keyring, KWallet, KeePassXC), items with attribute `service` = `dev.nugget.Nugget`. Without one, the store's key is `~/.local/share/nugget/store.key` |
| Your vault | `~/Nugget Vault` |

App data and models:

```sh
rm -rf ~/.local/share/nugget ~/.config/nugget ~/.cache/nugget \
  ~/.config/dev.nugget.desktop ~/.local/share/dev.nugget.desktop ~/.cache/dev.nugget.desktop
```

Saved keys (`secret-tool` is in the `libsecret-tools` package on Debian and Ubuntu, `libsecret` on
Fedora). Or search for `dev.nugget.Nugget` in Seahorse (**Passwords and Keys**) or KWalletManager:

```sh
secret-tool clear service dev.nugget.Nugget
secret-tool search --all service dev.nugget.Nugget    # prints nothing when all are gone; else clear again
```

Your vault, only if you no longer want your notes:

```sh
rm -rf ~/Nugget\ Vault
```

### Login entries left behind

If you removed the app before turning its login settings off, remove the entries by hand.

Linux:

```sh
systemctl --user disable --now nuggetd.service
rm -f ~/.config/systemd/user/nuggetd.service ~/.config/autostart/dev.nugget.desktop.desktop
systemctl --user daemon-reload
```

macOS:

```sh
launchctl bootout gui/$(id -u)/dev.nugget.nuggetd
launchctl bootout gui/$(id -u)/dev.nugget.desktop.autostart
rm -f ~/Library/LaunchAgents/dev.nugget.nuggetd.plist \
  ~/Library/LaunchAgents/dev.nugget.desktop.autostart.plist
```

Windows: the `setup.exe` uninstaller removes both. After an `.msi` uninstall, in PowerShell:

```powershell
schtasks /Delete /TN "Nugget Daemon" /F
reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v NuggetDesktop /f
```

## The cleanup script

On Linux and macOS,
[`packaging/scripts/remove-nugget-data.sh`](https://github.com/rayhankhilji/nugget/blob/main/packaging/scripts/remove-nugget-data.sh)
does step 3 for you. Read it before you run it. By default it only lists what it finds, with
sizes; nothing is deleted:

```sh
bash remove-nugget-data.sh                    # dry run: list everything, delete nothing
bash remove-nugget-data.sh --yes              # delete app data, models, saved keys, login entries
bash remove-nugget-data.sh --yes --include-vault   # also the vault; asks you to type "delete"
```

It refuses to run while `nuggetd` or the app is running, never deletes a vault you put somewhere
else with `[vault] path`, leaves `NUGGET_HOME` alone, and keeps the store keys of other data
directories (the commands above remove every key under `dev.nugget.Nugget`, including those).
