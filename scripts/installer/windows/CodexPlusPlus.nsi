Unicode true
!include "MUI2.nsh"
!include "x64.nsh"

!ifndef VERSION
  !define VERSION "1.3.2"
!endif
!ifndef ARCH
  !define ARCH "x64"
!endif
!if "${ARCH}" != "x64"
  !if "${ARCH}" != "arm64"
    !error "ARCH must be x64 or arm64"
  !endif
!endif
!define ROOT "..\..\.."

Name "Z8 Codex"
OutFile "${ROOT}\dist\windows\Z8Codex-${VERSION}-windows-${ARCH}-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\Z8 Codex"
InstallDirRegKey HKCU "Software\Z8 Codex" "InstallDir"
RequestExecutionLevel user
; Keep all shortcut and registry operations scoped to the installing user's profile.
; This prevents an upgrade or uninstall from touching another Windows user's Z8 or
; host configuration.
SetCompressor /SOLID lzma
; Never offer Ignore when an installed executable cannot be replaced.
AllowSkipFiles off

!define MUI_ICON "${ROOT}\apps\codex-plus-manager\src-tauri\icons\icon.ico"
!define MUI_UNICON "${ROOT}\apps\codex-plus-manager\src-tauri\icons\icon.ico"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

Function .onInit
!if "${ARCH}" == "arm64"
  ${IfNot} ${IsNativeARM64}
    MessageBox MB_OK|MB_ICONSTOP "此安装包仅支持 Windows ARM64。请下载与电脑芯片匹配的版本。"
    Abort "Windows ARM64 required"
  ${EndIf}
!else
  ${IfNot} ${IsNativeAMD64}
    MessageBox MB_OK|MB_ICONSTOP "此安装包仅支持 Windows x64。请下载与电脑芯片匹配的版本。"
    Abort "Windows x64 required"
  ${EndIf}
!endif
FunctionEnd

; Windows fixed-file versions require four numeric components. Preserve the full
; release/pre-release VERSION in File Properties and use its numeric core here.
!searchparse /noerrors "${VERSION}" "" Z8_VERSION_MAJOR "." Z8_VERSION_MINOR "." Z8_VERSION_PATCH "-" Z8_VERSION_SUFFIX
VIProductVersion "${Z8_VERSION_MAJOR}.${Z8_VERSION_MINOR}.${Z8_VERSION_PATCH}.0"
VIFileVersion "${Z8_VERSION_MAJOR}.${Z8_VERSION_MINOR}.${Z8_VERSION_PATCH}.0"
VIAddVersionKey /LANG=${LANG_ENGLISH} "ProductName" "Z8 Codex"
VIAddVersionKey /LANG=${LANG_ENGLISH} "CompanyName" "Z8"
VIAddVersionKey /LANG=${LANG_ENGLISH} "FileDescription" "Z8 Codex installer"
VIAddVersionKey /LANG=${LANG_ENGLISH} "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=${LANG_ENGLISH} "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=${LANG_ENGLISH} "OriginalFilename" "Z8Codex-${VERSION}-windows-${ARCH}-setup.exe"

Section "Install"
  SetShellVarContext current
  SetOutPath "$INSTDIR"

  ; Probe only the files at the selected Z8 install path. Append mode requests
  ; write access without truncating the file; a write lock is detected here.
  ; This is a preflight, not a guarantee: each File result is checked below.
  IfFileExists "$INSTDIR\z8-codex.exe" 0 z8_install_check_manager
  ClearErrors
  FileOpen $0 "$INSTDIR\z8-codex.exe" a
  IfErrors z8_install_locked
  FileClose $0
z8_install_check_manager:
  IfFileExists "$INSTDIR\z8-codex-manager.exe" 0 z8_install_check_legacy_launcher
  ClearErrors
  FileOpen $0 "$INSTDIR\z8-codex-manager.exe" a
  IfErrors z8_install_locked
  FileClose $0
z8_install_check_legacy_launcher:
  IfFileExists "$INSTDIR\codex-plus-plus.exe" 0 z8_install_check_legacy_manager
  ClearErrors
  FileOpen $0 "$INSTDIR\codex-plus-plus.exe" a
  IfErrors z8_install_locked
  FileClose $0
z8_install_check_legacy_manager:
  IfFileExists "$INSTDIR\codex-plus-plus-manager.exe" 0 z8_install_ready
  ClearErrors
  FileOpen $0 "$INSTDIR\codex-plus-plus-manager.exe" a
  IfErrors z8_install_locked
  FileClose $0
  Goto z8_install_ready
z8_install_locked:
  MessageBox MB_OK|MB_ICONEXCLAMATION "请先关闭正在运行的 Z8 Codex，再重新安装。"
  SetErrorLevel 2
  Abort "Z8 Codex 可执行文件正在使用或无法写入"
z8_install_ready:

  ClearErrors
  File "${ROOT}\dist\windows\app\z8-codex.exe"
  IfErrors z8_install_locked
  ClearErrors
  File "${ROOT}\dist\windows\app\z8-codex-manager.exe"
  IfErrors z8_install_locked
  IfFileExists "$INSTDIR\codex-plus-plus.exe" 0 z8_install_remove_legacy_manager
  ClearErrors
  Delete "$INSTDIR\codex-plus-plus.exe"
  IfErrors z8_install_locked
z8_install_remove_legacy_manager:
  IfFileExists "$INSTDIR\codex-plus-plus-manager.exe" 0 z8_install_legacy_removed
  ClearErrors
  Delete "$INSTDIR\codex-plus-plus-manager.exe"
  IfErrors z8_install_locked
z8_install_legacy_removed:
  File "${ROOT}\LICENSE"

  CreateShortcut "$DESKTOP\Codex.lnk" "$INSTDIR\z8-codex.exe" "" "$INSTDIR\z8-codex.exe"
  Delete "$DESKTOP\Z8 Codex.lnk"
  CreateShortcut "$DESKTOP\Z8 Codex 管理工具.lnk" "$INSTDIR\z8-codex-manager.exe" "" "$INSTDIR\z8-codex-manager.exe"
  CreateDirectory "$SMPROGRAMS\Z8 Codex"
  CreateShortcut "$SMPROGRAMS\Z8 Codex\Codex.lnk" "$INSTDIR\z8-codex.exe" "" "$INSTDIR\z8-codex.exe"
  Delete "$SMPROGRAMS\Z8 Codex\Z8 Codex.lnk"
  CreateShortcut "$SMPROGRAMS\Z8 Codex\Z8 Codex 管理工具.lnk" "$INSTDIR\z8-codex-manager.exe" "" "$INSTDIR\z8-codex-manager.exe"
  CreateShortcut "$SMPROGRAMS\Z8 Codex\卸载 Z8 Codex.lnk" "$INSTDIR\uninstall.exe" "" "$INSTDIR\z8-codex-manager.exe"

  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\Z8 Codex" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Z8 Codex" "DisplayName" "Z8 Codex"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Z8 Codex" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Z8 Codex" "Publisher" "Z8"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Z8 Codex" "DisplayIcon" "$INSTDIR\z8-codex-manager.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Z8 Codex" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Z8 Codex" "UninstallString" "$INSTDIR\uninstall.exe"
SectionEnd

Section "Uninstall"
  SetShellVarContext current

  ; Check both Z8-owned executables before deleting either one. The later
  ; Delete checks still catch a process started after this preflight.
  IfFileExists "$INSTDIR\z8-codex.exe" 0 z8_uninstall_check_manager
  ClearErrors
  FileOpen $0 "$INSTDIR\z8-codex.exe" a
  IfErrors z8_uninstall_locked
  FileClose $0
z8_uninstall_check_manager:
  IfFileExists "$INSTDIR\z8-codex-manager.exe" 0 z8_uninstall_check_legacy_launcher
  ClearErrors
  FileOpen $0 "$INSTDIR\z8-codex-manager.exe" a
  IfErrors z8_uninstall_locked
  FileClose $0
z8_uninstall_check_legacy_launcher:
  IfFileExists "$INSTDIR\codex-plus-plus.exe" 0 z8_uninstall_check_legacy_manager
  ClearErrors
  FileOpen $0 "$INSTDIR\codex-plus-plus.exe" a
  IfErrors z8_uninstall_locked
  FileClose $0
z8_uninstall_check_legacy_manager:
  IfFileExists "$INSTDIR\codex-plus-plus-manager.exe" 0 z8_uninstall_remove_files
  ClearErrors
  FileOpen $0 "$INSTDIR\codex-plus-plus-manager.exe" a
  IfErrors z8_uninstall_locked
  FileClose $0
  Goto z8_uninstall_remove_files
z8_uninstall_locked:
  MessageBox MB_OK|MB_ICONEXCLAMATION "请先关闭正在运行的 Z8 Codex，再重新卸载。"
  SetErrorLevel 2
  Abort "Z8 Codex 可执行文件正在使用或无法删除"
z8_uninstall_remove_files:
  IfFileExists "$INSTDIR\z8-codex.exe" 0 z8_uninstall_remove_manager
  ClearErrors
  Delete "$INSTDIR\z8-codex.exe"
  IfErrors z8_uninstall_locked
z8_uninstall_remove_manager:
  IfFileExists "$INSTDIR\z8-codex-manager.exe" 0 z8_uninstall_remove_legacy_launcher
  ClearErrors
  Delete "$INSTDIR\z8-codex-manager.exe"
  IfErrors z8_uninstall_locked
z8_uninstall_remove_legacy_launcher:
  IfFileExists "$INSTDIR\codex-plus-plus.exe" 0 z8_uninstall_remove_legacy_manager
  ClearErrors
  Delete "$INSTDIR\codex-plus-plus.exe"
  IfErrors z8_uninstall_locked
z8_uninstall_remove_legacy_manager:
  IfFileExists "$INSTDIR\codex-plus-plus-manager.exe" 0 z8_uninstall_remove_shortcuts
  ClearErrors
  Delete "$INSTDIR\codex-plus-plus-manager.exe"
  IfErrors z8_uninstall_locked
z8_uninstall_remove_shortcuts:
  Delete "$DESKTOP\Codex.lnk"
  Delete "$DESKTOP\Z8 Codex.lnk"
  Delete "$DESKTOP\Z8 Codex 管理工具.lnk"
  Delete "$SMPROGRAMS\Z8 Codex\Codex.lnk"
  Delete "$SMPROGRAMS\Z8 Codex\Z8 Codex.lnk"
  Delete "$SMPROGRAMS\Z8 Codex\Z8 Codex 管理工具.lnk"
  Delete "$SMPROGRAMS\Z8 Codex\卸载 Z8 Codex.lnk"
  RMDir "$SMPROGRAMS\Z8 Codex"

  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Z8 Codex"
  DeleteRegKey HKCU "Software\Z8 Codex"
SectionEnd
