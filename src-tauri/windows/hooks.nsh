; Installer hooks (bundle.windows.nsis.installerHooks in tauri.conf.json).

; ─── Desktop shortcut ────────────────────────────────────────────────────────
;
; Installed over an earlier version (by hand, or by winget), the installer
; puts the desktop shortcut back even when it had been deleted. The app's own
; updates do not (they pass /UPDATE). Here an install over an earlier one
; keeps the desktop as it was: the uninstaller notes whether the shortcut was
; there before a reinstall removes it, and a silent install looks for itself.
; A first install still creates it, as before.

; "1" or "0": whether the desktop had the shortcut; empty for a first install.
Var DesktopBefore

!macro NSIS_HOOK_PREINSTALL
  ReadRegStr $DesktopBefore HKCU "${MANUPRODUCTKEY}" "DesktopShortcut"
  DeleteRegValue HKCU "${MANUPRODUCTKEY}" "DesktopShortcut"
  ${If} $DesktopBefore == ""
    ReadRegStr $DesktopBefore SHCTX "${UNINSTKEY}" "UninstallString"
    ${If} $DesktopBefore != ""
      ${If} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
        StrCpy $DesktopBefore "1"
      ${Else}
        StrCpy $DesktopBefore "0"
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    ${If} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
      WriteRegStr HKCU "${MANUPRODUCTKEY}" "DesktopShortcut" "1"
    ${Else}
      WriteRegStr HKCU "${MANUPRODUCTKEY}" "DesktopShortcut" "0"
    ${EndIf}
  ${EndIf}
!macroend

; ─── Firewall ────────────────────────────────────────────────────────────────
;
; A firewall rule lets friends on the same network connect straight to the
; app (fast local transfers, LocalSend). Without it, Windows asks on first
; launch, for private networks only, and a "Cancel" blocks it for good.
; Adding the rule takes the administrator's yes (one prompt), asked on a
; first install only: not on updates or silent installs, nor when it exists.
; It stays after an uninstall (it only lets that program in): removing it
; would take another prompt, twice on each install over an earlier version.

!define FIREWALL_RULE "P2P Share"

!macro NSIS_HOOK_POSTINSTALL
  ; No shortcut before: none now, from a silent install (made just above) or
  ; from the box of the last page.
  ${If} $DesktopBefore == "0"
    Delete "$DESKTOP\${PRODUCTNAME}.lnk"
    StrCpy $NoShortcutMode 1
  ${EndIf}

  ${If} $UpdateMode <> 1
  ${AndIfNot} ${Silent}
    nsExec::ExecToStack 'netsh advfirewall firewall show rule name="${FIREWALL_RULE}"'
    Pop $0
    Pop $1
    ${If} $0 <> 0
      ExecShellWait "runas" "netsh" 'advfirewall firewall add rule name="${FIREWALL_RULE}" dir=in action=allow program="$INSTDIR\${MAINBINARYNAME}.exe" enable=yes profile=any' SW_HIDE
    ${EndIf}
  ${EndIf}
!macroend
