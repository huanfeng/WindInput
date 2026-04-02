param(
    [ValidateSet("debug", "release", "skip")]
    [string]$WailsMode = "debug",

    [switch]$SettingOnly,

    [switch]$DebugVariant
)

$ErrorActionPreference = "Stop"

Write-Host "======================================"
Write-Host "WindInput - Build All"
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
$VersionCore = ($AppVersion -split '-')[0]  # 去除预发布标签
$VersionParts = $VersionCore -split '\.'
$VerMajor = "0"; $VerMinor = "0"; $VerPatch = "0"
if ($VersionParts.Length -ge 1) { $VerMajor = $VersionParts[0] }
if ($VersionParts.Length -ge 2) { $VerMinor = $VersionParts[1] }
if ($VersionParts.Length -ge 3) { $VerPatch = $VersionParts[2] }

# 生成构建号（基于 git commit 数量，每次编译自动变化）
$VerBuild = "0"
try {
    $commitCount = git -C $ScriptDir rev-list --count HEAD 2>$null
    if ($commitCount) { $VerBuild = $commitCount.Trim() }
} catch { }
$AppVersionNum = "$VerMajor.$VerMinor.$VerPatch.$VerBuild"

Write-Host "版本: $AppVersion (构建号: $AppVersionNum)"
Write-Host ""

# SettingOnly 模式：只构建设置界面
if ($SettingOnly) {
    Write-Host "[SettingOnly] 仅构建设置界面..."
    Write-Host ""

    # 更新 wails.json 中的版本号
    $wailsJsonPath = Join-Path $ScriptDir "wind_setting\wails.json"
    if (Test-Path $wailsJsonPath) {
        $wailsJson = Get-Content $wailsJsonPath -Raw -Encoding UTF8 | ConvertFrom-Json
        if (-not $wailsJson.info) {
            $wailsJson | Add-Member -NotePropertyName "info" -NotePropertyValue ([PSCustomObject]@{
                companyName = "清风输入法"
                productName = "清风输入法 设置"
                productVersion = $VersionCore
                copyright = "Copyright © 2026 清风输入法"
                comments = "清风输入法设置工具"
            }) -Force
        } else {
            $wailsJson.info | Add-Member -NotePropertyName "productVersion" -NotePropertyValue $VersionCore -Force
        }
        $jsonText = $wailsJson | ConvertTo-Json -Depth 10
        [System.IO.File]::WriteAllText($wailsJsonPath, $jsonText, (New-Object System.Text.UTF8Encoding $false))
    }

    $settingLdflags = "-X main.version=$AppVersion"
    if ($DebugVariant) {
        $settingLdflags += " -X github.com/huanfeng/wind_input/pkg/buildvariant.variant=debug"
    }
    $settingDstName = if ($DebugVariant) { "wind_setting_debug.exe" } else { "wind_setting.exe" }

    Push-Location (Join-Path $ScriptDir "wind_setting")
    try {
        if (-not (Get-Command wails -ErrorAction SilentlyContinue)) {
            Write-Host "[错误] 未找到 Wails CLI" -ForegroundColor Red
            exit 1
        }
        if ($WailsMode -eq "debug") {
            & wails build -debug -ldflags $settingLdflags
        } else {
            & wails build -ldflags $settingLdflags
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
        Write-Host "设置界面构建成功 ($WailsMode 模式)"
    } finally {
        Pop-Location
    }
    Write-Host ""
    $buildDirName = if ($DebugVariant) { "build_debug" } else { "build" }
    Write-Host "输出: $buildDirName\$settingDstName"
    exit 0
}

# [1/6] 构建 Go 服务
$goExeName = if ($DebugVariant) { "wind_input_debug.exe" } else { "wind_input.exe" }
Write-Host "[1/6] 构建 Go 服务($goExeName)..."
if (-not (Test-Path $BuildDir)) { New-Item -ItemType Directory -Path $BuildDir -Force | Out-Null }
Push-Location (Join-Path $ScriptDir "wind_input")
try {
    # 生成版本资源文件 (.syso)
    Push-Location "cmd/service"
    if (Get-Command go-winres -ErrorAction SilentlyContinue) {
        & go-winres make --product-version "$AppVersion" --file-version "$AppVersionNum"
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[警告] go-winres 生成资源失败，继续构建（无版本信息）" -ForegroundColor Yellow
        }
    } else {
        Write-Host "[警告] go-winres 未安装，跳过版本资源生成" -ForegroundColor Yellow
    }
    Pop-Location

    $goLdflags = "-H windowsgui -X main.version=$AppVersion"
    if ($DebugVariant) {
        $goLdflags += " -X github.com/huanfeng/wind_input/pkg/buildvariant.variant=debug"
    }
    & go build -ldflags $goLdflags -o (Join-Path $BuildDir $goExeName) ./cmd/service
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[错误] Go 构建失败" -ForegroundColor Red
        exit 1
    }
} finally {
    Pop-Location
}
Write-Host "Go 服务构建成功"
Write-Host ""

# [2/6] 构建 C++ DLL
$dllName = if ($DebugVariant) { "wind_tsf_debug.dll" } else { "wind_tsf.dll" }
Write-Host "[2/6] 构建 C++ DLL ($dllName)..."
$cppBuildDir = if ($DebugVariant) { Join-Path $ScriptDir "wind_tsf\build_debug" } else { Join-Path $ScriptDir "wind_tsf\build" }
if (-not (Test-Path $cppBuildDir)) { New-Item -ItemType Directory -Path $cppBuildDir -Force | Out-Null }
Push-Location $cppBuildDir
try {
    # 删除旧缓存确保版本变量正确更新
    if (Test-Path (Join-Path $cppBuildDir "CMakeCache.txt")) {
        Remove-Item (Join-Path $cppBuildDir "CMakeCache.txt") -Force
        Remove-Item (Join-Path $cppBuildDir "CMakeFiles") -Recurse -Force -ErrorAction SilentlyContinue
    }
    $cmakeArgs = @("..", "-DAPP_VERSION_MAJOR=$VerMajor", "-DAPP_VERSION_MINOR=$VerMinor", "-DAPP_VERSION_PATCH=$VerPatch", "-DAPP_VERSION_BUILD=$VerBuild", "-DAPP_VERSION_STR=$VersionCore")
    if ($DebugVariant) {
        $cmakeArgs += "-DWIND_DEBUG_VARIANT=ON"
    }
    & cmake @cmakeArgs
    if ($LASTEXITCODE -ne 0) { Write-Host "[错误] CMake 配置失败" -ForegroundColor Red; exit 1 }
    & cmake --build . --config Release
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[错误] C++ 构建失败" -ForegroundColor Red
        exit 1
    }
} finally {
    Pop-Location
}

# 确保 DLL 在正确的输出目录中
if (-not (Test-Path (Join-Path $BuildDir $dllName))) {
    # CMake 可能将 DLL 输出到其 build 目录下，手动复制
    $cmakeDllRelease = Join-Path $cppBuildDir "Release\$dllName"
    $cmakeDllRoot = Join-Path $cppBuildDir $dllName
    if (Test-Path $cmakeDllRelease) {
        Copy-Item -Path $cmakeDllRelease -Destination $BuildDir -Force
    } elseif (Test-Path $cmakeDllRoot) {
        Copy-Item -Path $cmakeDllRoot -Destination $BuildDir -Force
    } else {
        Write-Host "[错误] C++ 构建完成但 $dllName 未找到" -ForegroundColor Red
        exit 1
    }
}
Write-Host "C++ DLL 构建成功"
Write-Host ""

# [3/6] 构建设置界面
$settingDstName = if ($DebugVariant) { "wind_setting_debug.exe" } else { "wind_setting.exe" }
Write-Host "[3/6] 构建设置界面($settingDstName)..."
if ($WailsMode -eq "skip") {
    Write-Host "[提示] 已按参数跳过 Wails 构建"
} else {
    Push-Location (Join-Path $ScriptDir "wind_setting")
    try {
        # 更新 wails.json 中的版本号
        $wailsJsonPath = Join-Path $ScriptDir "wind_setting\wails.json"
        if (Test-Path $wailsJsonPath) {
            $wailsJson = Get-Content $wailsJsonPath -Raw -Encoding UTF8 | ConvertFrom-Json
            $productDisplayName = if ($DebugVariant) { "清风输入法 Debug 设置" } else { "清风输入法 设置" }
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

        if (-not (Get-Command wails -ErrorAction SilentlyContinue)) {
            Write-Host "[错误] 未找到 Wails CLI,无法构建 wind_setting" -ForegroundColor Red
            Write-Host "       请先安装: go install github.com/wailsapp/wails/v2/cmd/wails@latest" -ForegroundColor Red
            Write-Host "       如需跳过此步骤,请使用: .\build_all.ps1 -WailsMode skip" -ForegroundColor Yellow
            exit 1
        } else {
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
                Write-Host "[错误] wind_setting 构建失败,终止后续流程。" -ForegroundColor Red
                exit 1
            }
            $settingExe = Join-Path $ScriptDir "wind_setting\build\bin\wind_setting.exe"
            if (-not (Test-Path $settingExe)) {
                Write-Host "[错误] wind_setting.exe 未生成,终止后续流程。" -ForegroundColor Red
                exit 1
            }
            Copy-Item -Path $settingExe -Destination (Join-Path $BuildDir $settingDstName) -Force
            if ($WailsMode -eq "debug") {
                Write-Host "设置界面构建成功 (debug 模式,可按 F12 打开 DevTools)"
            } else {
                Write-Host "设置界面构建成功 (release 模式)"
            }
        }
    } finally {
        Pop-Location
    }
}
Write-Host ""

# [4/6] 下载词库 (rime-ice + rime-wubi86-jidian)
Write-Host "[4/6] 下载词库..."

function Download-RemoteFile {
    param([string]$BaseUrl, [string]$FileName, [string]$TargetDir, [string]$Description)
    $targetPath = Join-Path $TargetDir $FileName
    if (Test-Path $targetPath) {
        Write-Host "  - $FileName 已存在,跳过下载"
        return
    }
    Write-Host "  - 下载 $FileName ($Description)..."
    try {
        Invoke-WebRequest -Uri "$BaseUrl/$FileName" -OutFile $targetPath -UseBasicParsing
    } catch {
        Write-Host "[错误] 下载 $FileName 失败" -ForegroundColor Red
        exit 1
    }
}

# 拼音词库 (rime-ice)，按原始目录结构下载
Write-Host "  拼音词库 (rime-ice):"
$RimePinyinDir = Join-Path $ScriptDir ".cache\rime"
$RimePinyinCnDicts = Join-Path $RimePinyinDir "cn_dicts"
if (-not (Test-Path $RimePinyinDir)) { New-Item -ItemType Directory -Path $RimePinyinDir -Force | Out-Null }
if (-not (Test-Path $RimePinyinCnDicts)) { New-Item -ItemType Directory -Path $RimePinyinCnDicts -Force | Out-Null }

$RimeIceBaseUrl = "https://raw.githubusercontent.com/iDvel/rime-ice/main"
Download-RemoteFile $RimeIceBaseUrl "rime_ice.dict.yaml" $RimePinyinDir "词库入口描述文件"
Download-RemoteFile "$RimeIceBaseUrl/cn_dicts" "8105.dict.yaml" $RimePinyinCnDicts "单字词库, 约112KB"
Download-RemoteFile "$RimeIceBaseUrl/cn_dicts" "base.dict.yaml" $RimePinyinCnDicts "基础词库, 约16MB"
Download-RemoteFile "$RimeIceBaseUrl/cn_dicts" "tencent.dict.yaml" $RimePinyinCnDicts "腾讯词频, 约17MB"

# 五笔词库 (rime-wubi86-jidian)
Write-Host "  五笔词库 (rime-wubi86-jidian):"
$RimeWubiDir = Join-Path $ScriptDir ".cache\rime-wubi"
if (-not (Test-Path $RimeWubiDir)) { New-Item -ItemType Directory -Path $RimeWubiDir -Force | Out-Null }
$RimeWubiUrl = "https://raw.githubusercontent.com/KyleBing/rime-wubi86-jidian/master"

Download-RemoteFile $RimeWubiUrl "wubi86_jidian.dict.yaml" $RimeWubiDir "主词库"
Download-RemoteFile $RimeWubiUrl "wubi86_jidian_extra.dict.yaml" $RimeWubiDir "扩展词库"
Download-RemoteFile $RimeWubiUrl "wubi86_jidian_extra_district.dict.yaml" $RimeWubiDir "行政区域词库"
Download-RemoteFile $RimeWubiUrl "wubi86_jidian_user.dict.yaml" $RimeWubiDir "用户词库模板"
Write-Host ""

# [5/6] 准备词库文件
Write-Host "[5/6] 准备词库文件..."
$DataDir = Join-Path $BuildDir "data"
if (-not (Test-Path $DataDir)) { New-Item -ItemType Directory -Path $DataDir -Force | Out-Null }
$pinyinDir = Join-Path $DataDir "dict\pinyin"
$wubiDir = Join-Path $DataDir "dict\wubi86"
if (-not (Test-Path $pinyinDir)) { New-Item -ItemType Directory -Path $pinyinDir -Force | Out-Null }
if (-not (Test-Path $wubiDir)) { New-Item -ItemType Directory -Path $wubiDir -Force | Out-Null }

# 复制拼音词库（保持原始目录结构）
$pinyinCnDictsDir = Join-Path $pinyinDir "cn_dicts"
if (-not (Test-Path $pinyinCnDictsDir)) { New-Item -ItemType Directory -Path $pinyinCnDictsDir -Force | Out-Null }

$rimeIceMain = Join-Path $RimePinyinDir "rime_ice.dict.yaml"
if (Test-Path $rimeIceMain) {
    Copy-Item -Path $rimeIceMain -Destination (Join-Path $pinyinDir "rime_ice.dict.yaml") -Force
    Write-Host "  - 已复制拼音词库入口 rime_ice.dict.yaml"
} else {
    Write-Host "[警告] 未找到 rime_ice.dict.yaml" -ForegroundColor Yellow
}

$pinyinDictFiles = @("8105.dict.yaml", "base.dict.yaml")
foreach ($df in $pinyinDictFiles) {
    $src = Join-Path $RimePinyinCnDicts $df
    if (Test-Path $src) {
        Copy-Item -Path $src -Destination (Join-Path $pinyinCnDictsDir $df) -Force
        Write-Host "  - 已复制 cn_dicts/$df"
    } else {
        Write-Host "[警告] 未找到 cn_dicts/$df" -ForegroundColor Yellow
    }
}

# 生成 Unigram 语言模型
$unigramSrcDir = Join-Path $ScriptDir ".cache\pinyin"
$unigramPath = Join-Path $unigramSrcDir "unigram.txt"
if (-not (Test-Path $unigramSrcDir)) { New-Item -ItemType Directory -Path $unigramSrcDir -Force | Out-Null }
if (-not (Test-Path $unigramPath)) {
    Write-Host "  - 生成 Unigram 语言模型..."
    Push-Location (Join-Path $ScriptDir "wind_input")
    try {
        & go run ./cmd/gen_unigram -rime $RimePinyinCnDicts -output $unigramPath
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[警告] Unigram 生成失败,智能组句功能不可用" -ForegroundColor Yellow
        } else {
            Write-Host "  - Unigram 语言模型生成成功"
        }
    } finally {
        Pop-Location
    }
} else {
    Write-Host "  - Unigram 语言模型已存在"
}

# 复制 Unigram
if (Test-Path $unigramPath) {
    Copy-Item -Path $unigramPath -Destination (Join-Path $pinyinDir "unigram.txt") -Force
    Write-Host "  - 已复制 Unigram 语言模型"
} else {
    Write-Host "[提示] Unigram 语言模型不存在,智能组句功能不可用" -ForegroundColor Cyan
}

# 复制五笔词库 (rime-wubi86-jidian)
$wubiFiles = @(
    "wubi86_jidian.dict.yaml",
    "wubi86_jidian_extra.dict.yaml",
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
    Write-Host "  - 已复制五笔词库 ($wubiCopied 个文件)"
} else {
    Write-Host "[警告] 未找到五笔词库文件" -ForegroundColor Yellow
}

# 复制常用字表
$commonChars = Join-Path $ScriptDir "data\dict\common_chars.txt"
if (Test-Path $commonChars) {
    Copy-Item -Path $commonChars -Destination (Join-Path $DataDir "dict\common_chars.txt") -Force
    Write-Host "  - 已复制常用字表"
} else {
    Write-Host "[警告] 未找到常用字表" -ForegroundColor Yellow
}

# 复制输入方案配置
$schemasDir = Join-Path $DataDir "schemas"
if (-not (Test-Path $schemasDir)) { New-Item -ItemType Directory -Path $schemasDir -Force | Out-Null }
$schemaFiles = Get-ChildItem -Path (Join-Path $ScriptDir "data\schemas") -Filter "*.schema.yaml" -ErrorAction SilentlyContinue
if ($schemaFiles) {
    $schemaFiles | Copy-Item -Destination $schemasDir -Force
    Write-Host "  - 已复制输入方案配置"
} else {
    Write-Host "[警告] 未找到输入方案配置文件" -ForegroundColor Yellow
}

# 复制默认配置文件
$configYaml = Join-Path $ScriptDir "data\config.yaml"
if (Test-Path $configYaml) {
    Copy-Item -Path $configYaml -Destination (Join-Path $DataDir "config.yaml") -Force
    Write-Host "  - 已复制默认配置文件"
} else {
    Write-Host "[警告] 未找到默认配置文件" -ForegroundColor Yellow
}

# 复制系统短语配置
$systemPhrases = Join-Path $ScriptDir "data\system.phrases.yaml"
if (Test-Path $systemPhrases) {
    Copy-Item -Path $systemPhrases -Destination (Join-Path $DataDir "system.phrases.yaml") -Force
    Write-Host "  - 已复制系统短语配置"
} else {
    Write-Host "[警告] 未找到系统短语配置文件" -ForegroundColor Yellow
}

# 复制主题文件
Write-Host "  - 复制主题文件..."
$themesSrc = Join-Path $ScriptDir "wind_input\themes"
$themesDst = Join-Path $DataDir "themes"
if (Test-Path $themesSrc) {
    Get-ChildItem -Path $themesSrc -Directory | ForEach-Object {
        $themeYaml = Join-Path $_.FullName "theme.yaml"
        if (Test-Path $themeYaml) {
            $destDir = Join-Path $themesDst $_.Name
            if (-not (Test-Path $destDir)) { New-Item -ItemType Directory -Path $destDir -Force | Out-Null }
            Copy-Item -Path $themeYaml -Destination $destDir -Force
            Write-Host "    - $($_.Name)"
        }
    }
    Write-Host "  - 主题文件复制完成"
} else {
    Write-Host "[警告] 未找到主题目录" -ForegroundColor Yellow
}
Write-Host ""

# [6/6] 检查输出文件
Write-Host "[6/6] 检查输出文件..."
$checkFiles = if ($DebugVariant) { @("wind_tsf_debug.dll", "wind_input_debug.exe") } else { @("wind_tsf.dll", "wind_input.exe") }
foreach ($f in $checkFiles) {
    if (-not (Test-Path (Join-Path $BuildDir $f))) {
        Write-Host "[错误] 未找到 $f" -ForegroundColor Red
        exit 1
    }
}

$buildDirLabel = if ($DebugVariant) { "build_debug" } else { "build" }
$dllLabel = if ($DebugVariant) { "wind_tsf_debug.dll" } else { "wind_tsf.dll" }
$exeLabel = if ($DebugVariant) { "wind_input_debug.exe" } else { "wind_input.exe" }
$settingLabel = if ($DebugVariant) { "wind_setting_debug.exe" } else { "wind_setting.exe" }

Write-Host ""
Write-Host "======================================"
Write-Host "构建完成！"
Write-Host "======================================"
Write-Host ""
Write-Host "输出文件:"
Write-Host "- $buildDirLabel\$dllLabel（TSF 桥接）"
Write-Host "- $buildDirLabel\$exeLabel（输入法服务）"
Write-Host "- $buildDirLabel\$settingLabel（设置界面）"
Write-Host "- $buildDirLabel\data\dict\pinyin\*.dict.yaml（拼音词库）"
Write-Host "- $buildDirLabel\data\dict\pinyin\unigram.txt（Unigram 语言模型）"
Write-Host "- $buildDirLabel\data\dict\wubi86\wubi86_jidian*.dict.yaml（五笔词库）"
Write-Host "- $buildDirLabel\data\dict\common_chars.txt（常用字表）"
Write-Host "- $buildDirLabel\data\config.yaml（默认配置）"
Write-Host "- $buildDirLabel\data\schemas\*.schema.yaml（输入方案配置）"
Write-Host "- $buildDirLabel\data\system.phrases.yaml（系统短语配置）"
Write-Host "- $buildDirLabel\data\themes\*\theme.yaml（主题配置）"
Write-Host ""
Write-Host "注: .wdb 二进制词库由运行时按需自动生成并缓存"
Write-Host ""
Write-Host "词库来源:"
Write-Host "  拼音: 雾凇拼音 rime-ice (https://github.com/iDvel/rime-ice)"
Write-Host "  五笔: 极点五笔 rime-wubi86-jidian (https://github.com/KyleBing/rime-wubi86-jidian)"
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
Write-Host ""
Write-Host "Wails 构建选项:"
Write-Host "  .\build_all.ps1                      (默认 debug)"
Write-Host "  .\build_all.ps1 -WailsMode release"
Write-Host "  .\build_all.ps1 -WailsMode skip"
Write-Host "  .\build_all.ps1 -DebugVariant         (调试版变体)"
exit 0
