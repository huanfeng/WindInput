param(
    [switch]$DebugVariant
)
#Requires -RunAsAdministrator
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition

if ($DebugVariant) {
    $AppDirName = "WindInputDebug"
    $DllName = "wind_tsf_debug.dll"
    $ExeName = "wind_input_debug.exe"
    $SettingName = "wind_setting_debug.exe"
    $ServiceProcessName = "wind_input_debug"
    $SettingProcessName = "wind_setting_debug"
    $RunKeyName = "WindInputDebug"
    $ShortcutFolder = "清风输入法 Debug"
    $ShortcutName = "清风输入法 Debug 设置"
    $DisplayName = "清风输入法 (Debug)"
    $ProfileStr = "0804:{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}{99C2DEB1-5C57-45A2-9C63-FB54B34FD90A}"
    $BuildDir = Join-Path (Split-Path -Parent $ScriptDir) "build_debug"
} else {
    $AppDirName = "WindInput"
    $DllName = "wind_tsf.dll"
    $ExeName = "wind_input.exe"
    $SettingName = "wind_setting.exe"
    $ServiceProcessName = "wind_input"
    $SettingProcessName = "wind_setting"
    $RunKeyName = "WindInput"
    $ShortcutFolder = "清风输入法"
    $ShortcutName = "清风输入法 设置"
    $DisplayName = "清风输入法"
    $ProfileStr = "0804:{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}"
    $BuildDir = Join-Path (Split-Path -Parent $ScriptDir) "build"
}

$InstallDir = if ($env:ProgramW6432) { Join-Path $env:ProgramW6432 $AppDirName } else { Join-Path $env:ProgramFiles $AppDirName }

Write-Host "======================================"
Write-Host "$DisplayName 安装程序"
Write-Host "======================================"
Write-Host ""

# [1/12] 检查文件
Write-Host "[1/12] 检查文件..."
$requiredFiles = @($DllName, $ExeName)
foreach ($f in $requiredFiles) {
    if (-not (Test-Path (Join-Path $BuildDir $f))) {
        Write-Host "[错误] 未找到 $f" -ForegroundColor Red
        Write-Host "请先运行 build_all.ps1"
        exit 1
    }
}

# [2/12] 停止旧进程
Write-Host "[2/12] 停止旧进程..."
Get-Process -Name $SettingProcessName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Get-Process -Name $ServiceProcessName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

# [3/12] 创建安装目录
Write-Host "[3/12] 创建安装目录..."
Write-Host "[4/12] 处理已有文件..."
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

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

Remove-OldFile -FilePath (Join-Path $InstallDir $DllName) -FileName $DllName -UnregisterCOM
Remove-OldFile -FilePath (Join-Path $InstallDir "wind_dwrite.dll") -FileName "wind_dwrite.dll"  # 清理旧版本遗留
Remove-OldFile -FilePath (Join-Path $InstallDir $ExeName) -FileName $ExeName
Remove-OldFile -FilePath (Join-Path $InstallDir $SettingName) -FileName $SettingName

# [5/12] 复制文件
Write-Host "[5/12] 复制文件..."
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

# [6/12] 复制数据目录（词库、方案、短语、主题）
Write-Host "[6/12] 复制数据目录(data/)..."
$BuildDataDir = Join-Path $BuildDir "data"
$InstallDataDir = Join-Path $InstallDir "data"

$dataDirs = @("dict\pinyin", "dict\pinyin\cn_dicts", "dict\wubi86", "schemas")
foreach ($d in $dataDirs) {
    $target = Join-Path $InstallDataDir $d
    if (-not (Test-Path $target)) { New-Item -ItemType Directory -Path $target -Force | Out-Null }
}

$dictFiles = @(
    @{ Src = "dict\pinyin\rime_ice.dict.yaml"; Desc = "拼音词库入口: rime_ice.dict.yaml" },
    @{ Src = "dict\pinyin\cn_dicts\8105.dict.yaml"; Desc = "拼音单字词库: cn_dicts/8105.dict.yaml" },
    @{ Src = "dict\pinyin\cn_dicts\base.dict.yaml"; Desc = "拼音基础词库: cn_dicts/base.dict.yaml" },
    @{ Src = "dict\pinyin\unigram.txt"; Desc = "语言模型: unigram.txt"; Optional = $true },
    @{ Src = "dict\wubi86\wubi86_jidian.dict.yaml"; Desc = "五笔主词库: wubi86_jidian.dict.yaml" },
    @{ Src = "dict\wubi86\wubi86_jidian_extra.dict.yaml"; Desc = "五笔扩展词库: wubi86_jidian_extra.dict.yaml"; Optional = $true },
    @{ Src = "dict\wubi86\wubi86_jidian_extra_district.dict.yaml"; Desc = "五笔行政区域词库: wubi86_jidian_extra_district.dict.yaml"; Optional = $true },
    @{ Src = "dict\wubi86\wubi86_jidian_user.dict.yaml"; Desc = "五笔用户词库模板: wubi86_jidian_user.dict.yaml"; Optional = $true },
    @{ Src = "dict\common_chars.txt"; Desc = "常用字表: common_chars.txt" },
    @{ Src = "system.phrases.yaml"; Desc = "系统短语配置: system.phrases.yaml" }
)

foreach ($df in $dictFiles) {
    $srcPath = Join-Path $BuildDataDir $df.Src
    $dstPath = Join-Path $InstallDataDir $df.Src
    $dstDir = Split-Path -Parent $dstPath
    if (-not (Test-Path $dstDir)) { New-Item -ItemType Directory -Path $dstDir -Force | Out-Null }

    if (Test-Path $srcPath) {
        Copy-Item -Path $srcPath -Destination $dstPath -Force
        Write-Host "  - $($df.Desc)"
    } elseif ($df.Optional) {
        Write-Host "[提示] $($df.Desc -replace ':.*', '') 不存在,智能组句功能不可用" -ForegroundColor Cyan
    } else {
        Write-Host "[警告] build\data 目录中未找到 $($df.Src),请先运行 build_all.ps1" -ForegroundColor Yellow
    }
}

# [7/12] 复制输入方案配置
Write-Host "[7/12] 复制输入方案配置..."
$schemasDir = Join-Path $InstallDataDir "schemas"
if (-not (Test-Path $schemasDir)) { New-Item -ItemType Directory -Path $schemasDir -Force | Out-Null }
$schemaFiles = Get-ChildItem -Path (Join-Path $BuildDataDir "schemas") -Filter "*.schema.yaml" -ErrorAction SilentlyContinue
if ($schemaFiles) {
    $schemaFiles | Copy-Item -Destination $schemasDir -Force
    Write-Host "  - 输入方案配置已复制"
} else {
    Write-Host "[警告] build\data 目录中未找到输入方案配置" -ForegroundColor Yellow
}

# [7b/12] 复制默认配置文件
$configSrc = Join-Path $BuildDataDir "config.yaml"
if (Test-Path $configSrc) {
    Copy-Item -Path $configSrc -Destination (Join-Path $InstallDataDir "config.yaml") -Force
    Write-Host "  - 默认配置文件已复制"
} else {
    Write-Host "[警告] build\data 目录中未找到默认配置文件 config.yaml" -ForegroundColor Yellow
}

# [8/12] 复制主题文件
Write-Host "[8/12] 复制主题文件..."
$themesSource = Join-Path $BuildDataDir "themes"
if (Test-Path $themesSource) {
    $themesTarget = Join-Path $InstallDataDir "themes"
    if (-not (Test-Path $themesTarget)) { New-Item -ItemType Directory -Path $themesTarget -Force | Out-Null }
    Get-ChildItem -Path $themesSource -Directory | ForEach-Object {
        $themeYaml = Join-Path $_.FullName "theme.yaml"
        if (Test-Path $themeYaml) {
            $destThemeDir = Join-Path $themesTarget $_.Name
            if (-not (Test-Path $destThemeDir)) { New-Item -ItemType Directory -Path $destThemeDir -Force | Out-Null }
            Copy-Item -Path $themeYaml -Destination $destThemeDir -Force
            Write-Host "  - 主题: $($_.Name)"
        }
    }
} else {
    Write-Host "[警告] build\data 目录中未找到主题文件" -ForegroundColor Yellow
}

# [9/12] 注册 COM 组件
Write-Host "[9/12] 注册 COM 组件..."
$regResult = & regsvr32 /s (Join-Path $InstallDir $DllName) 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Host "[错误] COM 注册失败" -ForegroundColor Red
    exit 1
}

# [10/13] 调用 InstallLayoutOrTip 将输入法注册到系统输入法列表
Write-Host "[10/13] 注册系统输入法..."
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

# [11/13] 配置开机自启动
Write-Host "[11/13] 配置开机自启动..."
$exePath = Join-Path $InstallDir $ExeName
try {
    Set-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name $RunKeyName -Value "`"$exePath`"" -Force
    Write-Host "  - 已添加开机自启动注册表项"
} catch {
    Write-Host "[警告] 添加开机自启动失败" -ForegroundColor Yellow
}

# [12/13] 预启动输入法服务
Write-Host "[12/13] 预启动输入法服务..."
Start-Process -FilePath $exePath
Write-Host "  - 服务已在后台启动"

# [13/13] 创建快捷方式
Write-Host "[13/13] 创建快捷方式..."
$settingInstalled = Join-Path $InstallDir $SettingName
if (Test-Path $settingInstalled) {
    $ws = New-Object -ComObject WScript.Shell
    $shortcut = $ws.CreateShortcut("$env:ProgramData\Microsoft\Windows\Start Menu\Programs\$ShortcutName.lnk")
    $shortcut.TargetPath = $settingInstalled
    $shortcut.WorkingDirectory = $InstallDir
    $shortcut.Description = $ShortcutName
    $shortcut.Save()
    Write-Host "  - 开始菜单快捷方式已创建"
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
Write-Host "- $DllName (TSF 桥接)"
Write-Host "- $ExeName (输入法服务)"
Write-Host "- $SettingName (设置界面)"
Write-Host "- data\dict\pinyin\rime_ice.dict.yaml (拼音词库入口)"
Write-Host "- data\dict\pinyin\cn_dicts\*.dict.yaml (拼音词库数据)"
Write-Host "- data\dict\pinyin\unigram.txt (语言模型)"
Write-Host "- data\dict\wubi86\wubi86_jidian*.dict.yaml (五笔86词库)"
Write-Host "- data\dict\common_chars.txt (常用字表)"
Write-Host "- data\schemas\*.schema.yaml (输入方案配置)"
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
