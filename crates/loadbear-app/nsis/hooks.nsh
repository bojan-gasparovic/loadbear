; Uninstall has to undo setup, and setup registered a Windows service.
;
; Without this, removing LoadBear leaves LoadBearHelper running as Local System,
; set to start again at every boot, pointing at an executable the user believes
; they deleted. An uninstaller that does that is worse than no uninstaller.
;
; The helper does the work rather than NSIS, because the service name, the
; installed path and the stop-then-delete ordering are already written once in
; Rust and a second copy of them here would drift.
;
; PREUNINSTALL, so the service is stopped and deregistered while its executable
; is still on disk to be run. Errors are ignored on purpose: a machine that
; never enabled temperature has no service to remove, and an uninstall must not
; stop for that.
!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Removing the LoadBear helper service..."
  nsExec::Exec '"$INSTDIR\loadbear-service.exe" --teardown'
  Pop $0
!macroend

; Upgrading over a running helper would fail, and fail quietly.
;
; The service runs the very executable this installer is about to overwrite, and
; Windows holds a running program open. Stopping and deregistering it first is
; the only way the copy can succeed. On a first install there is nothing at that
; path and nsExec simply reports an error we ignore.
;
; The consequence is that an upgrade leaves temperature switched off until the
; user presses Enable temperature once more. That is a click, it is a button the
; window already shows when the helper is absent, and it is a great deal better
; than an upgrade that appears to work while running the previous binary.
!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec '"$INSTDIR\loadbear-service.exe" --teardown'
  Pop $0
!macroend
