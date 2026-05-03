param(
    [ValidateSet("all", "dll", "service", "setting", "portable")]
    [string[]]$Module = @("all"),

    [ValidateSet("debug", "release", "skip")]
    [string]$WailsMode = "debug",

    [switch]$SettingOnly,

    [switch]$DebugVariant,

    [switch]$Brief
)

$ErrorActionPreference = "Stop"

# 向后兼容: -SettingOnly 映射为 -Module setting
if ($SettingOnly) {
    $Module = @("setting")
}

# 确定构建模块
$BuildAll = $Module -contains "all"
$BuildService = $BuildAll -or ($Module -contains "service")
$BuildDll = $BuildAll -or ($Module -contains "dll")
$BuildSetting = $BuildAll -or ($Module -contains "setting")
$BuildPortable = $BuildAll -or ($Module -contains "portable")

Write-Host "======================================"
Write-Host "WindInput - Build"
Write-Host "======================================"
Write-Host ""

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$BuildDir = Join-Path $ScriptDir "build"

if ($DebugVariant) {
    $BuildDir = Join-Path $ScriptDir "build_debug"
    Write-Host "*** DEBUG VARIANT BUILD ***" -ForegroundColor Magenta
}

# 读取版本号
$VersionFile = Join-Path $ScriptDir "VERSION"
if (Test-Path $VersionFile) {
    $AppVersion = (Get-Content $VersionFile -Raw).Trim()
} else {
    $AppVersion = "dev"
}

# 解析版本号为组件（major.minor.patch）
$VersionCore = ($AppVersion -split '-')[0]
$VersionParts = $VersionCore -split '\.'
$VerMajor = "0"; $VerMinor = "0"; $VerPatch = "0"
if ($VersionParts.Length -ge 1) { $VerMajor = $VersionParts[0] }
if ($VersionParts.Length -ge 2) { $VerMinor = $VersionParts[1] }
if ($VersionParts.Length -ge 3) { $VerPatch = $VersionParts[2] }

# 生成构建号（基于 git commit 数量）
$VerBuild = "0"
try {
    $commitCount = git -C $ScriptDir rev-list --count HEAD 2>$null
    if ($commitCount) { $VerBuild = $commitCount.Trim() }
} catch { }
$AppVersionNum = "$VerMajor.$VerMinor.$VerPatch.$VerBuild"

Write-Host "版本: $AppVersion (构建号: $AppVersionNum)"

# 步进计数器
$script:StepIdx = 0
if ($BuildAll) {
    $script:TotalSteps = 7
} else {
    $script:TotalSteps = (@($BuildService, $BuildDll, $BuildSetting, $BuildPortable) | Where-Object { $_ }).Count
}

function Write-Step([string]$Message) {
    $script:StepIdx++
    Write-Host "[$($script:StepIdx)/$($script:TotalSteps)] $Message"
}

function Write-Detail([string]$Message) {
    if (-not $Brief) {
        Write-Host $Message
    }
}

function Write-DetailLine {
    if (-not $Brief) {
        Write-Host ""
    }
}

if (-not $BuildAll) {
    $moduleNames = @()
    if ($BuildDll) { $moduleNames += "TSF DLL" }
    if ($BuildService) { $moduleNames += "GO 服务" }
    if ($BuildSetting) { $moduleNames += "设置" }
    if ($BuildPortable) { $moduleNames += "便携启动器" }
    Write-Host "构建模块: $($moduleNames -join ', ')"
}
Write-Host ""

if (-not (Test-Path $BuildDir)) { New-Item -ItemType Directory -Path $BuildDir -Force | Out-Null }

# ============================================================
# 构建函数
# ============================================================

function Build-GoService {
    $goExeName = if ($DebugVariant) { "wind_input_debug.exe" } else { "wind_input.exe" }
    Write-Step "构建 Go 服务($goExeName)..."
    Push-Location (Join-Path $ScriptDir "wind_input")
    try {
        # 生成版本资源文件 (.syso)
        Push-Location "cmd/service"
        if (Get-Command go-winres -ErrorAction SilentlyContinue) {
            # Debug 版本修改 winres.json 中的描述信息
            $winresJsonPath = "winres\winres.json"
            if ($DebugVariant -and (Test-Path $winresJsonPath)) {
                $winresJson = Get-Content $winresJsonPath -Raw -Encoding UTF8 | ConvertFrom-Json
                $winresJson.RT_MANIFEST.'#1'.'0409'.description = "清风输入法开发版服务"
                $winresJson.RT_VERSION.'#1'.'0000'.info.'0804'.CompanyName = "清风输入法开发版"
                $winresJson.RT_VERSION.'#1'.'0000'.info.'0804'.FileDescription = "清风输入法开发版服务进程"
                $winresJson.RT_VERSION.'#1'.'0000'.info.'0804'.ProductName = "清风输入法开发版"
                $winresJson.RT_VERSION.'#1'.'0000'.info.'0804'.LegalCopyright = "Copyright © 2026 清风输入法开发版"
                $jsonText = $winresJson | ConvertTo-Json -Depth 10
                [System.IO.File]::WriteAllText((Resolve-Path $winresJsonPath).Path, $jsonText, (New-Object System.Text.UTF8Encoding $false))
            }
            & go-winres make --product-version "$AppVersion" --file-version "$AppVersionNum"
            if ($LASTEXITCODE -ne 0) {
                Write-Host "[警告] go-winres 生成资源失败，继续构建（无版本信息）" -ForegroundColor Yellow
            }
            # 还原 winres.json
            if ($DebugVariant -and (Test-Path $winresJsonPath)) {
                & git checkout -- $winresJsonPath 2>$null
            }
        } else {
            Write-Host "[警告] go-winres 未安装，跳过版本资源生成" -ForegroundColor Yellow
        }
        Pop-Location

        $goLdflags = "-H windowsgui -X main.version=$AppVersion"
        $goBuildTags = @()
        if ($DebugVariant) {
            $goLdflags += " -X github.com/huanfeng/wind_input/pkg/buildvariant.variant=debug"
            $goBuildTags += "debugvariant"
        }
        $goBuildArgs = @("build", "-ldflags", $goLdflags)
        if ($goBuildTags.Count -gt 0) {
            $goBuildArgs += "-tags", ($goBuildTags -join ",")
        }
        $goBuildArgs += "-o", (Join-Path $BuildDir $goExeName), "./cmd/service"
        & go @goBuildArgs
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[错误] Go 构建失败" -ForegroundColor Red
            exit 1
        }
    } finally {
        Pop-Location
    }
    Write-Host "Go 服务构建成功"
    Write-Host ""
}

function Build-CppDll {
    $dllName = if ($DebugVariant) { "wind_tsf_debug.dll" } else { "wind_tsf.dll" }
    $dllNameX86 = if ($DebugVariant) { "wind_tsf_debug_x86.dll" } else { "wind_tsf_x86.dll" }

    Write-Step "构建 C++ DLL($dllName + $dllNameX86)..."

    # --- 构建 x64 DLL ---
    Write-Host "      构建 x64..."
    $cppBuildDir = if ($DebugVariant) { Join-Path $ScriptDir "wind_tsf\build_debug" } else { Join-Path $ScriptDir "wind_tsf\build" }
    if (-not (Test-Path $cppBuildDir)) { New-Item -ItemType Directory -Path $cppBuildDir -Force | Out-Null }
    Push-Location $cppBuildDir
    try {
        if (Test-Path (Join-Path $cppBuildDir "CMakeCache.txt")) {
            Remove-Item (Join-Path $cppBuildDir "CMakeCache.txt") -Force
            Remove-Item (Join-Path $cppBuildDir "CMakeFiles") -Recurse -Force -ErrorAction SilentlyContinue
        }
        $cmakeArgs = @("..", "-DAPP_VERSION_MAJOR=$VerMajor", "-DAPP_VERSION_MINOR=$VerMinor", "-DAPP_VERSION_PATCH=$VerPatch", "-DAPP_VERSION_BUILD=$VerBuild", "-DAPP_VERSION_STR=$VersionCore")
        if ($DebugVariant) {
            $cmakeArgs += "-DWIND_DEBUG_VARIANT=ON"
        }
        & cmake @cmakeArgs
        if ($LASTEXITCODE -ne 0) { Write-Host "[错误] CMake x64 配置失败" -ForegroundColor Red; exit 1 }
        & cmake --build . --config Release
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[错误] C++ x64 构建失败" -ForegroundColor Red
            exit 1
        }
    } finally {
        Pop-Location
    }

    # 确保 x64 DLL 在正确的输出目录中
    if (-not (Test-Path (Join-Path $BuildDir $dllName))) {
        $cmakeDllRelease = Join-Path $cppBuildDir "Release\$dllName"
        $cmakeDllRoot = Join-Path $cppBuildDir $dllName
        if (Test-Path $cmakeDllRelease) {
            Copy-Item -Path $cmakeDllRelease -Destination $BuildDir -Force
        } elseif (Test-Path $cmakeDllRoot) {
            Copy-Item -Path $cmakeDllRoot -Destination $BuildDir -Force
        } else {
            Write-Host "[错误] C++ x64 构建完成但 $dllName 未找到" -ForegroundColor Red
            exit 1
        }
    }
    Write-Host "      x64 构建成功"

    # --- 构建 x86 DLL ---
    Write-Host "      构建 x86..."
    $x64DllBackup = Join-Path $BuildDir "${dllName}.x64bak"
    Copy-Item -Path (Join-Path $BuildDir $dllName) -Destination $x64DllBackup -Force

    $cppBuildDirX86 = if ($DebugVariant) { Join-Path $ScriptDir "wind_tsf\build_debug_x86" } else { Join-Path $ScriptDir "wind_tsf\build_x86" }
    if (-not (Test-Path $cppBuildDirX86)) { New-Item -ItemType Directory -Path $cppBuildDirX86 -Force | Out-Null }
    Push-Location $cppBuildDirX86
    try {
        if (Test-Path (Join-Path $cppBuildDirX86 "CMakeCache.txt")) {
            Remove-Item (Join-Path $cppBuildDirX86 "CMakeCache.txt") -Force
            Remove-Item (Join-Path $cppBuildDirX86 "CMakeFiles") -Recurse -Force -ErrorAction SilentlyContinue
        }
        $cmakeArgsX86 = @("..", "-A", "Win32", "-DAPP_VERSION_MAJOR=$VerMajor", "-DAPP_VERSION_MINOR=$VerMinor", "-DAPP_VERSION_PATCH=$VerPatch", "-DAPP_VERSION_BUILD=$VerBuild", "-DAPP_VERSION_STR=$VersionCore")
        if ($DebugVariant) {
            $cmakeArgsX86 += "-DWIND_DEBUG_VARIANT=ON"
        }
        & cmake @cmakeArgsX86
        if ($LASTEXITCODE -ne 0) { Write-Host "[错误] CMake x86 配置失败" -ForegroundColor Red; exit 1 }
        & cmake --build . --config Release
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[错误] C++ x86 构建失败" -ForegroundColor Red
            exit 1
        }
    } finally {
        Pop-Location
    }

    # x86 DLL 已输出到 BuildDir（与 x64 同名），重命名为 x86 版本
    $x86DllPath = Join-Path $BuildDir $dllName
    if (Test-Path $x86DllPath) {
        Move-Item -Path $x86DllPath -Destination (Join-Path $BuildDir $dllNameX86) -Force
    } else {
        $x86Candidates = @((Join-Path $cppBuildDirX86 "Release\$dllName"), (Join-Path $cppBuildDirX86 $dllName))
        $x86Found = $false
        foreach ($c in $x86Candidates) {
            if (Test-Path $c) {
                Copy-Item -Path $c -Destination (Join-Path $BuildDir $dllNameX86) -Force
                $x86Found = $true
                break
            }
        }
        if (-not $x86Found) {
            Write-Host "[错误] C++ x86 构建完成但 DLL 未找到" -ForegroundColor Red
            exit 1
        }
    }

    # 恢复 x64 DLL
    Move-Item -Path $x64DllBackup -Destination (Join-Path $BuildDir $dllName) -Force
    Write-Host "      x86 构建成功"
    Write-Host ""
}

function Build-SettingUI {
    $settingDstName = if ($DebugVariant) { "wind_setting_debug.exe" } else { "wind_setting.exe" }
    Write-Step "构建设置界面($settingDstName)..."

    if ($WailsMode -eq "skip") {
        Write-Host "[提示] 已按参数跳过 Wails 构建"
        Write-Host ""
        return
    }

    # 更新 wails.json 中的版本号
    $wailsJsonPath = Join-Path $ScriptDir "wind_setting\wails.json"
    if (Test-Path $wailsJsonPath) {
        $wailsJson = Get-Content $wailsJsonPath -Raw -Encoding UTF8 | ConvertFrom-Json
        $productDisplayName = if ($DebugVariant) { "清风输入法开发版 设置" } else { "清风输入法 设置" }
        if (-not $wailsJson.info) {
            $wailsJson | Add-Member -NotePropertyName "info" -NotePropertyValue ([PSCustomObject]@{
                companyName = "清风输入法"
                productName = $productDisplayName
                productVersion = $VersionCore
                copyright = "Copyright © 2026 清风输入法"
                comments = "清风输入法设置工具"
            }) -Force
        } else {
            $wailsJson.info | Add-Member -NotePropertyName "productVersion" -NotePropertyValue $VersionCore -Force
            $wailsJson.info | Add-Member -NotePropertyName "productName" -NotePropertyValue $productDisplayName -Force
        }
        $jsonText = $wailsJson | ConvertTo-Json -Depth 10
        [System.IO.File]::WriteAllText($wailsJsonPath, $jsonText, (New-Object System.Text.UTF8Encoding $false))
    }

    Push-Location (Join-Path $ScriptDir "wind_setting")
    try {
        if (-not (Get-Command wails -ErrorAction SilentlyContinue)) {
            Write-Host "[错误] 未找到 Wails CLI,无法构建 wind_setting" -ForegroundColor Red
            Write-Host "       请先安装: go install github.com/wailsapp/wails/v2/cmd/wails@latest" -ForegroundColor Red
            Write-Host "       如需跳过此步骤,请使用: .\build_all.ps1 -WailsMode skip" -ForegroundColor Yellow
            exit 1
        }
        $wailsLdflags = "-X main.version=$AppVersion"
        if ($DebugVariant) {
            $wailsLdflags += " -X github.com/huanfeng/wind_input/pkg/buildvariant.variant=debug"
        }
        if ($WailsMode -eq "debug") {
            & wails build -debug -ldflags $wailsLdflags
        } else {
            & wails build -ldflags $wailsLdflags
        }
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[错误] wind_setting 构建失败" -ForegroundColor Red
            exit 1
        }
        $settingExe = Join-Path $ScriptDir "wind_setting\build\bin\wind_setting.exe"
        if (-not (Test-Path $settingExe)) {
            Write-Host "[错误] wind_setting.exe 未生成" -ForegroundColor Red
            exit 1
        }
        Copy-Item -Path $settingExe -Destination (Join-Path $BuildDir $settingDstName) -Force
        if ($WailsMode -eq "debug") {
            Write-Host "设置界面构建成功 (debug 模式,可按 F12 打开 DevTools)"
        } else {
            Write-Host "设置界面构建成功 (release 模式)"
        }
    } finally {
        Pop-Location
        # 还原 wails.json，避免脚本写入的版本/产品名进入 git 提交列表
        if (Test-Path $wailsJsonPath) {
            & git checkout -- $wailsJsonPath 2>$null
        }
    }
    Write-Host ""
}

function Build-PortableLauncher {
    # 便携启动器统一构建为 wind_portable.exe（运行时自动检测 debug/release 变体）
    $portableDstName = "wind_portable.exe"
    Write-Step "构建便携启动器($portableDstName)..."

    Push-Location (Join-Path $ScriptDir "wind_portable")
    try {
        # 更新 AssemblyInfo 版本号
        $assemblyInfoPath = Join-Path (Get-Location) "Properties\AssemblyInfo.cs"
        if (Test-Path $assemblyInfoPath) {
            $content = Get-Content $assemblyInfoPath -Raw -Encoding UTF8
            $content = $content -replace 'AssemblyVersion\("[^"]*"\)', "AssemblyVersion(`"$AppVersionNum`")"
            $content = $content -replace 'AssemblyFileVersion\("[^"]*"\)', "AssemblyFileVersion(`"$AppVersionNum`")"
            [System.IO.File]::WriteAllText($assemblyInfoPath, $content, (New-Object System.Text.UTF8Encoding $false))
        }

        & dotnet build -c Release -o $BuildDir /p:AssemblyName=wind_portable
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[错误] 便携启动器构建失败" -ForegroundColor Red
            exit 1
        }

        # 还原 AssemblyInfo
        & git checkout -- $assemblyInfoPath 2>$null
    } finally {
        Pop-Location
    }

    Write-Host "便携启动器构建成功"
    Write-Host ""
}

function Download-RemoteFile {
    param([string]$BaseUrl, [string]$FileName, [string]$TargetDir, [string]$Description)
    $targetPath = Join-Path $TargetDir $FileName
    if (Test-Path $targetPath) {
        Write-Detail "  - $FileName 已存在,跳过下载"
        return
    }
    Write-Detail "  - 下载 $FileName ($Description)..."
    try {
        Invoke-WebRequest -Uri "$BaseUrl/$FileName" -OutFile $targetPath -UseBasicParsing
    } catch {
        Write-Host "[错误] 下载 $FileName 失败" -ForegroundColor Red
        exit 1
    }
}

function Download-Dictionaries {
    Write-Step "下载词库..."

    # 拼音词库 (rime-ice)
    Write-Detail "  拼音词库 (rime-ice):"
    $RimePinyinDir = Join-Path $ScriptDir ".cache\rime"
    $RimePinyinCnDicts = Join-Path $RimePinyinDir "cn_dicts"
    if (-not (Test-Path $RimePinyinDir)) { New-Item -ItemType Directory -Path $RimePinyinDir -Force | Out-Null }
    if (-not (Test-Path $RimePinyinCnDicts)) { New-Item -ItemType Directory -Path $RimePinyinCnDicts -Force | Out-Null }

    $RimeIceBaseUrl = "https://raw.githubusercontent.com/iDvel/rime-ice/main"
    Download-RemoteFile $RimeIceBaseUrl "rime_ice.dict.yaml" $RimePinyinDir "词库入口描述文件"
    Download-RemoteFile "$RimeIceBaseUrl/cn_dicts" "8105.dict.yaml" $RimePinyinCnDicts "单字词库, 约112KB"
    Download-RemoteFile "$RimeIceBaseUrl/cn_dicts" "base.dict.yaml" $RimePinyinCnDicts "基础词库, 约16MB"
    Download-RemoteFile "$RimeIceBaseUrl/cn_dicts" "ext.dict.yaml" $RimePinyinCnDicts "扩展词库, 约5MB"
    Download-RemoteFile "$RimeIceBaseUrl/cn_dicts" "tencent.dict.yaml" $RimePinyinCnDicts "腾讯词频, 约17MB"

    # 英文词库 (rime-ice)
    Write-Detail "  英文词库 (rime-ice):"
    $RimeEnglishDir = Join-Path $ScriptDir ".cache\rime\en_dicts"
    if (-not (Test-Path $RimeEnglishDir)) { New-Item -ItemType Directory -Path $RimeEnglishDir -Force | Out-Null }
    Download-RemoteFile "$RimeIceBaseUrl/en_dicts" "en.dict.yaml" $RimeEnglishDir "英文主词库, 约350KB"
    Download-RemoteFile "$RimeIceBaseUrl/en_dicts" "en_ext.dict.yaml" $RimeEnglishDir "英文扩展词库, 约50KB"

    # 五笔词库 (rime-wubi86-jidian)
    # 主词库 wubi86_jidian.dict.yaml 下载后由 dictgen 工具重新排序，输出到 data/schemas/wubi86/
    Write-Detail "  五笔词库 (rime-wubi86-jidian):"
    $RimeWubiDir = Join-Path $ScriptDir ".cache\rime-wubi"
    if (-not (Test-Path $RimeWubiDir)) { New-Item -ItemType Directory -Path $RimeWubiDir -Force | Out-Null }
    $RimeWubiUrl = "https://raw.githubusercontent.com/KyleBing/rime-wubi86-jidian/master"

    Download-RemoteFile $RimeWubiUrl "wubi86_jidian.dict.yaml" $RimeWubiDir "主词库"
    Download-RemoteFile $RimeWubiUrl "wubi86_jidian_extra.dict.yaml" $RimeWubiDir "扩展词库"
    Download-RemoteFile $RimeWubiUrl "wubi86_jidian_extra_district.dict.yaml" $RimeWubiDir "行政区域词库"
    # Download-RemoteFile $RimeWubiUrl "wubi86_jidian_user.dict.yaml" $RimeWubiDir "用户词库模板"
    Write-DetailLine
}

function Prepare-DataFiles {
    Write-Step "准备词库和方案文件..."
    $DataDir = Join-Path $BuildDir "data"
    if (-not (Test-Path $DataDir)) { New-Item -ItemType Directory -Path $DataDir -Force | Out-Null }

    # 词库统一放在 schemas/<方案名>/ 目录下
    $schemasDir = Join-Path $DataDir "schemas"
    if (-not (Test-Path $schemasDir)) { New-Item -ItemType Directory -Path $schemasDir -Force | Out-Null }
    $pinyinDir = Join-Path $schemasDir "pinyin"
    $wubiDir = Join-Path $schemasDir "wubi86"
    if (-not (Test-Path $pinyinDir)) { New-Item -ItemType Directory -Path $pinyinDir -Force | Out-Null }
    if (-not (Test-Path $wubiDir)) { New-Item -ItemType Directory -Path $wubiDir -Force | Out-Null }

    # 复制拼音词库
    $RimePinyinDir = Join-Path $ScriptDir ".cache\rime"
    $RimePinyinCnDicts = Join-Path $RimePinyinDir "cn_dicts"
    $pinyinCnDictsDir = Join-Path $pinyinDir "cn_dicts"
    if (-not (Test-Path $pinyinCnDictsDir)) { New-Item -ItemType Directory -Path $pinyinCnDictsDir -Force | Out-Null }

    $rimeIceMain = Join-Path $RimePinyinDir "rime_ice.dict.yaml"
    if (Test-Path $rimeIceMain) {
        Copy-Item -Path $rimeIceMain -Destination (Join-Path $pinyinDir "rime_ice.dict.yaml") -Force
        Write-Detail "  - 已复制拼音词库入口 rime_ice.dict.yaml"
    } else {
        Write-Host "[警告] 未找到 rime_ice.dict.yaml" -ForegroundColor Yellow
    }

    $pinyinDictFiles = @("8105.dict.yaml", "base.dict.yaml", "ext.dict.yaml")
    foreach ($df in $pinyinDictFiles) {
        $src = Join-Path $RimePinyinCnDicts $df
        if (Test-Path $src) {
            Copy-Item -Path $src -Destination (Join-Path $pinyinCnDictsDir $df) -Force
            Write-Detail "  - 已复制 cn_dicts/$df"
        } else {
            Write-Host "[警告] 未找到 cn_dicts/$df" -ForegroundColor Yellow
        }
    }

    # 生成 Unigram 语言模型
    $unigramSrcDir = Join-Path $ScriptDir ".cache\pinyin"
    $unigramPath = Join-Path $unigramSrcDir "unigram.txt"
    if (-not (Test-Path $unigramSrcDir)) { New-Item -ItemType Directory -Path $unigramSrcDir -Force | Out-Null }
    if (-not (Test-Path $unigramPath)) {
        Write-Detail "  - 生成 Unigram 语言模型..."
        Push-Location (Join-Path $ScriptDir "wind_input")
        try {
            & go run ./cmd/gen_unigram -rime $RimePinyinCnDicts -output $unigramPath
            if ($LASTEXITCODE -ne 0) {
                Write-Host "[警告] Unigram 生成失败,智能组句功能不可用" -ForegroundColor Yellow
            } else {
                Write-Detail "  - Unigram 语言模型生成成功"
            }
        } finally {
            Pop-Location
        }
    } else {
        Write-Detail "  - Unigram 语言模型已存在"
    }

    # 复制 Unigram
    if (Test-Path $unigramPath) {
        Copy-Item -Path $unigramPath -Destination (Join-Path $pinyinDir "unigram.txt") -Force
        Write-Detail "  - 已复制 Unigram 语言模型"
    } else {
        Write-Host "[提示] Unigram 语言模型不存在,智能组句功能不可用" -ForegroundColor Cyan
    }

    # 生成五笔主词库（dictgen：按 unigram 词频重新排序，直接输出到 build 目录）
    $jidianCachePath = Join-Path $ScriptDir ".cache\rime-wubi\wubi86_jidian.dict.yaml"
    if (Test-Path $jidianCachePath) {
        Write-Detail "  - 生成五笔主词库（dictgen）..."
        $dictgenOutput = Join-Path $wubiDir "wubi86_jidian.dict.yaml"
        Push-Location (Join-Path $ScriptDir "wind_input")
        try {
            & go run ./tools/dictgen -config tools/dictgen/dictgen.yaml -output $dictgenOutput
            if ($LASTEXITCODE -ne 0) {
                Write-Host "[警告] dictgen 生成失败，五笔主词库缺失" -ForegroundColor Yellow
            } else {
                Write-Detail "  - 五笔主词库生成成功"
            }
        } finally {
            Pop-Location
        }
    } else {
        Write-Host "[警告] 未找到 jidian 原始词库，跳过 dictgen 生成" -ForegroundColor Yellow
    }

    # 复制五笔词库 (rime-wubi86-jidian)
    # 注：wubi86_jidian_extra.dict.yaml 已由 dictgen 拆分生成（cjk/emoji/english/symbols），不再从 cache 复制
    $RimeWubiDir = Join-Path $ScriptDir ".cache\rime-wubi"
    $wubiFiles = @(
        "wubi86_jidian_extra_district.dict.yaml",
        "wubi86_jidian_user.dict.yaml"
    )
    $wubiCopied = 0
    foreach ($wf in $wubiFiles) {
        $wubiSrc = Join-Path $RimeWubiDir $wf
        if (Test-Path $wubiSrc) {
            Copy-Item -Path $wubiSrc -Destination (Join-Path $wubiDir $wf) -Force
            $wubiCopied++
        }
    }
    if ($wubiCopied -gt 0) {
        Write-Detail "  - 已复制五笔词库 ($wubiCopied 个文件)"
    } else {
        Write-Host "[警告] 未找到五笔词库文件" -ForegroundColor Yellow
    }

    # 复制英文词库
    $RimeEnglishDir = Join-Path $ScriptDir ".cache\rime\en_dicts"
    $englishDir = Join-Path $schemasDir "english"
    if (-not (Test-Path $englishDir)) { New-Item -ItemType Directory -Path $englishDir -Force | Out-Null }
    $englishFiles = @("en.dict.yaml", "en_ext.dict.yaml")
    $englishCopied = 0
    foreach ($ef in $englishFiles) {
        $engSrc = Join-Path $RimeEnglishDir $ef
        if (Test-Path $engSrc) {
            Copy-Item -Path $engSrc -Destination (Join-Path $englishDir $ef) -Force
            $englishCopied++
        }
    }
    if ($englishCopied -gt 0) {
        Write-Detail "  - 已复制英文词库 ($englishCopied 个文件)"
    } else {
        Write-Host "[警告] 未找到英文词库文件" -ForegroundColor Yellow
    }

    # 复制预制数据文件（data/ → build/data/）
    # 黑名单方式：除排除项外的所有文件都会被复制，新增配置/补丁文件无需修改此脚本
    $dataSourceDir = Join-Path $ScriptDir "data"
    $excludeNames = @("AGENTS.md")
    $dataCopied = 0
    Get-ChildItem -Path $dataSourceDir -Recurse -File | Where-Object { $excludeNames -notcontains $_.Name } | ForEach-Object {
        $relativePath = $_.FullName.Substring($dataSourceDir.Length + 1)
        $destPath = Join-Path $DataDir $relativePath
        $destDir = Split-Path $destPath -Parent
        if (-not (Test-Path $destDir)) { New-Item -ItemType Directory -Path $destDir -Force | Out-Null }
        Copy-Item -Path $_.FullName -Destination $destPath -Force
        $dataCopied++
    }
    Write-Detail "  - 已复制预制数据文件 ($dataCopied 个文件)"

    # 复制主题文件
    Write-Detail "  - 复制主题文件..."
    $themesSrc = Join-Path $ScriptDir "wind_input\themes"
    $themesDst = Join-Path $DataDir "themes"
    if (Test-Path $themesSrc) {
        Get-ChildItem -Path $themesSrc -Directory | ForEach-Object {
            $themeYaml = Join-Path $_.FullName "theme.yaml"
            if (Test-Path $themeYaml) {
                $destDir = Join-Path $themesDst $_.Name
                if (-not (Test-Path $destDir)) { New-Item -ItemType Directory -Path $destDir -Force | Out-Null }
                Copy-Item -Path $themeYaml -Destination $destDir -Force
                Write-Detail "    - $($_.Name)"
            }
        }
        Write-Detail "  - 主题文件复制完成"
    } else {
        Write-Host "[警告] 未找到主题目录" -ForegroundColor Yellow
    }
    Write-DetailLine
}

# ============================================================
# 执行构建
# ============================================================

if ($BuildService) { Build-GoService }
if ($BuildDll) { Build-CppDll }
if ($BuildSetting) { Build-SettingUI }
if ($BuildPortable) { Build-PortableLauncher }
if ($BuildAll) {
    Download-Dictionaries
    Prepare-DataFiles
}

# ============================================================
# 检查输出文件
# ============================================================

$buildDirLabel = if ($DebugVariant) { "build_debug" } else { "build" }
$dllLabel = if ($DebugVariant) { "wind_tsf_debug.dll" } else { "wind_tsf.dll" }
$dllX86Label = if ($DebugVariant) { "wind_tsf_debug_x86.dll" } else { "wind_tsf_x86.dll" }
$exeLabel = if ($DebugVariant) { "wind_input_debug.exe" } else { "wind_input.exe" }
$settingLabel = if ($DebugVariant) { "wind_setting_debug.exe" } else { "wind_setting.exe" }
$portableLabel = "wind_portable.exe"

$checkFiles = @()
if ($BuildDll) { $checkFiles += $dllLabel, $dllX86Label }
if ($BuildService) { $checkFiles += $exeLabel }
if ($BuildSetting -and $WailsMode -ne "skip") { $checkFiles += $settingLabel }
if ($BuildPortable) { $checkFiles += $portableLabel }

foreach ($f in $checkFiles) {
    if (-not (Test-Path (Join-Path $BuildDir $f))) {
        Write-Host "[错误] 未找到 $f" -ForegroundColor Red
        exit 1
    }
}

# ============================================================
# 构建摘要
# ============================================================

Write-Host ""
if ($Brief) {
    $outputList = if ($checkFiles.Count -gt 0) {
        ($checkFiles | ForEach-Object { "$buildDirLabel\$_" }) -join ", "
    } else {
        "无需要检查的输出文件"
    }
    Write-Host "构建完成: $outputList"
    if ($BuildAll) {
        Write-Host "数据文件已准备: $buildDirLabel\data"
    }
    exit 0
}

Write-Host "======================================"
Write-Host "构建完成！"
Write-Host "======================================"
Write-Host ""
Write-Host "输出文件:"
foreach ($f in $checkFiles) {
    Write-Host "- $buildDirLabel\$f"
}

if ($BuildAll) {
    Write-Host "- $buildDirLabel\data\schemas\*.schema.yaml（输入方案配置）"
    Write-Host "- $buildDirLabel\data\schemas\pinyin\*.dict.yaml（拼音词库）"
    Write-Host "- $buildDirLabel\data\schemas\pinyin\unigram.txt（Unigram 语言模型）"
    Write-Host "- $buildDirLabel\data\schemas\wubi86\wubi86_jidian*.dict.yaml（五笔词库）"
    Write-Host "- $buildDirLabel\data\schemas\common_chars.txt（常用字表）"
    Write-Host "- $buildDirLabel\data\config.yaml（默认配置）"
    Write-Host "- $buildDirLabel\data\system.phrases.yaml（系统短语配置）"
    Write-Host "- $buildDirLabel\data\themes\*\theme.yaml（主题配置）"
    Write-Host ""
    Write-Host "注: .wdb 二进制词库由运行时按需自动生成并缓存"
    Write-Host ""
    Write-Host "词库来源:"
    Write-Host "  拼音: 雾凇拼音 rime-ice (https://github.com/iDvel/rime-ice)"
    Write-Host "  五笔: 极点五笔 rime-wubi86-jidian (https://github.com/KyleBing/rime-wubi86-jidian)，主词库由 wind_input/tools/dictgen 按 unigram 词频重新排序"
    Write-Host ""
    Write-Host "开发调试:"
    Write-Host "  cd $buildDirLabel; .\$exeLabel -log debug"
    Write-Host ""
    Write-Host "安装:"
    if ($DebugVariant) {
        Write-Host "  以管理员身份运行 installer\install.ps1 -DebugVariant"
    } else {
        Write-Host "  以管理员身份运行 installer\install.ps1"
    }
}

Write-Host ""
Write-Host "用法:"
Write-Host "  .\build_all.ps1                                (全量构建, 默认 debug)"
Write-Host "  .\build_all.ps1 -WailsMode release             (全量构建, release)"
Write-Host "  .\build_all.ps1 -WailsMode skip                (全量构建, 跳过设置界面)"
Write-Host "  .\build_all.ps1 -Module dll                    (仅构建 TSF DLL)"
Write-Host "  .\build_all.ps1 -Module service                (仅构建 GO 服务)"
Write-Host "  .\build_all.ps1 -Module setting                (仅构建设置界面)"
Write-Host "  .\build_all.ps1 -Module portable               (仅构建便携启动器)"
Write-Host "  .\build_all.ps1 -Module dll,service            (构建 DLL + 服务)"
Write-Host "  .\build_all.ps1 -DebugVariant                  (调试版变体)"
exit 0
