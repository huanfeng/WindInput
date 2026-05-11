param(
    [ValidateSet("all", "dll", "service", "setting", "portable")]
    [string[]]$Module = @("all"),

    [switch]$DebugVariant
)
#Requires -RunAsAdministrator
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition

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
    $BuildDir = Join-Path (Split-Path -Parent $ScriptDir) "build_debug"
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
    $BuildDir = Join-Path (Split-Path -Parent $ScriptDir) "build"
}

$InstallDir = if ($env:ProgramW6432) { Join-Path $env:ProgramW6432 $AppDirName } else { Join-Path $env:ProgramFiles $AppDirName }

# 确定部署模式
$DeployAll = $Module -contains "all"
$DeployDll = $DeployAll -or ($Module -contains "dll")
$DeployService = $DeployAll -or ($Module -contains "service")
$DeploySetting = $DeployAll -or ($Module -contains "setting")
$DeployPortable = $DeployAll -or ($Module -contains "portable")

$RandomSuffix = Get-Random -Maximum 99999999

# 处理旧文件的辅助函数
function Remove-OldFile {
    param([string]$FilePath, [string]$FileName, [switch]$UnregisterCOM)

    if (-not (Test-Path $FilePath)) { return }

    if ($UnregisterCOM) {
        & regsvr32 /u /s $FilePath 2>$null
    }

    try {
        Remove-Item -Path $FilePath -Force -ErrorAction Stop
    } catch {
        $oldName = "${FileName}.old_${RandomSuffix}"
        Write-Host "[WARN] Failed to delete old $FileName, renaming to $oldName" -ForegroundColor Yellow
        try {
            Rename-Item -Path $FilePath -NewName $oldName -Force -ErrorAction Stop
        } catch {
            $bakName = "$($FileName -replace '\.[^.]+$', '')_${RandomSuffix}$([System.IO.Path]::GetExtension($FileName)).bak"
            Write-Host "[WARN] Failed to rename old $FileName, trying backup name..." -ForegroundColor Yellow
            Rename-Item -Path $FilePath -NewName $bakName -Force -ErrorAction SilentlyContinue
        }
    }
}

# ============================================================
# 模块部署模式
# ============================================================

if (-not $DeployAll) {
    # 检查安装目录是否存在
    if (-not (Test-Path $InstallDir)) {
        Write-Host "[错误] 安装目录不存在: $InstallDir" -ForegroundColor Red
        Write-Host "请先进行完整安装（dev.ps1 选项 1 或 5）" -ForegroundColor Yellow
        exit 1
    }

    $moduleNames = @()
    if ($DeployDll) { $moduleNames += "TSF DLL" }
    if ($DeployService) { $moduleNames += "GO 服务" }
    if ($DeploySetting) { $moduleNames += "设置" }
    if ($DeployPortable) { $moduleNames += "便携启动器" }

    Write-Host "======================================"
    Write-Host "$DisplayName 模块部署"
    Write-Host "======================================"
    Write-Host ""
    Write-Host "部署模块: $($moduleNames -join ', ')"
    Write-Host "安装目录: $InstallDir"
    Write-Host ""

    # --- 部署 TSF DLL ---
    if ($DeployDll) {
        Write-Host "=== 部署 TSF DLL ==="

        # 检查构建产物
        foreach ($f in @($DllName, $DllNameX86)) {
            if (-not (Test-Path (Join-Path $BuildDir $f))) {
                Write-Host "[错误] 未找到 $f，请先构建" -ForegroundColor Red
                exit 1
            }
        }

        # 反注册旧 COM (x64)
        $tsfDll = Join-Path $InstallDir $DllName
        if (Test-Path $tsfDll) {
            Write-Host "  - 反注册 x64 COM..."
            & regsvr32 /u /s $tsfDll 2>$null
        }

        # 反注册旧 COM (x86)
        $regsvr32X86 = Join-Path $env:SystemRoot "SysWOW64\regsvr32.exe"
        $tsfDllX86 = Join-Path $InstallDir $DllNameX86
        if (Test-Path $tsfDllX86) {
            Write-Host "  - 反注册 x86 COM..."
            & $regsvr32X86 /u /s $tsfDllX86 2>$null
        }

        # 删除旧文件
        Remove-OldFile -FilePath (Join-Path $InstallDir $DllName) -FileName $DllName
        Remove-OldFile -FilePath (Join-Path $InstallDir $DllNameX86) -FileName $DllNameX86

        # 复制新文件
        Write-Host "  - 复制新 DLL..."
        Copy-Item -Path (Join-Path $BuildDir $DllName) -Destination $InstallDir -Force
        Copy-Item -Path (Join-Path $BuildDir $DllNameX86) -Destination $InstallDir -Force

        # 设置权限
        $appPackagesSid = "*S-1-15-2-1"
        & icacls (Join-Path $InstallDir $DllName) /grant "${appPackagesSid}:(RX)" /c | Out-Null
        & icacls (Join-Path $InstallDir $DllNameX86) /grant "${appPackagesSid}:(RX)" /c | Out-Null

        # 注册新 COM
        Write-Host "  - 注册 x64 COM..."
        & regsvr32 /s (Join-Path $InstallDir $DllName)
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[错误] COM x64 注册失败" -ForegroundColor Red
            exit 1
        }
        Write-Host "  - 注册 x86 COM..."
        & $regsvr32X86 /s (Join-Path $InstallDir $DllNameX86)
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[警告] COM x86 注册失败，32 位应用可能无法使用输入法" -ForegroundColor Yellow
        }

        Write-Host "[完成] TSF DLL 部署成功" -ForegroundColor Green
        Write-Host ""
    }

    # --- 部署 GO 服务 ---
    if ($DeployService) {
        Write-Host "=== 部署 GO 服务 ==="

        # 检查构建产物
        if (-not (Test-Path (Join-Path $BuildDir $ExeName))) {
            Write-Host "[错误] 未找到 $ExeName，请先构建" -ForegroundColor Red
            exit 1
        }

        # 停止旧进程
        Write-Host "  - 停止旧服务..."
        Get-Process -Name $ServiceProcessName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500

        # 删除旧文件
        Remove-OldFile -FilePath (Join-Path $InstallDir $ExeName) -FileName $ExeName

        # 复制新文件
        Write-Host "  - 复制新文件..."
        Copy-Item -Path (Join-Path $BuildDir $ExeName) -Destination $InstallDir -Force

        # 启动新服务
        Write-Host "  - 启动新服务..."
        if ($DebugVariant) { $env:GODEBUG = "gctrace=1" }
        Start-Process -FilePath (Join-Path $InstallDir $ExeName)
        if ($DebugVariant) { Remove-Item Env:\GODEBUG -ErrorAction SilentlyContinue }

        Write-Host "[完成] GO 服务部署成功" -ForegroundColor Green
        Write-Host ""
    }

    # --- 部署设置 ---
    if ($DeploySetting) {
        Write-Host "=== 部署设置 ==="

        # 检查构建产物
        $settingExe = Join-Path $BuildDir $SettingName
        if (-not (Test-Path $settingExe)) {
            Write-Host "[错误] 未找到 $SettingName，请先构建" -ForegroundColor Red
            exit 1
        }

        # 停止旧进程
        Write-Host "  - 停止旧设置程序..."
        Get-Process -Name $SettingProcessName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500

        # 删除旧文件
        Remove-OldFile -FilePath (Join-Path $InstallDir $SettingName) -FileName $SettingName

        # 复制新文件
        Write-Host "  - 复制新文件..."
        Copy-Item -Path $settingExe -Destination $InstallDir -Force

        Write-Host "[完成] 设置部署成功" -ForegroundColor Green
        Write-Host ""
    }

    # --- 部署便携启动器 ---
    if ($DeployPortable) {
        Write-Host "=== 部署便携启动器 ==="

        # 检查构建产物
        $portableExe = Join-Path $BuildDir $PortableName
        if (-not (Test-Path $portableExe)) {
            Write-Host "[错误] 未找到 $PortableName，请先构建" -ForegroundColor Red
            exit 1
        }

        # 停止旧进程
        Write-Host "  - 停止旧便携启动器..."
        Get-Process -Name $PortableProcessName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500

        # 删除旧文件
        Remove-OldFile -FilePath (Join-Path $InstallDir $PortableName) -FileName $PortableName

        # 复制新文件
        Write-Host "  - 复制新文件..."
        Copy-Item -Path $portableExe -Destination $InstallDir -Force

        Write-Host "[完成] 便携启动器部署成功" -ForegroundColor Green
        Write-Host ""
    }

    # 清理备份文件
    Get-ChildItem -Path $InstallDir -Filter "*.old_*" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
    Get-ChildItem -Path $InstallDir -Filter "*.bak" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

    Write-Host "======================================"
    Write-Host "模块部署完成！"
    Write-Host "======================================"
    exit 0
}

# ============================================================
# 完整安装模式（原有逻辑）
# ============================================================

Write-Host "======================================"
Write-Host "$DisplayName 安装程序"
Write-Host "======================================"
Write-Host ""

# [1/10] 检查文件
Write-Host "[1/10] 检查文件..."
$requiredFiles = @($DllName, $DllNameX86, $ExeName)
foreach ($f in $requiredFiles) {
    if (-not (Test-Path (Join-Path $BuildDir $f))) {
        Write-Host "[错误] 未找到 $f" -ForegroundColor Red
        Write-Host "请先运行 build_all.ps1"
        exit 1
    }
}

# [2/10] 停止旧进程
Write-Host "[2/10] 停止旧进程..."
Get-Process -Name $SettingProcessName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Get-Process -Name $PortableProcessName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Get-Process -Name $ServiceProcessName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

# [3/10] 创建安装目录 + 处理已有文件
Write-Host "[3/10] 创建安装目录..."
Write-Host "[4/10] 处理已有文件..."
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

Remove-OldFile -FilePath (Join-Path $InstallDir $DllName) -FileName $DllName -UnregisterCOM
# 注销 x86 DLL 需要使用 32 位 regsvr32
$x86DllPath = Join-Path $InstallDir $DllNameX86
if (Test-Path $x86DllPath) {
    $regsvr32X86 = Join-Path $env:SystemRoot "SysWOW64\regsvr32.exe"
    & $regsvr32X86 /u /s $x86DllPath 2>$null
}
Remove-OldFile -FilePath $x86DllPath -FileName $DllNameX86
Remove-OldFile -FilePath (Join-Path $InstallDir "wind_dwrite.dll") -FileName "wind_dwrite.dll"  # 清理旧版本遗留
Remove-OldFile -FilePath (Join-Path $InstallDir $ExeName) -FileName $ExeName
Remove-OldFile -FilePath (Join-Path $InstallDir $SettingName) -FileName $SettingName
Remove-OldFile -FilePath (Join-Path $InstallDir $PortableName) -FileName $PortableName

# [5/10] 复制文件
Write-Host "[5/10] 复制文件..."
foreach ($f in $requiredFiles) {
    Copy-Item -Path (Join-Path $BuildDir $f) -Destination $InstallDir -Force
}

$settingExe = Join-Path $BuildDir $SettingName
if (Test-Path $settingExe) {
    Copy-Item -Path $settingExe -Destination $InstallDir -Force
    Write-Host "  - $SettingName 已复制"
} else {
    Write-Host "[提示] 未找到 $SettingName,已跳过(可选)" -ForegroundColor Cyan
}

$portableExe = Join-Path $BuildDir $PortableName
if (Test-Path $portableExe) {
    Copy-Item -Path $portableExe -Destination $InstallDir -Force
    Write-Host "  - $PortableName 已复制"
} else {
    Write-Host "[提示] 未找到 $PortableName,已跳过(可选)" -ForegroundColor Cyan
}

# 为现代宿主（开始菜单 / 搜索等 AppContainer 进程）授予 TSF DLL 读取执行权限
Write-Host "  - 正在设置 TSF DLL 权限..."
$appPackagesSid = "*S-1-15-2-1"
$tsfDllPath = Join-Path $InstallDir $DllName
if (Test-Path $tsfDllPath) {
    & icacls $tsfDllPath /grant "${appPackagesSid}:(RX)" /c | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[警告] 设置 $DllName 的 ALL APPLICATION PACKAGES 权限失败" -ForegroundColor Yellow
    } else {
        Write-Host "    * $DllName 已授予 ALL APPLICATION PACKAGES 读取执行权限"
    }
}
$tsfDllPathX86 = Join-Path $InstallDir $DllNameX86
if (Test-Path $tsfDllPathX86) {
    & icacls $tsfDllPathX86 /grant "${appPackagesSid}:(RX)" /c | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[警告] 设置 $DllNameX86 的 ALL APPLICATION PACKAGES 权限失败" -ForegroundColor Yellow
    } else {
        Write-Host "    * $DllNameX86 已授予 ALL APPLICATION PACKAGES 读取执行权限"
    }
}

# [6/10] 复制数据目录（词库、方案、配置、主题、补丁等）
# 递归复制 build/data/ 下所有文件，新增数据文件无需修改此脚本
Write-Host "[6/10] 复制数据目录(data/)..."
$BuildDataDir = Join-Path $BuildDir "data"
$InstallDataDir = Join-Path $InstallDir "data"

if (Test-Path $BuildDataDir) {
    $dataCopied = 0
    Get-ChildItem -Path $BuildDataDir -Recurse -File | ForEach-Object {
        $relativePath = $_.FullName.Substring($BuildDataDir.Length + 1)
        $destPath = Join-Path $InstallDataDir $relativePath
        $destDir = Split-Path $destPath -Parent
        if (-not (Test-Path $destDir)) { New-Item -ItemType Directory -Path $destDir -Force | Out-Null }
        Copy-Item -Path $_.FullName -Destination $destPath -Force
        $dataCopied++
    }
    Write-Host "  - 已复制数据文件 ($dataCopied 个文件)"
} else {
    Write-Host "[警告] build\data 目录不存在，请先运行 build_all.ps1" -ForegroundColor Yellow
}

# 安装 PUA 字体到系统（供 DirectWrite fallback 使用）
$FontFile    = "HeiTiZiGen.ttf"
$FontDName   = "黑体字根 (TrueType)"
$FontSrc     = Join-Path $InstallDataDir "schemas\wubi86\$FontFile"
$FontDest    = Join-Path $env:SystemRoot "Fonts\$FontFile"
$FontRegKey  = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts"
$TrackingKey = "HKLM:\SOFTWARE\WindInput"

if (Test-Path $FontSrc) {
    try {
        Copy-Item -Path $FontSrc -Destination $FontDest -Force
        Set-ItemProperty -Path $FontRegKey -Name $FontDName -Value $FontFile -Force
        # 记录本次安装的字体，卸载时用于区分是否我们安装的
        if (-not (Test-Path $TrackingKey)) {
            New-Item -Path $TrackingKey -Force | Out-Null
        }
        Set-ItemProperty -Path $TrackingKey -Name "InstalledFont_HeiTiZiGen" -Value "1" -Force
        # 广播 WM_FONTCHANGE (0x001D) 通知所有应用字体列表已变化
        $code = @"
using System;using System.Runtime.InteropServices;
public class WinFont {
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h,uint m,IntPtr w,IntPtr l);
}
"@
        Add-Type -TypeDefinition $code -ErrorAction SilentlyContinue
        [WinFont]::SendMessage([IntPtr]([int]0xFFFF), 0x001D, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        Write-Host "  - 已安装字体: $FontDName" -ForegroundColor Green
    } catch {
        Write-Host "[警告] 安装字体失败: $_" -ForegroundColor Yellow
    }
} else {
    Write-Host "[警告] 未找到字体文件: $FontSrc" -ForegroundColor Yellow
}

# [7/10] 注册 COM 组件
Write-Host "[7/10] 注册 COM 组件..."
# 注册 x64 DLL（使用默认 64 位 regsvr32）
$regResult = & regsvr32 /s (Join-Path $InstallDir $DllName) 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Host "[错误] COM x64 注册失败" -ForegroundColor Red
    exit 1
}
# 注册 x86 DLL（使用 32 位 regsvr32，写入 WOW6432Node 供 32 位应用加载）
$regsvr32X86 = Join-Path $env:SystemRoot "SysWOW64\regsvr32.exe"
$x86DllInstalled = Join-Path $InstallDir $DllNameX86
if (Test-Path $x86DllInstalled) {
    $regResultX86 = & $regsvr32X86 /s $x86DllInstalled 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[警告] COM x86 注册失败，32 位应用可能无法使用输入法" -ForegroundColor Yellow
    } else {
        Write-Host "  - x86 COM 组件注册成功"
    }
}

# [8/10] 调用 InstallLayoutOrTip 将输入法注册到系统输入法列表
Write-Host "[8/10] 注册系统输入法..."
try {
    $inputDll = Join-Path $env:SystemRoot "System32\input.dll"
    if (Test-Path $inputDll) {
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
        # 格式: "LANGID:{CLSID}{ProfileGUID}"
        $result = [WindInputHelper]::InstallLayoutOrTip($ProfileStr, 0)
        if ($result) {
            Write-Host "  - 输入法已注册到系统输入法列表"
        } else {
            Write-Host "[警告] InstallLayoutOrTip 返回失败，输入法可能需要手动添加" -ForegroundColor Yellow
        }
    } else {
        Write-Host "[警告] 未找到 input.dll，跳过系统输入法注册" -ForegroundColor Yellow
    }
} catch {
    Write-Host "[警告] 系统输入法注册失败: $_" -ForegroundColor Yellow
}

# [9/10] 配置开机自启动
Write-Host "[9/10] 配置开机自启动..."
$exePath = Join-Path $InstallDir $ExeName
try {
    Set-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name $RunKeyName -Value "`"$exePath`"" -Force
    Write-Host "  - 已添加开机自启动注册表项"
} catch {
    Write-Host "[警告] 添加开机自启动失败" -ForegroundColor Yellow
}

# [10/10] 预启动输入法服务 + 创建快捷方式
Write-Host "[10/10] 预启动输入法服务..."
if ($DebugVariant) { $env:GODEBUG = "gctrace=1" }
Start-Process -FilePath $exePath
if ($DebugVariant) { Remove-Item Env:\GODEBUG -ErrorAction SilentlyContinue }
Write-Host "  - 服务已在后台启动"

# 创建快捷方式
Write-Host "创建快捷方式..."
$settingInstalled = Join-Path $InstallDir $SettingName
$uninstallExe = Join-Path $InstallDir "uninstall.exe"
$startMenuDir = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\$ShortcutFolder"

# 清理旧版散落在 Programs 根目录的快捷方式
$oldShortcut = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\$ShortcutName.lnk"
if (Test-Path $oldShortcut) {
    Remove-Item -Path $oldShortcut -Force -ErrorAction SilentlyContinue
    Write-Host "  - 已清理旧版快捷方式"
}

# 创建子目录并写入快捷方式
if (Test-Path $settingInstalled) {
    if (-not (Test-Path $startMenuDir)) {
        New-Item -ItemType Directory -Path $startMenuDir -Force | Out-Null
    }
    $ws = New-Object -ComObject WScript.Shell

    # 设置快捷方式
    $shortcut = $ws.CreateShortcut("$startMenuDir\$ShortcutName.lnk")
    $shortcut.TargetPath = $settingInstalled
    $shortcut.WorkingDirectory = $InstallDir
    $shortcut.Description = $ShortcutName
    $shortcut.Save()
    Write-Host "  - 开始菜单快捷方式已创建"

    # 卸载快捷方式
    if (Test-Path $uninstallExe) {
        $unShortcut = $ws.CreateShortcut("$startMenuDir\卸载 $DisplayName.lnk")
        $unShortcut.TargetPath = $uninstallExe
        $unShortcut.WorkingDirectory = $InstallDir
        $unShortcut.Description = "卸载 $DisplayName"
        $unShortcut.Save()
        Write-Host "  - 卸载快捷方式已创建"
    }
}

# 清理旧备份文件
Write-Host ""
Write-Host "正在清理旧备份文件..."
Get-ChildItem -Path $InstallDir -Filter "*.old_*" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
Get-ChildItem -Path $InstallDir -Filter "*.bak" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

Write-Host ""
Write-Host "======================================"
Write-Host "安装完成！"
Write-Host "======================================"
Write-Host ""
Write-Host "安装目录: $InstallDir"
Write-Host ""
Write-Host "已安装组件:"
Write-Host "- $DllName (TSF 桥接 x64)"
Write-Host "- $DllNameX86 (TSF 桥接 x86)"
Write-Host "- $ExeName (输入法服务)"
Write-Host "- $SettingName (设置界面)"
Write-Host "- $PortableName (便携启动器)"
Write-Host "- data\schemas\*.schema.yaml (输入方案配置)"
Write-Host "- data\schemas\pinyin\rime_ice.dict.yaml (拼音词库入口)"
Write-Host "- data\schemas\pinyin\cn_dicts\*.dict.yaml (拼音词库数据)"
Write-Host "- data\schemas\pinyin\unigram.txt (语言模型)"
Write-Host "- data\schemas\wubi86\wubi86_jidian*.dict.yaml (五笔86词库)"
Write-Host "- data\schemas\common_chars.txt (常用字表)"
Write-Host "- data\system.phrases.yaml (系统短语配置)"
Write-Host "- data\themes\*\theme.yaml (主题配置)"
Write-Host ""
Write-Host "服务已自动启动，并已配置开机自启动。"
Write-Host ""
Write-Host "使用方法:"
Write-Host "1. 按 Win+Space 切换输入法"
Write-Host "2. 从输入法列表选择`"$DisplayName`""
Write-Host "3. 开始输入(默认拼音模式)"
Write-Host ""
Write-Host "热键:"
Write-Host "- Shift: 切换中英文模式"
Write-Host "- Ctrl+Shift+E`: 切换拼音/五笔引擎"
Write-Host ""
Write-Host "设置:"
Write-Host "- 运行 $SettingName 或在开始菜单中找到`"$ShortcutName`""
Write-Host "- 配置位置: %APPDATA%\$AppDirName\config.yaml"
Write-Host ""
Write-Host "注意: 如果旧文件无法删除,请重启电脑后"
Write-Host "重新运行安装程序以完成清理。"
exit 0
