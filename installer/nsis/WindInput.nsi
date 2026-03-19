Unicode true
RequestExecutionLevel admin
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"

!ifndef APP_VERSION
!define APP_VERSION "0.1.0"
!endif

!ifndef APP_VERSION_NUM
!define APP_VERSION_NUM "0.1.0.0"
!endif

!define APP_NAME "清风输入法"
!define APP_PUBLISHER "清风输入法 项目"
!define APP_DIRNAME "WindInput"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"
!define BUILD_DIR "..\..\build"
!define OUTPUT_DIR "..\..\build\installer"

Var RANDOM_SUFFIX

!if /FileExists "${BUILD_DIR}\wind_tsf.dll"
!else
!error "Missing file: ${BUILD_DIR}\wind_tsf.dll. Run build_all.bat first."
!endif

!if /FileExists "${BUILD_DIR}\wind_input.exe"
!else
!error "Missing file: ${BUILD_DIR}\wind_input.exe. Run build_all.bat first."
!endif

!if /FileExists "${BUILD_DIR}\wind_dwrite.dll"
!else
!error "Missing file: ${BUILD_DIR}\wind_dwrite.dll. Run build_all.bat first."
!endif

!if /FileExists "${BUILD_DIR}\wind_setting.exe"
!else
!error "Missing file: ${BUILD_DIR}\wind_setting.exe. Run build_all.bat -wails-release first."
!endif

!if /FileExists "${BUILD_DIR}\dict\pinyin\8105.dict.yaml"
!else
!error "Missing file: ${BUILD_DIR}\dict\pinyin\8105.dict.yaml. Run build_all.bat first."
!endif

Name "${APP_NAME}"
OutFile "${OUTPUT_DIR}\${APP_NAME}-${APP_VERSION}-Setup.exe"
InstallDir "$PROGRAMFILES64\${APP_DIRNAME}"
InstallDirRegKey HKLM "${UNINST_KEY}" "InstallLocation"
ShowInstDetails show
ShowUninstDetails show
SilentInstall normal
SilentUnInstall normal

VIProductVersion "${APP_VERSION_NUM}"
VIFileVersion "${APP_VERSION_NUM}"
VIAddVersionKey "ProductName" "${APP_NAME}"
VIAddVersionKey "CompanyName" "${APP_PUBLISHER}"
VIAddVersionKey "FileDescription" "${APP_NAME} Installer"
VIAddVersionKey "ProductVersion" "${APP_VERSION}"
VIAddVersionKey "FileVersion" "${APP_VERSION_NUM}"
VIAddVersionKey "LegalCopyright" "Copyright (c) WindInput Project"

!define MUI_ABORTWARNING
!define MUI_ICON "..\..\wind_tsf\res\wind_input.ico"
!define MUI_UNICON "..\..\wind_tsf\res\wind_input.ico"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_REBOOTLATER_DEFAULT
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_WELCOME
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!define MUI_FINISHPAGE_REBOOTLATER_DEFAULT
!insertmacro MUI_UNPAGE_FINISH

!insertmacro MUI_LANGUAGE "SimpChinese"

Function .onInit
  ${IfNot} ${RunningX64}
    IfSilent +2 0
    MessageBox MB_ICONSTOP|MB_OK "清风输入法仅支持 64 位 Windows 系统。"
    SetErrorLevel 2
    Abort
  ${EndIf}
FunctionEnd

; ---------- Shared helpers ----------

Function GenRandomSuffix
  System::Call "kernel32::GetTickCount()i .r5"
  IntFmt $RANDOM_SUFFIX "%u" $5
FunctionEnd

; RenameViaCmdRen: rename using "cmd /c ren" (identical to install.bat).
;   $0 = full source path, $2 = new filename only (no path, ren syntax)
;   On return: check IfFileExists "$0" to see if it worked.
;   NOTE: nsExec::ExecToLog pushes exit code onto stack — must Pop to avoid corruption.
!macro _RenameViaCmdRen
  nsExec::ExecToLog 'cmd /c ren "$0" "$2"'
  Pop $4 ; discard nsExec exit code (avoid stack corruption)
!macroend

; BackupIfLocked: move a file out of the way so a new version can take its place.
;   Push <source_path>
;   Push <backup_base_path>    (only the filename stem is used for rename target)
;   Call BackupIfLocked
; On return: error flag set if file is still at source_path.
Function BackupIfLocked
  ClearErrors
  Exch $1 ; backup base path (e.g. "$INSTDIR\wind_tsf.dll.old")
  Exch
  Exch $0 ; source path       (e.g. "$INSTDIR\wind_tsf.dll")

  ; If file doesn't exist, nothing to do
  IfFileExists "$0" 0 backup_done

  ; Attempt 1: plain delete (works if file is not loaded)
  DetailPrint "  尝试删除: $0"
  Delete "$0"
  IfFileExists "$0" 0 backup_done

  ; File is locked. Use "cmd /c ren" — same as install.bat, proven to work
  ; on loaded DLLs. Note: ren takes just filename, not full path.
  Call GenRandomSuffix

  ; Attempt 2: ren to .old_<random>
  ; Extract just the filename from $0, append .old_<suffix>
  ${GetFileName} "$0" $3
  StrCpy $2 "$3.old_$RANDOM_SUFFIX"
  DetailPrint "  尝试重命名: $3 -> $2"
  !insertmacro _RenameViaCmdRen
  IfFileExists "$0" 0 backup_done

  ; Attempt 3: sleep and retry
  Sleep 500
  Call GenRandomSuffix
  StrCpy $2 "$3.old_$RANDOM_SUFFIX"
  DetailPrint "  重试重命名: $3 -> $2"
  !insertmacro _RenameViaCmdRen
  IfFileExists "$0" 0 backup_done

  ; Attempt 4: alternate extension
  StrCpy $2 "$3_$RANDOM_SUFFIX.bak"
  DetailPrint "  尝试重命名: $3 -> $2"
  !insertmacro _RenameViaCmdRen
  IfFileExists "$0" 0 backup_done

  ; All attempts failed
  DetailPrint "  错误: 无法删除或重命名 $3"
  SetErrors

backup_done:
  Pop $0
  Pop $1
FunctionEnd

Function un.GenRandomSuffix
  System::Call "kernel32::GetTickCount()i .r5"
  IntFmt $RANDOM_SUFFIX "%u" $5
FunctionEnd

Function un.BackupIfLocked
  ClearErrors
  Exch $1
  Exch
  Exch $0

  IfFileExists "$0" 0 un_backup_done

  DetailPrint "  尝试删除: $0"
  Delete "$0"
  IfFileExists "$0" 0 un_backup_done

  Call un.GenRandomSuffix
  ${GetFileName} "$0" $3
  StrCpy $2 "$3.old_$RANDOM_SUFFIX"
  DetailPrint "  尝试重命名: $3 -> $2"
  !insertmacro _RenameViaCmdRen
  IfFileExists "$0" 0 un_backup_done

  Sleep 500
  Call un.GenRandomSuffix
  StrCpy $2 "$3.old_$RANDOM_SUFFIX"
  DetailPrint "  重试重命名: $3 -> $2"
  !insertmacro _RenameViaCmdRen
  IfFileExists "$0" 0 un_backup_done

  StrCpy $2 "$3_$RANDOM_SUFFIX.bak"
  DetailPrint "  尝试重命名: $3 -> $2"
  !insertmacro _RenameViaCmdRen
  IfFileExists "$0" 0 un_backup_done

  DetailPrint "  错误: 无法删除或重命名 $3"
  SetErrors

un_backup_done:
  Pop $0
  Pop $1
FunctionEnd

Section "Install"
  SetShellVarContext all
  SetOutPath "$INSTDIR"

  ; --- Step 1: Stop processes ---
  DetailPrint "正在停止旧进程..."
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_input.exe >nul 2>&1'
  Pop $0 ; discard nsExec exit code
  Sleep 1000

  ; --- Step 2: Unregister old DLL ---
  IfFileExists "$INSTDIR\wind_tsf.dll" install_has_old_dll install_unreg_done
install_has_old_dll:
  ExecWait '"$SYSDIR\regsvr32.exe" /u /s "$INSTDIR\wind_tsf.dll"'
install_unreg_done:

  ; --- Step 3: Extract new binaries to staging dir (once, to avoid double-embed) ---
  DetailPrint "正在解压新文件..."
  InitPluginsDir
  SetOutPath "$PLUGINSDIR\stage"
  ClearErrors
  File "${BUILD_DIR}\wind_tsf.dll"
  File "${BUILD_DIR}\wind_dwrite.dll"
  File "${BUILD_DIR}\wind_input.exe"
  File "${BUILD_DIR}\wind_setting.exe"
  IfErrors 0 install_stage_ok
    IfSilent +2 0
    MessageBox MB_ICONSTOP|MB_OK "解压文件失败。"
    SetErrorLevel 2
    Abort
install_stage_ok:
  SetOutPath "$INSTDIR"

  ; --- Step 4: Replace each binary ---
  ; Strategy: rename old file to .old_<random> (MoveFileW, works on loaded DLLs),
  ;           then copy new file to the ORIGINAL name.
  ;           Old renamed files are cleaned up on reboot.
  ;           This guarantees the original filename always points to the NEW version.

  ; -- wind_tsf.dll --
  DetailPrint "正在安装 wind_tsf.dll..."
  Push "$INSTDIR\wind_tsf.dll"
  Push "$INSTDIR\wind_tsf.dll.old"
  Call BackupIfLocked
  ClearErrors
  CopyFiles /SILENT "$PLUGINSDIR\stage\wind_tsf.dll" "$INSTDIR\wind_tsf.dll"
  IfErrors 0 install_dll_done
    IfSilent +2 0
    MessageBox MB_ICONSTOP|MB_OK "安装 wind_tsf.dll 失败。"
    SetErrorLevel 3
    Abort
install_dll_done:

  ; -- wind_dwrite.dll --
  DetailPrint "正在安装 wind_dwrite.dll..."
  Push "$INSTDIR\wind_dwrite.dll"
  Push "$INSTDIR\wind_dwrite.dll.old"
  Call BackupIfLocked
  ClearErrors
  CopyFiles /SILENT "$PLUGINSDIR\stage\wind_dwrite.dll" "$INSTDIR\wind_dwrite.dll"
  IfErrors 0 install_dwrite_done
    IfSilent +2 0
    MessageBox MB_ICONSTOP|MB_OK "安装 wind_dwrite.dll 失败。"
    SetErrorLevel 3
    Abort
install_dwrite_done:

  ; -- wind_input.exe --
  DetailPrint "正在安装 wind_input.exe..."
  Push "$INSTDIR\wind_input.exe"
  Push "$INSTDIR\wind_input.exe.old"
  Call BackupIfLocked
  ClearErrors
  CopyFiles /SILENT "$PLUGINSDIR\stage\wind_input.exe" "$INSTDIR\wind_input.exe"
  IfErrors 0 install_input_done
    IfSilent +2 0
    MessageBox MB_ICONSTOP|MB_OK "安装 wind_input.exe 失败。"
    SetErrorLevel 3
    Abort
install_input_done:

  ; -- wind_setting.exe --
  DetailPrint "正在安装 wind_setting.exe..."
  Push "$INSTDIR\wind_setting.exe"
  Push "$INSTDIR\wind_setting.exe.old"
  Call BackupIfLocked
  ClearErrors
  CopyFiles /SILENT "$PLUGINSDIR\stage\wind_setting.exe" "$INSTDIR\wind_setting.exe"
  IfErrors 0 install_setting_done
    IfSilent +2 0
    MessageBox MB_ICONSTOP|MB_OK "安装 wind_setting.exe 失败。"
    SetErrorLevel 3
    Abort
install_setting_done:

  ; --- Step 5: Cleanup staging + old backup files ---
  DetailPrint "正在清理旧文件..."
  Delete "$PLUGINSDIR\stage\wind_tsf.dll"
  Delete "$PLUGINSDIR\stage\wind_dwrite.dll"
  Delete "$PLUGINSDIR\stage\wind_input.exe"
  Delete "$PLUGINSDIR\stage\wind_setting.exe"
  RMDir "$PLUGINSDIR\stage"
  ; Schedule reboot deletion for any .old_* / .bak files that can't be deleted now
  FindFirst $0 $1 "$INSTDIR\*.old_*"
install_cleanup_old_loop:
  StrCmp $1 "" install_cleanup_old_end
    Delete "$INSTDIR\$1"
    IfFileExists "$INSTDIR\$1" 0 install_cleanup_old_next
      Delete /REBOOTOK "$INSTDIR\$1"
      SetRebootFlag true
install_cleanup_old_next:
    FindNext $0 $1
    Goto install_cleanup_old_loop
install_cleanup_old_end:
  FindClose $0
  FindFirst $0 $1 "$INSTDIR\*.bak"
install_cleanup_bak_loop:
  StrCmp $1 "" install_cleanup_bak_end
    Delete "$INSTDIR\$1"
    IfFileExists "$INSTDIR\$1" 0 install_cleanup_bak_next
      Delete /REBOOTOK "$INSTDIR\$1"
      SetRebootFlag true
install_cleanup_bak_next:
    FindNext $0 $1
    Goto install_cleanup_bak_loop
install_cleanup_bak_end:
  FindClose $0

  ; --- Step 6: Dictionary files ---
  DetailPrint "正在复制词库文件..."
  SetOutPath "$INSTDIR\dict"
  File "${BUILD_DIR}\dict\common_chars.txt"
  SetOutPath "$INSTDIR\dict\pinyin"
  File "${BUILD_DIR}\dict\pinyin\8105.dict.yaml"
  File "${BUILD_DIR}\dict\pinyin\base.dict.yaml"
  File "${BUILD_DIR}\dict\pinyin\unigram.txt"
  SetOutPath "$INSTDIR\dict\wubi86"
  File "${BUILD_DIR}\dict\wubi86\wubi86.txt"

  ; --- Step 6b: Schema files (input method configurations) ---
  DetailPrint "正在复制输入方案配置..."
  SetOutPath "$INSTDIR\schemas"
  File "${BUILD_DIR}\schemas\wubi86.schema.yaml"
  File "${BUILD_DIR}\schemas\pinyin.schema.yaml"

  ; --- Step 6c: Theme files ---
  DetailPrint "正在复制主题文件..."
  SetOutPath "$INSTDIR\themes\default"
  File "${BUILD_DIR}\themes\default\theme.yaml"
  SetOutPath "$INSTDIR\themes\dark"
  File "${BUILD_DIR}\themes\dark\theme.yaml"
  SetOutPath "$INSTDIR\themes\msime"
  File "${BUILD_DIR}\themes\msime\theme.yaml"
  SetOutPath "$INSTDIR"

  ; --- Step 7: Register NEW DLL (always at original path, guaranteed new version) ---
  DetailPrint "正在注册 COM 组件..."
  ExecWait '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\wind_tsf.dll"' $0
  ${If} $0 != 0
    DetailPrint "警告: COM 注册失败 (错误码 $0)，将在重启后重试。"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\RunOnce" "WindInput_RegisterOnReboot" '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\wind_tsf.dll"'
    SetRebootFlag true
  ${EndIf}

  ; --- Step 8: Auto-start on login (registry Run key) ---
  DetailPrint "正在配置开机自启动..."
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "WindInput" '"$INSTDIR\wind_input.exe"'

  ; --- Step 9: Pre-start service (background, so dictionary can be pre-loaded) ---
  DetailPrint "正在预启动输入法服务..."
  Exec '"$INSTDIR\wind_input.exe"'

  DetailPrint "正在创建快捷方式..."
  CreateDirectory "$SMPROGRAMS\清风输入法"
  CreateShortcut "$SMPROGRAMS\清风输入法\清风输入法 设置.lnk" "$INSTDIR\wind_setting.exe" "" "$INSTDIR\wind_setting.exe" 0
  CreateShortcut "$SMPROGRAMS\清风输入法\卸载 清风输入法.lnk" "$INSTDIR\uninstall.exe"

  DetailPrint "正在写入卸载信息..."
  WriteUninstaller "$INSTDIR\uninstall.exe"

  WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "${APP_PUBLISHER}"
  WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\wind_setting.exe"
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1

  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKLM "${UNINST_KEY}" "EstimatedSize" "$0"

  IfRebootFlag 0 install_done
    IfSilent install_done 0
    MessageBox MB_ICONEXCLAMATION|MB_OK "部分文件正在使用中，已安排重启后清理旧文件。"
install_done:
SectionEnd

Section "Uninstall"
  SetShellVarContext all

  ; --- Step 1: Stop processes ---
  DetailPrint "正在停止进程..."
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_input.exe >nul 2>&1'
  Pop $0 ; discard nsExec exit code
  Sleep 1000

  ; --- Step 2: Unregister DLL ---
  IfFileExists "$INSTDIR\wind_tsf.dll" uninstall_has_dll uninstall_unreg_done
uninstall_has_dll:
  DetailPrint "正在注销 COM 组件..."
  ExecWait '"$SYSDIR\regsvr32.exe" /u /s "$INSTDIR\wind_tsf.dll"'
uninstall_unreg_done:

  ; --- Step 3: Remove binaries (rename if locked, schedule reboot cleanup) ---
  DetailPrint "正在删除已安装文件..."
  Push "$INSTDIR\wind_tsf.dll"
  Push "$INSTDIR\wind_tsf.dll.old"
  Call un.BackupIfLocked
  IfErrors 0 uninst_dll_done
    Delete /REBOOTOK "$INSTDIR\wind_tsf.dll"
    SetRebootFlag true
uninst_dll_done:
  Push "$INSTDIR\wind_dwrite.dll"
  Push "$INSTDIR\wind_dwrite.dll.old"
  Call un.BackupIfLocked
  IfErrors 0 uninst_dwrite_done
    Delete /REBOOTOK "$INSTDIR\wind_dwrite.dll"
    SetRebootFlag true
uninst_dwrite_done:
  Push "$INSTDIR\wind_input.exe"
  Push "$INSTDIR\wind_input.exe.old"
  Call un.BackupIfLocked
  IfErrors 0 uninst_input_done
    Delete /REBOOTOK "$INSTDIR\wind_input.exe"
    SetRebootFlag true
uninst_input_done:
  Push "$INSTDIR\wind_setting.exe"
  Push "$INSTDIR\wind_setting.exe.old"
  Call un.BackupIfLocked
  IfErrors 0 uninst_setting_done
    Delete /REBOOTOK "$INSTDIR\wind_setting.exe"
    SetRebootFlag true
uninst_setting_done:

  ; --- Step 4: Remove remaining files and directories ---
  Delete /REBOOTOK "$INSTDIR\uninstall.exe"
  RMDir /r /REBOOTOK "$INSTDIR\dict"
  RMDir /r /REBOOTOK "$INSTDIR\schemas"
  RMDir /r /REBOOTOK "$INSTDIR\themes"
  ; Cleanup .old_* and .bak files
  FindFirst $0 $1 "$INSTDIR\*.old_*"
uninst_cleanup_old_loop:
  StrCmp $1 "" uninst_cleanup_old_end
    Delete /REBOOTOK "$INSTDIR\$1"
  FindNext $0 $1
  Goto uninst_cleanup_old_loop
uninst_cleanup_old_end:
  FindClose $0
  FindFirst $0 $1 "$INSTDIR\*.bak"
uninst_cleanup_bak_loop:
  StrCmp $1 "" uninst_cleanup_bak_end
    Delete /REBOOTOK "$INSTDIR\$1"
  FindNext $0 $1
  Goto uninst_cleanup_bak_loop
uninst_cleanup_bak_end:
  FindClose $0
  RMDir /r /REBOOTOK "$INSTDIR"

  ; --- Step 5: Shortcuts and cache ---
  DetailPrint "正在删除快捷方式..."
  Delete "$SMPROGRAMS\清风输入法\清风输入法 设置.lnk"
  Delete "$SMPROGRAMS\清风输入法\卸载 清风输入法.lnk"
  RMDir "$SMPROGRAMS\清风输入法"

  DetailPrint "正在清理缓存..."
  RMDir /r /REBOOTOK "$LOCALAPPDATA\WindInput\cache"

  ; --- Step 6: Registry ---
  ; Remove auto-start entry
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "WindInput"
  DeleteRegKey HKLM "${UNINST_KEY}"
  IfRebootFlag 0 uninst_done
    IfSilent uninst_done 0
    MessageBox MB_ICONEXCLAMATION|MB_OK "部分文件正在使用中，将在重启后完成清理。"
uninst_done:
SectionEnd
