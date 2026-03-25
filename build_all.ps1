param(
    [ValidateSet("debug", "release", "skip")]
    [string]$WailsMode = "debug",

    [switch]$SettingOnly
)

$ErrorActionPreference = "Stop"

Write-Host "======================================"
Write-Host "WindInput - Build All"
Write-Host "======================================"
Write-Host ""

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$BuildDir = Join-Path $ScriptDir "build"

# 读取版本号
$VersionFile = Join-Path $ScriptDir "VERSION"
if (Test-Path $VersionFile) {
    $AppVersion = (Get-Content $VersionFile -Raw).Trim()
} else {
    $AppVersion = "dev"
}
Write-Host "版本: $AppVersion"
Write-Host ""

# SettingOnly 模式：只构建设置界面
if ($SettingOnly) {
    Write-Host "[SettingOnly] 仅构建设置界面..."
    Write-Host ""

    Push-Location (Join-Path $ScriptDir "wind_setting")
    try {
        if (-not (Get-Command wails -ErrorAction SilentlyContinue)) {
            Write-Host "[错误] 未找到 Wails CLI" -ForegroundColor Red
            exit 1
        }
        if ($WailsMode -eq "debug") {
            & wails build -debug -ldflags "-X main.version=$AppVersion"
        } else {
            & wails build -ldflags "-X main.version=$AppVersion"
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
        Copy-Item -Path $settingExe -Destination $BuildDir -Force
        Write-Host "设置界面构建成功 ($WailsMode 模式)"
    } finally {
        Pop-Location
    }
    Write-Host ""
    Write-Host "输出: build\wind_setting.exe"
    exit 0
}

# [1/6] 构建 Go 服务
Write-Host "[1/6] 构建 Go 服务(wind_input.exe)..."
if (-not (Test-Path $BuildDir)) { New-Item -ItemType Directory -Path $BuildDir -Force | Out-Null }
Push-Location (Join-Path $ScriptDir "wind_input")
try {
    & go build -ldflags "-H windowsgui -X main.version=$AppVersion" -o (Join-Path $BuildDir "wind_input.exe") ./cmd/service
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
Write-Host "[2/6] 构建 C++ DLL(wind_tsf.dll)..."
$cppBuildDir = Join-Path $ScriptDir "wind_tsf\build"
if (-not (Test-Path $cppBuildDir)) { New-Item -ItemType Directory -Path $cppBuildDir -Force | Out-Null }
Push-Location $cppBuildDir
try {
    if (-not (Test-Path (Join-Path $cppBuildDir "CMakeCache.txt"))) {
        & cmake ..
        if ($LASTEXITCODE -ne 0) { Write-Host "[错误] CMake 配置失败" -ForegroundColor Red; exit 1 }
    }
    & cmake --build . --config Release
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[错误] C++ 构建失败" -ForegroundColor Red
        exit 1
    }
} finally {
    Pop-Location
}

if (-not (Test-Path (Join-Path $BuildDir "wind_tsf.dll"))) {
    Write-Host "[错误] C++ 构建完成但 wind_tsf.dll 未生成到 build 目录" -ForegroundColor Red
    exit 1
}
if (-not (Test-Path (Join-Path $BuildDir "wind_dwrite.dll"))) {
    Write-Host "[错误] C++ 构建完成但 wind_dwrite.dll 未生成到 build 目录" -ForegroundColor Red
    exit 1
}
Write-Host "C++ DLL 构建成功"
Write-Host ""

# [3/6] 构建设置界面
Write-Host "[3/6] 构建设置界面(wind_setting.exe)..."
if ($WailsMode -eq "skip") {
    Write-Host "[提示] 已按参数跳过 Wails 构建"
} else {
    Push-Location (Join-Path $ScriptDir "wind_setting")
    try {
        if (-not (Get-Command wails -ErrorAction SilentlyContinue)) {
            Write-Host "[错误] 未找到 Wails CLI,无法构建 wind_setting" -ForegroundColor Red
            Write-Host "       请先安装: go install github.com/wailsapp/wails/v2/cmd/wails@latest" -ForegroundColor Red
            Write-Host "       如需跳过此步骤,请使用: .\build_all.ps1 -WailsMode skip" -ForegroundColor Yellow
            exit 1
        } else {
            if ($WailsMode -eq "debug") {
                & wails build -debug -ldflags "-X main.version=$AppVersion"
            } else {
                & wails build -ldflags "-X main.version=$AppVersion"
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
            Copy-Item -Path $settingExe -Destination $BuildDir -Force
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
$pinyinDir = Join-Path $BuildDir "dict\pinyin"
$wubiDir = Join-Path $BuildDir "dict\wubi86"
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
    Copy-Item -Path $commonChars -Destination (Join-Path $BuildDir "dict\common_chars.txt") -Force
    Write-Host "  - 已复制常用字表"
} else {
    Write-Host "[警告] 未找到常用字表" -ForegroundColor Yellow
}

# 复制输入方案配置
$schemasDir = Join-Path $BuildDir "schemas"
if (-not (Test-Path $schemasDir)) { New-Item -ItemType Directory -Path $schemasDir -Force | Out-Null }
$schemaFiles = Get-ChildItem -Path (Join-Path $ScriptDir "data\schemas") -Filter "*.schema.yaml" -ErrorAction SilentlyContinue
if ($schemaFiles) {
    $schemaFiles | Copy-Item -Destination $schemasDir -Force
    Write-Host "  - 已复制输入方案配置"
} else {
    Write-Host "[警告] 未找到输入方案配置文件" -ForegroundColor Yellow
}

# 复制主题文件
Write-Host "  - 复制主题文件..."
$themesSrc = Join-Path $ScriptDir "wind_input\themes"
$themesDst = Join-Path $BuildDir "themes"
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
$checkFiles = @("wind_tsf.dll", "wind_dwrite.dll", "wind_input.exe")
foreach ($f in $checkFiles) {
    if (-not (Test-Path (Join-Path $BuildDir $f))) {
        Write-Host "[错误] 未找到 $f" -ForegroundColor Red
        exit 1
    }
}

Write-Host ""
Write-Host "======================================"
Write-Host "构建完成！"
Write-Host "======================================"
Write-Host ""
Write-Host "输出文件:"
Write-Host "- build\wind_tsf.dll（TSF 桥接）"
Write-Host "- build\wind_dwrite.dll（DirectWrite 渲染 Shim）"
Write-Host "- build\wind_input.exe（输入法服务）"
Write-Host "- build\wind_setting.exe（设置界面）"
Write-Host "- build\dict\pinyin\8105.dict.yaml（拼音单字词库）"
Write-Host "- build\dict\pinyin\base.dict.yaml（拼音基础词库）"
Write-Host "- build\dict\pinyin\unigram.txt（Unigram 语言模型）"
Write-Host "- build\dict\wubi86\wubi86_jidian*.dict.yaml（五笔词库, rime 格式）"
Write-Host "- build\dict\common_chars.txt（常用字表）"
Write-Host "- build\schemas\*.schema.yaml（输入方案配置）"
Write-Host "- build\themes\*\theme.yaml（主题配置）"
Write-Host ""
Write-Host "注: .wdb 二进制词库由运行时按需自动生成并缓存"
Write-Host ""
Write-Host "词库来源:"
Write-Host "  拼音: 雾凇拼音 rime-ice (https://github.com/iDvel/rime-ice)"
Write-Host "  五笔: 极点五笔 rime-wubi86-jidian (https://github.com/KyleBing/rime-wubi86-jidian)"
Write-Host ""
Write-Host "开发调试:"
Write-Host "  cd build; .\wind_input.exe -log debug"
Write-Host ""
Write-Host "安装:"
Write-Host "  以管理员身份运行 installer\install.ps1"
Write-Host ""
Write-Host "Wails 构建选项:"
Write-Host "  .\build_all.ps1                      (默认 debug)"
Write-Host "  .\build_all.ps1 -WailsMode release"
Write-Host "  .\build_all.ps1 -WailsMode skip"
exit 0
