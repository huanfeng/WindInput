#Requires -RunAsAdministrator
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$ErrorActionPreference = "Stop"

Write-Host "======================================"
Write-Host "清风输入法安装程序"
Write-Host "======================================"
Write-Host ""

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$BuildDir = Join-Path (Split-Path -Parent $ScriptDir) "build"

# [1/12] 检查文件
Write-Host "[1/12] 检查文件..."
$requiredFiles = @("wind_tsf.dll", "wind_dwrite.dll", "wind_input.exe")
foreach ($f in $requiredFiles) {
    if (-not (Test-Path (Join-Path $BuildDir $f))) {
        Write-Host "[错误] 未找到 $f" -ForegroundColor Red
        Write-Host "请先运行 build_all.ps1"
        exit 1
    }
}

# [2/12] 停止旧进程
Write-Host "[2/12] 停止旧进程..."
Get-Process -Name "wind_input" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

# [3/12] 创建安装目录
Write-Host "[3/12] 创建安装目录..."
Write-Host "[4/12] 处理已有文件..."
$InstallDir = if ($env:ProgramW6432) { Join-Path $env:ProgramW6432 "WindInput" } else { Join-Path $env:ProgramFiles "WindInput" }
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

Remove-OldFile -FilePath (Join-Path $InstallDir "wind_tsf.dll") -FileName "wind_tsf.dll" -UnregisterCOM
Remove-OldFile -FilePath (Join-Path $InstallDir "wind_dwrite.dll") -FileName "wind_dwrite.dll"
Remove-OldFile -FilePath (Join-Path $InstallDir "wind_input.exe") -FileName "wind_input.exe"
Remove-OldFile -FilePath (Join-Path $InstallDir "wind_setting.exe") -FileName "wind_setting.exe"

# [5/12] 复制文件
Write-Host "[5/12] 复制文件..."
foreach ($f in $requiredFiles) {
    Copy-Item -Path (Join-Path $BuildDir $f) -Destination $InstallDir -Force
}

$settingExe = Join-Path $BuildDir "wind_setting.exe"
if (Test-Path $settingExe) {
    Copy-Item -Path $settingExe -Destination $InstallDir -Force
    Write-Host "  - wind_setting.exe 已复制"
} else {
    Write-Host "[提示] 未找到 wind_setting.exe,已跳过(可选)" -ForegroundColor Cyan
}

# [6/12] 复制词库
Write-Host "[6/12] 从 build 目录复制词库(源文件, wdb 运行时自动生成)..."
$dictDirs = @("dict\pinyin", "dict\pinyin\cn_dicts", "dict\wubi86")
foreach ($d in $dictDirs) {
    $target = Join-Path $InstallDir $d
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
    @{ Src = "dict\common_chars.txt"; Desc = "常用字表: common_chars.txt" }
)

foreach ($df in $dictFiles) {
    $srcPath = Join-Path $BuildDir $df.Src
    $dstPath = Join-Path $InstallDir $df.Src
    $dstDir = Split-Path -Parent $dstPath
    if (-not (Test-Path $dstDir)) { New-Item -ItemType Directory -Path $dstDir -Force | Out-Null }

    if (Test-Path $srcPath) {
        Copy-Item -Path $srcPath -Destination $dstPath -Force
        Write-Host "  - $($df.Desc)"
    } elseif ($df.Optional) {
        Write-Host "[提示] $($df.Desc -replace ':.*', '') 不存在,智能组句功能不可用" -ForegroundColor Cyan
    } else {
        Write-Host "[警告] build 目录中未找到 $($df.Src),请先运行 build_all.ps1" -ForegroundColor Yellow
    }
}

# [7/12] 复制输入方案配置
Write-Host "[7/12] 复制输入方案配置..."
$schemasDir = Join-Path $InstallDir "schemas"
if (-not (Test-Path $schemasDir)) { New-Item -ItemType Directory -Path $schemasDir -Force | Out-Null }
$schemaFiles = Get-ChildItem -Path (Join-Path $BuildDir "schemas") -Filter "*.schema.yaml" -ErrorAction SilentlyContinue
if ($schemaFiles) {
    $schemaFiles | Copy-Item -Destination $schemasDir -Force
    Write-Host "  - 输入方案配置已复制"
} else {
    Write-Host "[警告] build 目录中未找到输入方案配置" -ForegroundColor Yellow
}

# [8/12] 复制主题文件
Write-Host "[8/12] 复制主题文件..."
$themesSource = Join-Path $BuildDir "themes"
if (Test-Path $themesSource) {
    $themesTarget = Join-Path $InstallDir "themes"
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
    Write-Host "[警告] build 目录中未找到主题文件" -ForegroundColor Yellow
}

# [9/12] 注册 COM 组件
Write-Host "[9/12] 注册 COM 组件..."
$regResult = & regsvr32 /s (Join-Path $InstallDir "wind_tsf.dll") 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Host "[错误] COM 注册失败" -ForegroundColor Red
    exit 1
}

# [10/12] 配置开机自启动
Write-Host "[10/12] 配置开机自启动..."
$exePath = Join-Path $InstallDir "wind_input.exe"
try {
    Set-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name "WindInput" -Value "`"$exePath`"" -Force
    Write-Host "  - 已添加开机自启动注册表项"
} catch {
    Write-Host "[警告] 添加开机自启动失败" -ForegroundColor Yellow
}

# [11/12] 预启动输入法服务
Write-Host "[11/12] 预启动输入法服务..."
Start-Process -FilePath $exePath
Write-Host "  - 服务已在后台启动"

# [12/12] 创建快捷方式
Write-Host "[12/12] 创建快捷方式..."
$settingInstalled = Join-Path $InstallDir "wind_setting.exe"
if (Test-Path $settingInstalled) {
    $ws = New-Object -ComObject WScript.Shell
    $shortcut = $ws.CreateShortcut("$env:ProgramData\Microsoft\Windows\Start Menu\Programs\清风输入法 设置.lnk")
    $shortcut.TargetPath = $settingInstalled
    $shortcut.WorkingDirectory = $InstallDir
    $shortcut.Description = "清风输入法 设置"
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
Write-Host "- wind_tsf.dll (TSF 桥接)"
Write-Host "- wind_dwrite.dll (DirectWrite 渲染 Shim)"
Write-Host "- wind_input.exe (输入法服务)"
Write-Host "- wind_setting.exe (设置界面)"
Write-Host "- dict\pinyin\rime_ice.dict.yaml (拼音词库入口)"
Write-Host "- dict\pinyin\cn_dicts\*.dict.yaml (拼音词库数据)"
Write-Host "- dict\pinyin\unigram.txt (语言模型)"
Write-Host "- dict\wubi86\wubi86_jidian*.dict.yaml (五笔86词库, rime 格式)"
Write-Host "- dict\common_chars.txt (常用字表)"
Write-Host "- schemas\*.schema.yaml (输入方案配置)"
Write-Host "- themes\*\theme.yaml (主题配置)"
Write-Host ""
Write-Host "服务已自动启动，并已配置开机自启动。"
Write-Host ""
Write-Host "使用方法:"
Write-Host "1. 按 Win+Space 切换输入法"
Write-Host "2. 从输入法列表选择`"清风输入法`""
Write-Host "3. 开始输入(默认拼音模式)"
Write-Host ""
Write-Host "热键:"
Write-Host "- Shift: 切换中英文模式"
Write-Host "- Ctrl+``: 切换拼音/五笔引擎"
Write-Host ""
Write-Host "设置:"
Write-Host "- 运行 wind_setting.exe 或在开始菜单中找到`"清风输入法 设置`""
Write-Host "- 配置位置: %APPDATA%\WindInput\config.yaml"
Write-Host ""
Write-Host "注意: 如果旧文件无法删除,请重启电脑后"
Write-Host "重新运行安装程序以完成清理。"
exit 0
