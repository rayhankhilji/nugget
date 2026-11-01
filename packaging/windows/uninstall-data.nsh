; "Your Nugget data": the uninstaller page that says what an uninstall leaves behind and offers to
; remove it (GG6-9). Included from installer-hooks.nsh, so it lands near the top of Tauri's
; installer.nsi template, before the template's own uninstaller pages: this page comes first, then
; Tauri's confirm page, then the progress page.
;
; What Nugget keeps, per user (see docs/book/src/reference/data-dir.md and guide/uninstall.md):
;   %LOCALAPPDATA%\Nugget\Nugget\data     models, the encrypted store, logs (nugget-core paths.rs:
;                                         directories::ProjectDirs("dev", "Nugget", "Nugget"))
;   %LOCALAPPDATA%\Nugget\Nugget\cache
;   %APPDATA%\Nugget\Nugget\config        config.toml, sources.toml
;   %APPDATA%\dev.nugget.desktop          the desktop app's settings, history, window state
;   %LOCALAPPDATA%\dev.nugget.desktop     the WebView's data (EBWebView)
;   Credential Manager                    generic credentials "<account>.dev.nugget.Nugget"
;                                         (keyring-rs names a Windows credential "<user>.<service>")
;   %USERPROFILE%\Nugget Vault            the vault: the user's notes, unless [vault] path is set
;
; Rules:
; - Nothing is removed unless the user ticks a box. Both boxes start unticked.
; - The vault box is enabled only once "Remove models and Nugget's data" is ticked, and asks again
;   before it is accepted. Keeping the data and deleting only the notes is not offered: the index
;   would keep pointing at pages that are gone, and the vault is the part people care about most.
; - A vault at a folder the user chose ([vault] path in config.toml) is listed but never deleted.
; - Updates (/UPDATE) never show the page and never remove anything. Silent (/S) and passive (/P)
;   uninstalls keep everything, unless the command line asks with /REMOVEDATA (and /REMOVEVAULT).
; - Tauri's own "Delete the application data" box on the next page removes only the
;   dev.nugget.desktop folders. Removing Nugget's data here includes them, so it turns that on too.
;
; Variables Tauri declares later in the template ($UpdateMode, $PassiveMode,
; $DeleteAppDataCheckboxState) are only used from the hook macros in installer-hooks.nsh, which are
; expanded inside Section Uninstall, after those declarations. The functions here read the command
; line themselves.

!include LogicLib.nsh
!include FileFunc.nsh
!include nsDialogs.nsh
!include WinMessages.nsh

; This file's directory, captured now: inside a macro, ${__FILEDIR__} would be Tauri's build dir.
!define NUGGET_WIN_DIR "${__FILEDIR__}"
!define NUGGET_KEY_SUFFIX ".dev.nugget.Nugget"

Var NuggetRemoveData   ; 1: remove models, data, config, cache, app window data and saved keys
Var NuggetRemoveVault  ; 1: also delete $NuggetVaultDir
Var NuggetVaultDir     ; the default vault when it exists and may be offered, else ""
Var NuggetVaultCustom  ; `[vault] path` from config.toml as written, else ""
Var NuggetPageShown    ; 1 once the page was displayed
Var NuggetLeftovers    ; what could not be removed, one line each
Var NuggetDataCheck
Var NuggetVaultCheck
Var NuggetVaultText
Var NuggetList

; The confirm page that follows is Tauri's; say what its checkbox does, so the two pages don't read
; as two answers to one question. (Consumed by the template's MUI_UNPAGE_CONFIRM.)
!define MUI_UNCONFIRMPAGE_TEXT_TOP "Nugget will be uninstalled from the following folder. The box below clears only the app window's settings and cache. Models, data and your vault are kept unless you chose otherwise on the previous page."

UninstPage custom un.NuggetDataPage un.NuggetDataLeave

; --- helpers ---------------------------------------------------------------------------------

; Strip leading and trailing spaces, tabs and line breaks. In and out: $R0.
Function un.NuggetTrim
  Push $R1
  ${Do}
    StrCpy $R1 $R0 1
    ${If} $R1 == " "
    ${OrIf} $R1 == "$\t"
      StrCpy $R0 $R0 "" 1
    ${Else}
      ${ExitDo}
    ${EndIf}
  ${Loop}
  ${Do}
    StrCpy $R1 $R0 1 -1
    ${If} $R1 == " "
    ${OrIf} $R1 == "$\t"
    ${OrIf} $R1 == "$\r"
    ${OrIf} $R1 == "$\n"
      StrCpy $R0 $R0 -1
    ${Else}
      ${ExitDo}
    ${EndIf}
  ${Loop}
  Pop $R1
FunctionEnd

; Size of a folder in KB, 0 when it is missing. In: $R0 path. Out: $R1.
Function un.NuggetSizeKB
  Push $R2
  Push $R3
  StrCpy $R1 0
  ${If} ${FileExists} "$R0\*.*"
    ClearErrors
    ${GetSize} "$R0" "/S=0K /G=1" $R1 $R2 $R3
    ${If} ${Errors}
      StrCpy $R1 0
    ${EndIf}
  ${EndIf}
  Pop $R3
  Pop $R2
FunctionEnd

; KB as "under 1 MB", "37 MB" or "2.4 GB". In: $R1. Out: $R1.
Function un.NuggetFormatKB
  Push $R2
  Push $R3
  ${If} $R1 < 1024
    StrCpy $R1 "under 1 MB"
  ${ElseIf} $R1 < 1048576
    IntOp $R1 $R1 / 1024
    StrCpy $R1 "$R1 MB"
  ${Else}
    IntOp $R2 $R1 / 1024  ; MB
    IntOp $R2 $R2 * 10
    IntOp $R2 $R2 / 1024  ; tenths of a GB
    IntOp $R3 $R2 % 10
    IntOp $R2 $R2 / 10
    StrCpy $R1 "$R2.$R3 GB"
  ${EndIf}
  Pop $R3
  Pop $R2
FunctionEnd

; Find the vault. Sets $NuggetVaultCustom to `[vault] path` from config.toml (as written, quotes
; removed) and $NuggetVaultDir to %USERPROFILE%\Nugget Vault when no path is set and it exists.
Function un.NuggetFindVault
  Push $R0
  Push $R1
  Push $R2
  Push $R3
  StrCpy $NuggetVaultCustom ""
  StrCpy $NuggetVaultDir ""
  SetShellVarContext current
  StrCpy $R3 ""  ; current [section]
  ClearErrors
  FileOpen $R2 "$APPDATA\Nugget\Nugget\config\config.toml" r
  ${IfNot} ${Errors}
    ${Do}
      ClearErrors
      FileRead $R2 $R0
      ${If} ${Errors}
        ${ExitDo}
      ${EndIf}
      Call un.NuggetTrim
      StrCpy $R1 $R0 1
      ${If} $R1 == "["
        StrCpy $R3 $R0 7
      ${ElseIf} $R3 == "[vault]"
        StrCpy $R1 $R0 4
        ${If} $R1 == "path"
          StrCpy $R0 $R0 "" 4
          Call un.NuggetTrim
          StrCpy $R1 $R0 1
          ${If} $R1 == "="
            StrCpy $R0 $R0 "" 1
            Call un.NuggetTrim
            ; Keep what is inside the quotes, if any.
            StrCpy $R1 $R0 1
            ${If} $R1 == '"'
            ${OrIf} $R1 == "'"
              StrCpy $R0 $R0 "" 1
              Push $R4
              Push $R5
              StrCpy $R4 0
              ${Do}
                StrCpy $R5 $R0 1 $R4
                ${If} $R5 == ""
                ${OrIf} $R5 == $R1
                  ${ExitDo}
                ${EndIf}
                IntOp $R4 $R4 + 1
              ${Loop}
              StrCpy $R0 $R0 $R4
              Pop $R5
              Pop $R4
            ${EndIf}
            StrCpy $NuggetVaultCustom $R0
          ${EndIf}
        ${EndIf}
      ${EndIf}
    ${Loop}
    FileClose $R2
  ${EndIf}
  ${If} $NuggetVaultCustom == ""
  ${AndIf} $PROFILE != ""
  ${AndIf} ${FileExists} "$PROFILE\Nugget Vault\*.*"
    StrCpy $NuggetVaultDir "$PROFILE\Nugget Vault"
  ${EndIf}
  Pop $R3
  Pop $R2
  Pop $R1
  Pop $R0
FunctionEnd

; Append one line to the read-only list on the page (no 1024-character limit this way).
!macro NUGGET_LIST_LINE text
  SendMessage $NuggetList ${EM_SETSEL} -1 -1
  SendMessage $NuggetList ${EM_REPLACESEL} 0 "STR:${text}$\r$\n"
!macroend

; One folder on the list, with its size, if it exists. Sets $R9 to 1 when it does.
!macro NUGGET_LIST_DIR label path
  StrCpy $R0 "${path}"
  ${If} ${FileExists} "$R0\*.*"
    Call un.NuggetSizeKB
    Call un.NuggetFormatKB
    !insertmacro NUGGET_LIST_LINE "${label}: $R1"
    !insertmacro NUGGET_LIST_LINE "      ${path}"
    StrCpy $R9 1
  ${EndIf}
!macroend

; Command-line choices for uninstalls that show no page (/S, /P).
Function un.NuggetReadFlags
  Push $R0
  ClearErrors
  ${GetOptions} $CMDLINE "/REMOVEDATA" $R0
  ${IfNot} ${Errors}
    StrCpy $NuggetRemoveData 1
    ClearErrors
    ${GetOptions} $CMDLINE "/REMOVEVAULT" $R0
    ${IfNot} ${Errors}
      Call un.NuggetFindVault
      ${If} $NuggetVaultDir != ""
        StrCpy $NuggetRemoveVault 1
      ${EndIf}
    ${EndIf}
  ${EndIf}
  Pop $R0
FunctionEnd

; --- the page --------------------------------------------------------------------------------

Function un.NuggetDataPage
  ; Updates and passive uninstalls skip the page (the hook then reads /REMOVEDATA). Silent
  ; uninstalls never show pages at all.
  ClearErrors
  ${GetOptions} $CMDLINE "/UPDATE" $R0
  ${IfNot} ${Errors}
    Abort
  ${EndIf}
  ClearErrors
  ${GetOptions} $CMDLINE "/P" $R0
  ${IfNot} ${Errors}
    Abort
  ${EndIf}

  SetShellVarContext current
  ${If} $LOCALAPPDATA == ""
  ${OrIf} $APPDATA == ""
    Abort
  ${EndIf}
  Call un.NuggetFindVault

  ; Nothing to say when Nugget never ran.
  ${IfNot} ${FileExists} "$LOCALAPPDATA\Nugget\Nugget\*.*"
  ${AndIfNot} ${FileExists} "$APPDATA\Nugget\Nugget\*.*"
  ${AndIfNot} ${FileExists} "$APPDATA\dev.nugget.desktop\*.*"
  ${AndIfNot} ${FileExists} "$LOCALAPPDATA\dev.nugget.desktop\*.*"
  ${AndIf} $NuggetVaultDir == ""
  ${AndIf} $NuggetVaultCustom == ""
    Abort
  ${EndIf}

  !insertmacro MUI_HEADER_TEXT "Your Nugget data" "Nothing below is deleted unless you tick a box."
  nsDialogs::Create 1018
  Pop $R0
  ${If} $R0 == error
    Abort
  ${EndIf}
  StrCpy $NuggetPageShown 1

  ${NSD_CreateLabel} 0 0 100% 18u "Uninstalling removes the Nugget app. These stay on this computer, so a reinstall picks up where you left off:"
  Pop $R0

  nsDialogs::CreateControl EDIT "${DEFAULT_STYLES}|${WS_TABSTOP}|${WS_VSCROLL}|${ES_MULTILINE}|${ES_READONLY}|${ES_AUTOVSCROLL}" "${WS_EX_CLIENTEDGE}" 0 19u 100% 58u ""
  Pop $NuggetList

  ; Models first: usually the biggest. The rest of the data folder is the store, index and logs.
  StrCpy $R9 0
  StrCpy $R0 "$LOCALAPPDATA\Nugget\Nugget\data\models"
  Call un.NuggetSizeKB
  StrCpy $R8 $R1
  ${If} ${FileExists} "$R0\*.*"
    Call un.NuggetFormatKB
    !insertmacro NUGGET_LIST_LINE "Models: $R1"
    !insertmacro NUGGET_LIST_LINE "      $LOCALAPPDATA\Nugget\Nugget\data\models"
  ${EndIf}
  StrCpy $R0 "$LOCALAPPDATA\Nugget\Nugget\data"
  ${If} ${FileExists} "$R0\*.*"
    Call un.NuggetSizeKB
    IntOp $R1 $R1 - $R8
    ${If} $R1 < 0
      StrCpy $R1 0
    ${EndIf}
    Call un.NuggetFormatKB
    !insertmacro NUGGET_LIST_LINE "Search index, encrypted store and logs: $R1"
    !insertmacro NUGGET_LIST_LINE "      $LOCALAPPDATA\Nugget\Nugget\data"
  ${EndIf}
  !insertmacro NUGGET_LIST_DIR "Settings" "$APPDATA\Nugget\Nugget\config"
  !insertmacro NUGGET_LIST_DIR "Cache" "$LOCALAPPDATA\Nugget\Nugget\cache"
  !insertmacro NUGGET_LIST_DIR "App settings and history" "$APPDATA\dev.nugget.desktop"
  !insertmacro NUGGET_LIST_DIR "App window data (WebView)" "$LOCALAPPDATA\dev.nugget.desktop"
  !insertmacro NUGGET_LIST_LINE "Saved keys in Windows Credential Manager: entries ending in ${NUGGET_KEY_SUFFIX} (the store's key, API keys, account sign-ins)"
  ${If} $NuggetVaultDir != ""
    !insertmacro NUGGET_LIST_DIR "Your Nugget Vault (your notes)" "$NuggetVaultDir"
  ${EndIf}
  ${If} $NuggetVaultCustom != ""
    !insertmacro NUGGET_LIST_LINE "Your Nugget Vault, in a folder you chose (never deleted here):"
    !insertmacro NUGGET_LIST_LINE "      $NuggetVaultCustom"
  ${EndIf}
  ReadEnvStr $R0 NUGGET_HOME
  ${If} $R0 != ""
    !insertmacro NUGGET_LIST_LINE "NUGGET_HOME is set: the data in $R0 is never touched here."
  ${EndIf}
  !insertmacro NUGGET_LIST_LINE "Backups you made stay where you saved them."
  SendMessage $NuggetList ${EM_SETSEL} 0 0

  ${NSD_CreateCheckbox} 0 80u 100% 11u "&Remove models and Nugget's data (index, settings, logs, saved keys)"
  Pop $NuggetDataCheck
  ${NSD_OnClick} $NuggetDataCheck un.NuggetDataClick

  ${NSD_CreateCheckbox} 12u 93u -12u 11u "Also &delete my Nugget Vault (your notes)"
  Pop $NuggetVaultCheck

  ${If} $NuggetVaultDir != ""
    ${NSD_CreateLabel} 12u 105u -12u 26u "Your vault holds your notes as Markdown files in $NuggetVaultDir. Deleting it cannot be undone: copy anything you want to keep first."
  ${ElseIf} $NuggetVaultCustom != ""
    ${NSD_CreateLabel} 12u 105u -12u 26u "Your vault is in a folder you chose, so the uninstaller leaves it alone. Delete it yourself if you no longer want it."
  ${Else}
    ${NSD_CreateLabel} 12u 105u -12u 26u "No Nugget Vault was found in $PROFILE."
  ${EndIf}
  Pop $NuggetVaultText

  ; Coming back from the next page: show what was chosen.
  ${If} $NuggetRemoveData = 1
    ${NSD_Check} $NuggetDataCheck
  ${EndIf}
  ${If} $NuggetRemoveVault = 1
    ${NSD_Check} $NuggetVaultCheck
  ${EndIf}
  Call un.NuggetSyncVaultCheck

  nsDialogs::Show
FunctionEnd

; The vault box only makes sense together with the data box, and only for the default vault.
Function un.NuggetSyncVaultCheck
  Push $R0
  ${NSD_GetState} $NuggetDataCheck $R0
  ${If} $R0 == ${BST_CHECKED}
  ${AndIf} $NuggetVaultDir != ""
    EnableWindow $NuggetVaultCheck 1
  ${Else}
    ${NSD_Uncheck} $NuggetVaultCheck
    EnableWindow $NuggetVaultCheck 0
  ${EndIf}
  Pop $R0
FunctionEnd

Function un.NuggetDataClick
  Pop $R0  ; the control
  Call un.NuggetSyncVaultCheck
FunctionEnd

Function un.NuggetDataLeave
  ${NSD_GetState} $NuggetDataCheck $R0
  ${NSD_GetState} $NuggetVaultCheck $R1
  StrCpy $NuggetRemoveData 0
  StrCpy $NuggetRemoveVault 0
  ${If} $R0 == ${BST_CHECKED}
    StrCpy $NuggetRemoveData 1
    ${If} $R1 == ${BST_CHECKED}
    ${AndIf} $NuggetVaultDir != ""
      StrCpy $R0 "$NuggetVaultDir"
      Call un.NuggetSizeKB
      Call un.NuggetFormatKB
      MessageBox MB_YESNO|MB_ICONEXCLAMATION|MB_DEFBUTTON2 "Delete your Nugget Vault?$\r$\n$\r$\n$NuggetVaultDir ($R1)$\r$\n$\r$\nThis deletes your notes and cannot be undone." /SD IDNO IDYES nugget_vault_yes
      ${NSD_Uncheck} $NuggetVaultCheck
      Abort  ; stay on the page
      nugget_vault_yes:
      StrCpy $NuggetRemoveVault 1
    ${EndIf}
  ${EndIf}
FunctionEnd

; --- removal ---------------------------------------------------------------------------------

; Delete a folder with `rmdir /s /q`, which removes junctions and symlinks inside it without
; following them, so nothing outside the folder is touched. Notes what is left in $NuggetLeftovers.
!macro NUGGET_RMDIR path
  ${If} ${FileExists} "${path}\*.*"
    DetailPrint "Deleting ${path}"
    nsExec::ExecToLog '"$SYSDIR\cmd.exe" /c rmdir /s /q "${path}"'
    Pop $R0
    ${If} ${FileExists} "${path}\*.*"
      StrCpy $NuggetLeftovers "$NuggetLeftovers$\r$\n${path}"
    ${EndIf}
  ${EndIf}
!macroend

; Saved keys: run remove-nugget-keys.ps1 (see that file). The script is read into a script block
; rather than run with -File, so a machine-wide execution policy cannot block it.
Function un.NuggetRemoveKeys
  InitPluginsDir
  File "/oname=$PLUGINSDIR\remove-nugget-keys.ps1" "${NUGGET_WIN_DIR}\remove-nugget-keys.ps1"
  System::Call 'kernel32::SetEnvironmentVariable(t "NUGGET_REMOVE_KEYS_PS1", t "$PLUGINSDIR\remove-nugget-keys.ps1") i'
  DetailPrint "Removing Nugget's saved keys from Windows Credential Manager"
  nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -Command "& ([scriptblock]::Create([IO.File]::ReadAllText($$env:NUGGET_REMOVE_KEYS_PS1))); exit $$LASTEXITCODE"'
  Pop $R0
  ${If} $R0 != 0
    StrCpy $NuggetLeftovers "$NuggetLeftovers$\r$\nsaved keys in Credential Manager (names ending in ${NUGGET_KEY_SUFFIX})"
  ${EndIf}
FunctionEnd

; Called from NSIS_HOOK_POSTUNINSTALL on a real uninstall when $NuggetRemoveData = 1. Tauri has
; already removed the app's files and (since the hook turned its box on) the dev.nugget.desktop
; folders; nuggetd was stopped in NSIS_HOOK_PREUNINSTALL, and its model servers die with it (they
; run in a kill-on-close job object).
Function un.NuggetRemoveAll
  Push $R0
  SetShellVarContext current
  StrCpy $NuggetLeftovers ""
  ${If} $LOCALAPPDATA == ""
  ${OrIf} $APPDATA == ""
    DetailPrint "Nugget's data was kept: the AppData folders could not be resolved"
    Pop $R0
    Return
  ${EndIf}

  ; Keys first, so a failure to delete files never leaves keys without their data's owner knowing.
  Call un.NuggetRemoveKeys

  !insertmacro NUGGET_RMDIR "$LOCALAPPDATA\Nugget\Nugget"  ; data (models, store, logs) and cache
  !insertmacro NUGGET_RMDIR "$APPDATA\Nugget\Nugget"       ; config
  RMDir "$LOCALAPPDATA\Nugget"  ; only when empty; usually the install folder itself
  RMDir "$APPDATA\Nugget"
  RMDir "$INSTDIR"
  ; Tauri's own removal uses RmDir /r and may fail on a locked WebView file; report it too.
  ${If} ${FileExists} "$APPDATA\dev.nugget.desktop\*.*"
    StrCpy $NuggetLeftovers "$NuggetLeftovers$\r$\n$APPDATA\dev.nugget.desktop"
  ${EndIf}
  ${If} ${FileExists} "$LOCALAPPDATA\dev.nugget.desktop\*.*"
    StrCpy $NuggetLeftovers "$NuggetLeftovers$\r$\n$LOCALAPPDATA\dev.nugget.desktop"
  ${EndIf}

  ${If} $NuggetRemoveVault = 1
  ${AndIf} $NuggetVaultDir != ""
    !insertmacro NUGGET_RMDIR "$NuggetVaultDir"
  ${EndIf}

  ${If} $NuggetLeftovers != ""
    DetailPrint "Could not remove:$NuggetLeftovers"
    MessageBox MB_OK|MB_ICONEXCLAMATION "Some of Nugget's data could not be removed:$\r$\n$NuggetLeftovers$\r$\n$\r$\nDelete it yourself; the Nugget guide's Uninstall page lists every location." /SD IDOK
  ${EndIf}
  Pop $R0
FunctionEnd
