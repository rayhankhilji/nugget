; NSIS installer hooks for Nugget (wired in apps/desktop/src-tauri/tauri.conf.json,
; bundle.windows.nsis.installerHooks). Tauri inserts these macros into its installer template.
;
; The installer never registers anything to run at login on its own: that is the user's choice,
; made in the app ("Start Nugget at login"), which runs `nuggetd install-service` and creates
; the per-user scheduled task "Nugget Daemon". These hooks only keep that task consistent with
; the files on disk.

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
  ${EndIf}
  nsExec::Exec 'taskkill /F /IM nuggetd.exe'
  Pop $0
!macroend
