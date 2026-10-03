; The Windows installer: duscape for the user who runs it, in Settings → Apps and the Start
; menu, uninstalled from either. Built by `make installer`, and by deploy.yml from the
; release's exes:
;
;   makensis -NOCD -DVERSION=0.2.1 -DBIN=<folder of duscape.exe and duscape-windows.exe> \
;     -DSETUP=<setup.exe> installer/duscape.nsi
;
; Paths here are from the repository's root (-NOCD), written with backslashes: Windows's
; makensis finds a `File` only by the part after the last backslash (`dir/name` is not found),
; and the Linux one reads a backslash as a slash.
;
; Per user, with no administrator asked: the window asks for elevation itself (through `runas`,
; `duscape_windows::elevate`) only when a whole volume wants it, so nothing is gained by
; installing for every user, and the install, an upgrade and the uninstall need no prompt.
; The user's config (`~/.config/duscape`) is theirs and is left by the uninstall.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma
RequestExecutionLevel user

!ifndef VERSION
  !error "-DVERSION=<the package's version> is needed"
!endif
!ifndef BIN
  !error "-DBIN=<the folder holding duscape.exe and duscape-windows.exe> is needed"
!endif
!ifndef SETUP
  !error "-DSETUP=<the setup program to write> is needed"
!endif

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\duscape"
!define CLASSES "Software\Classes"
!define WINDOW "$INSTDIR\duscape-windows.exe"
!define POWERSHELL "$SYSDIR\WindowsPowerShell\v1.0\powershell.exe"

Name "duscape"
Caption "duscape ${VERSION} Setup"
OutFile "${SETUP}"
InstallDir "$LOCALAPPDATA\Programs\duscape"
; An upgrade goes where the last install went.
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"

; The setup program's own version: four numbers, a pre-release's suffix dropped.
!searchparse "v${VERSION}-" "v" VERSION_MAJOR "." VERSION_MINOR "." VERSION_PATCH "-"
VIProductVersion "${VERSION_MAJOR}.${VERSION_MINOR}.${VERSION_PATCH}.0"
VIAddVersionKey "ProductName" "duscape"
VIAddVersionKey "FileDescription" "duscape setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "Ang Chin Han"
VIAddVersionKey "LegalCopyright" "Copyright (c) 2020 Aram Drevekenin; Copyright (c) 2026 Ang Chin Han and duscape contributors; MIT licence"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "WinMessages.nsh"

!define MUI_ICON "viewers\windows\duscape.ico"
!define MUI_UNICON "viewers\windows\duscape.ico"
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_NODESC
!define MUI_FINISHPAGE_RUN "${WINDOW}"
!define MUI_FINISHPAGE_RUN_TEXT "Open duscape"

!insertmacro MUI_PAGE_LICENSE "LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; What the optional sections add, taken away: before they run again (so an upgrade with one
; unticked loses it) and by the uninstall.
!macro RemoveExtras
  Delete "$SMPROGRAMS\duscape.lnk"
  Delete "$DESKTOP\duscape.lnk"
  DeleteRegKey HKCU "${CLASSES}\Directory\shell\duscape"
  DeleteRegKey HKCU "${CLASSES}\Drive\shell\duscape"
!macroend

; `path.ps1 add|remove`, on this install's folder; the shell told of the change after.
!macro EditPath ACTION
  InitPluginsDir
  File "/oname=$PLUGINSDIR\path.ps1" "installer\path.ps1"
  nsExec::ExecToLog '"${POWERSHELL}" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\path.ps1" ${ACTION} "$INSTDIR"'
  Pop $0
  StrCmp $0 "0" +2
    DetailPrint "The PATH was not changed ($0)."
  SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
!macroend

Section "duscape" SectionProgram
  SectionIn RO
  SetOutPath "$INSTDIR"
  ; A running duscape holds its exe: File then offers Retry, naming it.
  File "${BIN}\duscape.exe"
  File "${BIN}\duscape-windows.exe"
  File "/oname=LICENSE.txt" "LICENSE"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  !insertmacro RemoveExtras

  ; Settings → Apps.
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "duscape"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Ang Chin Han"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "${WINDOW},0"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/angch/duscape"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "HelpLink" "https://github.com/angch/duscape"
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" $0
SectionEnd

; The window alone, a windows-subsystem program: no console flashes before Windows 11 24H2,
; as one would for duscape.exe.
Section "Start menu shortcut" SectionStartMenu
  CreateShortcut "$SMPROGRAMS\duscape.lnk" "${WINDOW}"
SectionEnd

Section /o "Desktop shortcut" SectionDesktop
  CreateShortcut "$DESKTOP\duscape.lnk" "${WINDOW}"
SectionEnd

; duscape.exe in a terminal: the terminal viewer, or the window with --gui.
Section "Add to PATH, for duscape in a terminal" SectionPath
  !insertmacro EditPath add
SectionEnd

; "Open in duscape" on a folder and on a drive. A drive's `%1` is `C:\`, left unquoted:
; quoted, `"C:\"` reads as `C:"`, the backslash escaping the quote (CommandLineToArgvW), and a
; drive's path has no space. Not on a folder's background, whose `%V` is a drive's root too.
; On Windows 11 these are under "Show more options": its first menu takes only packaged
; handlers.
Section "Explorer menu: Open in duscape" SectionExplorer
  WriteRegStr HKCU "${CLASSES}\Directory\shell\duscape" "" "Open in duscape"
  WriteRegStr HKCU "${CLASSES}\Directory\shell\duscape" "Icon" "${WINDOW},0"
  WriteRegStr HKCU "${CLASSES}\Directory\shell\duscape\command" "" '"${WINDOW}" "%1"'
  WriteRegStr HKCU "${CLASSES}\Drive\shell\duscape" "" "Open in duscape"
  WriteRegStr HKCU "${CLASSES}\Drive\shell\duscape" "Icon" "${WINDOW},0"
  WriteRegStr HKCU "${CLASSES}\Drive\shell\duscape\command" "" '"${WINDOW}" %1'
SectionEnd

; A file the uninstall cannot delete is one a running duscape holds: ask, rather than leave it.
!macro DeleteOrAsk FILE
  Delete "${FILE}"
  IfFileExists "${FILE}" 0 +3
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "duscape is running. Close it, then Retry." /SD IDCANCEL IDRETRY -2
    Abort "duscape is running."
!macroend

Section "Uninstall"
  !insertmacro DeleteOrAsk "$INSTDIR\duscape.exe"
  !insertmacro DeleteOrAsk "$INSTDIR\duscape-windows.exe"
  !insertmacro RemoveExtras
  !insertmacro EditPath remove
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\uninstall.exe"
  ; Not /r: only what was put there, so a folder chosen by mistake keeps whatever else it holds.
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd
