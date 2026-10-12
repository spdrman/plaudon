; Plaudon Windows installer (EPIC O, task O2.6; migration plan §3 "Bundling").
;
; NSIS rather than WiX: the toolchain has to be redistributable under terms
; compatible with the rest of this repository, and NSIS ships under the
; zlib/libpng licence (its LZMA and bzip2 components under their own permissive
; terms), while the WiX Toolset is MS-RL. The register entry is in
; docs/licensing/third-party-register.md.
;
; What this installer is allowed to do, and nothing more:
;
;   * install per user, under %LOCALAPPDATA%\Programs\Plaudon, with
;     RequestExecutionLevel user. No elevation, no service, no scheduled task,
;     no machine-wide registry key, no PATH edit, no autostart.
;   * claim the `.plaudon` archive extension and the `plaudon://` scheme under
;     HKCU\Software\Classes. Those keys are *generated* by
;     `plaudon_platform::associations::windows_registry_script`;
;     `apps/plaudon-egui/tests/packaging.rs` parses this file and fails if a
;     key, value or datum here differs from that generator, so the two cannot
;     drift. `OpenWithProgids` is used rather than a forced default, so
;     Windows' own "choose a default app" flow stays in charge.
;   * remove exactly what it wrote, on uninstall.
;
; It deliberately does **not** touch the library directory
; (%APPDATA%\app.plaudon.desktop): an uninstaller that deletes recordings is
; a data-loss bug, and the user's library outlives the application.
;
; Built by scripts/package-windows-egui.ps1, which passes:
;   /DPLAUDON_VERSION=<semver>  /DPLAUDON_STAGE=<dir with the payload>
;   /DPLAUDON_OUTFILE=<installer path>

Unicode true
ManifestDPIAware true
; Without a supportedOS declaration Windows decides that a file named
; "…setup.exe" is a legacy installer and injects compatibility shims into the
; process before any of our code runs. Declaring the OS list turns that off;
; makensis /WX reports its absence as warning 9000.
ManifestSupportedOS all
SetCompressor /SOLID lzma

!ifndef PLAUDON_VERSION
  !error "PLAUDON_VERSION must be defined by the packaging script"
!endif
!ifndef PLAUDON_STAGE
  !error "PLAUDON_STAGE must be defined by the packaging script"
!endif
!ifndef PLAUDON_OUTFILE
  !define PLAUDON_OUTFILE "Plaudon-Setup.exe"
!endif

!define PRODUCT "Plaudon"
!define EXECUTABLE "plaudon-egui.exe"
!define CLI "plaudon.exe"
!define PROGID "Plaudon.Archive"
!define ARCHIVE_TYPE_NAME "Plaudon portable archive"
!define URL_SCHEME "plaudon"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}"

Name "${PRODUCT}"
OutFile "${PLAUDON_OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${PRODUCT}"
InstallDirRegKey HKCU "Software\${PRODUCT}" "InstallDir"
RequestExecutionLevel user

VIProductVersion "${PLAUDON_VERSION}.0"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "FileDescription" "Plaudon installer"
VIAddVersionKey "FileVersion" "${PLAUDON_VERSION}"
VIAddVersionKey "ProductVersion" "${PLAUDON_VERSION}"
VIAddVersionKey "LegalCopyright" "Licensed under the Apache License, Version 2.0"

!include "MUI2.nsh"
!define MUI_ICON "plaudon.ico"
!define MUI_UNICON "plaudon.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXECUTABLE}"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Plaudon" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "${PLAUDON_STAGE}\${EXECUTABLE}"
  File "${PLAUDON_STAGE}\${CLI}"
  File "plaudon.ico"

  WriteRegStr HKCU "Software\${PRODUCT}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\${PRODUCT}" "Version" "${PLAUDON_VERSION}"

  ; ---- file association and protocol handler -------------------------------
  ; Mirrors plaudon_platform::associations::windows_registry_script with
  ; %EXEPATH% resolved to the installed executable. Keep in lockstep; the
  ; packaging test enforces it.
  WriteRegStr HKCU "Software\Classes\.plaudon" "" "${PROGID}"
  WriteRegStr HKCU "Software\Classes\.plaudon\OpenWithProgids" "${PROGID}" ""
  WriteRegStr HKCU "Software\Classes\${PROGID}" "" "${ARCHIVE_TYPE_NAME}"
  WriteRegStr HKCU "Software\Classes\${PROGID}\DefaultIcon" "" '"$INSTDIR\${EXECUTABLE}",0'
  WriteRegStr HKCU "Software\Classes\${PROGID}\shell\open\command" "" '"$INSTDIR\${EXECUTABLE}" "%1"'
  WriteRegStr HKCU "Software\Classes\${URL_SCHEME}" "" "URL:Plaudon Protocol"
  WriteRegStr HKCU "Software\Classes\${URL_SCHEME}" "URL Protocol" ""
  WriteRegStr HKCU "Software\Classes\${URL_SCHEME}\DefaultIcon" "" '"$INSTDIR\${EXECUTABLE}",0'
  WriteRegStr HKCU "Software\Classes\${URL_SCHEME}\shell\open\command" "" '"$INSTDIR\${EXECUTABLE}" "%1"'

  ; SHCNE_ASSOCCHANGED, so Explorer notices without a sign-out.
  System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'

  CreateShortCut "$SMPROGRAMS\${PRODUCT}.lnk" "$INSTDIR\${EXECUTABLE}" "" "$INSTDIR\plaudon.ico"

  ; ---- Programs and Features -----------------------------------------------
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${PLAUDON_VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\plaudon.ico"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "Plaudon"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\${EXECUTABLE}"
  Delete "$INSTDIR\${CLI}"
  Delete "$INSTDIR\plaudon.ico"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${PRODUCT}.lnk"

  DeleteRegKey HKCU "Software\Classes\.plaudon"
  DeleteRegKey HKCU "Software\Classes\${PROGID}"
  DeleteRegKey HKCU "Software\Classes\${URL_SCHEME}"
  DeleteRegKey HKCU "Software\${PRODUCT}"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'

  ; The library under %APPDATA%\app.plaudon.desktop is the user's data and is
  ; deliberately left in place.
SectionEnd
