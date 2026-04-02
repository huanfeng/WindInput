param(
    [Parameter(Position=0)]
    [string]$Choice = ""
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$ScriptPath = $MyInvocation.MyCommand.Definition

function Ensure-Admin {
    $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $isAdmin) {
        Write-Host "[INFO] 需要管理员权限，正在请求提升..."
        # 提权窗口中用 & 执行脚本，完成后 pause 防止窗口闪退
        $cmd = "& '$ScriptPath' -Choice '$Choice'; Write-Host ''; Write-Host '按任意键关闭窗口...'; `$null = `$Host.UI.RawUI.ReadKey('NoEcho,IncludeKeyDown')"
        Start-Process powershell -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -Command `"$cmd`""
        exit 0
    }
}

# ============ 正式版操作 ============

function Do-BuildRelease {
    & "$ScriptDir\build_all.ps1" -WailsMode release
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-BuildDebug {
    & "$ScriptDir\build_all.ps1" -WailsMode debug
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-Install {
    & "$ScriptDir\installer\install.ps1"
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-Uninstall {
    & "$ScriptDir\installer\uninstall.ps1"
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-BuildInstaller {
    & "$ScriptDir\installer\build_nsis.ps1"
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-BuildInstallerSkip {
    & "$ScriptDir\installer\build_nsis.ps1" -SkipBuild
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-BuildSetting {
    & "$ScriptDir\build_all.ps1" -WailsMode release -SettingOnly
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-DeploySetting {
    $InstallDir = if ($env:ProgramW6432) { Join-Path $env:ProgramW6432 "WindInput" } else { Join-Path $env:ProgramFiles "WindInput" }
    $settingExe = Join-Path $ScriptDir "build\wind_setting.exe"

    if (-not (Test-Path $settingExe)) {
        Write-Host "[错误] 未找到 build\wind_setting.exe，请先构建" -ForegroundColor Red
        exit 1
    }
    if (-not (Test-Path $InstallDir)) {
        Write-Host "[错误] 安装目录不存在: $InstallDir，请先完整安装" -ForegroundColor Red
        exit 1
    }

    # 关闭已运行的设置程序
    Get-Process -Name "wind_setting" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500

    Copy-Item -Path $settingExe -Destination $InstallDir -Force
    Write-Host "[完成] wind_setting.exe 已部署到 $InstallDir" -ForegroundColor Green
}

# ============ 调试版操作 ============

function Do-BuildReleaseDebugVariant {
    & "$ScriptDir\build_all.ps1" -WailsMode release -DebugVariant
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-BuildDebugDebugVariant {
    & "$ScriptDir\build_all.ps1" -WailsMode debug -DebugVariant
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-InstallDebugVariant {
    & "$ScriptDir\installer\install.ps1" -DebugVariant
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-UninstallDebugVariant {
    & "$ScriptDir\installer\uninstall.ps1" -DebugVariant
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-BuildSettingDebugVariant {
    & "$ScriptDir\build_all.ps1" -WailsMode release -SettingOnly -DebugVariant
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-DeploySettingDebugVariant {
    $InstallDir = if ($env:ProgramW6432) { Join-Path $env:ProgramW6432 "WindInputDebug" } else { Join-Path $env:ProgramFiles "WindInputDebug" }
    $settingExe = Join-Path $ScriptDir "build_debug\wind_setting_debug.exe"

    if (-not (Test-Path $settingExe)) {
        Write-Host "[错误] 未找到 build_debug\wind_setting_debug.exe，请先构建" -ForegroundColor Red
        exit 1
    }
    if (-not (Test-Path $InstallDir)) {
        Write-Host "[错误] 安装目录不存在: $InstallDir，请先完整安装" -ForegroundColor Red
        exit 1
    }

    # 关闭已运行的设置程序
    Get-Process -Name "wind_setting_debug" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500

    Copy-Item -Path $settingExe -Destination $InstallDir -Force
    Write-Host "[完成] wind_setting_debug.exe 已部署到 $InstallDir" -ForegroundColor Green
}

# ============ 交互式菜单 ============

if (-not $Choice) {
    Write-Host "======================================"
    Write-Host "WindInput - Dev Menu"
    Write-Host "======================================"
    Write-Host ""
    Write-Host "  --- 正式版 ---"
    Write-Host "  1.  卸载 / 构建(Release) / 安装"
    Write-Host "  2.  卸载 / 构建(Debug) / 安装"
    Write-Host "  3.  构建(Release)"
    Write-Host "  4.  构建(Debug)"
    Write-Host "  5.  安装"
    Write-Host "  6.  卸载"
    Write-Host "  7.  卸载 / 安装"
    Write-Host "  8.  生成安装包(Release)"
    Write-Host "  9.  生成安装包(跳过编译)"
    Write-Host "  0.  构建设置 / 部署设置"
    Write-Host ""
    Write-Host "  --- 调试版 ---"
    Write-Host "  d1. 卸载 / 构建(Release) / 安装"
    Write-Host "  d2. 卸载 / 构建(Debug) / 安装"
    Write-Host "  d3. 构建(Release)"
    Write-Host "  d4. 构建(Debug)"
    Write-Host "  d5. 安装"
    Write-Host "  d6. 卸载"
    Write-Host "  d7. 卸载 / 安装"
    Write-Host "  d0. 构建设置 / 部署设置"
    Write-Host ""
    $Choice = Read-Host "请选择"
    if (-not $Choice) { exit 1 }
}

switch ($Choice) {
    # 正式版
    "1"  { Ensure-Admin; Do-Uninstall; Do-BuildRelease; Do-Install }
    "2"  { Ensure-Admin; Do-Uninstall; Do-BuildDebug; Do-Install }
    "3"  { Do-BuildRelease }
    "4"  { Do-BuildDebug }
    "5"  { Ensure-Admin; Do-Install }
    "6"  { Ensure-Admin; Do-Uninstall }
    "7"  { Ensure-Admin; Do-Uninstall; Do-Install }
    "8"  { Do-BuildInstaller }
    "9"  { Do-BuildInstallerSkip }
    "0"  { Ensure-Admin; Do-BuildSetting; Do-DeploySetting }

    # 调试版
    "d1" { Ensure-Admin; Do-UninstallDebugVariant; Do-BuildReleaseDebugVariant; Do-InstallDebugVariant }
    "d2" { Ensure-Admin; Do-UninstallDebugVariant; Do-BuildDebugDebugVariant; Do-InstallDebugVariant }
    "d3" { Do-BuildReleaseDebugVariant }
    "d4" { Do-BuildDebugDebugVariant }
    "d5" { Ensure-Admin; Do-InstallDebugVariant }
    "d6" { Ensure-Admin; Do-UninstallDebugVariant }
    "d7" { Ensure-Admin; Do-UninstallDebugVariant; Do-InstallDebugVariant }
    "d0" { Ensure-Admin; Do-BuildSettingDebugVariant; Do-DeploySettingDebugVariant }

    default {
        Write-Host "[ERROR] 无效选项: $Choice" -ForegroundColor Red
        exit 1
    }
}
