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
Var IsUpgrade
Var KeepUserData
Var UserDataDir
Var CustomDataDir
Var UseCustomDataDir
Var hDataDirDefault
Var hDataDirCustom
Var hDataDirPath
Var hDataDirBrowse
Var SavedStandardDir
Var OldUninstallString
Var DoUninstallOld
Var QuietMode

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

!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipPageIfQuiet
!insertmacro MUI_PAGE_WELCOME
Page custom InstallModePageCreate InstallModePageLeave
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipPageIfQuiet
!insertmacro MUI_PAGE_DIRECTORY
!undef MUI_PAGE_CUSTOMFUNCTION_PRE
Page custom DataDirPageCreate DataDirPageLeave
!insertmacro MUI_PAGE_INSTFILES
; 完成页：便携模式提供启动器运行选项（标准模式通过 FinishPage_Show 隐藏此选项）
; 安静模式通过 FinishPage_Pre 跳过此页，安装完成后自动关闭
!define MUI_FINISHPAGE_RUN "placeholder"
!define MUI_FINISHPAGE_RUN_TEXT "启动便携启动器"
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchPortableStarter
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipPageIfQuiet
!define MUI_PAGE_CUSTOMFUNCTION_SHOW FinishPage_Show
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

  ; 初始化变量
  StrCpy $InstallMode "standard"
  StrCpy $IsUpgrade "0"
  StrCpy $UseCustomDataDir "0"
  StrCpy $CustomDataDir ""
  StrCpy $OldUninstallString ""
  StrCpy $SavedStandardDir ""
  StrCpy $DoUninstallOld "0"
  StrCpy $QuietMode "0"

  ; 解析 /QUIET 参数（显示进度窗口但跳过所有向导页面，用于应用内自动更新）
  ${GetParameters} $0
  ${GetOptions} $0 "/QUIET" $1
  IfErrors +2 0
    StrCpy $QuietMode "1"

  ; 读取已有的 datadir.conf（可能是上次卸载时保留的）
  SetShellVarContext current
  IfFileExists "$LOCALAPPDATA\${APP_DIRNAME}\datadir.conf" 0 init_no_datadir
    FileOpen $0 "$LOCALAPPDATA\${APP_DIRNAME}\datadir.conf" r
    FileRead $0 $1
    FileClose $0
    ; 去掉可能的换行符
    StrCpy $2 $1 1 -1
    StrCmp $2 "$\n" 0 +2
      StrCpy $1 $1 -1
    StrCpy $2 $1 1 -1
    StrCmp $2 "$\r" 0 +2
      StrCpy $1 $1 -1
    StrCmp $1 "" init_no_datadir
      StrCpy $CustomDataDir $1
      StrCpy $UseCustomDataDir "1"
  init_no_datadir:
  SetShellVarContext all

  ; 检测已安装版本
  ReadRegStr $0 HKLM "${UNINST_KEY}" "UninstallString"
  StrCmp $0 "" init_no_prev_install

  StrCpy $IsUpgrade "1"

  ; 提取卸载程序路径（去掉引号）
  StrCpy $1 $0 1
  StrCmp $1 '"' 0 +3
    StrCpy $0 $0 "" 1
    StrCpy $0 $0 -1

  ; 确认卸载程序存在，并保存路径（卸载将在确认安装类型后执行）
  IfFileExists "$0" 0 init_no_prev_install
  StrCpy $OldUninstallString "$0"

  ; 读取旧安装目录作为默认值
  ReadRegStr $INSTDIR HKLM "${UNINST_KEY}" "InstallLocation"

init_no_prev_install:
  ; 保存标准模式默认安装目录（供切换安装类型时恢复使用）
  StrCpy $SavedStandardDir $INSTDIR
FunctionEnd

; 完成页：运行便携启动器（仅便携模式执行，标准模式不应到达此处）
Function LaunchPortableStarter
  StrCmp $InstallMode "portable" 0 +2
    Exec '"$INSTDIR\wind_portable.exe"'
FunctionEnd


; 安静模式（/QUIET）：跳过当前向导页，用于应用内自动更新场景
Function SkipPageIfQuiet
  StrCmp $QuietMode "1" 0 +2
    Abort
FunctionEnd

; 完成页：标准模式隐藏并取消勾选"启动便携启动器"复选框
; MUI2 在完成页 Leave 时读取控件状态（不论是否可见），必须取消勾选才能阻止调用 LaunchPortableStarter
Function FinishPage_Show
  StrCmp $InstallMode "portable" finish_show_done
  ShowWindow $mui.FinishPage.Run 0
  SendMessage $mui.FinishPage.Run 0x00F1 0 0  ; BM_SETCHECK, BST_UNCHECKED
finish_show_done:
FunctionEnd

; OnClick 处理器：用户点击单选按钮时立即更新状态变量，不依赖 Leave 时读取
Function OnStandardRadioClicked
  StrCpy $InstallMode "standard"
  StrCpy $INSTDIR $SavedStandardDir
  ${If} $OldUninstallString != ""
    StrCpy $DoUninstallOld "1"
  ${Else}
    StrCpy $DoUninstallOld "0"
  ${EndIf}
FunctionEnd

Function OnPortableRadioClicked
  StrCpy $InstallMode "portable"
  StrCpy $INSTDIR "$DESKTOP\WindInput_Portable"
  StrCpy $DoUninstallOld "0"
FunctionEnd

Function InstallModePageCreate
  ; 初始化默认选项（标准安装）的状态，确保用户不点击单选按钮直接 Next 时状态也正确
  StrCpy $InstallMode "standard"
  StrCpy $INSTDIR $SavedStandardDir
  ${If} $OldUninstallString != ""
    StrCpy $DoUninstallOld "1"
  ${Else}
    StrCpy $DoUninstallOld "0"
  ${EndIf}

  ; 安静模式：默认值已设置，跳过此页
  StrCmp $QuietMode "1" 0 +2
    Abort

  !insertmacro MUI_HEADER_TEXT "选择安装类型" "请选择安装方式"

  nsDialogs::Create 1018
  Pop $0

  ${NSD_CreateLabel} 0 0 100% 24u "请选择您希望的安装方式："
  Pop $0

  ${NSD_CreateRadioButton} 12u 30u 100% 12u "标准安装（推荐）—— 注册输入法到系统，开机自动启动"
  Pop $hStandard
  ${NSD_SetState} $hStandard ${BST_CHECKED}
  ${NSD_OnClick} $hStandard OnStandardRadioClicked

  ${NSD_CreateLabel} 24u 44u 100% 12u "安装到 Program Files，注册为系统输入法，适合日常使用"
  Pop $0
  SetCtlColors $0 808080 transparent

  ${NSD_CreateRadioButton} 12u 66u 100% 12u "便携模式 —— 仅解压文件到指定目录，不修改系统"
  Pop $hPortable
  ${NSD_OnClick} $hPortable OnPortableRadioClicked

  ${NSD_CreateLabel} 24u 80u 100% 24u "适合 U 盘携带或临时使用，需通过便携启动器手动启动"
  Pop $0
  SetCtlColors $0 808080 transparent

  nsDialogs::Show
FunctionEnd

Function InstallModePageLeave
  ; $InstallMode / $INSTDIR / $DoUninstallOld 已由 OnClick 处理器实时更新，此处无需再读取状态
FunctionEnd

; ---------- Data Directory Page (first install only) ----------

Function DataDirPageCreate
  ; 安静模式 / 升级安装 / 便携模式：跳过此页
  StrCmp $QuietMode "1" 0 +2
    Abort
  StrCmp $IsUpgrade "1" 0 +2
    Abort
  StrCmp $InstallMode "portable" 0 +2
    Abort

  !insertmacro MUI_HEADER_TEXT "数据存储目录" "选择数据文件的存放位置"

  nsDialogs::Create 1018
  Pop $0

  ${NSD_CreateLabel} 0 0 100% 24u "请选择输入法数据文件（用户词库、配置等）的存储位置："
  Pop $0

  ${NSD_CreateRadioButton} 12u 28u 100% 12u "默认位置（推荐）—— %APPDATA%\${APP_DIRNAME}"
  Pop $hDataDirDefault
  ${NSD_OnClick} $hDataDirDefault OnDataDirRadioChange

  ${NSD_CreateRadioButton} 12u 50u 100% 12u "自定义位置"
  Pop $hDataDirCustom
  ${NSD_OnClick} $hDataDirCustom OnDataDirRadioChange

  ${NSD_CreateText} 24u 68u -80u 14u ""
  Pop $hDataDirPath

  ${NSD_CreateButton} -52u 68u 40u 14u "浏览..."
  Pop $hDataDirBrowse
  ${NSD_OnClick} $hDataDirBrowse OnBrowseDataDir

  ; 如果已有自定义数据目录配置，自动选中并填入
  StrCmp $UseCustomDataDir "1" 0 datadir_use_default
    ${NSD_SetState} $hDataDirCustom ${BST_CHECKED}
    ${NSD_SetText} $hDataDirPath "$CustomDataDir"
    EnableWindow $hDataDirPath 1
    EnableWindow $hDataDirBrowse 1
    Goto datadir_radio_done
  datadir_use_default:
    ${NSD_SetState} $hDataDirDefault ${BST_CHECKED}
    EnableWindow $hDataDirPath 0
    EnableWindow $hDataDirBrowse 0
  datadir_radio_done:

  ${NSD_CreateLabel} 24u 88u 100% 24u "建议选择一个专用目录来存放输入法数据文件。"
  Pop $0
  SetCtlColors $0 808080 transparent

  nsDialogs::Show
FunctionEnd

Function OnDataDirRadioChange
  ${NSD_GetState} $hDataDirCustom $0
  ${If} $0 == ${BST_CHECKED}
    EnableWindow $hDataDirPath 1
    EnableWindow $hDataDirBrowse 1
  ${Else}
    EnableWindow $hDataDirPath 0
    EnableWindow $hDataDirBrowse 0
  ${EndIf}
FunctionEnd

Function OnBrowseDataDir
  nsDialogs::SelectFolderDialog "选择数据存储目录" ""
  Pop $0
  ${If} $0 != error
    StrCpy $CustomDataDir "$0"
    ${NSD_SetText} $hDataDirPath "$CustomDataDir"
  ${EndIf}
FunctionEnd

Function DataDirPageLeave
  ${NSD_GetState} $hDataDirCustom $0
  ${If} $0 != ${BST_CHECKED}
    StrCpy $UseCustomDataDir "0"
    Return
  ${EndIf}

  StrCpy $UseCustomDataDir "1"
  ${NSD_GetText} $hDataDirPath $CustomDataDir

  ; 验证：路径不能为空
  StrCmp $CustomDataDir "" 0 validate_notempty
    MessageBox MB_OK|MB_ICONEXCLAMATION "请输入数据目录路径"
    Abort
  validate_notempty:

  ; 验证：必须是绝对路径（含 :\）
  StrCpy $1 $CustomDataDir 1 1
  StrCmp $1 ":" validate_abs_ok
    MessageBox MB_OK|MB_ICONEXCLAMATION "必须是绝对路径（如 D:\WindData）"
    Abort
  validate_abs_ok:

  ; 验证：不能是安装目录
  StrCmp $CustomDataDir $INSTDIR 0 validate_not_instdir
    MessageBox MB_OK|MB_ICONEXCLAMATION "不能使用应用安装目录作为数据目录"
    Abort
  validate_not_instdir:

  ; 验证：不能是安装目录的 data 子目录
  StrCmp $CustomDataDir "$INSTDIR\data" 0 validate_not_instdata
    MessageBox MB_OK|MB_ICONEXCLAMATION "不能使用应用安装目录的 data 目录"
    Abort
  validate_not_instdata:

  ; 验证：不能是 Windows 系统目录
  StrLen $1 $WINDIR
  StrCpy $2 $CustomDataDir $1
  StrCmp $2 $WINDIR 0 validate_not_windir
    MessageBox MB_OK|MB_ICONEXCLAMATION "不能使用 Windows 系统目录"
    Abort
  validate_not_windir:

  ; 验证：不能在 Program Files 下
  StrLen $1 $PROGRAMFILES64
  StrCpy $2 $CustomDataDir $1
  StrCmp $2 $PROGRAMFILES64 0 validate_not_pf64
    MessageBox MB_OK|MB_ICONEXCLAMATION "不能使用系统程序目录"
    Abort
  validate_not_pf64:
  StrLen $1 $PROGRAMFILES32
  StrCpy $2 $CustomDataDir $1
  StrCmp $2 $PROGRAMFILES32 0 validate_not_pf32
    MessageBox MB_OK|MB_ICONEXCLAMATION "不能使用系统程序目录"
    Abort
  validate_not_pf32:

  ; 验证：写入权限（尝试创建目录）
  ClearErrors
  CreateDirectory "$CustomDataDir"
  IfErrors 0 validate_writable_ok
    MessageBox MB_OK|MB_ICONEXCLAMATION "无法创建目录，请检查路径是否正确以及是否有写入权限"
    Abort
  validate_writable_ok:

  ; 验证：目录非空时二次确认
  FindFirst $0 $1 "$CustomDataDir\*.*"
validate_empty_loop:
  StrCmp $1 "" validate_is_empty
  StrCmp $1 "." validate_empty_next
  StrCmp $1 ".." validate_empty_next
  ; 发现文件或子目录
  FindClose $0
  MessageBox MB_YESNO|MB_ICONQUESTION \
    "目录 $CustomDataDir 已有文件，同名文件将被跳过。$\r$\n确定使用此目录？" \
    IDYES validate_nonempty_ok
  Abort
validate_empty_next:
  FindNext $0 $1
  Goto validate_empty_loop
validate_is_empty:
  FindClose $0
validate_nonempty_ok:
FunctionEnd

; ---------- Uninstaller ----------

Function un.onInit
  StrCpy $CleanRoaming ${BST_UNCHECKED}
  StrCpy $CleanLocal ${BST_CHECKED}
  StrCpy $BackupToDesktop ${BST_CHECKED}
  StrCpy $KeepUserData "0"

  ; 解析 /KEEP_USER_DATA 参数
  ${GetParameters} $0
  ${GetOptions} $0 "/KEEP_USER_DATA" $1
  IfErrors +2 0
    StrCpy $KeepUserData "1"

  ; 读取 datadir.conf 确定实际用户数据目录
  SetShellVarContext current
  StrCpy $UserDataDir "$APPDATA\${APP_DIRNAME}"
  IfFileExists "$LOCALAPPDATA\${APP_DIRNAME}\datadir.conf" 0 un_init_datadir_done
    FileOpen $0 "$LOCALAPPDATA\${APP_DIRNAME}\datadir.conf" r
    FileRead $0 $1
    FileClose $0
    ; 去掉可能的换行符
    StrCpy $2 $1 1 -1
    StrCmp $2 "$\n" 0 +2
      StrCpy $1 $1 -1
    StrCpy $2 $1 1 -1
    StrCmp $2 "$\r" 0 +2
      StrCpy $1 $1 -1
    StrCmp $1 "" un_init_datadir_done
      StrCpy $UserDataDir $1
  un_init_datadir_done:
  SetShellVarContext all
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
  ; 静默+保留数据模式下跳过此页
  StrCmp $KeepUserData "1" 0 +2
    Abort

  SetShellVarContext current

  ; If neither user data dir nor Local data exists, skip this page
  IfFileExists "$UserDataDir\*.*" un_userdata_show 0
  IfFileExists "$LOCALAPPDATA\${APP_DIRNAME}\*.*" un_userdata_show 0
  Abort
un_userdata_show:

  !insertmacro MUI_HEADER_TEXT "清理用户数据" "选择是否清除用户配置和缓存数据"

  nsDialogs::Create 1018
  Pop $0

  ${NSD_CreateLabel} 0 0 100% 24u "卸载程序检测到以下用户数据，请选择是否清除："
  Pop $0

  ; Checkbox: clean user config data (user config, state, phrases)
  ${NSD_CreateCheckbox} 0 30u 100% 12u "清除用户配置数据（用户配置、输入状态、自定义短语）"
  Pop $hCleanRoaming
  ${NSD_SetState} $hCleanRoaming ${BST_UNCHECKED}
  ${NSD_OnClick} $hCleanRoaming un.OnCleanRoamingClick

  ${NSD_CreateLabel} 12u 44u 100% 12u "$UserDataDir"
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
  ; 安静模式自动关闭（完成页已跳过）；正常模式让用户查看日志后手动点击 Next
  ${If} $QuietMode == "1"
    SetAutoClose true
  ${Else}
    SetAutoClose false
  ${EndIf}

  ; --- 仅标准安装升级时静默卸载旧版（便携模式不卸载已有系统安装）---
  ; DoUninstallOld 由 InstallModePageLeave 在用户确认安装类型时设置
  StrCmp $DoUninstallOld "1" 0 install_skip_old_uninstall

install_silent_uninstall_retry:
  ExecWait '"$OldUninstallString" /S /KEEP_USER_DATA' $1
  ${If} $1 != 0
    MessageBox MB_ABORTRETRYIGNORE|MB_ICONEXCLAMATION \
      "旧版本卸载失败（错误码：$1）。$\r$\n$\r$\n中止 = 取消安装$\r$\n重试 = 重新卸载$\r$\n忽略 = 跳过继续安装" \
      IDRETRY install_silent_uninstall_retry \
      IDIGNORE install_skip_old_uninstall
    SetErrorLevel 1
    Abort
  ${EndIf}

install_skip_old_uninstall:
  SetOutPath "$INSTDIR"

  ; --- Step 1: Stop processes ---
  StrCmp $InstallMode "portable" install_stop_portable_only
  ; 标准模式：停止所有相关进程
  DetailPrint "正在停止旧进程..."
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_setting.exe >nul 2>&1'
  Pop $0
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_portable.exe >nul 2>&1'
  Pop $0
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_input.exe >nul 2>&1'
  Pop $0
  Sleep 1000
  Goto install_stop_procs_done

install_stop_portable_only:
  ; 便携模式：只停旧便携进程，不触碰系统安装的服务进程
  DetailPrint "正在停止旧便携进程..."
  nsExec::ExecToLog 'cmd /c taskkill /F /IM wind_portable.exe >nul 2>&1'
  Pop $0
  Sleep 500

install_stop_procs_done:

  ; --- Step 2: Unregister old DLLs (standard mode only) ---
  ; 便携模式跳过：不能清除系统安装的 COM 注册信息，否则已有标准版输入法将无法切换
  StrCmp $InstallMode "portable" install_unreg_done
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

  ; --- Step 6.5: Install PUA font to system (for DirectWrite fallback) ---
  DetailPrint "正在安装字体到系统..."
  CopyFiles /SILENT "$INSTDIR\data\schemas\wubi86\HeiTiZiGen.ttf" "$WINDIR\Fonts\HeiTiZiGen.ttf"
  WriteRegStr HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts" "黑体字根 (TrueType)" "HeiTiZiGen.ttf"
  WriteRegStr HKLM "SOFTWARE\WindInput" "InstalledFont_HeiTiZiGen" "1"
  System::Call 'user32::SendMessage(i 65535, i 29, i 0, i 0)'

  ; --- Portable mode: skip registration, create marker, launch ---
  StrCmp $InstallMode "portable" 0 install_standard_mode

  DetailPrint "正在配置便携模式..."
  FileOpen $0 "$INSTDIR\wind_portable_mode" w
  FileWrite $0 "wind_portable=1$\n"
  FileClose $0

  DetailPrint "便携模式部署完成，可手动运行 wind_portable.exe 启动"
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

  ; --- 写入自定义数据目录配置 ---
  StrCmp $UseCustomDataDir "1" 0 skip_write_datadir_conf
    SetShellVarContext current
    CreateDirectory "$LOCALAPPDATA\${APP_DIRNAME}"
    FileOpen $0 "$LOCALAPPDATA\${APP_DIRNAME}\datadir.conf" w
    FileWrite $0 "$CustomDataDir"
    FileClose $0
    CreateDirectory "$CustomDataDir"
    SetShellVarContext all
  skip_write_datadir_conf:

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

  ; --- Step 2.5: Remove system font if we installed it ---
  DetailPrint "正在卸载系统字体..."
  ReadRegStr $0 HKLM "SOFTWARE\WindInput" "InstalledFont_HeiTiZiGen"
  StrCmp $0 "1" 0 uninst_font_skip
    Delete "$WINDIR\Fonts\HeiTiZiGen.ttf"
    DeleteRegValue HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts" "黑体字根 (TrueType)"
    DeleteRegValue HKLM "SOFTWARE\WindInput" "InstalledFont_HeiTiZiGen"
    System::Call 'user32::SendMessage(i 65535, i 29, i 0, i 0)'
  uninst_font_skip:

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

  ; 如果是静默+保留数据模式，跳过所有用户数据清理
  StrCmp $KeepUserData "1" uninst_skip_userdata

  ${If} $CleanRoaming == ${BST_CHECKED}
    ${If} $BackupToDesktop == ${BST_CHECKED}
      DetailPrint "正在备份用户数据到桌面..."
      CreateDirectory "$DESKTOP\${APP_DIRNAME}_Backup"
      CopyFiles /SILENT "$UserDataDir\*.*" "$DESKTOP\${APP_DIRNAME}_Backup"
    ${EndIf}
    DetailPrint "正在清除用户配置数据..."
    RMDir /r "$UserDataDir"
    ; 完全清除时也删除 datadir.conf
    Delete "$LOCALAPPDATA\${APP_DIRNAME}\datadir.conf"
  ${EndIf}

  ${If} $CleanLocal == ${BST_CHECKED}
    DetailPrint "正在清除本地缓存数据..."
    ; 仅清理缓存和日志子目录，保留 datadir.conf（除非已在上面删除）
    RMDir /r "$LOCALAPPDATA\${APP_DIRNAME}\cache"
    RMDir /r "$LOCALAPPDATA\${APP_DIRNAME}\logs"
    ; 尝试删除目录（如果 datadir.conf 已被删除则目录可移除，否则保留）
    RMDir "$LOCALAPPDATA\${APP_DIRNAME}"
  ${Else}
    DetailPrint "正在清理缓存..."
    RMDir /r "$LOCALAPPDATA\${APP_DIRNAME}\cache"
  ${EndIf}

uninst_skip_userdata:

  ; WebView2 缓存始终清理（即使 KEEP_USER_DATA 也会清理）
  DetailPrint "正在清理设置程序缓存..."
  RMDir /r "$APPDATA\wind_setting.exe"
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
