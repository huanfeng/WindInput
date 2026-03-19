param(
    [string]$Version = "0.1.0-dev",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$RootDir = (Resolve-Path (Join-Path $ScriptDir "..")).Path

# 解析版本号
$VersionCore = ($Version -split '[-+]')[0]
$parts = $VersionCore -split '\.'
$V1 = if ($parts.Length -ge 1) { $parts[0] } else { "0" }
$V2 = if ($parts.Length -ge 2) { $parts[1] } else { "0" }
$V3 = if ($parts.Length -ge 3) { $parts[2] } else { "0" }
$V4 = if ($parts.Length -ge 4) { $parts[3] } else { "0" }
$VersionNum = "$V1.$V2.$V3.$V4"

Write-Host "======================================"
Write-Host "清风输入法 NSIS Installer Builder"
Write-Host "======================================"
Write-Host "Version: $Version"
Write-Host "Version(Numeric): $VersionNum"
Write-Host ""

# [1/3] 构建
if (-not $SkipBuild) {
    Write-Host "[1/3] Build release artifacts..."
    & "$RootDir\build_all.ps1" -WailsMode release
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[ERROR] build_all.ps1 failed." -ForegroundColor Red
        exit 1
    }
} else {
    Write-Host "[1/3] Skip build stage."
}

# [2/3] 检查 makensis
Write-Host "[2/3] Check makensis..."
if (-not (Get-Command makensis -ErrorAction SilentlyContinue)) {
    Write-Host "[ERROR] makensis not found in PATH." -ForegroundColor Red
    Write-Host "Please install NSIS and ensure makensis.exe is in PATH."
    exit 1
}

$installerOutput = Join-Path $RootDir "build\installer"
if (-not (Test-Path $installerOutput)) {
    New-Item -ItemType Directory -Path $installerOutput -Force | Out-Null
}

# [3/3] 生成安装包
Write-Host "[3/3] Build installer..."
Push-Location (Join-Path $RootDir "installer\nsis")
try {
    & makensis "/DAPP_VERSION=$Version" "/DAPP_VERSION_NUM=$VersionNum" WindInput.nsi
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[ERROR] NSIS build failed." -ForegroundColor Red
        exit 1
    }
} finally {
    Pop-Location
}

Write-Host ""
Write-Host "======================================"
Write-Host "Installer build completed."
Write-Host "Output: build\installer\清风输入法-${Version}-Setup.exe"
Write-Host "======================================"
exit 0
