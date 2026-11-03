# Uninstall

Removing the app does **not** remove your data: models, the encrypted store, settings, logs, saved
keys in your OS keychain and your vault (`~/Nugget Vault`) all stay, so a reinstall picks up where you
left off. Only the Windows `setup.exe` uninstaller offers to remove them; otherwise use the commands
below or the [cleanup script](#the-cleanup-script).

To keep a copy first, use `nuggetd backup` or `nuggetd export --to DIR` (see
[Backup, restore and export](backup.md)). Nothing on this page removes backups or exports.

## 1. Stop the login service, then quit

On the Status page turn off **Start the Heed background service at login** and **Open Heed in the
tray when I log in**, then quit Heed. Or run:

```sh
nuggetd uninstall-service
```

The `setup.exe` uninstaller does this for you. If the app is already gone, see
[login entries left behind](#login-entries-left-behind).

## 2. Remove the app

- **macOS:** drag `/Applications/Heed.app` to the Trash.
- **Windows:** **Settings → Apps → Installed apps → Heed → Uninstall**.
- **Linux:** `sudo apt remove nugget`, `sudo dnf remove nugget` or `sudo zypper remove nugget`; for an
  AppImage, delete the file.

## 3. Remove what stays

App data and your vault are removed separately: the vault is your notes. Deleting the store and its
key can't be undone (a backup can still be restored with its passphrase). A vault you placed with
`[vault] path` ([config.toml](../reference/config.md#vault)) is never deleted here. With
`HEED_HOME` or `--home`, data is in that directory (see [Data directory](../reference/data-dir.md)).

Folder and key names from before the rename (Nugget) are listed too: the first launch of this
version copies them to the new names and leaves the originals (see
[Data directory](../reference/data-dir.md#moving-from-the-names-before-the-rename)).

### Windows

Data, models, cache and settings are in `%LOCALAPPDATA%\heed\Heed` and
`%APPDATA%\heed\Heed` (see [Data directory](../reference/data-dir.md)); the app's own state is in
the two `app.heed.desktop` folders; saved keys are Credential Manager generic credentials ending in
`.app.heed.Heed`; the vault is `%USERPROFILE%\Nugget Vault`.

The `setup.exe` uninstaller shows a **Your Heed data** page with two unticked boxes:
**Remove models and Heed's data** (everything above except the vault, including saved keys) and,
after that, **Also delete my Nugget Vault** (asks again; a vault in a folder you chose is never
deleted). Updates never remove data. Silent options for `%LOCALAPPDATA%\Heed\uninstall.exe`:
`/S` (app only), `/S /REMOVEDATA` (plus data), `/S /REMOVEDATA /REMOVEVAULT` (plus the vault).

After an `.msi` uninstall, or to do it by hand, quit Heed and in PowerShell remove app data and
models:

```powershell
cmd /c rmdir /s /q "%LOCALAPPDATA%\heed\Heed"
cmd /c rmdir /s /q "%APPDATA%\heed\Heed"
cmd /c rmdir /s /q "%APPDATA%\app.heed.desktop"
cmd /c rmdir /s /q "%LOCALAPPDATA%\app.heed.desktop"
rem Left by a version from before the rename (Nugget):
cmd /c rmdir /s /q "%LOCALAPPDATA%\Nugget\Nugget"
cmd /c rmdir /s /q "%APPDATA%\Nugget\Nugget"
cmd /c rmdir /s /q "%APPDATA%\dev.nugget.desktop"
cmd /c rmdir /s /q "%LOCALAPPDATA%\dev.nugget.desktop"
```

Saved keys (or remove them in **Credential Manager → Windows Credentials**):

```powershell
cmdkey /list | Select-String 'target=(.+\.(app\.heed\.Heed|dev\.nugget\.Nugget|nugget))\s*$' |
  ForEach-Object { cmdkey "/delete:$($_.Matches[0].Groups[1].Value)" }
```

Your vault, only if you no longer want your notes:
`cmd /c rmdir /s /q "%USERPROFILE%\Nugget Vault"`. A models folder moved elsewhere with a junction is
not followed; delete it yourself.

### macOS

Data, models and settings are in `~/Library/Application Support/app.heed.Heed`, the app's own
state and web data under `app.heed.desktop` names in `~/Library`, saved keys in your login keychain
(service `app.heed.Heed`), and the vault in `~/Nugget Vault`.

App data and models:

```sh
rm -rf ~/Library/Application\ Support/app.heed.Heed ~/Library/Caches/app.heed.Heed \
  ~/Library/Application\ Support/dev.Nugget.Nugget ~/Library/Caches/dev.Nugget.Nugget \
  ~/Library/Application\ Support/dev.nugget.desktop ~/Library/WebKit/dev.nugget.desktop \
  ~/Library/Application\ Support/app.heed.desktop ~/Library/Caches/app.heed.desktop \
  ~/Library/WebKit/app.heed.desktop ~/Library/HTTPStorages/app.heed.desktop \
  ~/Library/HTTPStorages/app.heed.desktop.binarycookies \
  ~/Library/Saved\ Application\ State/app.heed.desktop.savedState \
  ~/Library/Preferences/app.heed.desktop.plist
```

Saved keys (or delete `app.heed.Heed` items in **Keychain Access**):

```sh
for s in app.heed.Heed dev.nugget.Nugget nugget; do
  while security delete-generic-password -s "$s" >/dev/null 2>&1; do :; done
done
```

Your vault, only if you no longer want your notes: `rm -rf ~/Nugget\ Vault`.

### Linux (`.deb`, `.rpm`, AppImage)

Data and models are in `~/.local/share/heed`, settings in `~/.config/heed`, cache in
`~/.cache/heed`, the app's own state in `app.heed.desktop` folders, saved keys in the Secret
Service (attribute `service` = `app.heed.Heed`; without one, the key is
`~/.local/share/heed/store.key`), and the vault in `~/Nugget Vault`. `$XDG_*_HOME` variables
replace these base folders when set.

App data and models:

```sh
rm -rf ~/.local/share/heed ~/.config/heed ~/.cache/heed \
  ~/.config/app.heed.desktop ~/.local/share/app.heed.desktop ~/.cache/app.heed.desktop \
  ~/.local/share/nugget ~/.config/nugget ~/.cache/nugget \
  ~/.config/dev.nugget.desktop ~/.local/share/dev.nugget.desktop ~/.cache/dev.nugget.desktop
```

Saved keys (`secret-tool` is in `libsecret-tools` on Debian/Ubuntu, `libsecret` on Fedora; or use
Seahorse or KWalletManager):

```sh
for s in app.heed.Heed dev.nugget.Nugget nugget; do secret-tool clear service "$s"; done
secret-tool search --all service app.heed.Heed    # prints nothing when all are gone; else clear again
```

Your vault, only if you no longer want your notes: `rm -rf ~/Nugget\ Vault`.

### Login entries left behind

If you removed the app before turning its login settings off:

Linux:

```sh
systemctl --user disable --now nuggetd.service
rm -f ~/.config/systemd/user/nuggetd.service ~/.config/autostart/app.heed.desktop.desktop
systemctl --user daemon-reload
```

macOS:

```sh
launchctl bootout gui/$(id -u)/app.heed.nuggetd
launchctl bootout gui/$(id -u)/app.heed.desktop.autostart
rm -f ~/Library/LaunchAgents/app.heed.nuggetd.plist \
  ~/Library/LaunchAgents/app.heed.desktop.autostart.plist
```

Windows (after an `.msi` uninstall; `setup.exe` removes both):

```powershell
schtasks /Delete /TN "Nugget Daemon" /F
reg delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v NuggetDesktop /f
```

## The cleanup script

On Linux and macOS,
[`packaging/scripts/remove-nugget-data.sh`](https://github.com/rayhankhilji/nugget/blob/main/packaging/scripts/remove-nugget-data.sh)
does step 3. Read it first. By default it only lists what it finds:

```sh
bash remove-nugget-data.sh                    # dry run: list everything, delete nothing
bash remove-nugget-data.sh --yes              # delete app data, models, saved keys, login entries
bash remove-nugget-data.sh --yes --include-vault   # also the vault; asks you to type "delete"
```

It refuses to run while Heed is running, never deletes a vault you placed with `[vault] path`,
and leaves `HEED_HOME` alone. (The manual commands above remove every key under
`app.heed.Heed`, including other data directories' store keys; the script keeps those.)
