param(
    [switch]$DebugVariant
)
#Requires -RunAsAdministrator
$ErrorActionPreference = "Continue"

if ($DebugVariant) {
    $AppDirName = "WindInputDebug"
    $DllName = "wind_tsf_debug.dll"
    $DllNameX86 = "wind_tsf_debug_x86.dll"
    $ExeName = "wind_input_debug.exe"
    $SettingName = "wind_setting_debug.exe"
    $PortableName = "wind_portable.exe"
    $ServiceProcessName = "wind_input_debug"
    $SettingProcessName = "wind_setting_debug"
    $PortableProcessName = "wind_portable"
    $RunKeyName = "WindInputDebug"
    $ShortcutFolder = "清风输入法 Debug"
    $ShortcutName = "清风输入法 Debug 设置"
    $DisplayName = "清风输入法 (Debug)"
    $ProfileStr = "0804:{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}{99C2DEB1-5C57-45A2-9C63-FB54B34FD90A}"
} else {
    $AppDirName = "WindInput"
    $DllName = "wind_tsf.dll"
    $DllNameX86 = "wind_tsf_x86.dll"
    $ExeName = "wind_input.exe"
    $SettingName = "wind_setting.exe"
    $PortableName = "wind_portable.exe"
    $ServiceProcessName = "wind_input"
    $SettingProcessName = "wind_setting"
    $PortableProcessName = "wind_portable"
    $RunKeyName = "WindInput"
    $ShortcutFolder = "清风输入法"
    $ShortcutName = "清风输入法 设置"
    $DisplayName = "清风输入法"
    $ProfileStr = "0804:{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}"
}

$InstallDir = if ($env:ProgramW6432) { Join-Path $env:ProgramW6432 $AppDirName } else { Join-Path $env:ProgramFiles $AppDirName }

Write-Host "======================================"
Write-Host "$DisplayName 卸载程序"
Write-Host "======================================"
Write-Host ""

# [1/8] 停止服务
Write-Host "[1/8] 停止服务..."
Get-Process -Name $SettingProcessName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Get-Process -Name $PortableProcessName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Get-Process -Name $ServiceProcessName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2

# [2/8] 从系统输入法列表移除
Write-Host "[2/8] 从系统输入法列表移除..."
try {
    if (-not ([System.Management.Automation.PSTypeName]'WindInputHelper').Type) {
        Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public class WindInputHelper {
    [DllImport("input.dll", CharSet = CharSet.Unicode)]
    public static extern bool InstallLayoutOrTip(string profile, uint flags);
}
"@
    }
    [WindInputHelper]::InstallLayoutOrTip($ProfileStr, 0x00000001) | Out-Null
    Write-Host "  - 已从系统输入法列表移除"
} catch {
    Write-Host "[警告] 移除系统输入法注册失败: $_" -ForegroundColor Yellow
}

# [3/8] 注销 COM 组件
Write-Host "[3/8] 注销 COM 组件..."
# 注销 x64 DLL
$tsfDll = Join-Path $InstallDir $DllName
if (Test-Path $tsfDll) {
    & regsvr32 /u /s $tsfDll 2>$null
}
# 注销 x86 DLL（使用 32 位 regsvr32）
$regsvr32X86 = Join-Path $env:SystemRoot "SysWOW64\regsvr32.exe"
$tsfDllX86 = Join-Path $InstallDir $DllNameX86
if (Test-Path $tsfDllX86) {
    & $regsvr32X86 /u /s $tsfDllX86 2>$null
}
# 同时尝试注销旧备份 DLL
$dllPattern = if ($DebugVariant) { "wind_tsf_debug*.dll" } else { "wind_tsf*.dll" }
Get-ChildItem -Path $InstallDir -Filter $dllPattern -ErrorAction SilentlyContinue | ForEach-Object {
    & regsvr32 /u /s $_.FullName 2>$null
}

# [3.5/8] 卸载我们安装的系统字体
Write-Host "[3.5/8] 卸载系统字体..."
$TrackingKey = "HKLM:\SOFTWARE\WindInput"
$FontRegKey  = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts"
$tracked = (Get-ItemProperty -Path $TrackingKey -Name "InstalledFont_HeiTiZiGen" -ErrorAction SilentlyContinue)?.InstalledFont_HeiTiZiGen
if ($tracked -eq "1") {
    $FontFile  = "HeiTiZiGen.ttf"
    $FontDName = "黑体字根 (TrueType)"
    $FontPath  = Join-Path $env:SystemRoot "Fonts\$FontFile"
    if (Test-Path $FontPath) {
        Remove-Item -Path $FontPath -Force -ErrorAction SilentlyContinue
        Write-Host "  - 已删除字体文件: $FontFile"
    }
    Remove-ItemProperty -Path $FontRegKey -Name $FontDName -ErrorAction SilentlyContinue
    Remove-ItemProperty -Path $TrackingKey -Name "InstalledFont_HeiTiZiGen" -ErrorAction SilentlyContinue
    # 广播 WM_FONTCHANGE 通知所有应用
    try {
        $code = @"
using System;using System.Runtime.InteropServices;
public class WinFontU {
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h,uint m,IntPtr w,IntPtr l);
}
"@
        Add-Type -TypeDefinition $code -ErrorAction SilentlyContinue
        [WinFontU]::SendMessage([IntPtr]([int]0xFFFF), 0x001D, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    } catch {}
    Write-Host "  - 已从系统移除字体: $FontDName"
} else {
    Write-Host "  - 无需卸载字体（非本程序安装）"
}

# [4/8] 删除文件
Write-Host "[4/8] 删除文件..."
$RandomSuffix = Get-Random -Maximum 99999999

$filesToDelete = @($DllName, $DllNameX86, $ExeName, $SettingName, "wind_dwrite.dll")
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

# [5/8] 清理备份文件
Write-Host "[5/8] 清理备份文件..."
Get-ChildItem -Path $InstallDir -Filter "*.old_*" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
Get-ChildItem -Path $InstallDir -Filter "*.bak" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

# [6/8] 移除开机自启动
Write-Host "[6/8] 移除开机自启动..."
try {
    Remove-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name $RunKeyName -Force -ErrorAction Stop
    Write-Host "  - 已移除开机自启动注册表项"
} catch {
    Write-Host "  - 未找到开机自启动注册表项(已跳过)"
}

# [7/8] 移除目录
Write-Host "[7/8] 移除目录..."
# 删除开始菜单快捷方式（子目录版 + 兼容清理旧版散落的）
$startMenuDir = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\$ShortcutFolder"
if (Test-Path $startMenuDir) {
    Remove-Item -Path $startMenuDir -Recurse -Force -ErrorAction SilentlyContinue
}
$oldShortcut = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\$ShortcutName.lnk"
if (Test-Path $oldShortcut) {
    Remove-Item -Path $oldShortcut -Force -ErrorAction SilentlyContinue
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

# [8/8] 清理缓存目录
Write-Host "[8/8] 清理缓存目录..."
$cacheDir = Join-Path $env:LOCALAPPDATA "$AppDirName\cache"
if (Test-Path $cacheDir) {
    Remove-Item -Path $cacheDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "  - 已清理词库缓存"
}
# 清理 wind_setting WebView2 缓存数据（位于 %TEMP%\wind_setting）
$settingCacheDir = Join-Path $env:TEMP $SettingProcessName
if (Test-Path $settingCacheDir) {
    Remove-Item -Path $settingCacheDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "  - 已清理设置程序缓存"
}
$windInputLocal = Join-Path $env:LOCALAPPDATA $AppDirName
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
