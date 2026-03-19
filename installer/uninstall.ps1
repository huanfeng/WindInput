#Requires -RunAsAdministrator
$ErrorActionPreference = "Continue"

Write-Host "======================================"
Write-Host "清风输入法卸载程序"
Write-Host "======================================"
Write-Host ""

$InstallDir = if ($env:ProgramW6432) { Join-Path $env:ProgramW6432 "WindInput" } else { Join-Path $env:ProgramFiles "WindInput" }

# [1/7] 停止服务
Write-Host "[1/7] 停止服务..."
Get-Process -Name "wind_input" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2

# [2/7] 注销 COM 组件
Write-Host "[2/7] 注销 COM 组件..."
$tsfDll = Join-Path $InstallDir "wind_tsf.dll"
if (Test-Path $tsfDll) {
    & regsvr32 /u /s $tsfDll 2>$null
}
# 同时尝试注销旧备份 DLL
Get-ChildItem -Path $InstallDir -Filter "wind_tsf*.dll" -ErrorAction SilentlyContinue | ForEach-Object {
    & regsvr32 /u /s $_.FullName 2>$null
}

# [3/7] 删除文件
Write-Host "[3/7] 删除文件..."
$RandomSuffix = Get-Random -Maximum 99999999

$filesToDelete = @("wind_tsf.dll", "wind_dwrite.dll", "wind_input.exe")
foreach ($f in $filesToDelete) {
    $filePath = Join-Path $InstallDir $f
    if (-not (Test-Path $filePath)) { continue }

    try {
        Remove-Item -Path $filePath -Force -ErrorAction Stop
    } catch {
        Write-Host "[警告] 无法删除 $f,重命名以便稍后清理" -ForegroundColor Yellow
        Rename-Item -Path $filePath -NewName "${f}.old_${RandomSuffix}" -Force -ErrorAction SilentlyContinue
    }
}

# [4/7] 清理备份文件
Write-Host "[4/7] 清理备份文件..."
Get-ChildItem -Path $InstallDir -Filter "*.old_*" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
Get-ChildItem -Path $InstallDir -Filter "*.bak" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

# [5/7] 移除开机自启动
Write-Host "[5/7] 移除开机自启动..."
try {
    Remove-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "WindInput" -Force -ErrorAction Stop
    Write-Host "  - 已移除开机自启动注册表项"
} catch {
    Write-Host "  - 未找到开机自启动注册表项(已跳过)"
}

# [6/7] 移除目录
Write-Host "[6/7] 移除目录..."
# 删除开始菜单快捷方式
$shortcutPath = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\清风输入法 设置.lnk"
if (Test-Path $shortcutPath) {
    Remove-Item -Path $shortcutPath -Force -ErrorAction SilentlyContinue
}

# 删除子目录
foreach ($subDir in @("dict", "schemas", "themes")) {
    $dirPath = Join-Path $InstallDir $subDir
    if (Test-Path $dirPath) {
        Remove-Item -Path $dirPath -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# 尝试删除安装目录
Remove-Item -Path $InstallDir -Recurse -Force -ErrorAction SilentlyContinue

# [7/7] 清理缓存目录
Write-Host "[7/7] 清理缓存目录..."
$cacheDir = Join-Path $env:LOCALAPPDATA "WindInput\cache"
if (Test-Path $cacheDir) {
    Remove-Item -Path $cacheDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "  - 已清理词库缓存"
}
$windInputLocal = Join-Path $env:LOCALAPPDATA "WindInput"
if ((Test-Path $windInputLocal) -and ((Get-ChildItem $windInputLocal -ErrorAction SilentlyContinue | Measure-Object).Count -eq 0)) {
    Remove-Item -Path $windInputLocal -Force -ErrorAction SilentlyContinue
}

# 检查残留
if (Test-Path $InstallDir) {
    Write-Host ""
    Write-Host "[警告] 部分文件无法删除,已重命名。" -ForegroundColor Yellow
    Write-Host "       重启后可完成清理。"
    Write-Host ""
    Write-Host "剩余文件:"
    Get-ChildItem -Path $InstallDir -Name
}

Write-Host ""
Write-Host "======================================"
Write-Host "卸载完成！"
Write-Host "======================================"
Write-Host ""
Write-Host "注意: 如果仍有文件无法删除:"
Write-Host "1. 重启电脑"
Write-Host "2. 手动删除: $InstallDir"
Write-Host ""
Write-Host "如果输入法仍出现在列表中,"
Write-Host "请注销或重启电脑。"
exit 0
