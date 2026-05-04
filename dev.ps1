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

function Invoke-BuildAll {
    param(
        [string[]]$Module = @("all"),
        [ValidateSet("debug", "release", "skip")]
        [string]$WailsMode = "release",
        [switch]$DebugVariant
    )

    $buildParams = @{
        Module = $Module
        WailsMode = $WailsMode
        Brief = $true
    }
    if ($DebugVariant) { $buildParams.DebugVariant = $true }

    & "$ScriptDir\build_all.ps1" @buildParams
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-BuildRelease {
    Invoke-BuildAll -WailsMode release
}

function Do-BuildDebug {
    Invoke-BuildAll -WailsMode debug
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

function Do-BuildModule {
    param(
        [string[]]$Modules,
        [ValidateSet("debug", "release")]
        [string]$WailsMode = "release"
    )
    Invoke-BuildAll -Module $Modules -WailsMode $WailsMode
}

function Do-DeployModule {
    param([string[]]$Modules)
    & "$ScriptDir\installer\install.ps1" -Module $Modules
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

# ============ 调试版操作 ============

function Do-BuildReleaseDebugVariant {
    Invoke-BuildAll -WailsMode release -DebugVariant
}

function Do-BuildDebugDebugVariant {
    Invoke-BuildAll -WailsMode debug -DebugVariant
}

function Do-InstallDebugVariant {
    & "$ScriptDir\installer\install.ps1" -DebugVariant
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-UninstallDebugVariant {
    & "$ScriptDir\installer\uninstall.ps1" -DebugVariant
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

function Do-BuildModuleDebugVariant {
    param(
        [string[]]$Modules,
        [ValidateSet("debug", "release")]
        [string]$WailsMode = "release"
    )
    Invoke-BuildAll -Module $Modules -WailsMode $WailsMode -DebugVariant
}

function Do-DeployModuleDebugVariant {
    param([string[]]$Modules)
    & "$ScriptDir\installer\install.ps1" -Module $Modules -DebugVariant
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

# ============ 便携模式部署 ============

$PortableDir = "D:\Software\WindInputPortable"
$PortableDebugDir = "D:\Software\WindInputPortableDebug"

function Portable-ReplaceFile {
    param([string]$TargetDir, [string]$FileName, [string]$SrcPath)
    $dstPath = Join-Path $TargetDir $FileName
    if (-not (Test-Path $dstPath)) {
        Copy-Item -Path $SrcPath -Destination $dstPath -Force
        Write-Host "  - $FileName"
        return
    }
    try {
        Copy-Item -Path $SrcPath -Destination $dstPath -Force -ErrorAction Stop
        Write-Host "  - $FileName"
        return
    } catch { }
    $randomSuffix = Get-Random -Maximum 99999999
    $oldName = "${FileName}.old_${randomSuffix}"
    try {
        Rename-Item -Path $dstPath -NewName $oldName -Force -ErrorAction Stop
        Copy-Item -Path $SrcPath -Destination $dstPath -Force
        Write-Host "  - $FileName (旧文件已重命名为 $oldName)"
    } catch {
        Write-Host "  [错误] 无法替换 ${FileName}: $_" -ForegroundColor Red
    }
}

function Do-PortableDeploy {
    param([string]$TargetDir, [string]$BuildDir, [string]$PortableExe, [string]$ServiceExe, [string]$SettingExe)

    Write-Host "======================================"
    Write-Host "便携模式部署"
    Write-Host "目标目录: $TargetDir"
    Write-Host "======================================"
    Write-Host ""

    $portableProcName = [System.IO.Path]::GetFileNameWithoutExtension($PortableExe)
    $serviceProcName = [System.IO.Path]::GetFileNameWithoutExtension($ServiceExe)
    $settingProcName = [System.IO.Path]::GetFileNameWithoutExtension($SettingExe)

    # 通过启动器优雅停止服务（注销 + 关闭）
    Write-Host "[1/5] 停止旧服务..."
    $oldPortable = Join-Path $TargetDir $PortableExe
    if (Test-Path $oldPortable) {
        Write-Host "  - 通过启动器停止服务..."
        & $oldPortable -stop 2>&1 | ForEach-Object { Write-Host "    $_" }
        Start-Sleep -Milliseconds 500
    }

    # 强制清理残留进程
    Write-Host "[2/5] 清理残留进程..."
    Get-Process -Name $settingProcName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Get-Process -Name $portableProcName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Get-Process -Name $serviceProcName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500

    # 创建目录
    Write-Host "[3/5] 准备目录..."
    if (-not (Test-Path $TargetDir)) { New-Item -ItemType Directory -Path $TargetDir -Force | Out-Null }

    # 复制文件（处理被占用的 DLL/EXE）
    Write-Host "[4/5] 复制文件..."
    foreach ($name in @($PortableExe, $ServiceExe, $SettingExe)) {
        $src = Join-Path $BuildDir $name
        if (Test-Path $src) {
            Portable-ReplaceFile -TargetDir $TargetDir -FileName $name -SrcPath $src
        } else {
            Write-Host "  [警告] 未找到 $name" -ForegroundColor Yellow
        }
    }

    foreach ($dll in (Get-ChildItem -Path $BuildDir -Filter "wind_tsf*.dll" -ErrorAction SilentlyContinue)) {
        Portable-ReplaceFile -TargetDir $TargetDir -FileName $dll.Name -SrcPath $dll.FullName
    }

    # 复制 data 目录
    $dataDir = Join-Path $BuildDir "data"
    if (Test-Path $dataDir) {
        $targetDataDir = Join-Path $TargetDir "data"
        if (Test-Path $targetDataDir) { Remove-Item -Path $targetDataDir -Recurse -Force -ErrorAction SilentlyContinue }
        Copy-Item -Path $dataDir -Destination $TargetDir -Recurse -Force
        Write-Host "  - data\ (词库、方案、主题)"
    }

    # 清理旧备份文件
    Get-ChildItem -Path $TargetDir -Filter "*.old_*" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

    # 启动便携启动器
    Write-Host "[5/5] 启动便携启动器..."
    $newPortable = Join-Path $TargetDir $PortableExe
    if (Test-Path $newPortable) {
        Start-Process -FilePath $newPortable
        Write-Host "  - 已启动 $PortableExe"
    }

    Write-Host ""
    Write-Host "======================================"
    Write-Host "便携模式部署完成！"
    Write-Host "目录: $TargetDir"
    Write-Host "======================================"
}

function Do-PortableDeployModule {
    param(
        [string]$TargetDir,
        [string]$BuildDir,
        [string]$PortableExe,
        [string]$ServiceExe,
        [string]$SettingExe,
        [string[]]$Modules
    )

    Write-Host "======================================"
    Write-Host "便携模式 - 模块部署"
    Write-Host "目标目录: $TargetDir"
    Write-Host "模块: $($Modules -join ', ')"
    Write-Host "======================================"
    Write-Host ""

    $portableProcName = [System.IO.Path]::GetFileNameWithoutExtension($PortableExe)
    $serviceProcName = [System.IO.Path]::GetFileNameWithoutExtension($ServiceExe)
    $settingProcName = [System.IO.Path]::GetFileNameWithoutExtension($SettingExe)

    # 通过启动器优雅停止服务
    Write-Host "[1/4] 停止旧服务..."
    $oldPortable = Join-Path $TargetDir $PortableExe
    if (Test-Path $oldPortable) {
        & $oldPortable -stop 2>&1 | ForEach-Object { Write-Host "    $_" }
        Start-Sleep -Milliseconds 500
    }

    # 清理相关进程
    Write-Host "[2/4] 清理残留进程..."
    if ($Modules -contains "setting") { Get-Process -Name $settingProcName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue }
    if ($Modules -contains "portable") { Get-Process -Name $portableProcName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue }
    if ($Modules -contains "service" -or $Modules -contains "dll") {
        Get-Process -Name $serviceProcName -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 500

    # 复制模块文件
    Write-Host "[3/4] 复制模块文件..."
    foreach ($mod in $Modules) {
        switch ($mod) {
            "dll" {
                foreach ($dll in (Get-ChildItem -Path $BuildDir -Filter "wind_tsf*.dll" -ErrorAction SilentlyContinue)) {
                    Portable-ReplaceFile -TargetDir $TargetDir -FileName $dll.Name -SrcPath $dll.FullName
                }
            }
            "service" {
                $src = Join-Path $BuildDir $ServiceExe
                if (Test-Path $src) { Portable-ReplaceFile -TargetDir $TargetDir -FileName $ServiceExe -SrcPath $src }
                else { Write-Host "  [警告] 未找到 $ServiceExe" -ForegroundColor Yellow }
            }
            "setting" {
                $src = Join-Path $BuildDir $SettingExe
                if (Test-Path $src) { Portable-ReplaceFile -TargetDir $TargetDir -FileName $SettingExe -SrcPath $src }
                else { Write-Host "  [警告] 未找到 $SettingExe" -ForegroundColor Yellow }
            }
            "portable" {
                $src = Join-Path $BuildDir $PortableExe
                if (Test-Path $src) { Portable-ReplaceFile -TargetDir $TargetDir -FileName $PortableExe -SrcPath $src }
                else { Write-Host "  [警告] 未找到 $PortableExe" -ForegroundColor Yellow }
            }
        }
    }

    # 清理旧备份 + 启动
    Get-ChildItem -Path $TargetDir -Filter "*.old_*" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

    Write-Host "[4/4] 启动便携启动器..."
    $newPortable = Join-Path $TargetDir $PortableExe
    if (Test-Path $newPortable) {
        Start-Process -FilePath $newPortable
        Write-Host "  - 已启动 $PortableExe"
    }

    Write-Host ""
    Write-Host "======================================"
    Write-Host "便携模式模块部署完成！"
    Write-Host "目录: $TargetDir"
    Write-Host "======================================"
}

# ============ 交互式菜单 ============

if (-not $Choice) {
    Write-Host "======================================"
    Write-Host "WindInput - Dev Menu"
    Write-Host "======================================"
    Write-Host ""
    Write-Host "  --- 基本操作 ---"
    Write-Host "  1    构建(Release) + 卸载 + 安装"
    Write-Host "  1d   构建(Debug)   + 卸载 + 安装"
    Write-Host "  1s   卸载 + 安装（跳过构建）"
    Write-Host "  2    仅构建(Release)"
    Write-Host "  2d   仅构建(Debug)"
    Write-Host "  3    仅安装"
    Write-Host "  4    仅卸载"
    Write-Host "  8    生成安装包"
    Write-Host "  8s   生成安装包（跳过编译）"
    Write-Host ""
    Write-Host "  --- 模块快速部署 (1=DLL 2=服务 3=设置 4=便携) ---"
    Write-Host "  m[N]   构建 + 部署  (如: m1, m12, m3d, m123d; 3d=设置 debug)"
    Write-Host ""
    Write-Host "  --- 便携模式 ---"
    Write-Host "  p      构建 + 全量部署  -> $PortableDir"
    Write-Host "  pm[N]  构建 + 模块部署  (如: pm2, pm12, pm3d)"
    Write-Host ""
    Write-Host "  前缀 d = 调试版 (如: d1, d2d, dm12, dp, dpm12)"
    Write-Host "  调试版便携目录: $PortableDebugDir"
    Write-Host ""
    $Choice = Read-Host "请选择"
    if (-not $Choice) { exit 1 }
}

# ============ 命令解析与执行 ============

$cmd = $Choice.Trim().ToLower()
$isDebugVariant = $false
$isPortable = $false

# 解析调试版前缀 'd'
if ($cmd.Length -gt 1 -and $cmd[0] -eq 'd') {
    $isDebugVariant = $true
    $cmd = $cmd.Substring(1)
}

# 解析便携前缀 'p'
if ($cmd.Length -ge 1 -and $cmd[0] -eq 'p') {
    $isPortable = $true
    $cmd = $cmd.Substring(1)
}

# 便携模式辅助变量
if ($isPortable) {
    if ($isDebugVariant) {
        $pTargetDir = $PortableDebugDir
        $pBuildDir = Join-Path $ScriptDir "build_debug"
        $pPortableExe = "wind_portable.exe"
        $pServiceExe = "wind_input_debug.exe"
        $pSettingExe = "wind_setting_debug.exe"
    } else {
        $pTargetDir = $PortableDir
        $pBuildDir = Join-Path $ScriptDir "build"
        $pPortableExe = "wind_portable.exe"
        $pServiceExe = "wind_input.exe"
        $pSettingExe = "wind_setting.exe"
    }
}

# 解析模块数字辅助函数
function Parse-Modules([string]$Digits) {
    $mods = @()
    if ($Digits -match '1') { $mods += "dll" }
    if ($Digits -match '2') { $mods += "service" }
    if ($Digits -match '3') { $mods += "setting" }
    if ($Digits -match '4') { $mods += "portable" }
    return ,$mods
}

# ============ 数据维护 ============

function Do-GenPinyinData {
    param(
        [string]$Src = ""  # 本地 pinyin-data 目录（留空时从 GitHub 下载）
    )
    $windInputDir = Join-Path $ScriptDir "wind_input"
    Push-Location $windInputDir
    try {
        $genArgs = @("run", "./cmd/gen_pinyin_data", "-out", "internal/tooltip/pinyin_data_generated.go")
        if ($Src) { $genArgs += @("-src", $Src) }
        & go @genArgs
        if ($LASTEXITCODE -ne 0) { exit 1 }
        go fmt ./internal/tooltip/pinyin_data_generated.go
        Write-Host "[OK] 拼音数据已更新" -ForegroundColor Green
    } finally {
        Pop-Location
    }
}

# ---- 路由 ----

if ($isPortable -and $cmd -eq '') {
    # p / dp: 便携全量部署
    if ($isDebugVariant) { Do-BuildReleaseDebugVariant } else { Do-BuildRelease }
    Do-PortableDeploy -TargetDir $pTargetDir -BuildDir $pBuildDir `
        -PortableExe $pPortableExe -ServiceExe $pServiceExe -SettingExe $pSettingExe

} elseif ($isPortable -and $cmd -match '^m([1234]+)(d)?$') {
    # pm[N] / pm[N]d / dpm[N] / dpm[N]d: 便携模块部署
    $modules = Parse-Modules $Matches[1]
    $wailsMode = if ($Matches[2] -eq 'd') { "debug" } else { "release" }
    if ($isDebugVariant) { Do-BuildModuleDebugVariant -Modules $modules -WailsMode $wailsMode } else { Do-BuildModule -Modules $modules -WailsMode $wailsMode }
    Do-PortableDeployModule -TargetDir $pTargetDir -BuildDir $pBuildDir `
        -PortableExe $pPortableExe -ServiceExe $pServiceExe -SettingExe $pSettingExe -Modules $modules

} elseif ($isPortable) {
    Write-Host "[ERROR] 便携模式仅支持: p, pm[1234][d] (如: p, pm12, pm3d, dpm123)" -ForegroundColor Red
    exit 1

} elseif ($cmd -match '^m([1234]+)(d)?$') {
    # m[N] / m[N]d / dm[N] / dm[N]d: 系统模块部署
    Ensure-Admin
    $modules = Parse-Modules $Matches[1]
    $wailsMode = if ($Matches[2] -eq 'd') { "debug" } else { "release" }
    if ($isDebugVariant) {
        Do-BuildModuleDebugVariant -Modules $modules -WailsMode $wailsMode
        Do-DeployModuleDebugVariant -Modules $modules
    } else {
        Do-BuildModule -Modules $modules -WailsMode $wailsMode
        Do-DeployModule -Modules $modules
    }

} elseif ($cmd -match '^1([ds])?$') {
    # 1 / 1d: 构建 + 卸载 + 安装；1s: 卸载 + 安装
    Ensure-Admin
    $suffix = $Matches[1]
    if ($isDebugVariant) {
        if ($suffix -eq 'd') { Do-BuildDebugDebugVariant }
        elseif ($suffix -ne 's') { Do-BuildReleaseDebugVariant }
        Do-UninstallDebugVariant
        Do-InstallDebugVariant
    } else {
        if ($suffix -eq 'd') { Do-BuildDebug }
        elseif ($suffix -ne 's') { Do-BuildRelease }
        Do-Uninstall
        Do-Install
    }

} elseif ($cmd -match '^2(d)?$') {
    # 2 / 2d: 仅构建
    if ($Matches[1] -eq 'd') {
        if ($isDebugVariant) { Do-BuildDebugDebugVariant } else { Do-BuildDebug }
    } else {
        if ($isDebugVariant) { Do-BuildReleaseDebugVariant } else { Do-BuildRelease }
    }

} elseif ($cmd -eq '3') {
    # 3 / d3: 仅安装
    Ensure-Admin
    if ($isDebugVariant) { Do-InstallDebugVariant } else { Do-Install }

} elseif ($cmd -eq '4') {
    # 4 / d4: 仅卸载
    Ensure-Admin
    if ($isDebugVariant) { Do-UninstallDebugVariant } else { Do-Uninstall }

} elseif ($cmd -match '^8(s)?$') {
    # 8 / 8s: 生成安装包
    if ($Matches[1] -eq 's') { Do-BuildInstallerSkip } else { Do-BuildInstaller }

} elseif ($cmd -eq 'gen') {
    # gen: 重新生成拼音数据（需要 pinyin-data 仓库与主仓库同级）
    Do-GenPinyinData

} else {
    Write-Host "[ERROR] 无效选项: $Choice" -ForegroundColor Red
    Write-Host ""
    Write-Host "格式: [d]<操作>"
    Write-Host "  操作: 1[d|s], 2[d], 3, 4, 8[s], m[1234][d], p, pm[1234][d]"
    Write-Host "  前缀 d = 调试版"
    Write-Host "  数据: gen (重新生成拼音数据)"
    exit 1
}
