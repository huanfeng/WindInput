param(
    [ValidateSet("debug", "release", "skip")]
    [string]$WailsMode = "debug"
)

$ErrorActionPreference = "Stop"

Write-Host "======================================"
Write-Host "WindInput - Build All"
Write-Host "======================================"
Write-Host ""

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$BuildDir = Join-Path $ScriptDir "build"

# [1/6] 构建 Go 服务
Write-Host "[1/6] 构建 Go 服务(wind_input.exe)..."
if (-not (Test-Path $BuildDir)) { New-Item -ItemType Directory -Path $BuildDir -Force | Out-Null }
Push-Location (Join-Path $ScriptDir "wind_input")
try {
    & go build -ldflags "-H windowsgui" -o (Join-Path $BuildDir "wind_input.exe") ./cmd/service
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
            Write-Host "[警告] 未找到 Wails CLI,已跳过 wind_setting 构建" -ForegroundColor Yellow
            Write-Host "       安装命令: go install github.com/wailsapp/wails/v2/cmd/wails@latest"
        } else {
            if ($WailsMode -eq "debug") {
                & wails build -debug
            } else {
                & wails build
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

# [4/6] 下载拼音词库
Write-Host "[4/6] 下载拼音词库(rime-ice)..."
$RimeDir = Join-Path $ScriptDir ".cache\rime"
if (-not (Test-Path $RimeDir)) { New-Item -ItemType Directory -Path $RimeDir -Force | Out-Null }

$RimeBaseUrl = "https://raw.githubusercontent.com/iDvel/rime-ice/main/cn_dicts"

function Download-RimeFile {
    param([string]$FileName, [string]$Description)
    $targetPath = Join-Path $RimeDir $FileName
    if (Test-Path $targetPath) {
        Write-Host "  - $FileName 已存在,跳过下载"
        return
    }
    Write-Host "  - 下载 $FileName ($Description)..."
    try {
        Invoke-WebRequest -Uri "$RimeBaseUrl/$FileName" -OutFile $targetPath -UseBasicParsing
    } catch {
        Write-Host "[错误] 下载 $FileName 失败" -ForegroundColor Red
        exit 1
    }
}

Download-RimeFile "8105.dict.yaml" "单字词库, 约112KB"
Download-RimeFile "base.dict.yaml" "基础词库, 约16MB"
Download-RimeFile "tencent.dict.yaml" "腾讯词频, 约17MB"
Write-Host ""

# [5/6] 准备词库文件
Write-Host "[5/6] 准备词库文件..."
$pinyinDir = Join-Path $BuildDir "dict\pinyin"
$wubiDir = Join-Path $BuildDir "dict\wubi86"
if (-not (Test-Path $pinyinDir)) { New-Item -ItemType Directory -Path $pinyinDir -Force | Out-Null }
if (-not (Test-Path $wubiDir)) { New-Item -ItemType Directory -Path $wubiDir -Force | Out-Null }

# 复制拼音词库
$rime8105 = Join-Path $RimeDir "8105.dict.yaml"
if (Test-Path $rime8105) {
    Copy-Item -Path $rime8105 -Destination (Join-Path $pinyinDir "8105.dict.yaml") -Force
    Write-Host "  - 已复制拼音单字词库 8105.dict.yaml"
} else {
    Write-Host "[警告] 未找到 8105.dict.yaml" -ForegroundColor Yellow
}

$rimeBase = Join-Path $RimeDir "base.dict.yaml"
if (Test-Path $rimeBase) {
    Copy-Item -Path $rimeBase -Destination (Join-Path $pinyinDir "base.dict.yaml") -Force
    Write-Host "  - 已复制拼音基础词库 base.dict.yaml"
} else {
    Write-Host "[警告] 未找到 base.dict.yaml" -ForegroundColor Yellow
}

# 生成 Unigram 语言模型
$unigramSrcDir = Join-Path $ScriptDir "dict\pinyin"
$unigramPath = Join-Path $unigramSrcDir "unigram.txt"
if (-not (Test-Path $unigramSrcDir)) { New-Item -ItemType Directory -Path $unigramSrcDir -Force | Out-Null }
if (-not (Test-Path $unigramPath)) {
    Write-Host "  - 生成 Unigram 语言模型..."
    Push-Location (Join-Path $ScriptDir "wind_input")
    try {
        & go run ./cmd/gen_unigram -rime $RimeDir -output $unigramPath
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

# 复制五笔词库
$wubiSrc = Join-Path $ScriptDir "ref\极爽词库6.txt"
if (Test-Path $wubiSrc) {
    Copy-Item -Path $wubiSrc -Destination (Join-Path $wubiDir "wubi86.txt") -Force
    Write-Host "  - 已复制五笔词库"
} else {
    Write-Host "[警告] ref 目录中未找到五笔词库" -ForegroundColor Yellow
}

# 复制常用字表
$commonChars = Join-Path $ScriptDir "dict\common_chars.txt"
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
Write-Host "- build\dict\wubi86\wubi86.txt（五笔词库）"
Write-Host "- build\dict\common_chars.txt（常用字表）"
Write-Host "- build\schemas\*.schema.yaml（输入方案配置）"
Write-Host "- build\themes\*\theme.yaml（主题配置）"
Write-Host ""
Write-Host "注: .wdb 二进制词库由运行时按需自动生成并缓存"
Write-Host ""
Write-Host "词库来源: 雾凇拼音 rime-ice (https://github.com/iDvel/rime-ice)"
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
