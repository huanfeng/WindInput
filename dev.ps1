param(
    [Parameter(Position=0)]
    [ValidateSet("1","2","3","4","5","6","7","8","9","")]
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

# 交互式菜单
if (-not $Choice) {
    Write-Host "======================================"
    Write-Host "WindInput - Dev Menu"
    Write-Host "======================================"
    Write-Host "1. 卸载 / 构建(Release) / 安装"
    Write-Host "2. 卸载 / 构建(Debug) / 安装"
    Write-Host "3. 构建(Release)"
    Write-Host "4. 构建(Debug)"
    Write-Host "5. 安装"
    Write-Host "6. 卸载"
    Write-Host "7. 卸载 / 安装"
    Write-Host "8. 生成安装包(Release)"
    Write-Host "9. 生成安装包(跳过编译)"
    Write-Host ""
    $Choice = Read-Host "请选择 (1-9)"
    if (-not $Choice) { exit 1 }
}

switch ($Choice) {
    "1" { Ensure-Admin; Do-Uninstall; Do-BuildRelease; Do-Install }
    "2" { Ensure-Admin; Do-Uninstall; Do-BuildDebug; Do-Install }
    "3" { Do-BuildRelease }
    "4" { Do-BuildDebug }
    "5" { Ensure-Admin; Do-Install }
    "6" { Ensure-Admin; Do-Uninstall }
    "7" { Ensure-Admin; Do-Uninstall; Do-Install }
    "8" { Do-BuildInstaller }
    "9" { Do-BuildInstallerSkip }
    default {
        Write-Host "[ERROR] 无效选项: $Choice" -ForegroundColor Red
        exit 1
    }
}
