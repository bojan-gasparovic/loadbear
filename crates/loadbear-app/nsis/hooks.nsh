; Setup registers a Windows service. Both halves of the installer have to deal
; with that, and for the same underlying reason: the service runs an executable
; the installer is about to write to, and Windows holds a running program open.
;
; Uninstalling without this leaves LoadBearHelper running as Local System, set
; to start again at every boot, pointing at a program the user believes they
; deleted. Installing without it silently keeps the previous helper, which is a
; failure this project has already paid for three times.
;
; `sc.exe` rather than the helper's own `--teardown`, deliberately. At preinstall
; the executable sitting in the target directory is the *previous* version, and
; asking a binary from before this feature existed to perform it does nothing at
; all, quietly. That is not a hypothetical: it is what the first build of this
; hook actually did. `sc.exe` ships with Windows and has no version to be wrong
; about.
;
; Every result is popped and discarded. A machine that never enabled temperature
; has no service, `sc.exe` returns 1060, and neither installing nor uninstalling
; may stop for that.
!macro LOADBEAR_REMOVE_SERVICE
  DetailPrint "Removing the LoadBear helper service..."
  nsExec::Exec 'sc.exe stop LoadBearHelper'
  Pop $0
  ; Stopping is asynchronous and the file stays locked until the process is
  ; gone. Deleting too early leaves the registration marked for deletion and
  ; the executable still held open, which is the worst of both.
  Sleep 3000
  nsExec::Exec 'sc.exe delete LoadBearHelper'
  Pop $0
  Sleep 1000
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro LOADBEAR_REMOVE_SERVICE
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro LOADBEAR_REMOVE_SERVICE
!macroend
