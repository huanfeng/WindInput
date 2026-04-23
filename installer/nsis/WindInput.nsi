Unicode true
RequestExecutionLevel admin
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"
!include "nsDialogs.nsh"

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
Var CleanRoaming
Var CleanLocal
Var BackupToDesktop
Var hCleanRoaming
Var hCleanLocal
Var hBackupToDesktop
Var InstallMode
Var hStandard
Var hPortable

!if /FileExists "${BUILD_DIR}\wind_tsf.dll"
!else
!error "Missing file: ${BUILD_DIR}\wind_tsf.dll. Run build_all.ps1 first."
!endif

!if /FileExists "${BUILD_DIR}\wind_tsf_x86.dll"
!else
!error "Missing file: ${BUILD_DIR}\wind_tsf_x86.dll. Run build_all.ps1 first."
!endif

!if /FileExists "${BUILD_DIR}\wind_input.exe"
!else
!error "Missing file: ${BUILD_DIR}\wind_input.exe. Run build_all.ps1 first."
!endif

!if /FileExists "${BUILD_DIR}\wind_setting.exe"
!else
!error "Missing file: ${BUILD_DIR}\wind_setting.exe. Run build_all.ps1 -WailsMode release first."
!endif

!if /FileExists "${BUILD_DIR}\wind_portable.exe"
!else
!error "Missing file: ${BUILD_DIR}\wind_portable.exe. Run build_all.ps1 first."
!endif

!if /FileExists "${BUILD_DIR}\data\schemas\pinyin\cn_dicts\8105.dict.yaml"
!else
!error "Missing file: ${BUILD_DIR}\data\schemas\pinyin\cn_dicts\8105.dict.yaml. Run build_all.ps1 first."
!endif

Name "${APP_NAME} ${APP_VERSION}"
OutFile "${OUTPUT_DIR}\WindInput-${APP_VERSION}-Setup.exe"
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

; --- 安装欢迎页 ---
!define MUI_WELCOMEPAGE_TITLE "欢迎安装 ${APP_NAME} ${APP_VERSION}"
!define MUI_WELCOMEPAGE_TEXT "安装向导将引导您完成 ${APP_NAME} ${APP_VERSION} 的安装。$\r$\n$\r$\n建议在安装前关闭所有正在运行的应用程序，以便安装程序更新相关文件。$\r$\n$\r$\n点击「下一步」继续。"

; --- 安装完成页 ---
!define MUI_FINISHPAGE_TITLE "${APP_NAME} ${APP_VERSION} 安装完成"
!define MUI_FINISHPAGE_TEXT "${APP_NAME} ${APP_VERSION} 已成功安装到您的计算机。$\r$\n$\r$\n点击「完成」关闭安装向导。"

!insertmacro MUI_PAGE_WELCOME
Page custom InstallModePageCreate InstallModePageLeave
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_REBOOTLATER_DEFAULT
!insertmacro MUI_PAGE_FINISH

; --- 卸载欢迎页 ---
!define MUI_WELCOMEPAGE_TITLE "卸载 ${APP_NAME} ${APP_VERSION}"
!define MUI_WELCOMEPAGE_TEXT "此向导将引导您卸载 ${APP_NAME} ${APP_VERSION}。$\r$\n$\r$\n卸载前请确保 ${APP_NAME} 未在运行中。$\r$\n$\r$\n点击「下一步」继续。"

!insertmacro MUI_UNPAGE_WELCOME
!insertmacro MUI_UNPAGE_CONFIRM
UninstPage custom un.UserDataPageCreate un.UserDataPageLeave
!insertmacro MUI_UNPAGE_INSTFILES
; --- 卸载完成页 ---
!define MUI_FINISHPAGE_TITLE "${APP_NAME} ${APP_VERSION} 卸载完成"
!define MUI_FINISHPAGE_TEXT "${APP_NAME} ${APP_VERSION} 已从您的计算机中移除。$\r$\n$\r$\n点击「完成」关闭卸载向导。"
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

Function InstallModePageCreate
  StrCpy $InstallMode "standard"

  !insertmacro MUI_HEADER_TEXT "选择安装类型" "请选择安装方式"

  nsDialogs::Create 1018
  Pop $0

  ${NSD_CreateLabel} 0 0 100% 24u "请选择您希望的安装方式："
  Pop $0

  ${NSD_CreateRadioButton} 12u 30u 100% 12u "标准安装（推荐）—— 注册输入法到系统，开机自动启动"
  Pop $hStandard
  ${NSD_SetState} $hStandard ${BST_CHECKED}

  ${NSD_CreateLabel} 24u 44u 100% 12u "安装到 Program Files，注册为系统输入法，适合日常使用"
  Pop $0
  SetCtlColors $0 808080 transparent

  ${NSD_CreateRadioButton} 12u 66u 100% 12u "便携模式 —— 仅解压文件到指定目录，不修改系统"
  Pop $hPortable

  ${NSD_CreateLabel} 24u 80u 100% 24u "适合 U 盘携带或临时使用，需通过便携启动器手动启动"
  Pop $0
  SetCtlColors $0 808080 transparent

  nsDialogs::Show
FunctionEnd

Function InstallModePageLeave
  ${NSD_GetState} $hPortable $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $InstallMode "portable"
    StrCpy $INSTDIR "$DESKTOP\WindInput_Portable"
  ${Else}
    StrCpy $InstallMode "standard"
    StrCpy $INSTDIR "$PROGRAMFILES64\${APP_DIRNAME}"
  ${EndIf}
FunctionEnd

Function un.onInit
  StrCpy $CleanRoaming ${BST_UNCHECKED}
  StrCpy $CleanLocal ${BST_CHECKED}
  StrCpy $BackupToDesktop ${BST_CHECKED}
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

; ---------- Uninstall: user data cleanup page ----------

Function un.UserDataPageCreate
  SetShellVarContext current

  ; If neither Roaming nor Local user data exists, skip this page
  IfFileExists "$APPDATA\${APP_DIRNAME}\*.*" un_userdata_show 0
  IfFileExists "$LOCALAPPDATA\${APP_DIRNAME}\*.*" un_userdata_show 0
  Abort
un_userdata_show:

  !insertmacro MUI_HEADER_TEXT "清理用户数据" "选择是否清除用户配置和缓存数据"

  nsDialogs::Create 1018
  Pop $0

  ${NSD_CreateLabel} 0 0 100% 24u "卸载程序检测到以下用户数据，请选择是否清除："
  Pop $0

  ; Checkbox: clean Roaming data (user config, state, phrases)
  ${NSD_CreateCheckbox} 0 30u 100% 12u "清除用户配置数据（用户配置、输入状态、自定义短语）"
  Pop $hCleanRoaming
  ${NSD_SetState} $hCleanRoaming ${BST_UNCHECKED}
  ${NSD_OnClick} $hCleanRoaming un.OnCleanRoamingClick

  ${NSD_CreateLabel} 12u 44u 100% 12u "$APPDATA\${APP_DIRNAME}"
  Pop $0
  SetCtlColors $0 808080 transparent

  ; Checkbox: clean Local data (dict cache)
  ${NSD_CreateCheckbox} 0 62u 100% 12u "清除本地缓存数据（词库缓存，可安全删除）"
  Pop $hCleanLocal
  ${NSD_SetState} $hCleanLocal ${BST_CHECKED}

  ${NSD_CreateLabel} 12u 76u 100% 12u "$LOCALAPPDATA\${APP_DIRNAME}"
  Pop $0
  SetCtlColors $0 808080 transparent

  ; Checkbox: backup Roaming to desktop before deletion
  ${NSD_CreateCheckbox} 0 94u 100% 12u "备份配置数据到桌面（推荐）"
  Pop $hBackupToDesktop
  ${NSD_SetState} $hBackupToDesktop ${BST_CHECKED}
  EnableWindow $hBackupToDesktop 0 ; disabled until "clean Roaming" is checked

  ${NSD_CreateLabel} 0 116u 100% 24u "注意：自定义短语等数据删除后无法恢复，建议勾选备份选项。"
  Pop $0
  SetCtlColors $0 CC6600 transparent

  nsDialogs::Show
FunctionEnd

Function un.OnCleanRoamingClick
  ${NSD_GetState} $hCleanRoaming $0
  ${If} $0 == ${BST_CHECKED}
    EnableWindow $hBackupToDesktop 1
  ${Else}
    EnableWindow $hBackupToDesktop 0
  ${EndIf}
FunctionEnd

Function un.UserDataPageLeave
  ${NSD_GetState} $hCleanRoaming $CleanRoaming
  ${NSD_GetState} $hCleanLocal $CleanLocal
  ${NSD_GetState} $hBackupToDesktop $BackupToDesktop
FunctionEnd

Section "Install"
  SetShellVarContext all
  SetOutPath "$INSTDIR"

  ; --- Step 1: Stop processes ---
  DetailPrint "正在停止旧进程..."
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_setting.exe >nul 2>&1'
  Pop $0 ; discard nsExec exit code
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_portable.exe >nul 2>&1'
  Pop $0 ; discard nsExec exit code
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_input.exe >nul 2>&1'
  Pop $0 ; discard nsExec exit code
  Sleep 1000

  ; --- Step 2: Unregister old DLLs ---
  IfFileExists "$INSTDIR\wind_tsf.dll" install_has_old_dll install_unreg_x64_done
install_has_old_dll:
  ExecWait '"$SYSDIR\regsvr32.exe" /u /s "$INSTDIR\wind_tsf.dll"'
install_unreg_x64_done:
  ; Unregister old x86 DLL using 32-bit regsvr32
  IfFileExists "$INSTDIR\wind_tsf_x86.dll" install_has_old_x86_dll install_unreg_done
install_has_old_x86_dll:
  ExecWait '"$WINDIR\SysWOW64\regsvr32.exe" /u /s "$INSTDIR\wind_tsf_x86.dll"'
install_unreg_done:

  ; --- Step 3: Extract new binaries to staging dir (once, to avoid double-embed) ---
  DetailPrint "正在解压新文件..."
  InitPluginsDir
  SetOutPath "$PLUGINSDIR\stage"
  ClearErrors
  File "${BUILD_DIR}\wind_tsf.dll"
  File "${BUILD_DIR}\wind_tsf_x86.dll"
  File "${BUILD_DIR}\wind_input.exe"
  File "${BUILD_DIR}\wind_setting.exe"
  File "${BUILD_DIR}\wind_portable.exe"
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

  ; -- wind_tsf_x86.dll --
  DetailPrint "正在安装 wind_tsf_x86.dll..."
  Push "$INSTDIR\wind_tsf_x86.dll"
  Push "$INSTDIR\wind_tsf_x86.dll.old"
  Call BackupIfLocked
  ClearErrors
  CopyFiles /SILENT "$PLUGINSDIR\stage\wind_tsf_x86.dll" "$INSTDIR\wind_tsf_x86.dll"
  IfErrors 0 install_x86_dll_done
    IfSilent +2 0
    MessageBox MB_ICONSTOP|MB_OK "安装 wind_tsf_x86.dll 失败。"
    SetErrorLevel 3
    Abort
install_x86_dll_done:

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

  ; -- wind_portable.exe --
  DetailPrint "正在安装 wind_portable.exe..."
  Push "$INSTDIR\wind_portable.exe"
  Push "$INSTDIR\wind_portable.exe.old"
  Call BackupIfLocked
  ClearErrors
  CopyFiles /SILENT "$PLUGINSDIR\stage\wind_portable.exe" "$INSTDIR\wind_portable.exe"
  IfErrors 0 install_portable_done
    IfSilent +2 0
    MessageBox MB_ICONSTOP|MB_OK "安装 wind_portable.exe 失败。"
    SetErrorLevel 3
    Abort
install_portable_done:

  ; --- Step 4b: Grant read/execute to ALL APPLICATION PACKAGES for TSF DLLs ---
  DetailPrint "正在设置现代宿主 DLL 权限..."
  nsExec::ExecToLog 'cmd /c icacls "$INSTDIR\wind_tsf.dll" /grant *S-1-15-2-1:^(RX^) /c'
  Pop $0
  nsExec::ExecToLog 'cmd /c icacls "$INSTDIR\wind_tsf_x86.dll" /grant *S-1-15-2-1:^(RX^) /c'
  Pop $0

  ; --- Step 5: Cleanup staging + old backup files ---
  DetailPrint "正在清理旧文件..."
  Delete "$PLUGINSDIR\stage\wind_tsf.dll"
  Delete "$PLUGINSDIR\stage\wind_tsf_x86.dll"
  Delete "$PLUGINSDIR\stage\wind_input.exe"
  Delete "$PLUGINSDIR\stage\wind_setting.exe"
  Delete "$PLUGINSDIR\stage\wind_portable.exe"
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

  ; --- Step 6: Data files (dict, schema, config, theme, patches) ---
  ; 递归复制 build/data/ 下所有文件，新增数据文件无需修改此脚本
  ; build/data/ 由 build_all.ps1 的 Prepare-DataFiles 准备（已排除 AGENTS.md 等开发文件）
  DetailPrint "正在复制数据文件..."
  SetOutPath "$INSTDIR\data"
  File /r "${BUILD_DIR}\data\*.*"
  SetOutPath "$INSTDIR"

  ; --- Portable mode: skip registration, create marker, launch ---
  StrCmp $InstallMode "portable" 0 install_standard_mode

  DetailPrint "正在配置便携模式..."
  FileOpen $0 "$INSTDIR\wind_portable_mode" w
  FileWrite $0 "wind_portable=1$\n"
  FileClose $0

  DetailPrint "便携模式部署完成"
  Exec '"$INSTDIR\wind_portable.exe"'
  Goto install_done

install_standard_mode:

  ; --- Step 7: Register NEW DLLs (always at original path, guaranteed new version) ---
  DetailPrint "正在注册 COM 组件..."
  ; Register x64 DLL (64-bit regsvr32)
  ExecWait '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\wind_tsf.dll"' $0
  ${If} $0 != 0
    DetailPrint "警告: COM x64 注册失败 (错误码 $0)，将在重启后重试。"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\RunOnce" "WindInput_RegisterOnReboot" '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\wind_tsf.dll"'
    SetRebootFlag true
  ${EndIf}
  ; Register x86 DLL (32-bit regsvr32, writes to WOW6432Node for 32-bit apps)
  ExecWait '"$WINDIR\SysWOW64\regsvr32.exe" /s "$INSTDIR\wind_tsf_x86.dll"' $0
  ${If} $0 != 0
    DetailPrint "警告: COM x86 注册失败 (错误码 $0)，32 位应用可能无法使用输入法。"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\RunOnce" "WindInput_RegisterX86OnReboot" '"$WINDIR\SysWOW64\regsvr32.exe" /s "$INSTDIR\wind_tsf_x86.dll"'
    SetRebootFlag true
  ${EndIf}

  ; --- Step 8: Register input method via InstallLayoutOrTip ---
  DetailPrint "正在注册系统输入法..."
  System::Call 'input::InstallLayoutOrTip(w "0804:{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}", i 0) i .r0'
  ${If} $0 == 0
    DetailPrint "警告: InstallLayoutOrTip 调用失败，输入法可能需要手动添加"
  ${EndIf}

  ; --- Step 9: Auto-start on login (registry Run key) ---
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
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_setting.exe >nul 2>&1'
  Pop $0 ; discard nsExec exit code
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_portable.exe >nul 2>&1'
  Pop $0 ; discard nsExec exit code
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_input.exe >nul 2>&1'
  Pop $0 ; discard nsExec exit code
  Sleep 1000

  ; --- Step 2: Unregister input method and DLL ---
  DetailPrint "正在从系统输入法列表移除..."
  System::Call 'input::InstallLayoutOrTip(w "0804:{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}", i 0x00000001) i .r0'

  IfFileExists "$INSTDIR\wind_tsf.dll" uninstall_has_dll uninstall_unreg_x64_done
uninstall_has_dll:
  DetailPrint "正在注销 COM x64 组件..."
  ExecWait '"$SYSDIR\regsvr32.exe" /u /s "$INSTDIR\wind_tsf.dll"'
uninstall_unreg_x64_done:
  ; Unregister x86 DLL using 32-bit regsvr32
  IfFileExists "$INSTDIR\wind_tsf_x86.dll" uninstall_has_x86_dll uninstall_unreg_done
uninstall_has_x86_dll:
  DetailPrint "正在注销 COM x86 组件..."
  ExecWait '"$WINDIR\SysWOW64\regsvr32.exe" /u /s "$INSTDIR\wind_tsf_x86.dll"'
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
  ; Remove x86 DLL
  Push "$INSTDIR\wind_tsf_x86.dll"
  Push "$INSTDIR\wind_tsf_x86.dll.old"
  Call un.BackupIfLocked
  IfErrors 0 uninst_x86_dll_done
    Delete /REBOOTOK "$INSTDIR\wind_tsf_x86.dll"
    SetRebootFlag true
uninst_x86_dll_done:
  ; Clean up legacy wind_dwrite.dll if present from older versions
  Delete "$INSTDIR\wind_dwrite.dll"
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
  Push "$INSTDIR\wind_portable.exe"
  Push "$INSTDIR\wind_portable.exe.old"
  Call un.BackupIfLocked
  IfErrors 0 uninst_portable_done
    Delete /REBOOTOK "$INSTDIR\wind_portable.exe"
    SetRebootFlag true
uninst_portable_done:

  ; --- Step 4: Remove remaining files and directories ---
  Delete /REBOOTOK "$INSTDIR\uninstall.exe"
  RMDir /r /REBOOTOK "$INSTDIR\data"
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

  ; --- Step 6: Clean user data ---
  SetShellVarContext current

  ${If} $CleanRoaming == ${BST_CHECKED}
    ${If} $BackupToDesktop == ${BST_CHECKED}
      DetailPrint "正在备份用户数据到桌面..."
      CreateDirectory "$DESKTOP\${APP_DIRNAME}_Backup"
      CopyFiles /SILENT "$APPDATA\${APP_DIRNAME}\*.*" "$DESKTOP\${APP_DIRNAME}_Backup"
    ${EndIf}
    DetailPrint "正在清除用户配置数据..."
    RMDir /r "$APPDATA\${APP_DIRNAME}"
  ${EndIf}

  ${If} $CleanLocal == ${BST_CHECKED}
    DetailPrint "正在清除本地缓存数据..."
    RMDir /r "$LOCALAPPDATA\${APP_DIRNAME}"
  ${Else}
    DetailPrint "正在清理缓存..."
    RMDir /r "$LOCALAPPDATA\${APP_DIRNAME}\cache"
  ${EndIf}

  ; 清理旧 wind_setting WebView2 缓存数据（位于 %APPDATA%\wind_setting.exe）
  DetailPrint "正在清理设置程序缓存..."
  RMDir /r "$APPDATA\wind_setting.exe"

  ; 清理 wind_setting WebView2 缓存数据（位于 %TEMP%\wind_setting）
  DetailPrint "正在清理设置程序缓存..."
  RMDir /r "$TEMP\wind_setting"

  SetShellVarContext all

  ; --- Step 7: Registry ---
  ; Remove auto-start entry
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "WindInput"
  DeleteRegKey HKLM "${UNINST_KEY}"
  IfRebootFlag 0 uninst_done
    IfSilent uninst_done 0
    MessageBox MB_ICONEXCLAMATION|MB_OK "部分文件正在使用中，将在重启后完成清理。"
uninst_done:
SectionEnd
