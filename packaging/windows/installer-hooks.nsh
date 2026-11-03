; NSIS installer hooks for Heed (wired in apps/desktop/src-tauri/tauri.conf.json,
; bundle.windows.nsis.installerHooks). Tauri inserts these macros into its installer template.
;
; The installer never registers anything to run at login on its own: that is the user's choice,
; made in the app ("Start Heed at login"), which runs `nuggetd install-service` and creates
; the per-user scheduled task "Nugget Daemon". These hooks only keep that task consistent with
; the files on disk.
;
; Uninstalling also offers to remove Heed's data (models, store, settings, logs, saved keys) and,
; separately, the vault: see uninstall-data.nsh, which adds that page. Updates never remove data.

!include "${__FILEDIR__}\uninstall-data.nsh"

!macro NSIS_HOOK_PREINSTALL
  ; A running nuggetd.exe locks its file, so an update could not replace it. Stop this user's
  ; copy; it starts again below (if start-at-login is on) or when the app is next opened.
  nsExec::Exec 'taskkill /F /IM nuggetd.exe'
  Pop $0
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Restart the daemon if the user opted into the login task. When there is no such task,
  ; schtasks fails and nothing happens.
  nsExec::Exec 'schtasks /Run /TN "Nugget Daemon"'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; A real uninstall removes the login task. An update runs the old uninstaller with /UPDATE,
  ; and then the user's choice is kept.
  ${If} $UpdateMode <> 1
    nsExec::Exec '"$INSTDIR\nuggetd.exe" uninstall-service'
    Pop $0
    ; The app's own "Open Heed in the tray when I log in" entry (autostart.rs). Tauri's
    ; uninstaller only removes a Run value named after the product, "Heed".
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "NuggetDesktop"
  ${EndIf}
  nsExec::Exec 'taskkill /F /IM nuggetd.exe'
  Pop $0

  ; What to remove (uninstall-data.nsh). Updates: nothing, whatever was asked. No page shown
  ; (silent /S, passive /P, or nothing found): only what /REMOVEDATA and /REMOVEVAULT ask for.
  ${If} $UpdateMode = 1
    StrCpy $NuggetRemoveData 0
    StrCpy $NuggetRemoveVault 0
  ${ElseIf} $NuggetPageShown <> 1
    Call un.NuggetReadFlags
  ${EndIf}
  ; Removing Heed's data includes the app's own app.heed.desktop folders, which is what
  ; Tauri's "Delete the application data" box removes; turn it on so both agree.
  ${If} $NuggetRemoveData = 1
    StrCpy $DeleteAppDataCheckboxState 1
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Runs after Tauri removed the app (and, when asked, the app.heed.desktop folders).
  ${If} $UpdateMode <> 1
  ${AndIf} $NuggetRemoveData = 1
    Call un.NuggetRemoveAll
  ${EndIf}
!macroend
