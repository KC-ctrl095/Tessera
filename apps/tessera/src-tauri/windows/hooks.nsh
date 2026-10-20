; Tessera's own steps in the Windows installer and uninstaller (Tauri's
; NSIS template inserts these macros; see tauri.windows.conf.json).
;
; The server and this computer's machine run in the background from
; tessera.exe and tessera-service.exe in the install folder, started at
; sign-in. An update stops them before it replaces those files (a running
; program can't be replaced) and starts them again afterwards; removing
; Tessera stops them and takes them out of the programs Windows starts at
; sign-in. Your chats and settings (%APPDATA%\tessera) are never touched.
;
; Only commands every earlier tessera.exe knows are used before the files
; are replaced: during an update the one in the folder is still the old one.
; They stop the server cleanly, so chats that were working carry on when it
; starts again. A server the app started only for now isn't started here:
; the app starts the server it set up on this PC again when it opens. A step
; that fails is said, never silently dropped, and each one has a time limit.
;
; Before anything is stopped, each startup entry's whole command line (its
; data folder, its address) is noted in HKCU\Software\Tessera\Update; after
; the new files are in place `tessera service restore-update` puts the same
; entries back exactly and starts them, and only then forgets the notes. An
; update cut short keeps them: the next installer, or the app when it opens,
; finishes it. Stopped work stays stopped (the user's Stop is kept apart from
; all this and nothing here touches it).

Var TesseraServerWasOn
Var TesseraMachineWasOn
Var TesseraNoteFailed

Var TesseraOtherServerEntry

; Match the first program including its argument boundary. A substring in
; another path or in --log must not take over a different installation.
!macro TESSERA_ENTRY_FROM_HERE LINE RESULT
  StrCpy ${RESULT} 0
  StrCpy $R2 '"$INSTDIR\tessera-service.exe" '
  StrLen $R3 $R2
  StrCpy $R4 "${LINE}" $R3
  ${If} $R4 == $R2
    StrCpy ${RESULT} 1
  ${Else}
    StrCpy $R2 "$INSTDIR\tessera-service.exe "
    StrLen $R3 $R2
    StrCpy $R4 "${LINE}" $R3
    ${If} $R4 == $R2
      StrCpy ${RESULT} 1
    ${EndIf}
  ${EndIf}
!macroend

!define TESSERA_RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define TESSERA_UPDATE_KEY "Software\Tessera\Update"

; Note startup entry NAME's whole command line when it starts this folder's
; Tessera (an earlier note of a cut-short update stays: the entry may
; already be out).
!macro TesseraNote NAME
  ReadRegStr $R0 HKCU "${TESSERA_RUN_KEY}" "${NAME}"
  !insertmacro TESSERA_ENTRY_FROM_HERE "$R0" $R1
  ${If} $R1 = 1
    ClearErrors
    WriteRegStr HKCU "${TESSERA_UPDATE_KEY}" "${NAME}" "$R0"
    ${If} ${Errors}
      StrCpy $TesseraNoteFailed 1
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  StrCpy $TesseraServerWasOn 0
  StrCpy $TesseraMachineWasOn 0
  StrCpy $TesseraNoteFailed 0
  ${If} ${FileExists} "$INSTDIR\tessera.exe"
    ; Notes of a cut-short update of another folder are that one's.
    ReadRegStr $R3 HKCU "${TESSERA_UPDATE_KEY}" "Folder"
    ${If} $R3 != ""
    ${AndIf} $R3 != "$INSTDIR"
      DetailPrint "Replacing the notes of an unfinished update of Tessera in $R3"
      DeleteRegKey HKCU "${TESSERA_UPDATE_KEY}"
    ${EndIf}
    System::Call 'kernel32::GetCurrentProcessId() i .R4'
    ClearErrors
    WriteRegStr HKCU "${TESSERA_UPDATE_KEY}" "Folder" "$INSTDIR"
    WriteRegStr HKCU "${TESSERA_UPDATE_KEY}" "Installer" "$R4"
    ${If} ${Errors}
      StrCpy $TesseraNoteFailed 1
    ${EndIf}
    !insertmacro TesseraNote "Tessera"
    !insertmacro TesseraNote "Tessera machine"
    ${If} $TesseraNoteFailed = 1
      MessageBox MB_OK|MB_ICONSTOP "Tessera couldn't note how it starts when you sign in, so the update changed nothing. Your chats and settings are kept. Try the update again." /SD IDOK
      Abort "Tessera couldn't note how it starts when you sign in; nothing was changed"
    ${EndIf}
    ; Only these update stops preserve the notes for restore-update.
    System::Call 'kernel32::SetEnvironmentVariable(t "TESSERA_UPDATE", t "1")'
    ReadRegStr $R0 HKCU "${TESSERA_UPDATE_KEY}" "Tessera"
    !insertmacro TESSERA_ENTRY_FROM_HERE "$R0" $R1
    ${If} $R1 = 1
      StrCpy $TesseraServerWasOn 1
      DetailPrint "Stopping the Tessera server for the update"
      nsExec::Exec /TIMEOUT=60000 '"$INSTDIR\tessera.exe" service uninstall'
      Pop $R2
      ${If} $R2 != 0
        DetailPrint "It didn't stop by itself ($R2); it is ended below"
      ${EndIf}
    ${EndIf}
    ReadRegStr $R0 HKCU "${TESSERA_UPDATE_KEY}" "Tessera machine"
    !insertmacro TESSERA_ENTRY_FROM_HERE "$R0" $R1
    ${If} $R1 = 1
      StrCpy $TesseraMachineWasOn 1
      DetailPrint "Stopping this computer's Tessera machine for the update"
      nsExec::Exec /TIMEOUT=60000 '"$INSTDIR\tessera.exe" node service uninstall --json'
      Pop $R2
      ${If} $R2 != 0
        DetailPrint "It didn't stop by itself ($R2); it is ended below"
      ${EndIf}
    ${EndIf}
    System::Call 'kernel32::SetEnvironmentVariable(t "TESSERA_UPDATE", p 0)'
    ; Whatever else still runs from this folder (a server the app started
    ; only for now, a machine the app runs, one that didn't stop in time):
    ; end it, or its files can't be replaced.
    !insertmacro RestartManager_ShutdownFile "$INSTDIR\tessera-service.exe" $R2
    !insertmacro RestartManager_ShutdownFile "$INSTDIR\tessera.exe" $R2
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ${If} $TesseraServerWasOn = 1
  ${OrIf} $TesseraMachineWasOn = 1
    DetailPrint "Starting Tessera again as it was before the update"
    nsExec::Exec /TIMEOUT=150000 '"$INSTDIR\tessera.exe" service restore-update'
    Pop $R2
    ${If} $R2 != 0
      ; Your chats and settings are kept; the notes too, so the app finishes it.
      DetailPrint "Tessera didn't start again as it was ($R2)"
      MessageBox MB_OK|MB_ICONINFORMATION "Tessera's server or machine on this PC didn't start again after the update. Your chats and settings are kept, and the Tessera app finishes this when you open it.$\r$\n$\r$\nTo do it now, run this in PowerShell:$\r$\n& $\"$INSTDIR\tessera.exe$\" service restore-update" /SD IDOK
    ${EndIf}
  ${Else}
    ; Nothing was taken out: nothing to keep noted.
    ReadRegStr $R3 HKCU "${TESSERA_UPDATE_KEY}" "Folder"
    ${If} $R3 == "$INSTDIR"
      DeleteRegKey HKCU "${TESSERA_UPDATE_KEY}"
    ${EndIf}
  ${EndIf}
  ; Guard remains opt-in: never elevate a silent install, or enable access.
  ${IfNot} ${Silent}
    ReadRegStr $R3 HKLM "SYSTEM\CurrentControlSet\Services\TesseraGuard" "ImagePath"
    ${If} $R3 == ""
      MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Install Tessera Guard for agent commands and desktop control? It keeps your Stop in a Windows service. Windows will ask an administrator. This does not turn agent access on." /SD IDNO IDNO tessera_guard_install_done
    ${Else}
      MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Update Tessera Guard too? Stop agent work on this PC first. Windows will ask an administrator. Your access settings and saved Stop are kept." /SD IDNO IDNO tessera_guard_install_done
    ${EndIf}
    nsExec::Exec /TIMEOUT=660000 '"$INSTDIR\tessera.exe" guard install'
    Pop $R2
    ${If} $R2 != 0
      DetailPrint "Tessera Guard wasn't installed or updated ($R2)"
      MessageBox MB_OK|MB_ICONINFORMATION "Tessera Guard wasn't installed or updated. Agent access wasn't changed. Open Settings, Devices, This computer, Install Tessera Guard to try again." /SD IDOK
    ${EndIf}
    tessera_guard_install_done:
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  StrCpy $TesseraOtherServerEntry ""
  ${If} $UpdateMode <> 1
    ReadRegStr $R0 HKCU "${TESSERA_RUN_KEY}" "Tessera"
    !insertmacro TESSERA_ENTRY_FROM_HERE "$R0" $R1
    ${If} $R1 = 0
      StrCpy $TesseraOtherServerEntry $R0
    ${EndIf}
  ${EndIf}
  ; An update runs the old uninstaller first: the installer's own steps
  ; above stop and start things then.
  ${If} $UpdateMode <> 1
  ${AndIf} ${FileExists} "$INSTDIR\tessera.exe"
    ReadRegStr $R0 HKCU "${TESSERA_RUN_KEY}" "Tessera machine"
    !insertmacro TESSERA_ENTRY_FROM_HERE "$R0" $R1
    ${If} $R1 = 1
      DetailPrint "Stopping this computer's Tessera machine"
      nsExec::Exec /TIMEOUT=60000 '"$INSTDIR\tessera.exe" node service uninstall --json'
      Pop $R2
    ${EndIf}
    ReadRegStr $R0 HKCU "${TESSERA_RUN_KEY}" "Tessera"
    !insertmacro TESSERA_ENTRY_FROM_HERE "$R0" $R1
    ${If} $R1 = 1
      DetailPrint "Stopping the Tessera server"
      nsExec::Exec /TIMEOUT=60000 '"$INSTDIR\tessera.exe" service uninstall'
      Pop $R2
    ${EndIf}
    ; The guard is shared by installations: removal needs explicit consent
    ; and its protected program asks Windows for administrator approval.
    ReadRegStr $R3 HKLM "SYSTEM\CurrentControlSet\Services\TesseraGuard" "ImagePath"
    ${If} $R3 != ""
      MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Remove Tessera Guard too? Other Tessera installations on this PC will no longer be able to run agent commands or use desktop control. Windows will ask an administrator." /SD IDNO IDNO tessera_guard_remove_kept
      nsExec::Exec /TIMEOUT=660000 '"$PROGRAMFILES64\Tessera Guard\tessera-guard.exe" guard uninstall'
      Pop $R2
      ${If} $R2 != 0
        DetailPrint "Tessera Guard wasn't removed ($R2)"
        MessageBox MB_OK|MB_ICONINFORMATION "Tessera Guard wasn't removed. To try again in PowerShell:$\r$\n& $\"$PROGRAMFILES64\Tessera Guard\tessera-guard.exe$\" guard uninstall" /SD IDOK
      ${EndIf}
      Goto tessera_guard_remove_done
      tessera_guard_remove_kept:
      DetailPrint "Tessera Guard stays installed: remove it with tessera-guard.exe guard uninstall"
      tessera_guard_remove_done:
    ${EndIf}
    ; Removed for good: an unfinished update's notes go too.
    ReadRegStr $R3 HKCU "${TESSERA_UPDATE_KEY}" "Folder"
    ${If} $R3 == "$INSTDIR"
      DeleteRegKey HKCU "${TESSERA_UPDATE_KEY}"
    ${EndIf}
    !insertmacro RestartManager_ShutdownFile "$INSTDIR\tessera-service.exe" $R2
    !insertmacro RestartManager_ShutdownFile "$INSTDIR\tessera.exe" $R2
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Tauri's template deletes the Run value "Tessera" by product name.
  ; Restore a different install's entry, unless something already replaced it.
  ${If} $TesseraOtherServerEntry != ""
    ReadRegStr $R0 HKCU "${TESSERA_RUN_KEY}" "Tessera"
    ${If} $R0 == ""
      WriteRegStr HKCU "${TESSERA_RUN_KEY}" "Tessera" $TesseraOtherServerEntry
    ${EndIf}
  ${EndIf}
!macroend
