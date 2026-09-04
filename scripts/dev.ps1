# WindInput 开发菜单 (Windows 原生构建 / MSVC)
#
# 用法:
#   .\scripts\dev.ps1            # 交互式菜单 (对齐 dev.sh)
#   .\scripts\dev.ps1 <命令>     # 非交互直调, 如 .\scripts\dev.ps1 release
#   (dev.bat 已转发 %*, 故 dev.bat release / dev.bat m2 等价)
#
# 本机 (Windows) 原生构建:
#   - Rust(wind_input): cargo build --release (host = x86_64-pc-windows-msvc)
#   - Rust(../wind-portable): 独立仓库, 不存在则跳过便携启动器
#   - Rust(../wind-setting):  独立仓库, 不存在则跳过设置程序
#   - C++ TSF: CMake + "Visual Studio 17 2022" 生成器 (x64 + Win32, 自动定位 MSVC)
#   - 词库数据: 下载 rime-frost/pinyin-data/OpenCC + 生成 unigram/pinyin_map + 编 octrie
#   - 全构建产物落【产品根】build/(release) 或 build_dev/(dev); 内容 == 安装内容
#
# 命令 (菜单与命令行直调同一套; 前缀 d=dev, p=push/部署, m=单模块):
#   1            Release 全构建: wind_input + tsf(x64/x86) + setting + portable + 词库数据 → build/
#   d1           Dev 全构建 → build_dev/
#   m1 / dm1     仅 tsf (x64+x86)            release / dev
#   m2 / dm2     仅 wind_input (核心 exe)     release / dev
#   m3 / dm3     仅 wind_setting (../wind-setting)              release / dev (不存在则跳过)
#   m4 / dm4     仅 wind_portable (绿色版, ../wind-portable)   release / dev (不存在则跳过)
#   p1 / pd1     系统安装全部 (release / dev): 复制 + 注册 TSF + 开机自启 + 启动服务
#   u1/u / ud1/ud  系统卸载全部 (release / dev): 反注册 + 移出输入法列表 + 移除自启 + 删目录
#   pm1..pm4     系统安装单模块 (tsf / 核心 / setting / portable, release)
#   pdm1..pdm4   系统安装单模块 (dev)
#   pb1 / pbd1   便携部署全部 (release / dev): 纯复制 + 写便携标记 + 启动 wind_portable
#   pbm1..pbm4   便携部署单模块 (release)      pbdm1..pbdm4  便携部署单模块 (dev)
#   ub1/ub / ubd1/ubd  便携卸载 (release / dev): 停进程 + 删程序文件 (userdata\ 保留)
#   8  / d8      生成安装包 (release / dev): 全构建 + wind-installer 打包 → dist\*-Setup.exe
#   8s / d8s     生成安装包 (跳过重建, 直接打包现有 build[_dev]/)
#   9  / d9      生成便携包 (release / dev): 全构建 + 打 zip → dist\*-Portable-<版本>.zip
#                (免安装; 不依赖 wind-installer; 内含便携标记, 不含 userdata\)
#   9s / d9s     生成便携包 (跳过重建, 直接打包现有 build[_dev]/)
#   k=check  l=clippy  t=test  f=fmt  fmt-check  ci(=fmt+clippy+test)  hooks(=激活pre-commit)  clean
#   gd=gen-data  r=repl
#   av           配置 Defender 编译排除项 (自动 UAC 提权; 详见 scripts\defender-exclusions.ps1)
#   avc          仅预览排除项改动 (免管理员)      avr  移除本脚本添加的排除项
#   wtinit       新 worktree 首次全量构建 (sccache 过首关 + 切回 incremental; 实测省 30%)
#
# 部署目标 (在 scripts\deploy.local.ps1 覆盖, PowerShell 赋值格式):
#   ── 系统安装 (注册 COM/写自启; 默认在 Program Files 下, 部署自动 UAC 提权)
#   WIND_DIR_RELEASE          = C:\Program Files\WindInput      # p1 / pm* 目标
#   WIND_DIR_DEV              = C:\Program Files\WindInputDev   # pd1 / pdm* 目标
#   ── 便携部署 (纯复制, 免注册免自启; 无需管理员)
#   WIND_DIR_PORTABLE_RELEASE = D:\WindInputPortable            # pb1 / pbm* 目标
#   WIND_DIR_PORTABLE_DEV     = D:\WindInputPortableDev         # pbd1 / pbdm* 目标
#   注: 便携目录【不可】落在 Program Files\ 或 Windows\ 下 —— wind_portable 的
#       is_protected_dir (layout.rs) 会拒绝在系统保护目录启动便携模式。
#   注: 便携用户数据在 <便携目录>\userdata\, 重新部署与卸载均保留该目录。
#
# 数据目录说明:
#   data/                源文件(入库): 配置、五笔词库、主题等手工维护文件
#   .cache/              外部下载/生成(gitignore): rime-frost、opencc、unigram 等
#   build/ build_dev/  全构建产物(gitignore); 内容即部署到目标目录的内容

param(
    # 支持连续命令: .\dev.ps1 d1 pd1 (前者失败则后者不执行)
    # repl 命令后接数据路径: .\dev.ps1 r build_dev/data
    [Parameter(Position = 0, ValueFromRemainingArguments)] [string[]]$Commands = @()
)

$ErrorActionPreference = "Stop"

# ---------- 路径 ----------
# 目录层级: <产品仓>\scripts\dev.ps1
#   ScriptDir   = <产品仓>\scripts
#   ProductRoot = <产品仓>            (含 docs\VERSION、data\、.cache\ 等)
#   ProjectRoot = <产品仓>\wind_input (Cargo workspace 根)
$ScriptDir     = $PSScriptRoot
$ProductRoot   = Split-Path $ScriptDir -Parent
$ProjectRoot   = "$ProductRoot\wind_input"
$TsfDir        = "$ProductRoot\wind_tsf"      # C++ TSF 核心层 (CMake/MSVC)
$SettingDir    = [System.IO.Path]::GetFullPath("$ProductRoot\..\wind-setting")  # 设置程序 (独立仓库)
$PortableDir   = [System.IO.Path]::GetFullPath("$ProductRoot\..\wind-portable") # 绿色版启动器 (独立仓库)
$Version       = (Get-Content "$ProductRoot\docs\VERSION" -Raw).Trim()
$BuildDir      = "$ProductRoot\build"
$BuildDevDir = "$ProductRoot\build_dev"
$CacheDir      = "$ProductRoot\.cache"        # 外部下载/生成 (不入库)
$DistDir       = "$ProductRoot\dist"          # 安装包输出目录 (gitignore)

# ---------- 部署目标 (Go 便携式: 复制到指定本地目录) ----------
$WIND_DIR_RELEASE = "C:\Program Files\WindInput"
$WIND_DIR_DEV     = "C:\Program Files\WindInputDev"
# 便携部署目标 (绿色版: 纯复制 + 便携标记, 不注册 COM/不写自启, 无需管理员)。
# 默认放 D:\ 而非 Program Files —— wind_portable 探测到自身位于系统保护目录会直接拒绝
# 启动便携模式 (wind-portable\src\layout.rs is_protected_dir), 故此处不能沿用安装目录。
$WIND_DIR_PORTABLE_RELEASE = "D:\WindInputPortable"
$WIND_DIR_PORTABLE_DEV     = "D:\WindInputPortableDev"
# 便携标记文件名与用户数据目录名: 真源是 wind-config\src\variant.rs 的 PORTABLE_MARKER_NAME
# / PORTABLE_DATA_DIR 常量。标记名已与 config\app.toml 的 portable_marker (安装器侧字段)
# 统一 —— 此前两侧各叫各的, 安装包便携模式装出的目录主程序不认, 数据落回 %APPDATA%。
# 改这两个常量时须同步此处、app.toml、wind-portable 与 IPCClient.cpp。
$PortableMarkerName = "portable_mode"
# 旧标记名: 仅用于部署时识别存量便携目录 (读取兼容), 不再写入。
$PortableMarkerLegacy = "wind_portable_mode"
$PortableDataDir    = "userdata"
# wind-installer: 通用安装器生成器 (兄弟项目, app.toml 驱动); 8/d8 打包命令调用其 pack.ps1。
$InstallerDir  = "$ProductRoot\..\wind-installer"
# 在线升级元数据里的下载地址前缀 (不含结尾斜杠); 打包后生成的 latest*.json 据此拼 exeUrl。
$CdnBase       = "https://dl.windinput.com"
# 可在 scripts\deploy.local.ps1 覆盖上述变量 (PowerShell 赋值语法; 该文件 gitignore)。
$deployCfg = "$ScriptDir\deploy.local.ps1"
if (Test-Path $deployCfg) { . $deployCfg }

# ---------- 远程构建 (可选; 不配置则本行之后一切照旧) ----------
# 存在 scripts\build.local.ps1 且其中设了 $WIND_REMOTE_HOST 时, Dispatch 会把构建/检查类
# 命令转发到远程 Windows 编译机执行, 产物回传本机 build[_dev]\ —— 部署链一行不变。
# 模板与编译机搭建清单见 scripts\build.local.ps1.example。
# 编译机必须是 Windows 原生 MSVC: clang 交叉编译的 TSF DLL 在加固宿主 COM 激活失败 (6dbc8595)。
$buildCfg = "$ScriptDir\build.local.ps1"
if (Test-Path $buildCfg) { . $buildCfg }

# ---------- 静态链接 MSVC CRT(+crt-static) ----------
# 所有 Rust 产物(wind_input / wind_setting / wind_portable, 及子进程 pack.ps1 的
# installer stub)自包含, 目标机无需装 VC++ 运行库 —— VCRUNTIME140.dll 非系统内置,
# 缺它则原生 cargo build 默认动态 CRT, 干净机器上 exe 报「找不到 VCRUNTIME140.dll」
# / 0xc000007b 而无法启动。与 dev.sh 的 RUSTFLAGS 注入(+crt-static)对齐。
# 追加而非覆盖以保留调用方既有 RUSTFLAGS; 已含则跳过, 幂等避免重复追加。
if ($env:RUSTFLAGS -notmatch 'crt-static') {
    $env:RUSTFLAGS = (@($env:RUSTFLAGS, '-C target-feature=+crt-static') -join ' ').Trim()
}

# ---------- 输出辅助 ----------
function Say  ([string]$m) { Write-Host $m -ForegroundColor Green }
function Warn ([string]$m) { Write-Host $m -ForegroundColor Yellow }
function ErrMsg ([string]$m) { Write-Host $m -ForegroundColor Red }
function Gray ([string]$m) { Write-Host $m -ForegroundColor DarkGray }

# release → BUILD_DIR; dev → BUILD_DEV_DIR
function Out-For ([string]$profile) { if ($profile -eq "dev") { $BuildDevDir } else { $BuildDir } }

# cargo 项目的 target 目录 (产物落点)。
#
# 不能拼 "<项目>\target" —— 本机若在 ~/.cargo/config.toml 里设了 build.target-dir
# (几个 Rust 项目共用一份依赖编译产物, 省下几十 G 磁盘), 或设了 CARGO_TARGET_DIR,
# 产物就根本不在项目目录内, 硬拼出来的路径会指向一个空壳, 或上一次的旧二进制 ——
# 后者更坏: 构建报成功、装上去的却是旧的。
#
# 向 cargo 自己要这个值是唯一可靠来源: 各设备的共享目录路径不同也无需改脚本; 没设
# 共享时它返回的就是 <项目>\target, 与旧行为完全一致。cargo 不可用或项目不在时回落
# 硬拼, 裸环境仍可用。PowerShell 自带 ConvertFrom-Json, 故不像 sh 版那样依赖 jq。
#
# 结果按项目缓存: cargo metadata 要解析整份 JSON, 同一项目问一次就够。
$script:CargoTargetDirCache = @{}
function Get-CargoTargetDir ([string]$projDir) {
    if ($script:CargoTargetDirCache.ContainsKey($projDir)) { return $script:CargoTargetDirCache[$projDir] }
    $d = $null
    $manifest = Join-Path $projDir "Cargo.toml"
    if (Test-Path $manifest) {
        # 原生命令的失败不触发 try/catch, 只能看 $LASTEXITCODE。多行输出要先拼回单串,
        # 否则 ConvertFrom-Json 会按行逐个解析而失败。
        $json = (& cargo metadata --format-version 1 --no-deps --manifest-path $manifest 2>$null) -join "`n"
        if ($LASTEXITCODE -eq 0 -and $json) {
            try { $d = ($json | ConvertFrom-Json).target_directory } catch { $d = $null }
        }
    }
    if (-not $d) { $d = Join-Path $projDir "target" }
    $script:CargoTargetDirCache[$projDir] = $d
    return $d
}

# ---------- 构建: 核心 exe ----------
# dev 变体 = dev-variant profile（继承 dev + 关断言）:
#   ① debug_assertions 关闭 → windows_subsystem="windows" 生效, 无控制台窗口;
#   ② 优化构建, 输入法手感正常; ③ 仍是独立 _dev 身份 (管道/目录隔离)。
function Build-Core ([string]$profile = "release", [string]$outdir = $null) {
    if (-not $outdir) { $outdir = Out-For $profile }
    New-Item -ItemType Directory -Path $outdir -Force | Out-Null
    $suffix = ""; $prof = "release"
    if ($profile -eq "dev") { $suffix = "_dev"; $prof = "dev-variant" }
    Say "`n[core] 构建 wind_input ($prof)..."
    Push-Location $ProjectRoot
    try {
        cargo build --profile $prof -p wind_service
        if ($LASTEXITCODE -ne 0) { ErrMsg "wind_input 构建失败!"; return $false }
    } finally { Pop-Location }
    $src = Join-Path (Get-CargoTargetDir $ProjectRoot) "$prof\wind_input.exe"
    if (-not (Test-Path $src)) { ErrMsg "未找到产物: $src"; return $false }
    Copy-Item $src "$outdir\wind_input$suffix.exe" -Force
    $sz = [math]::Round((Get-Item "$outdir\wind_input$suffix.exe").Length / 1MB, 1)
    Gray "已构建: wind_input$suffix.exe (${sz}MB)"
    # CLI 包装器 (wind_input config ...; 运行时自辨 dev/release exe, 两变体共用一份)
    $cli = "$ProjectRoot\scripts\wind_cli.bat"
    if (Test-Path $cli) { Copy-Item $cli "$outdir\wind_cli.bat" -Force; Gray "已复制: wind_cli.bat" }
    return $true
}

# ---------- 校验: PE 头的机器类型 ----------
# x86/x64 共用同一个输出文件名, 改名一旦失手就会把 x64 当 x86 交付, 而 32 位宿主加载它
# 只是静默失败 (无日志无报错)。故构建后按 PE 头实测一次, 把「产物架构」变成硬判据。
function Test-PeArch ([string]$path, [string]$platform) {
    if (-not (Test-Path $path)) { return $false }
    $want = if ($platform -eq "Win32") { 0x14c } else { 0x8664 }
    $fs = [IO.File]::OpenRead($path)
    try {
        $br = New-Object IO.BinaryReader($fs)
        $fs.Position = 0x3C
        $fs.Position = $br.ReadInt32()                            # e_lfanew → PE 签名
        if ($br.ReadUInt32() -ne 0x00004550) { return $false }    # 'PE\0\0'
        return ($br.ReadUInt16() -eq $want)                       # COFF Machine
    } catch { return $false }
    finally { $fs.Dispose() }
}

# ---------- 构建: C++ TSF DLL (x64 + x86; CMake/MSVC) ----------
# CMakeLists 把 DLL 写死输出到 ..\build[_dev], x86/x64 同名 wind_tsf.dll。
# 故先编 x86 → 改名 _x86, 再编 x64 (保留无后缀名), 避免互相覆盖。
function Build-TsfAll ([string]$profile = "release", [string]$outdir = $null) {
    if (-not $outdir) { $outdir = Out-For $profile }
    New-Item -ItemType Directory -Path $outdir -Force | Out-Null
    if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
        Warn "未找到 cmake; 跳过 TSF (安装 CMake + VS2022 C++ 工具后可构建)。"; return $true
    }
    $suffix = ""; $dvFlag = "OFF"
    if ($profile -eq "dev") { $suffix = "_dev"; $dvFlag = "ON" }
    # 解析版本号 (写入版本资源)
    $vp = ($Version -split '[.\-]')
    $vMaj = if ($vp.Count -ge 1) { $vp[0] } else { "0" }
    $vMin = if ($vp.Count -ge 2) { $vp[1] } else { "0" }
    $vPat = if ($vp.Count -ge 3) { $vp[2] } else { "0" }
    Say "`n[tsf] CMake 交叉构建 x64 + x86 ($profile, VS2022/MSVC)..."
    # arch: cmake -A 平台名 / 产物后缀
    $arches = @(
        @{ A = "Win32"; Sfx = "_x86" },   # 先 x86 → 改名
        @{ A = "x64";   Sfx = "" }        # 后 x64 → 保留无后缀
    )
    foreach ($a in $arches) {
        $bin = "$CacheDir\tsf-cmake\$($a.A)$suffix"
        New-Item -ItemType Directory -Path $bin -Force | Out-Null
        cmake -S $TsfDir -B $bin -G "Visual Studio 17 2022" -A $a.A `
            "-DWIND_DEV_VARIANT=$dvFlag" `
            "-DAPP_VERSION_STR=$Version" `
            "-DAPP_VERSION_MAJOR=$vMaj" "-DAPP_VERSION_MINOR=$vMin" "-DAPP_VERSION_PATCH=$vPat" `
            | Out-Null
        if ($LASTEXITCODE -ne 0) { ErrMsg "TSF $($a.A) CMake 配置失败!"; return $false }
        # CMakeLists 输出到 $outdir\wind_tsf$suffix.dll; x86 需改名加 _x86。
        # 构建前必须先删这个文件: 两个架构共用它, MSBuild 的增量判据也看它。x86 这轮若
        # 撞见上一轮 x64 留下的同名文件比输入新, 会判定「已最新」而跳过链接, 下面的改名
        # 就把那个 x64 产物当成 x86 交付 (实测踩中)。删掉输出即强制重新链接, 不重编 .obj。
        $produced = "$outdir\wind_tsf$suffix.dll"
        Remove-Item $produced -Force -ErrorAction SilentlyContinue
        # MSBuild 的编译警告走 stdout, 整条 | Out-Null 会连警告一起吞掉 (C++ 侧等于零编译期
        # 信号)。故只丢进度噪音, 保留 warning/error 行原样打出。Select-String 不改 $LASTEXITCODE
        # (它由最后一个原生命令 cmake 设定), 下面的失败判定照常成立。
        cmake --build $bin --config Release |
            Select-String -Pattern 'warning|error|警告|错误' |
            ForEach-Object { Warn "  $($_.Line.Trim())" }
        if ($LASTEXITCODE -ne 0) { ErrMsg "TSF $($a.A) 构建失败!"; return $false }
        # 末尾化: 架构后缀在前, 变体后缀在后 → wind_tsf_x86_dev.dll
        $final    = "$outdir\wind_tsf$($a.Sfx)$suffix.dll"
        if ((Test-Path $produced) -and ($produced -ne $final)) {
            Move-Item $produced $final -Force
        }
        if (-not (Test-PeArch $final $a.A)) {
            ErrMsg "TSF 产物架构不符: $(Split-Path $final -Leaf) 不是 $($a.A)!"; return $false
        }
    }
    # 清理 CMake 顺带产出的导入库/导出表, 保持 outdir == 安装内容
    Get-ChildItem -Path $outdir -Include "*.lib", "*.exp" -File -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
    $dlls = (Get-ChildItem -Path $outdir -Filter "wind_tsf*.dll" -ErrorAction SilentlyContinue | ForEach-Object { $_.Name }) -join " "
    Gray "已构建: $dlls"
    return $true
}

# ---------- 构建: wind-setting (设置程序) ----------
# 独立仓库; 不存在时跳过。dev 变体产物重命名为 wind_setting_dev.exe。
function Build-Setting ([string]$profile = "release", [string]$outdir = $null) {
    if (-not $outdir) { $outdir = Out-For $profile }
    if (-not (Test-Path $SettingDir)) { Warn "../wind-setting 仓库不存在, 跳过设置程序。"; return $true }
    $suffix = ""; $targetDir = "release"
    if ($profile -eq "dev") { $suffix = "_dev"; $targetDir = "debug" }
    New-Item -ItemType Directory -Path $outdir -Force | Out-Null
    Say "`n[setting] 构建 wind_setting ($profile)..."
    $env:WIND_APP_VERSION = $Version   # 版本注入: docs/VERSION → wind-setting (与主仓统一)
    Push-Location $SettingDir
    try {
        if ($profile -eq "dev") { cargo build } else { cargo build --release }
        if ($LASTEXITCODE -ne 0) { ErrMsg "wind_setting 构建失败!"; return $false }
    } finally { Pop-Location }
    $exe = Join-Path (Get-CargoTargetDir $SettingDir) "$targetDir\wind_setting.exe"
    if (-not (Test-Path $exe)) { ErrMsg "未找到产物: $exe"; return $false }
    Copy-Item $exe "$outdir\wind_setting$suffix.exe" -Force
    $sz = [math]::Round((Get-Item "$outdir\wind_setting$suffix.exe").Length / 1MB, 1)
    Gray "已构建: wind_setting$suffix.exe (${sz}MB)"
    return $true
}

# ---------- 构建: wind-portable (绿色版便携启动器) ----------
# 独立仓库; 不存在时跳过。dev/release 产出同一份 exe。
function Build-Portable ([string]$profile = "release", [string]$outdir = $null) {
    if (-not $outdir) { $outdir = Out-For $profile }
    if (-not (Test-Path $PortableDir)) { Warn "../wind-portable 仓库不存在, 跳过便携启动器。"; return $true }
    New-Item -ItemType Directory -Path $outdir -Force | Out-Null
    Say "`n[portable] 构建 wind_portable ($profile → 单一二进制)..."
    $env:WIND_APP_VERSION = $Version   # 版本注入: docs/VERSION → wind-portable (与主仓统一)
    Push-Location $PortableDir
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { ErrMsg "wind_portable 构建失败!"; return $false }
    } finally { Pop-Location }
    $exe = Join-Path (Get-CargoTargetDir $PortableDir) "release\wind_portable.exe"
    if (-not (Test-Path $exe)) { ErrMsg "未找到产物: $exe"; return $false }
    Copy-Item $exe "$outdir\wind_portable.exe" -Force
    $sz = [math]::Round((Get-Item "$outdir\wind_portable.exe").Length / 1MB, 1)
    Gray "已构建: wind_portable.exe (${sz}MB)"
    return $true
}

# ---------- 代码质量 ----------
function Do-Check  { Say "`n正在运行 cargo check (全工作区)...";  Push-Location $ProjectRoot; try { cargo check --workspace }  finally { Pop-Location } }
# -Deny 把警告升为错误(CI 走这条)。本地 `dev.ps1 l` 不带, 迭代中途的 warning 不该中断。
# --all-targets 不可省: 不带它连测试代码都不检查, 而测试里同样会长出警告。
function Do-Clippy {
    param([switch]$Deny)
    Say "`n正在运行 cargo clippy (全工作区含测试)..."
    Push-Location $ProjectRoot
    try {
        if ($Deny) { cargo clippy --workspace --all-targets -- -D warnings }
        else { cargo clippy --workspace --all-targets }
    } finally { Pop-Location }
}
function Do-Test   { Say "`n正在运行 cargo test (全工作区)...";   Push-Location $ProjectRoot; try { cargo test --workspace }   finally { Pop-Location } }
function Do-Fmt    { Say "`n正在运行 cargo fmt...";                Push-Location $ProjectRoot; try { cargo fmt }                finally { Pop-Location } }
function Do-FmtCheck { Say "`n正在运行 cargo fmt --check...";      Push-Location $ProjectRoot; try { cargo fmt --all -- --check } finally { Pop-Location } }
function Do-Clean  { Say "`n正在运行 cargo clean...";              Push-Location $ProjectRoot; try { cargo clean }              finally { Pop-Location } }
function Do-HooksInstall {
    Say "`n激活 .githooks/pre-commit (git config core.hooksPath .githooks)..."
    Push-Location $ProductRoot; try { git config core.hooksPath .githooks } finally { Pop-Location }
    Say "已激活：提交前将自动跑 cargo fmt --check"
}

function Do-Ci {
    Push-Location $ProjectRoot
    try {
        Do-FmtCheck; if ($LASTEXITCODE -ne 0) { ErrMsg "fmt 检查失败!"; return $false }
        Do-Clippy -Deny; if ($LASTEXITCODE -ne 0) { ErrMsg "clippy 失败!"; return $false }
        Do-Test;     if ($LASTEXITCODE -ne 0) { ErrMsg "test 失败!";   return $false }
    } finally { Pop-Location }
    Say "`nCI 全部通过 ✓"; return $true
}

# ---------- 词库下载 ----------
function Get-Dict ([string]$url, [string]$dst, [string]$desc = "") {
    if (Test-Path $dst) { Gray "[skip] $(Split-Path $dst -Leaf) 已存在"; return $true }
    Gray "[get ] $(Split-Path $dst -Leaf) $desc"
    # 用 PowerShell 原生下载 (Invoke-WebRequest), 静默进度条以提速; 最多重试 3 次。
    $old = $ProgressPreference; $ProgressPreference = "SilentlyContinue"
    try {
        for ($i = 1; $i -le 3; $i++) {
            try {
                Invoke-WebRequest -Uri $url -OutFile $dst -UseBasicParsing -TimeoutSec 120
                return $true
            } catch {
                if (Test-Path $dst) { Remove-Item $dst -Force -ErrorAction SilentlyContinue }  # 清理半截文件
                if ($i -eq 3) { ErrMsg "下载失败 ($i/3): $url`n  $($_.Exception.Message)"; return $false }
                Warn "下载重试 ($i/3): $(Split-Path $dst -Leaf)"
                Start-Sleep -Seconds 2
            }
        }
    } finally { $ProgressPreference = $old }
    return $false
}

function Download-Dicts {
    Say "`n下载外部词库 → $CacheDir"
    $rimeFrost   = "$CacheDir\rime-frost"
    $rimeFrostCn = "$rimeFrost\cn_dicts"
    $rimeFrostEn = "$rimeFrost\en_dicts"
    $opencc      = "$CacheDir\opencc\dictionaries"
    $pinyinData  = "$CacheDir\pinyin-data"
    $rimeWubi    = "$CacheDir\rime-wubi"
    $cldr        = "$CacheDir\cldr"
    $auxCode     = "$CacheDir\aux-code"
    foreach ($d in @($rimeFrostCn, $rimeFrostEn, $opencc, $pinyinData, $rimeWubi, $cldr, $auxCode)) { New-Item -ItemType Directory -Path $d -Force | Out-Null }

    $frostBase = "https://raw.githubusercontent.com/gaboolic/rime-frost/master"
    Gray "rime-frost (拼音):"
    Get-Dict "$frostBase/rime_frost.dict.yaml"           "$rimeFrost\rime_frost.dict.yaml"      "词库入口"     | Out-Null
    Get-Dict "$frostBase/cn_dicts/8105.dict.yaml"        "$rimeFrostCn\8105.dict.yaml"          "单字词库"     | Out-Null
    Get-Dict "$frostBase/cn_dicts/41448.dict.yaml"       "$rimeFrostCn\41448.dict.yaml"         "扩展字表"     | Out-Null
    Get-Dict "$frostBase/cn_dicts/base.dict.yaml"        "$rimeFrostCn\base.dict.yaml"          "基础词库"     | Out-Null
    Get-Dict "$frostBase/cn_dicts/ext.dict.yaml"         "$rimeFrostCn\ext.dict.yaml"           "扩展词库"     | Out-Null
    Get-Dict "$frostBase/cn_dicts/others.dict.yaml"      "$rimeFrostCn\others.dict.yaml"        "容错词"       | Out-Null
    Get-Dict "$frostBase/cn_dicts/corrections.dict.yaml" "$rimeFrostCn\corrections.dict.yaml"   "错音词"       | Out-Null
    Get-Dict "$frostBase/cn_dicts/tencent.dict.yaml"     "$rimeFrostCn\tencent.dict.yaml"       "腾讯词频"     | Out-Null

    Gray "rime-frost (英文):"
    Get-Dict "$frostBase/en_dicts/en.dict.yaml"     "$rimeFrostEn\en.dict.yaml"     "主词库" | Out-Null
    Get-Dict "$frostBase/en_dicts/en_ext.dict.yaml" "$rimeFrostEn\en_ext.dict.yaml" "扩展"   | Out-Null

    $pinyinBase = "https://raw.githubusercontent.com/mozillazg/pinyin-data/master"
    Gray "pinyin-data (汉字拼音反查):"
    Get-Dict "$pinyinBase/pinyin.txt"         "$pinyinData\pinyin.txt"         "全量底表(官方合成)" | Out-Null
    Get-Dict "$pinyinBase/kXHC1983.txt"       "$pinyinData\kXHC1983.txt"       "新华字典多音字" | Out-Null
    Get-Dict "$pinyinBase/kTGHZ2013.txt"      "$pinyinData\kTGHZ2013.txt"      "通用规范汉字"   | Out-Null
    Get-Dict "$pinyinBase/kMandarin_8105.txt" "$pinyinData\kMandarin_8105.txt" "8105 标准首音"  | Out-Null
    Get-Dict "$pinyinBase/overwrite.txt"      "$pinyinData\overwrite.txt"      "手工纠正"       | Out-Null

    # 五笔词库: 下载上游原始档, 主库与 extra 由 gen_dict 重排/拆分后写入 build 目录;
    # district 不经 gen_dict, 原样复制 (见 Assemble-Data)
    $wubiBase = "https://raw.githubusercontent.com/KyleBing/rime-wubi86-jidian/master"
    Gray "rime-wubi86-jidian (五笔):"
    Get-Dict "$wubiBase/wubi86_jidian.dict.yaml"                "$rimeWubi\wubi86_jidian.dict.yaml"                "主词库"     | Out-Null
    Get-Dict "$wubiBase/wubi86_jidian_extra.dict.yaml"          "$rimeWubi\wubi86_jidian_extra.dict.yaml"          "扩展词库"   | Out-Null
    Get-Dict "$wubiBase/wubi86_jidian_extra_district.dict.yaml" "$rimeWubi\wubi86_jidian_extra_district.dict.yaml" "行政区域"   | Out-Null

    # Unicode CLDR emoji 中文注解 + emoji 白名单。不参与常规构建 —— emoji 命名表
    # (custom_emoji_named.txt) 已入库, 这些原始档只在需要重新生成它时用到:
    #   cargo run -p wind-tools --bin gen_emoji_names -- --cldr .cache\cldr `
    #     --stopwords <gen_dict数据目录>\emoji_stopwords.txt --out <同目录>\custom_emoji_named.txt
    # 生成的命名表交给 gen_dict 反查五笔码 (「足球」-> khgf)。
    # 许可证 Unicode-3.0, 见 NOTICE.md
    $cldrBase = "https://raw.githubusercontent.com/unicode-org/cldr/main/common"
    Gray "Unicode CLDR (emoji 中文名):"
    Get-Dict "$cldrBase/annotations/zh.xml"        "$cldr\zh.xml"         "emoji 注解"     | Out-Null
    Get-Dict "$cldrBase/annotationsDerived/zh.xml" "$cldr\zh_derived.xml" "派生注解(国旗)" | Out-Null
    Get-Dict "https://unicode.org/Public/emoji/latest/emoji-test.txt" "$cldr\emoji-test.txt" "emoji 白名单" | Out-Null

    # 辅助码表: 拼音候选的字形二次筛选 (默认关闭的功能, 见 schema.pinyin.aux_code)。
    # 小鹤/自然码两张已是 `字=码` 行格式, 零转换; 笔画表来自 rime-stroke 的 .dict.yaml,
    # 由 gen_aux_code 剥 YAML 头 + 按字集裁剪 (字表来自 zispace/hanzi-chars)。
    # ⚠️ rime-stroke 是 LGPL-3.0, 与本仓 MIT 不同 —— 同 rime-frost 处理: 只下载不入库,
    # 产物随发行版分发并适用原许可, 见 NOTICE.md。
    $auxBase = "https://raw.githubusercontent.com/HowcanoeWang/rime-lua-aux-code/main/aux_code"
    Gray "辅助码表:"
    Get-Dict "$auxBase/flypy_full.txt"   "$auxCode\flypy_full.txt"   "小鹤形码"   | Out-Null
    Get-Dict "$auxBase/ZRM-wanxiang.txt" "$auxCode\ZRM-wanxiang.txt" "自然码形码" | Out-Null
    Get-Dict "https://raw.githubusercontent.com/rime/rime-stroke/master/stroke.dict.yaml" `
             "$auxCode\stroke.dict.yaml" "笔画(上游全表)" | Out-Null
    # 笔画表裁剪用的字集 (文件名须与 gen_aux_code::CHARSET_FILES 一致)
    $charsetDir = "$auxCode\charset"
    New-Item -ItemType Directory -Path $charsetDir -Force | Out-Null
    $hanziBase = "https://raw.githubusercontent.com/zispace/hanzi-chars/main"
    foreach ($cs in @(
        @{ Path = "data-charset/GB 18030-2000.txt";                 Label = "GB18030 基本集" },
        @{ Path = "data-charlist/《通用规范汉字表》（2013年）.txt";  Label = "通用规范汉字表" },
        @{ Path = "data-unicode/Unicode-CJK 〇.txt";                 Label = "〇" }
    )) {
        $leaf = Split-Path $cs.Path -Leaf
        # 路径含中文/空格, 需 URL 编码后再拼接
        $encoded = ($cs.Path -split '/' | ForEach-Object { [uri]::EscapeDataString($_) }) -join '/'
        Get-Dict "$hanziBase/$encoded" "$charsetDir\$leaf" "字集: $($cs.Label)" | Out-Null
    }

    $openccBase = "https://raw.githubusercontent.com/BYVoid/OpenCC/master/data/dictionary"
    Gray "OpenCC 简繁词典:"
    Get-Dict "$openccBase/STCharacters.txt" "$opencc\STCharacters.txt" "简->繁 字级" | Out-Null
    Get-Dict "$openccBase/STPhrases.txt"    "$opencc\STPhrases.txt"    "简->繁 词级" | Out-Null
    Get-Dict "$openccBase/TWVariants.txt"   "$opencc\TWVariants.txt"   "台湾字形"   | Out-Null
    Get-Dict "$openccBase/TWPhrases.txt"    "$opencc\TWPhrases.txt"    "台湾词汇"   | Out-Null
    Get-Dict "$openccBase/HKVariants.txt"   "$opencc\HKVariants.txt"   "香港字形"   | Out-Null
    return $true
}

# 从 data/(源) + .cache/(下载/生成) 组装完整运行时数据到 $outdir\data\
function Assemble-Data ([string]$outdir = $BuildDevDir) {
    $data      = "$outdir\data"
    $schemas   = "$data\schemas"
    $pinyin    = "$schemas\pinyin"
    $pinyinCn  = "$pinyin\cn_dicts"
    $english   = "$schemas\english"
    $rimeFrost = "$CacheDir\rime-frost"

    Say "`n组装 data/ → $data"
    if (Test-Path $data) { Remove-Item -Recurse -Force $data }

    # 1. 复制 data/ 源文件 (configs、五笔词库、主题等)
    New-Item -ItemType Directory -Path $outdir -Force | Out-Null
    Copy-Item "$ProductRoot\data" -Destination $outdir -Recurse -Force

    # 1b. 合并 wind_input\data\settings\ (manifest.toml 等 RPC 元数据)
    if (Test-Path "$ProjectRoot\data\settings") {
        New-Item -ItemType Directory -Path "$data\settings" -Force | Out-Null
        Copy-Item "$ProjectRoot\data\settings\*" -Destination "$data\settings" -Recurse -Force
    }

    # 2. rime-frost 拼音词库
    New-Item -ItemType Directory -Path $pinyinCn -Force | Out-Null
    if (Test-Path "$rimeFrost\rime_frost.dict.yaml") {
        Copy-Item "$rimeFrost\rime_frost.dict.yaml" $pinyin -Force
        foreach ($f in @("8105.dict.yaml", "41448.dict.yaml", "base.dict.yaml", "ext.dict.yaml", "others.dict.yaml", "corrections.dict.yaml")) {
            if (Test-Path "$rimeFrost\cn_dicts\$f") { Copy-Item "$rimeFrost\cn_dicts\$f" $pinyinCn -Force }
        }
    } else { Warn "缺 .cache\rime-frost\, 拼音词库不可用 (运行 gen-data 下载)" }

    # 3. 英文词库
    New-Item -ItemType Directory -Path $english -Force | Out-Null
    foreach ($f in @("en.dict.yaml", "en_ext.dict.yaml")) {
        if (Test-Path "$rimeFrost\en_dicts\$f") { Copy-Item "$rimeFrost\en_dicts\$f" $english -Force }
    }

    # 4. (unigram.txt 不再随 data/ 分发：引擎侧的读取链已移除, 词图打分改用词条自身的
    #    词典权重, 见 wind-engine/pinyin/lattice.rs::score_node_inner。
    #    .cache 里的 unigram.txt 仍由 gen-data 生成 —— gen_dict 用它给五笔扩展词库的
    #    CJK 条目赋权, 见 gen_dict/extra.rs::assign_bucket_weights。)

    # 4b. 汉字拼音反查表
    $pinyinMap = "$CacheDir\pinyin-data\pinyin_map.txt"
    if (Test-Path $pinyinMap) { Copy-Item $pinyinMap "$data\pinyin_map.txt" -Force }
    else { Warn "缺 pinyin_map.txt (运行 gen-data 生成)" }

    # 5. OpenCC 编译 .octrie (Rust 工具 gen_opencc)
    New-Item -ItemType Directory -Path "$data\opencc" -Force | Out-Null
    if ((Test-Path "$CacheDir\opencc\dictionaries") -and (Get-ChildItem "$CacheDir\opencc\dictionaries\*.txt" -ErrorAction SilentlyContinue)) {
        Gray "编译 OpenCC → .octrie ..."
        Push-Location $ProjectRoot
        try {
            cargo run -q -p wind-tools --bin gen_opencc -- --src "$CacheDir\opencc\dictionaries" --out "$data\opencc"
            if ($LASTEXITCODE -ne 0) { Warn "OpenCC 编译失败 (简繁转换不可用)" }
        } finally { Pop-Location }
    } else { Warn "缺 .cache\opencc\, OpenCC 不可用 (运行 gen-data 下载)" }

    # 6. 五笔词库 (Rust 工具 gen_dict): 主库按词频重排 + extra 拆成 4 库
    #    产物直接写进 build 目录, 不入版本库 —— 源码树 data\schemas\wubi86\ 只保留
    #    wubi86.schema.toml 与字体等真正的源文件, 避免再把生成物误当源文件手工编辑
    $wubiOut  = "$schemas\wubi86"
    $rimeWubi = "$CacheDir\rime-wubi"
    if (Test-Path "$rimeWubi\wubi86_jidian.dict.yaml") {
        Gray "生成五笔词库 (gen_dict) ..."
        New-Item -ItemType Directory -Path $wubiOut -Force | Out-Null
        Push-Location $ProjectRoot
        try {
            # district 由 gen_dict 的 passthrough 一并处理 (原样透传 + 清洗头部)
            cargo run -q -p wind-tools --bin gen_dict -- --cache $CacheDir --out $wubiOut --report $rimeWubi
            if ($LASTEXITCODE -ne 0) { Warn "五笔词库生成失败 (五笔方案不可用)" }
        } finally { Pop-Location }
    } else { Warn "缺 .cache\rime-wubi\, 五笔词库不可用 (运行 gen-data 下载)" }

    # 7. 辅助码表 (Rust 工具 gen_aux_code): 小鹤/自然码原样透传, 笔画表 YAML→`字=码` + 字集裁剪。
    #    与五笔同理: 产物只进 build 目录, 不入版本库 (rime-stroke 是 LGPL-3.0, 见 NOTICE.md)。
    #    功能出厂关闭, 故缺表只是「辅助码用不了」, 不影响其它一切 —— 用 Warn 不中断构建。
    $auxCache = "$CacheDir\aux-code"
    if (Test-Path "$auxCache\stroke.dict.yaml") {
        Gray "生成辅助码表 (gen_aux_code) ..."
        New-Item -ItemType Directory -Path "$schemas\aux_code" -Force | Out-Null
        Push-Location $ProjectRoot
        try {
            cargo run -q -p wind-tools --bin gen_aux_code -- --cache $CacheDir --out "$schemas\aux_code"
            if ($LASTEXITCODE -ne 0) { Warn "辅助码表生成失败 (辅助码功能不可用)" }
        } finally { Pop-Location }
    } else { Warn "缺 .cache\aux-code\, 辅助码不可用 (运行 gen-data 下载)" }

    $cnt = (Get-ChildItem $data -Recurse -File).Count
    Gray "data/ 组装完成 ($cnt 文件)"
    return $true
}

# 下载外部词库 + 生成 unigram/pinyin + 组装 data/
function Do-GenData ([string]$outdir = $BuildDevDir) {
    if (-not (Download-Dicts)) { return $false }

    # 生成 unigram 词频表 (Rust 工具 gen_unigram)。仅供 gen_dict 给五笔扩展词库的
    # CJK 条目赋权, 不随 data\ 分发 —— 引擎侧已改用词条自身的词典权重打分。
    $unigram = "$CacheDir\pinyin-frost\unigram.txt"
    New-Item -ItemType Directory -Path (Split-Path $unigram -Parent) -Force | Out-Null
    if (-not (Test-Path $unigram)) {
        Say "生成 unigram 词频表..."
        Push-Location $ProjectRoot
        try {
            cargo run -q -p wind-tools --bin gen_unigram -- --rime "$CacheDir\rime-frost\cn_dicts" --out $unigram
            if ($LASTEXITCODE -ne 0) { Warn "unigram 生成失败 (gen_dict 五笔赋权将随之失败)" }
        } finally { Pop-Location }
    } else { Gray "unigram 已缓存" }

    # 生成汉字拼音反查表 (Rust 工具 gen_pinyin)
    $pinyinMap = "$CacheDir\pinyin-data\pinyin_map.txt"
    if (Test-Path "$CacheDir\pinyin-data\pinyin.txt") {
        Say "生成汉字拼音反查表..."
        Push-Location $ProjectRoot
        try {
            cargo run -q -p wind-tools --bin gen_pinyin -- --src "$CacheDir\pinyin-data" --out $pinyinMap
            if ($LASTEXITCODE -ne 0) { Warn "拼音反查表生成失败 (候选拼音提示不可用)" }
        } finally { Pop-Location }
    } else { Warn "缺 .cache\pinyin-data\, 拼音反查表不可用" }

    Assemble-Data $outdir | Out-Null
    Say "gen-data 完成 → $outdir\data"
    return $true
}

# 发布前硬门禁: 校验关键运行时数据完整 (缺失/过小即失败)
function Verify-DistData ([string]$outdir = $BuildDir) {
    $data = "$outdir\data"
    $ok = $true
    $checks = @(
        @{ Path = "schemas\pinyin\cn_dicts\base.dict.yaml"; Min = 1000000 },
        @{ Path = "schemas\pinyin\cn_dicts\8105.dict.yaml"; Min = 10000 },
        @{ Path = "schemas\english\en.dict.yaml";           Min = 1000 },
        @{ Path = "pinyin_map.txt";                         Min = 10000 },
        # 五笔词库为 gen_dict 生成物, 不入版本库 —— 忘跑 gen-data 时必须在此拦下,
        # 否则打出来的包五笔方案整个不可用
        @{ Path = "schemas\wubi86\wubi86_jidian.dict.yaml";       Min = 1000000 },
        @{ Path = "schemas\wubi86\wubi86_jidian_extra.dict.yaml"; Min = 10000 },
        @{ Path = "schemas\wubi86\wubi86_jidian_emoji.dict.yaml"; Min = 1000 },
        @{ Path = "schemas\wubi86\wubi86_jidian_extra_district.dict.yaml"; Min = 10000 },
        # 辅助码表同为生成物、同样不入库。功能虽出厂关闭, 缺表的后果仍是「用户在设置里
        # 打开了却毫无反应」—— 门卫只 warn 一行日志, 用户看不到。故照样在此硬拦。
        @{ Path = "schemas\aux_code\stroke.txt";     Min = 500000 },
        @{ Path = "schemas\aux_code\flypy_full.txt"; Min = 50000 }
    )
    Say "`n校验发布数据完整性 → $data"
    foreach ($c in $checks) {
        $p = "$data\$($c.Path)"
        if (-not (Test-Path $p)) { ErrMsg "  ✗ 缺失: $($c.Path)"; $ok = $false; continue }
        $sz = (Get-Item $p).Length
        if ($sz -lt $c.Min) { ErrMsg "  ✗ 过小 (${sz}B < 期望 $($c.Min)B): $($c.Path)"; $ok = $false }
        else { Gray "  ✓ $($c.Path) ($([math]::Round($sz/1KB))KB)" }
    }
    $octrie = @(Get-ChildItem "$data\opencc\*.octrie" -ErrorAction SilentlyContinue | Where-Object { $_.Length -gt 0 })
    if ($octrie.Count -lt 1) { ErrMsg "  ✗ 缺失: opencc\*.octrie (简繁转换编译失败)"; $ok = $false }
    else { Gray "  ✓ opencc\*.octrie ($($octrie.Count) 个)" }

    if (-not $ok) {
        ErrMsg "`n发布数据校验失败! 上述文件缺失或异常会导致功能残缺。"
        ErrMsg "请排查 gen-data 的下载/生成 (词库源、网络、gen_opencc/gen_dict)。"
        return $false
    }
    Say "发布数据校验通过 ✓"; return $true
}

# ---------- 全构建 (1 / d1) ----------
# 全部模块 + 数据落到【产品根】build/(release) 或 build_dev/(dev)。
# 先清空输出目录, 确保内容 == 部署到目标目录的内容, 无任何中间产物。
# ---------- 版本变化侦测: 版本号变更时强制重建关键产物 (确定性保险) ----------
# 产品版本唯一真源是 docs/VERSION。cargo 的 rerun-if-env-changed 与 CMake -D 已能在
# 版本变化时自动重建; 此处再加一道保险: 记录上次构建版本, 一旦变化即清理最终产物
# (Rust 最终二进制包 + TSF 的 CMake 缓存目录), 强制重新写入版本资源。仅版本真变时付代价。
function Sync-VersionStamp {
    $stampFile = "$CacheDir\.last_build_version"
    $lastVer = if (Test-Path $stampFile) { (Get-Content $stampFile -Raw).Trim() } else { "" }
    if ($lastVer -eq $Version) { return }   # 版本未变 → 走增量, 不清理

    if ($lastVer) { Say "`n[version] 版本变化 $lastVer -> $Version, 清理关键产物强制刷新版本号..." }
    else          { Say "`n[version] 首次记录版本 $Version, 清理关键产物确保版本号写入..." }

    # 1. Rust: 仅清最终二进制包 (依赖库保留, 秒级); build.rs 随之重跑注入新版本资源。
    # 注意: $ProjectRoot 已是 wind_input 目录 (见路径定义), 勿再拼 \wind_input。
    Push-Location $ProjectRoot
    try { cargo clean -p wind_service 2>&1 | Out-Null } catch {} finally { Pop-Location }
    if (Test-Path $SettingDir) {
        Push-Location $SettingDir
        try { cargo clean -p wind_setting 2>&1 | Out-Null } catch {} finally { Pop-Location }
    }
    if (Test-Path $PortableDir) {
        Push-Location $PortableDir
        try { cargo clean -p wind_portable 2>&1 | Out-Null } catch {} finally { Pop-Location }
    }

    # 2. TSF: 删 CMake 缓存目录, 强制 configure_file 重新生成 version.rc。
    $tsfCache = "$CacheDir\tsf-cmake"
    if (Test-Path $tsfCache) { Remove-Item -Recurse -Force $tsfCache -ErrorAction SilentlyContinue }

    # 记录当前版本, 避免下次重复清理。
    New-Item -ItemType Directory -Path $CacheDir -Force | Out-Null
    Set-Content -Path $stampFile -Value $Version -NoNewline
}

# ---------- 全构建的并行编排 (仅在 $env:WIND_PARALLEL_BUILD 非空时启用) ----------
# core / tsf / setting / portable 四步互不依赖: 各写各的产物文件, 且 setting 与 portable
# 用的是各自仓库的 target\, 与主仓的 cargo 锁不冲突。串行跑纯属浪费 —— 实测 48 核编译机
# 上整个构建期间平均只用到 1 个核, 因为每步内部主要是单线程的链接与 LTO。
#
# ⚠️ 默认关闭, 且必须默认关闭: 本机走的是同一段代码, 而在 12 核机器上同时跑四个构建会把
#    机器压死 —— 那正是当初把编译搬去远程的原因。由 remote-build.ps1 在远程侧注入开启。
#
# 用子进程而非 Start-ThreadJob: 这三步各自就是一条现成的子命令 (dm1/dm3/dm4, release 为
# m1/m3/m4), 直接复用比把函数和几十个脚本级变量搬进独立 runspace 可靠得多。
# Do-GenData 不参与并行: 它跑 cargo run -p wind-tools, 与 Build-Core 共用主仓的 target
# 锁, 并行只会互相等锁, 还多占一份内存。
function Invoke-BuildStagesParallel ([string]$profile, [string]$outdir) {
    $p = if ($profile -eq "dev") { "d" } else { "" }
    $stages = @(
        @{ Cmd = "${p}m1"; Name = "tsf" },
        @{ Cmd = "${p}m3"; Name = "setting" },
        @{ Cmd = "${p}m4"; Name = "portable" }
    )
    Say "`n[parallel] core 与 tsf / setting / portable 并行构建 ($($stages.Count + 1) 路)..."
    # 子进程必须带上 WIND_NO_REMOTE: 万一这台机器自身也配了 build.local.ps1, 子进程会把
    # 命令再转发一次 —— 而转发目标很可能就是它自己。
    $prevNoRemote = $env:WIND_NO_REMOTE
    $env:WIND_NO_REMOTE = "1"
    $pwshPath = (Get-Process -Id $PID).Path
    $jobs = @()
    try {
        foreach ($s in $stages) {
            $log = Join-Path $env:TEMP "wind-par-$($s.Name)-$PID.log"
            $proc = Start-Process -FilePath $pwshPath -PassThru -NoNewWindow `
                -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath, $s.Cmd) `
                -RedirectStandardOutput $log -RedirectStandardError "$log.err"
            $jobs += @{ Proc = $proc; Log = $log; Name = $s.Name }
        }
        # 主进程自己跑 core, 与上面三路并发
        $swCore = [System.Diagnostics.Stopwatch]::StartNew()
        $ok = Build-Core $profile $outdir
        $swCore.Stop()
    } finally { $env:WIND_NO_REMOTE = $prevNoRemote }

    foreach ($j in $jobs) {
        $j.Proc.WaitForExit()
        # 子进程输出攒到各自结束后一次性打印 —— 四路实时交织完全没法读
        foreach ($f in @($j.Log, "$($j.Log).err")) {
            if (Test-Path $f) {
                $t = Get-Content $f -Raw -ErrorAction SilentlyContinue
                if ($t -and $t.Trim()) { Write-Host $t.TrimEnd() }
                Remove-Item $f -Force -ErrorAction SilentlyContinue
            }
        }
        if ($j.Proc.ExitCode -ne 0) {
            ErrMsg "$($j.Name) 构建失败 (退出码 $($j.Proc.ExitCode))"
            $ok = $false
        }
    }
    # ⚠️ 每路耗时必须取子进程自己的 ExitTime-StartTime。用外层 Stopwatch 是错的: 停表发生在
    #    按顺序 WaitForExit 之后, 先结束那几路会把「等主进程轮到自己」的时间一并计入, 四路
    #    因此显示成同一个数 (踩过一次, 差点据此误判成并行没生效)。
    $each = $jobs | ForEach-Object {
        @{ Name = $_.Name; Sec = ($_.Proc.ExitTime - $_.Proc.StartTime).TotalSeconds }
    }
    $sum = ($each | ForEach-Object { $_.Sec } | Measure-Object -Sum).Sum + $swCore.Elapsed.TotalSeconds
    $wall = [math]::Max((($each | ForEach-Object { $_.Sec } | Measure-Object -Maximum).Maximum), $swCore.Elapsed.TotalSeconds)
    $detail = (@("core {0:N1}s" -f $swCore.Elapsed.TotalSeconds) +
               ($each | ForEach-Object { "{0} {1:N1}s" -f $_.Name, $_.Sec })) -join "  ·  "
    Gray "[parallel] $detail"
    # 判据: 墙钟 ≈ 最慢那一路 = 真并行; 墙钟 ≈ 各路之和 = 卡在了 cargo 的全局 package
    # cache 锁上 (子进程输出里会出现 "Blocking waiting for file lock on package cache")。
    Gray ("[parallel] 墙钟 {0:N1}s, 串行则需 {1:N1}s (并行度 {2:N2}x)" -f $wall, $sum, ($sum / [math]::Max($wall, 0.1)))
    return $ok
}

function Do-Full ([string]$profile = "release") {
    $outdir = Out-For $profile
    Sync-VersionStamp   # 版本号变化则强制重建关键产物 (确定性保险)
    Say "`n========== 全构建 ($profile) → $outdir =========="
    if (Test-Path $outdir) { Remove-Item -Recurse -Force $outdir }
    New-Item -ItemType Directory -Path $outdir -Force | Out-Null
    # Sync-VersionStamp 必须已经跑完再分叉: 版本变化时它会 cargo clean + 删 tsf-cmake 缓存,
    # 几个进程同时干这件事必然打架。跑完之后它对子进程是幂等的 (版本已匹配, 直接 return)。
    if ($env:WIND_PARALLEL_BUILD) {
        if (-not (Invoke-BuildStagesParallel $profile $outdir)) { return $false }
    } else {
    if (-not (Build-Core     $profile $outdir)) { return $false }   # wind_input[_dev].exe
    if (-not (Build-TsfAll   $profile $outdir)) { return $false }   # wind_tsf[_x86][_dev].dll
    if (-not (Build-Setting  $profile $outdir)) { return $false }   # wind_setting[_dev].exe (可选)
    if (-not (Build-Portable $profile $outdir)) { return $false }   # wind_portable.exe (可选)
    }
    if (-not (Do-GenData     $outdir))          { return $false }   # data/
    if (-not (Verify-DistData $outdir))         { return $false }   # 硬门禁
    Say "`n========== 全构建完成 ($profile) → $outdir =========="
    Gray "内容即部署到目标目录的内容 (无中间产物)"
    return $true
}

# ---------- 部署 (Go 非便携式 / 系统安装) ----------
# 与便携式不同: 复制到安装目录后, regsvr32 注册 TSF COM (DllRegisterServer 自带
# AddLanguageProfile + RegisterCategories, 输入法直接进系统列表), 授权 AppContainer
# 宿主读取 DLL, 安装字根字体, 写开机自启, 直接启动 wind_input[_dev].exe (不靠
function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

# 部署命令 → 目标安装目录; 非部署命令返回 $null (兼作"是否部署命令"判断)。
# 注: 便携命令 (pb*/ub*) 刻意【不】列入 —— 便携部署是纯文件复制, 目标在普通目录,
# 不注册 COM 也不装字体, 无需管理员; 列进来只会让每次部署白弹一次 UAC。
function Deploy-TargetForCmd ([string]$cmd) {
    if (@("p1","pm1","pm2","pm3","pm4","u1","u") -contains $cmd)            { return $WIND_DIR_RELEASE }
    if (@("pd1","pdm1","pdm2","pdm3","pdm4","ud1","ud") -contains $cmd)     { return $WIND_DIR_DEV }
    return $null
}

# 需管理员但【非】部署的命令。刻意与 Deploy-TargetForCmd 分开: 后者的返回值是
# 真实的部署目标目录, 会被部署流程当路径使用; 把非部署命令塞进去就得编造假目录,
# 使一个返回值同时承担"目标路径"与"要不要提权"两个语义 —— 二者对边缘输入的期望相反。
# 目前只有 Defender 排除项的写入与移除属于此类 (写系统策略)。只读预览刻意不列入
# —— 让预览也弹一次 UAC 会毁掉它"随手看一眼"的用途。
function Test-NeedsAdminCmd ([string]$cmd) {
    return (@("av", "avr") -contains $cmd)
}

# 系统安装(注册 COM/icacls/字体)始终需管理员。非管理员执行部署命令时自动 UAC 提权。
# 返回三态: "skip" = 非部署命令/已是管理员 (调用方本地执行);
#           "done" = 提权进程已执行完毕, 输出已在当前窗口显示 (调用方直接继续);
#           "fail" = 提权被取消/失败 (调用方报错并以非零码退出)。
function Invoke-Elevated ([string]$cmd, [string]$arg) {
    $isDeploy = [bool](Deploy-TargetForCmd $cmd)
    if (-not $isDeploy -and -not (Test-NeedsAdminCmd $cmd)) { return "skip" }   # 无需管理员
    if (Test-Admin) { return "skip" }
    $why = if ($isDeploy) { "系统安装" } else { "修改 Defender 排除项" }
    Warn "$why 需要管理员权限, 正在请求 UAC 提升..."
    $host_exe = (Get-Process -Id $PID).Path   # pwsh.exe 或 powershell.exe
    if (-not $host_exe) { $host_exe = "pwsh.exe" }
    # 临时日志文件捕获提权子进程的全部输出流 (*>), 执行后读回在本窗口显示。
    $TmpLog = Join-Path $env:TEMP "wind_deploy_$(Get-Random -Maximum 99999999).log"
    $argPart = if ($arg) { " `"$arg`"" } else { "" }
    $innerCmd = "& `"$PSCommandPath`" `"$cmd`"$argPart *> `"$TmpLog`""
    $encodedCmd = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($innerCmd))
    try {
        # -PassThru 取得进程对象; 用 WaitForExit() 替代 -Wait 以确保子进程真正退出后再读日志。
        # (-Verb RunAs + -Wait 在部分 PS5.1 版本下存在不可靠的竞争问题)
        $proc = Start-Process -FilePath $host_exe -Verb RunAs -PassThru -ErrorAction Stop `
            -ArgumentList "-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-EncodedCommand", $encodedCmd
        $proc.WaitForExit()
        if (Test-Path $TmpLog) {
            Get-Content $TmpLog | ForEach-Object { Write-Host $_ }
            Remove-Item $TmpLog -ErrorAction SilentlyContinue
        }
        if ($proc.ExitCode -ne 0) { return "fail" }
        return "done"
    } catch {
        if (Test-Path $TmpLog) { Remove-Item $TmpLog -ErrorAction SilentlyContinue }
        ErrMsg "提权失败或被取消: $($_.Exception.Message)"
        ErrMsg "可【以管理员身份】重开 PowerShell 再运行本脚本。"
        return "fail"
    }
}

# 部署安全网: 非管理员 (如被直接调用未经提权) → 明确报错。
function Require-Admin {
    if (-not (Test-Admin)) {
        ErrMsg "系统安装需要管理员权限 (注册 TSF COM / 设置权限 / 安装字体)。"
        ErrMsg "请以【管理员身份】打开 PowerShell 后重试。"
        return $false
    }
    return $true
}

# 32 位 regsvr32 (注册 x86 TSF DLL, 写 WOW6432Node 供 32 位应用加载)。
function Get-Regsvr32X86 { Join-Path $env:SystemRoot "SysWOW64\regsvr32.exe" }

# 反注册安装目录中的旧 TSF COM (x64 + x86)。
function Unregister-Tsf ([string]$dir, [string]$suffix) {
    $x64 = Join-Path $dir "wind_tsf$suffix.dll"
    $x86 = Join-Path $dir "wind_tsf_x86${suffix}.dll"
    if (Test-Path $x64) { & regsvr32 /u /s $x64 2>$null }
    if (Test-Path $x86) { & (Get-Regsvr32X86) /u /s $x86 2>$null }
}

# 注册 TSF COM (x64 必须成功; x86 失败仅告警, 不阻断 64 位使用)。
function Register-Tsf ([string]$dir, [string]$suffix) {
    $x64 = Join-Path $dir "wind_tsf$suffix.dll"
    $x86 = Join-Path $dir "wind_tsf_x86${suffix}.dll"
    & regsvr32 /s $x64
    if ($LASTEXITCODE -ne 0) { ErrMsg "  - x64 COM 注册失败: $x64"; return $false }
    Gray "  - x64 COM 已注册"
    if (Test-Path $x86) {
        & (Get-Regsvr32X86) /s $x86
        if ($LASTEXITCODE -ne 0) { Warn "  - x86 COM 注册失败 (32 位应用可能无法使用输入法)" }
        else { Gray "  - x86 COM 已注册" }
    }
    return $true
}

# 将本变体 TSF 输入法加入【当前用户】中文(zh-CN)输入法列表 → 默认启用, 免去手动"添加键盘"。
# 背景: regsvr32/DllRegisterServer 只把 IME 注册为系统级"可用"; 对已配置好的语言, Windows
#       不会自动把新 TIP 追加进用户启用列表 (RegisterProfile 的 bEnabledByDefault 仅在该语言
#       【首次添加】时生效)。故此处显式追加。
# 注1: CLSID/Profile GUID 必须与 wind_tsf\src\Globals.cpp 一致 (dev=DEB0/DEB1, release=EE30/EE31)。
# 注2: 仅"添加"本变体, 绝不删除其它输入法 → 与系统已装的标准版清风/微软拼音等共存。
# 注3: 部署在管理员令牌下运行; 同账户 UAC 提升时 HKCU 仍指向本人, 故对当前用户生效。
function Enable-TsfForUser ([string]$profile) {
    if ($profile -eq "dev") {
        $tip = "0804:{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}{99C2DEB1-5C57-45A2-9C63-FB54B34FD90A}"
    } else {
        $tip = "0804:{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}"
    }
    try {
        $list = Get-WinUserLanguageList
        $zh = $list | Where-Object { $_.LanguageTag -like "zh-Hans*" -or $_.LanguageTag -like "zh-CN*" } | Select-Object -First 1
        if (-not $zh) {
            $list.Add("zh-Hans-CN")
            $zh = $list | Where-Object { $_.LanguageTag -like "zh-Hans*" } | Select-Object -First 1
        }
        if ($zh -and ($zh.InputMethodTips -notcontains $tip)) {
            $zh.InputMethodTips.Add($tip)
            Set-WinUserLanguageList -LanguageList $list -Force
            Gray "  - 已加入当前用户输入法列表 (默认启用, 与标准版共存)"
        } else {
            Gray "  - 输入法已在用户列表, 跳过"
        }
    } catch {
        Warn "  - 自动启用输入法失败 (可在 设置>时间和语言>语言>中文>选项>键盘 手动添加): $($_.Exception.Message)"
    }
}

# 授权 ALL APPLICATION PACKAGES 读取执行 TSF DLL (开始菜单/搜索等 AppContainer 宿主需要)。
function Grant-TsfAcl ([string]$dir, [string]$suffix) {
    $sid = "*S-1-15-2-1"
    foreach ($n in @("wind_tsf$suffix.dll", "wind_tsf_x86${suffix}.dll")) {
        $p = Join-Path $dir $n
        if (Test-Path $p) { & icacls $p /grant "${sid}:(RX)" /c | Out-Null }
    }
}

# 安装 PUA 字根字体到系统 (供 DirectWrite fallback; 已存在且一致则跳过)。best-effort。
function Install-WubiFont ([string]$dir) {
    $src = Join-Path $dir "data\schemas\wubi86\HeiTiZiGen.ttf"
    if (-not (Test-Path $src)) { return }
    $dest = Join-Path $env:SystemRoot "Fonts\HeiTiZiGen.ttf"
    try {
        $need = $true
        if (Test-Path $dest) {
            try { if ((Get-FileHash $src -Algorithm SHA1).Hash -eq (Get-FileHash $dest -Algorithm SHA1).Hash) { $need = $false } } catch { $need = $true }
        }
        if ($need) { Copy-Item $src $dest -Force }
        Set-ItemProperty -Path "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts" -Name "黑体字根 (TrueType)" -Value "HeiTiZiGen.ttf" -Force
        Gray "  - 字体: 黑体字根 $(if($need){'已安装'}else{'已存在,跳过'})"
    } catch { Warn "  - 安装字体失败: $($_.Exception.Message)" }
}

# 写开机自启 (HKCU Run; 免管理员)。
function Set-AutoStart ([string]$dir, [string]$suffix) {
    $name = if ($suffix) { "WindInputDev" } else { "WindInput" }
    $exe  = Join-Path $dir "wind_input$suffix.exe"
    try {
        Set-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name $name -Value "`"$exe`"" -Force
        Gray "  - 已配置开机自启 ($name)"
    } catch { Warn "  - 配置开机自启失败" }
}

# 终止占用目标 exe 的进程 (按镜像名), 等其退出让出文件锁; 仅对 .exe 生效。
# 背景: Stop-WindService 只杀核心服务 wind_input; 独立打开的设置程序 wind_setting[_dev].exe /
#       便携版 wind_portable.exe 不随之退出, 覆盖前需先按名杀掉 (对齐 ../wind-setting Do-Copy 的处理)。
# DLL 由宿主进程加载, 没有独立进程可杀 → 跳过, 仍靠 Copy-Replace 的改名让路兜底。
function Stop-ProcessForFile ([string]$fileName) {
    if ($fileName -notmatch '\.exe$') { return }
    $procName = [System.IO.Path]::GetFileNameWithoutExtension($fileName)
    $procs = @(Get-Process -Name $procName -ErrorAction SilentlyContinue)
    if ($procs.Count -gt 0) {
        Gray "  - 终止运行中的 $fileName ($($procs.Count) 个进程)..."
        $procs | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500
    }
}

# 复制单个文件, 处理被占用的 DLL/EXE。
# 顺序: ① 先杀占用该 exe 的进程 (如独立开着的设置程序) 并等待让出文件锁
#       ② 尝试覆盖 (= 删旧写新) ③ 仍被锁 (如已加载的 TSF DLL) 则改名让路再写。
function Copy-Replace ([string]$targetDir, [string]$fileName, [string]$srcPath) {
    $dst = Join-Path $targetDir $fileName
    if (-not (Test-Path $dst)) { Copy-Item $srcPath $dst -Force; Gray "  - $fileName"; return }
    Stop-ProcessForFile $fileName   # 先判断并杀进程等待, 再尝试覆盖; 覆盖失败才改名让路
    try { Copy-Item $srcPath $dst -Force -ErrorAction Stop; Gray "  - $fileName"; return } catch { }
    # 让路后缀必须每次唯一: NTFS 允许改名在用文件, 但不允许改名去【覆盖】一个在用文件。
    # 曾用固定 .old 槽复用, 结果上轮 .old 仍被宿主进程 map 着时 Move -Force 直接失败, 部署中断
    # (TSF DLL in-proc 常驻, 宿主不重启就一直锁旧代, 双代同锁是常态)。唯一后缀则目标必不存在,
    # 改名恒成功。垃圾累积由 Remove-OrRename 侧「不重复改名已让路文件」+ 各处 *.old* 清理消化。
    $old = "$dst.old_$(Get-Random -Maximum 99999999)"
    try {
        Move-Item $dst $old -Force -ErrorAction Stop
        Copy-Item $srcPath $dst -Force
        Gray "  - $fileName (旧文件已改名 $(Split-Path $old -Leaf))"
    } catch { ErrMsg "  [错误] 无法替换 ${fileName}: 旧文件被锁定且改名让路失败, 请重启后重试" }
}

function Stop-WindService ([string]$suffix) {
    Get-Process -Name "wind_input$suffix" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 600
}

# 部署期抑制 TSF DLL 自动拉起服务 (必须成对调用, 用 try/finally 保证清零)。
#
# 背景: 宿主进程里常驻的 wind_tsf DLL 连不上管道时会 CreateProcess 自行拉起服务
# (wind_tsf\src\IPCClient.cpp 的 _StartService)。部署期服务已被杀、data\ 正在重建,
# 此刻任何敲键/切焦点都会把服务拉起来读到【半截 data】—— 主题 not found、码表
# os error 3、拆字库缺失。更坏的是它占住单例后, 脚本末尾那次正规启动会被单例挡掉,
# 且单例检查在 init_logger 之前就 exit(1), 连一行日志都不留。于是那个残废进程一直
# 服务到下次重启, 表现为"重启一下就好了"的间歇性主题/词库丢失。
#
# DLL 侧早有闸门: HKLM\Software\WindInput 的 InstallerRunning="1" 时直接不拉
# (NSIS 安装器走的就是这条路), 但本脚本此前从不写它 —— 读端在本仓、写端在安装器仓,
# 没有任何编译期约束能发现这条部署路径漏接。
#
# 注: 该键 release/dev 共用一个路径 (DLL 读的是同一个), 故两变体部署不可并行。
function Set-InstallerRunning ([bool]$on) {
    $key = "HKLM:\Software\WindInput"
    try {
        if ($on) {
            if (-not (Test-Path $key)) { New-Item -Path $key -Force | Out-Null }
            Set-ItemProperty -Path $key -Name "InstallerRunning" -Value "1" -Type String -Force -ErrorAction Stop
            Gray "  - InstallerRunning=1 (部署期禁止 TSF DLL 拉起服务)"
        }
        elseif (Test-Path $key) {
            Set-ItemProperty -Path $key -Name "InstallerRunning" -Value "0" -Type String -Force -ErrorAction Stop
        }
    }
    catch {
        # 写不进去只是失去保护, 不该中断部署 —— 但必须喊出来, 否则又变成静默的竞态温床。
        Warn "  - InstallerRunning 闸门$(if($on){'开启'}else{'清除'})失败 ($_); 部署期请勿敲键或切换窗口"
    }
}

# 原子替换 data\: 先复制到同盘暂存目录, 再用两次改名切过去。
#
# 直接 Remove + Copy 会让 data\ 有【数秒】处于空/半截状态 (几万个文件), 这正是上面
# 那个抢跑窗口最危险的一段。改成暂存 + 改名后, 复制全程 data\ 仍是旧的【完整】数据,
# 真正的空窗压缩到两次改名之间的几毫秒 —— 即便闸门失效 (旧版 DLL 没有该判据、或
# 并发跑了别的部署), 抢跑进程拿到的也是一套自洽的旧数据, 而不是残废态。
# 暂存目录必须与目标同盘, 否则 Rename-Item 跨卷失败。
function Copy-DataAtomic ([string]$srcData, [string]$targetDir) {
    $td      = Join-Path $targetDir "data"
    $staging = Join-Path $targetDir "data.new_$(Get-Random -Maximum 99999999)"
    try {
        Copy-Item $srcData -Destination $staging -Recurse -Force -ErrorAction Stop
    }
    catch {
        ErrMsg "  [错误] data\ 暂存复制失败: $_"
        Remove-Item $staging -Recurse -Force -ErrorAction SilentlyContinue
        return $false
    }
    $retired = $null
    if (Test-Path $td) {
        $retired = "$td.old_$(Get-Random -Maximum 99999999)"
        try { Rename-Item $td $retired -ErrorAction Stop }
        catch {
            ErrMsg "  [错误] 旧 data\ 改名让路失败 (被进程占用?): $_"
            Remove-Item $staging -Recurse -Force -ErrorAction SilentlyContinue
            return $false
        }
    }
    try { Rename-Item $staging $td -ErrorAction Stop }
    catch {
        ErrMsg "  [错误] 新 data\ 就位失败: $_"
        # 切换失败必须把旧数据放回去, 否则留下一个没有 data\ 的安装目录。
        if ($retired) { Rename-Item $retired $td -ErrorAction SilentlyContinue }
        return $false
    }
    if ($retired) { Remove-Item $retired -Recurse -Force -ErrorAction SilentlyContinue }
    Gray "  - data\ (词库、方案、主题; 暂存+改名原子切换)"
    return $true
}

# 系统安装: 全部 build[_dev]/ → 安装目录, 注册 TSF + 开机自启 + 启动服务 (p1 / pd1)。
function Deploy-Full ([string]$profile = "release") {
    $outdir = Out-For $profile
    $targetDir = if ($profile -eq "dev") { $WIND_DIR_DEV } else { $WIND_DIR_RELEASE }
    $suffix = if ($profile -eq "dev") { "_dev" } else { "" }
    if (-not (Require-Admin)) { return $false }
    if (-not (Test-Path "$outdir\wind_input$suffix.exe")) {
        ErrMsg "无 $outdir 产物; 请先 '$(if($profile -eq 'dev'){'d1'}else{'1'})' 全构建。"; return $false
    }
    Say "`n========== 系统安装 ($profile) → $targetDir =========="
    # 闸门必须在停服务【之前】拉起 —— 服务一死, 宿主里的 DLL 立刻就有拉起它的动机。
    Set-InstallerRunning $true
    try {
        Say "[1/7] 停止旧进程..."; Stop-WindService $suffix
        Say "[2/7] 反注册旧 TSF COM..."; Unregister-Tsf $targetDir $suffix
        Say "[3/7] 准备目录..."; New-Item -ItemType Directory -Path $targetDir -Force | Out-Null
        Say "[4/7] 复制文件..."
        Copy-Replace $targetDir "wind_input$suffix.exe" "$outdir\wind_input$suffix.exe"
        if (Test-Path "$outdir\wind_cli.bat")      { Copy-Replace $targetDir "wind_cli.bat"      "$outdir\wind_cli.bat" }
        foreach ($dll in (Get-ChildItem "$outdir\wind_tsf*.dll" -ErrorAction SilentlyContinue)) {
            Copy-Replace $targetDir $dll.Name $dll.FullName
        }
        if (Test-Path "$outdir\wind_setting$suffix.exe") { Copy-Replace $targetDir "wind_setting$suffix.exe" "$outdir\wind_setting$suffix.exe" }
        if (Test-Path "$outdir\wind_portable.exe")       { Copy-Replace $targetDir "wind_portable.exe"       "$outdir\wind_portable.exe" }
        if (Test-Path "$outdir\data") {
            if (-not (Copy-DataAtomic "$outdir\data" $targetDir)) { return $false }
        }
        Say "[5/7] 设置权限 + 注册 TSF COM..."
        Grant-TsfAcl $targetDir $suffix
        if (-not (Register-Tsf $targetDir $suffix)) { return $false }
        Install-WubiFont $targetDir
        Say "[6/7] 配置开机自启 + 默认启用输入法..."
        Set-AutoStart $targetDir $suffix
        Enable-TsfForUser $profile
        Get-ChildItem "$targetDir\*.old*" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
        Say "[7/7] 启动输入法服务..."
        $exe = Join-Path $targetDir "wind_input$suffix.exe"
        Start-Process -FilePath $exe; Gray "  - 已启动 wind_input$suffix.exe"
        Say "`n系统安装完成 ($profile) → $targetDir"
        Say "提示: 按 Win+Space 切换到清风输入法$(if($suffix){' (Dev)'})。"
        return $true
    }
    finally {
        # 任何出口 (含中途 return $false 与异常) 都必须清零, 否则闸门永久留在 1,
        # DLL 从此再不肯拉起服务 —— 那会是个比本竞态更难查的故障。
        Set-InstallerRunning $false
    }
}

# 模块名 → 该模块的产物文件。Required 缺失即判失败 (防"部署成功但什么都没换"),
# Optional 有则一并带上。系统安装与便携部署共用同一份映射, 免得两处各写一遍而漂移。
function Get-ModuleFiles ([string]$mod, [string]$suffix) {
    switch ($mod) {
        "tsf"     { @{ Required = @("wind_tsf$suffix.dll", "wind_tsf_x86${suffix}.dll"); Optional = @() } }
        # wind_cli.bat 两变体共用一份、且 Build-Core 里是"存在才复制", 故列为可选
        "core"    { @{ Required = @("wind_input$suffix.exe"); Optional = @("wind_cli.bat") } }
        "setting" { @{ Required = @("wind_setting$suffix.exe"); Optional = @() } }
        # 便携启动器是单一二进制, 不带变体后缀 (运行时按同级有无 _dev exe 自辨变体)
        "portable" { @{ Required = @("wind_portable.exe"); Optional = @() } }
        default   { $null }
    }
}

# 系统安装单模块 (不重编, 用现有产物): pm1=tsf pm2=core pm3=setting pm4=portable (pd 前缀=dev)。
#   tsf : 停服务 → 反注册旧 COM → 复制 → icacls → 重注册 → 重启服务
#   core: 停服务 → 复制 (含 wind_cli.bat) → 重启服务
#   setting/portable: 仅复制 —— 它们是独立进程, 不参与输入法运行时。停/重启核心服务会
#   平白打断正在使用的输入法, 故这两个模块不碰服务 (Copy-Replace 内部按镜像名杀各自进程)。
function Deploy-Module ([string]$profile, [string]$mod) {
    $outdir = Out-For $profile
    $targetDir = if ($profile -eq "dev") { $WIND_DIR_DEV } else { $WIND_DIR_RELEASE }
    $suffix = if ($profile -eq "dev") { "_dev" } else { "" }
    $spec = Get-ModuleFiles $mod $suffix
    if (-not $spec) { ErrMsg "未知模块: $mod (tsf|core|setting|portable)"; return $false }
    $touchesService = @("tsf", "core") -contains $mod
    if (-not (Require-Admin)) { return $false }
    if (-not (Test-Path $targetDir)) {
        ErrMsg "安装目录不存在: $targetDir; 请先 '$(if($profile -eq 'dev'){'pd1'}else{'p1'})' 完整安装。"; return $false
    }
    foreach ($f in $spec.Required) {
        if (-not (Test-Path "$outdir\$f")) { ErrMsg "本地无 $outdir\$f (先构建对应模块)"; return $false }
    }
    Say "`n========== 系统安装模块 ($profile/$mod) → $targetDir =========="
    # 模块部署不动 data\, 但 core/tsf 会杀服务或换 DLL —— 同样给了 DLL 抢跑的机会,
    # 拉起的是新旧混搭的一代 (如新 DLL 配旧 exe), 故一并上闸门。
    if ($touchesService) { Set-InstallerRunning $true }
    try {
        if ($touchesService) { Say "[1/4] 停止旧进程..."; Stop-WindService $suffix }
        else                 { Say "[1/4] (跳过停服务: $mod 不参与输入法运行时)" }
        if ($mod -eq "tsf") { Say "[2/4] 反注册旧 TSF COM..."; Unregister-Tsf $targetDir $suffix }
        else                { Say "[2/4] ($mod 无需反注册 COM)" }
        Say "[3/4] 复制模块文件..."
        foreach ($f in $spec.Required) { Copy-Replace $targetDir $f "$outdir\$f" }
        foreach ($f in $spec.Optional) { if (Test-Path "$outdir\$f") { Copy-Replace $targetDir $f "$outdir\$f" } }
        if ($mod -eq "tsf") {
            Grant-TsfAcl $targetDir $suffix
            if (-not (Register-Tsf $targetDir $suffix)) { return $false }
            Enable-TsfForUser $profile
        }
        Get-ChildItem "$targetDir\*.old*" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
        if ($touchesService) {
            Say "[4/4] 启动输入法服务..."
            $exe = Join-Path $targetDir "wind_input$suffix.exe"
            if (Test-Path $exe) { Start-Process -FilePath $exe; Gray "  - 已启动 wind_input$suffix.exe" }
        } else {
            Say "[4/4] (跳过重启服务: $mod 不参与输入法运行时)"
        }
        Say "`n模块部署完成 ($profile/$mod)"
        return $true
    }
    finally {
        if ($touchesService) { Set-InstallerRunning $false }
    }
}

# ---------- 便携部署 (绿色版: 纯复制到本地目录, 免注册免污染) ----------
# 与系统安装的区别: 不 regsvr32、不写 HKCU Run、不装字体、不改用户输入法列表 —— 这些全部
# 由 wind_portable.exe 运行期自行完成 (registration.rs 见到便携标记才走便携注册路径), 退出时
# 再自行撤销。故便携部署 = 复制文件 + 写标记 + 拉起 launcher, 全程无需管理员。
function Portable-TargetFor ([string]$profile) {
    if ($profile -eq "dev") { return $WIND_DIR_PORTABLE_DEV }
    return $WIND_DIR_PORTABLE_RELEASE
}

# 便携根目录合法性校验 —— 对齐 wind-portable\src\layout.rs 的 is_protected_dir。
# 提前拦下, 否则文件都复制完了才在启动 launcher 时收到"不支持便携模式"而白忙一场。
# 注: $profile 显式传参 —— 不可省。PowerShell 的 $profile 就是自动变量 $PROFILE (用户配置
# 文件路径, 变量名不区分大小写); 函数内不声明就会靠动态作用域偷读调用方的同名参数, 调用链
# 一变即静默读到那个路径串。
function Test-PortableRoot ([string]$root, [string]$profile = "release") {
    $prefixes = @($env:ProgramFiles, ${env:ProgramFiles(x86)}, $env:ProgramW6432, $env:SystemRoot) |
        Where-Object { $_ }
    $lower = $root.ToLower().TrimEnd('\')
    foreach ($p in $prefixes) {
        $pl = $p.ToLower().TrimEnd('\')
        if ($lower -eq $pl -or $lower.StartsWith("$pl\")) {
            ErrMsg "便携目录不能位于系统保护目录下: $root"
            ErrMsg "  (命中前缀: $p) —— wind_portable 会拒绝在此启动便携模式。"
            $varName = if ($profile -eq 'dev') { 'WIND_DIR_PORTABLE_DEV' } else { 'WIND_DIR_PORTABLE_RELEASE' }
            ErrMsg "请在 scripts\deploy.local.ps1 中把 `$$varName 改到普通目录, 如 D:\WindInputPortable。"
            return $false
        }
    }
    return $true
}

# 只停【本便携目录下】的进程。便携版与系统安装版镜像名完全相同 (都是 wind_input.exe),
# 按名杀会连正在使用的系统安装版一起干掉, 故必须按 Path 前缀过滤。
function Stop-PortableProcesses ([string]$root, [string]$suffix) {
    $names = @("wind_portable", "wind_input$suffix", "wind_setting$suffix")
    $rootPrefix = $root.TrimEnd('\') + "\"
    $killed = 0
    foreach ($n in $names) {
        $procs = @(Get-Process -Name $n -ErrorAction SilentlyContinue | Where-Object {
            $_.Path -and $_.Path.StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)
        })
        foreach ($p in $procs) { $p | Stop-Process -Force -ErrorAction SilentlyContinue; $killed++ }
    }
    if ($killed -gt 0) { Gray "  - 已停止 $killed 个便携进程"; Start-Sleep -Milliseconds 600 }
    else { Gray "  - 无运行中的便携进程" }
}

# 写便携标记文件 (root\portable_mode)。launcher 启动时也会自建, 此处先写是为了让
# "复制完还没跑过 launcher"的目录就已经是合法便携包 (直接双击 wind_input.exe 也走便携路径)。
# 内容与 wind-portable\src\service.rs ensure_portable_layout 一致; 已存在则不覆盖 ——
# 运行期可能写入 stopped=1 等守卫位, 覆盖会抹掉状态。
# 存量目录里可能只有旧名: 补写新名完成迁移, 旧名保留 (回退到旧版程序时它仍是唯一被认的标记)。
function Write-PortableMarker ([string]$root) {
    $marker = Join-Path $root $PortableMarkerName
    if (Test-Path $marker) { Gray "  - 便携标记已存在, 保留 ($PortableMarkerName)"; return }
    [System.IO.File]::WriteAllText($marker, "wind_portable=1`n", (New-Object System.Text.UTF8Encoding($false)))
    if (Test-Path (Join-Path $root $PortableMarkerLegacy)) {
        Gray "  - 已补写新便携标记 ($PortableMarkerName); 旧名 $PortableMarkerLegacy 保留"
    } else {
        Gray "  - 已写便携标记 ($PortableMarkerName)"
    }
}

# 启动便携 launcher。WorkingDirectory 必须设为便携根 —— layout.rs 的候选根探测会用到
# current_dir, 从别处拉起可能探到错误的根。
function Start-Portable ([string]$root) {
    $exe = Join-Path $root "wind_portable.exe"
    if (-not (Test-Path $exe)) {
        Warn "  - 无 wind_portable.exe, 跳过启动 (可手动运行目录下的 wind_input*.exe)"
        return
    }
    Start-Process -FilePath $exe -WorkingDirectory $root
    Gray "  - 已启动 wind_portable.exe"
}

# 便携部署全部 (pb1 / pbd1): build[_dev]\ 全量 → 便携目录。
# userdata\ 是便携版的用户数据 (词库/配置/统计), 就在便携根目录内部 —— 全量部署只替换
# 程序文件与 data\, 绝不碰 userdata\, 否则一次重新部署就抹掉用户全部个人数据。
function Deploy-Portable ([string]$profile = "release") {
    $outdir = Out-For $profile
    $root = Portable-TargetFor $profile
    $suffix = if ($profile -eq "dev") { "_dev" } else { "" }
    if (-not (Test-Path "$outdir\wind_input$suffix.exe")) {
        ErrMsg "无 $outdir 产物; 请先 '$(if($profile -eq 'dev'){'d1'}else{'1'})' 全构建。"; return $false
    }
    if (-not (Test-PortableRoot $root $profile)) { return $false }
    Say "`n========== 便携部署 ($profile) → $root =========="
    Say "[1/5] 停止便携进程..."; Stop-PortableProcesses $root $suffix
    Say "[2/5] 准备目录..."; New-Item -ItemType Directory -Path $root -Force | Out-Null
    Say "[3/5] 复制程序文件..."
    Copy-Replace $root "wind_input$suffix.exe" "$outdir\wind_input$suffix.exe"
    if (Test-Path "$outdir\wind_cli.bat") { Copy-Replace $root "wind_cli.bat" "$outdir\wind_cli.bat" }
    foreach ($dll in (Get-ChildItem "$outdir\wind_tsf*.dll" -ErrorAction SilentlyContinue)) {
        Copy-Replace $root $dll.Name $dll.FullName
    }
    if (Test-Path "$outdir\wind_setting$suffix.exe") { Copy-Replace $root "wind_setting$suffix.exe" "$outdir\wind_setting$suffix.exe" }
    if (Test-Path "$outdir\wind_portable.exe")       { Copy-Replace $root "wind_portable.exe"       "$outdir\wind_portable.exe" }
    # 便携版同样走原子替换 (理由见 Copy-DataAtomic)。此处不上 InstallerRunning 闸门:
    # 便携部署不要求管理员, 写不了 HKLM; 便携实例的服务由 launcher 托管、DLL 另走
    # portable_mode 标记路径, 抢跑面小于系统安装。
    if (Test-Path "$outdir\data") {
        if (-not (Copy-DataAtomic "$outdir\data" $root)) { return $false }
    }
    Get-ChildItem "$root\*.old*" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
    Say "[4/5] 写便携标记..."; Write-PortableMarker $root
    Say "[5/5] 启动便携版..."; Start-Portable $root
    Say "`n便携部署完成 ($profile) → $root"
    Gray "用户数据目录: $root\$PortableDataDir (部署不会覆盖)"
    return $true
}

# 便携部署单模块 (pbm1..pbm4 / pbdm1..pbdm4): 只换指定模块文件, 不重编不动 data\。
# 停的是整个便携实例 —— 便携版由 launcher 统一托管 (TSF DLL 由它注册、服务由它拉起),
# 换任何一个模块都得让它整体重启才能生效, 单独换文件而不重启等于没换。
function Deploy-PortableModule ([string]$profile, [string]$mod) {
    $outdir = Out-For $profile
    $root = Portable-TargetFor $profile
    $suffix = if ($profile -eq "dev") { "_dev" } else { "" }
    $spec = Get-ModuleFiles $mod $suffix
    if (-not $spec) { ErrMsg "未知模块: $mod (tsf|core|setting|portable)"; return $false }
    if (-not (Test-PortableRoot $root $profile)) { return $false }
    if (-not (Test-Path $root)) {
        ErrMsg "便携目录不存在: $root; 请先 '$(if($profile -eq 'dev'){'pbd1'}else{'pb1'})' 完整部署。"; return $false
    }
    foreach ($f in $spec.Required) {
        if (-not (Test-Path "$outdir\$f")) { ErrMsg "本地无 $outdir\$f (先构建对应模块)"; return $false }
    }
    Say "`n========== 便携部署模块 ($profile/$mod) → $root =========="
    Say "[1/3] 停止便携进程..."; Stop-PortableProcesses $root $suffix
    Say "[2/3] 复制模块文件..."
    foreach ($f in $spec.Required) { Copy-Replace $root $f "$outdir\$f" }
    foreach ($f in $spec.Optional) { if (Test-Path "$outdir\$f") { Copy-Replace $root $f "$outdir\$f" } }
    Get-ChildItem "$root\*.old*" -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
    Say "[3/3] 重启便携版..."; Write-PortableMarker $root; Start-Portable $root
    Say "`n便携模块部署完成 ($profile/$mod)"
    return $true
}

# 便携卸载 (ub1/ub / ubd1/ubd): 停进程 + 删程序文件; userdata\ 保留。
# 【不】动 HKCU Run 与 TSF COM 注册 —— 便携版用的是与系统安装版【完全相同】的自启值名
# (WindInput) 和 CLSID, 脚本在此清理会连同已装的系统版一起废掉。这些注册项由便携版自己
# 在退出时撤销, 故正确姿势是先在便携版托盘里退出, 再跑本命令。
function Uninstall-Portable ([string]$profile = "release") {
    $root = Portable-TargetFor $profile
    $suffix = if ($profile -eq "dev") { "_dev" } else { "" }
    if (-not (Test-Path $root)) { Warn "便携目录不存在, 无需卸载: $root"; return $true }
    Say "`n========== 便携卸载 ($profile) → $root =========="
    Say "[1/2] 停止便携进程..."; Stop-PortableProcesses $root $suffix
    Say "[2/2] 删除程序文件 (保留 $PortableDataDir\)..."
    $dataRoot = (Join-Path $root $PortableDataDir).TrimEnd('\') + "\"
    Get-ChildItem "$root\*.old*" -Recurse -File -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
    $allGone = $true
    Get-ChildItem $root -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object {
        if ($_.FullName.StartsWith($dataRoot, [System.StringComparison]::OrdinalIgnoreCase)) { return }
        if (-not (Remove-OrRename $_.FullName)) { $allGone = $false }
    }
    # 清空的子目录一并删掉 (userdata\ 除外); 目录本身留待最后判断
    Get-ChildItem $root -Directory -ErrorAction SilentlyContinue | ForEach-Object {
        if ($_.Name -ieq $PortableDataDir) { return }
        if (-not (Get-ChildItem $_.FullName -Recurse -File -ErrorAction SilentlyContinue)) {
            Remove-Item $_.FullName -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
    if (Test-Path (Join-Path $root $PortableDataDir)) {
        Say "`n便携卸载完成 ($profile); 用户数据已保留:"
        Warn "  $root\$PortableDataDir  (如需彻底清除请手动删除整个 $root)"
    } elseif ($allGone) {
        try { Remove-Item $root -Recurse -Force -ErrorAction Stop; Gray "  - 已删除便携目录 $root" }
        catch { Warn "  - 文件已清空, 但目录未能删除 (重启后可删): $root" }
        Say "`n便携卸载完成 ($profile)。"
    } else {
        Warn "  - 部分文件被占用已改名让路; 重启后重跑本命令或手动删除: $root"
    }
    Gray "注: 便携版的开机自启与 TSF 注册由其自身托管, 本命令未改动 (与系统安装版共用同一注册项)。"
    return $true
}

# ---------- 卸载 (系统卸载 = 安装的逆操作) ----------
# 从当前用户中文输入法列表移除本变体 TIP (Enable-TsfForUser 的逆操作)。
function Disable-TsfForUser ([string]$profile) {
    if ($profile -eq "dev") {
        $tip = "0804:{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}{99C2DEB1-5C57-45A2-9C63-FB54B34FD90A}"
    } else {
        $tip = "0804:{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}"
    }
    try {
        $list = Get-WinUserLanguageList
        $changed = $false
        foreach ($l in $list) {
            if ($l.InputMethodTips -contains $tip) { [void]$l.InputMethodTips.Remove($tip); $changed = $true }
        }
        if ($changed) { Set-WinUserLanguageList -LanguageList $list -Force; Gray "  - 已从用户输入法列表移除" }
        else { Gray "  - 用户列表无此输入法, 跳过" }
    } catch { Warn "  - 移除用户输入法失败: $($_.Exception.Message)" }
}

# 移除开机自启 (HKCU Run; Set-AutoStart 的逆操作)。
function Remove-AutoStart ([string]$suffix) {
    $name = if ($suffix) { "WindInputDev" } else { "WindInput" }
    Remove-ItemProperty -Path "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name $name -ErrorAction SilentlyContinue
    Gray "  - 已移除开机自启 ($name)"
}

# 删除单个文件; 被占用(已加载的 DLL)时改名让路 — NTFS 允许改名在用文件, 仅不可删。
# 返回 $true=已真正删除; $false=删不掉(已改名让路或失败)。与 Copy-Replace 同一唯一后缀让路策略。
function Remove-OrRename ([string]$path) {
    if (-not (Test-Path $path)) { return $true }
    $leaf = Split-Path $path -Leaf
    try { Remove-Item $path -Force -ErrorAction Stop; Gray "  - 删除 $leaf"; return $true }
    catch {
        # 已带让路标记的文件不再重复改名: 它已经让过路了, 改成 .old_a.old_b 既无意义,
        # 又是垃圾累积的真正来源 (卸载遍历目录下所有文件, 每轮把删不掉的存量整体翻新一遍)。
        # 只标记未让路的原文件, 则每个原文件至多留一个残留, 待宿主释放后被 *.old* 清理带走。
        if ($leaf -match '\.old(_\d+)?$') {
            Warn "  - $leaf 仍被占用 (历史让路文件, 不再改名); 重启后可清除"
            return $false
        }
        $old = "$path.old_$(Get-Random -Maximum 99999999)"
        try {
            Move-Item $path $old -Force -ErrorAction Stop
            Warn "  - $leaf 被占用, 已改名让路 ($(Split-Path $old -Leaf)); 重启后可清除"
        } catch {
            ErrMsg "  - $leaf 删除/改名均失败: $($_.Exception.Message)"
        }
        return $false
    }
}

# 系统卸载: 完整撤销 Deploy-Full 的副作用 (u1 / ud1)。
#   停进程 → 移出用户输入法列表 → 反注册 TSF COM(x64+x86) → 移除开机自启 → 删安装目录。
# 共存安全: 仅动本变体 (CLSID/目录/自启名均带本变体后缀), 不影响另一变体或系统其它输入法。
# 字体(黑体字根)为两变体共享, 故【不】卸载, 以免影响仍在用的另一变体。
# 个人数据(词库/配置/统计)默认保留; 仅打印路径供手动清除。
function Uninstall-Full ([string]$profile = "release") {
    $targetDir = if ($profile -eq "dev") { $WIND_DIR_DEV } else { $WIND_DIR_RELEASE }
    $suffix = if ($profile -eq "dev") { "_dev" } else { "" }
    if (-not (Require-Admin)) { return $false }
    Say "`n========== 系统卸载 ($profile) → $targetDir =========="
    Say "[1/5] 停止进程..."; Stop-WindService $suffix
    Say "[2/5] 移出用户输入法列表..."; Disable-TsfForUser $profile
    Say "[3/5] 反注册 TSF COM..."
    if (Test-Path $targetDir) { Unregister-Tsf $targetDir $suffix; Gray "  - 已反注册 (x64 + x86)" }
    else { Warn "  - 安装目录不存在, 跳过反注册 (可能已卸载)" }
    Say "[4/5] 移除开机自启..."; Remove-AutoStart $suffix
    Say "[5/5] 删除安装文件 (锁定的 DLL 改名让路)..."
    if (Test-Path $targetDir) {
        # 先清掉历史改名残留 (上次卸载留下、此刻或已可删)
        Get-ChildItem "$targetDir\*.old*" -Recurse -File -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
        # 逐文件删除; 占用的(TSF DLL 等)改名让路, 不再因单个锁定文件整体失败
        $allGone = $true
        Get-ChildItem $targetDir -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object {
            if (-not (Remove-OrRename $_.FullName)) { $allGone = $false }
        }
        if ($allGone) {
            try { Remove-Item $targetDir -Recurse -Force -ErrorAction Stop; Gray "  - 已删除安装目录 $targetDir" }
            catch { Warn "  - 文件已清空, 但目录未能删除 (重启后可删): $targetDir" }
        } else {
            Warn "  - 部分文件被占用已改名让路; 重启系统后重跑本命令或手动删除残留目录:"
            Warn "    $targetDir"
        }
    } else { Gray "  - 目录不存在, 跳过" }
    Say "`n系统卸载完成 ($profile)。"
    $appName = if ($suffix) { "WindInputDev" } else { "WindInput" }
    Warn "提示: 个人数据已保留, 如需彻底清除请手动删除:"
    Warn "  漫游配置/词库: $env:APPDATA\$appName"
    Warn "  本机缓存/日志: $env:LOCALAPPDATA\$appName"
    return $true
}

# ---------- 安装包打包 (调用兄弟项目 wind-installer, app.toml 驱动) ----------
# wind-installer 是「通用安装器生成器」: 同一预编译 stub 配不同 app.toml 即生成不同安装包。
# 安装目录由 app.toml 的 [app] id 派生 (ProgramFiles\<id>), 故 dev=WindInputDev、release=WindInput
# 自然落到与 pd1/p1 一致的目录; IME 注册 GUID/文件名/字体亦全部由清单描述, 无需改安装器源码。
#
# 生成变体 app.toml: 全用绝对路径 + 正斜杠 (TOML 与 Windows 均接受正斜杠, 免去反斜杠转义;
# 且 pack.ps1 用 "([^"]+)" 正则解析 source_dir, 双引号字符串才能匹配)。落到 dist\ (在 source 之外,
# 不会被 packer 递归打进包)。GUID 必须与 wind_tsf\src\Globals.cpp 一致 (dev=DEB0/DEB1, release=EE30/EE31)。
function New-InstallerConfig ([string]$profile, [string]$outdir, [string]$cfgPath, [string]$assetsDir) {
    if ($profile -eq "dev") {
        $id = "WindInputDev"; $disp = "清风输入法 (开发版)"; $mainExe = "wind_input_dev.exe"
        $menu = "清风输入法 (开发版)"; $title = "清风输入法 (开发版) 安装向导"; $proto = "windinputdev"
        $settingExe = "wind_setting_dev.exe"
        $procs = '["wind_setting_dev", "wind_portable", "wind_input_dev"]'
        $acl   = '["wind_tsf_dev.dll", "wind_tsf_x86_dev.dll"]'
        $clsid = "{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}"; $prof = "{99C2DEB1-5C57-45A2-9C63-FB54B34FD90A}"
        $dllX64 = "wind_tsf_dev.dll"; $dllX86 = "wind_tsf_x86_dev.dll"; $outName = "WindInputDev-Setup"
    } else {
        $id = "WindInput"; $disp = "清风输入法"; $mainExe = "wind_input.exe"
        $menu = "清风输入法"; $title = "清风输入法 安装向导"; $proto = "windinput"
        $settingExe = "wind_setting.exe"
        $procs = '["wind_setting", "wind_portable", "wind_input"]'
        $acl   = '["wind_tsf.dll", "wind_tsf_x86.dll"]'
        $clsid = "{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}"; $prof = "{99C2EE31-5C57-45A2-9C63-FB54B34FD90A}"
        $dllX64 = "wind_tsf.dll"; $dllX86 = "wind_tsf_x86.dll"; $outName = "WindInput-Setup"
    }
    # 设置程序为可选模块: ../wind-setting 不存在时 Build-Setting 会跳过, build/ 里就没有产物。
    # 此时必须置空 setting_exe, 否则安装器会为不存在的文件建开始菜单快捷方式。
    if (-not (Test-Path (Join-Path $outdir $settingExe))) {
        Warn "未找到 $outdir\$settingExe, 本次打包不含设置程序 (setting_exe 置空)"
        $settingExe = ""
    }
    $srcFwd  = $outdir.Replace('\', '/')
    $distFwd = $DistDir.Replace('\', '/')
    $logoFwd = (Join-Path $assetsDir "logo.png").Replace('\', '/')
    $iconFwd = (Join-Path $assetsDir "installer.ico").Replace('\', '/')

    # 单一真相: 读 config\app.toml, 仅把 [app]/[ime]/[package] 替换为变体/机器相关值;
    # [[font]]/[autostart]/[[shortcut]]/[startup]/[datadir]/[strings]/[ui] 等能力与文案段原样继承。
    # 快捷方式用 {setting_exe}/{main_exe}/{display_name} 占位符, 安装器运行期按 [app] 字段替换,
    # 故一份 config 对 dev/release 通用; {setting_exe} 为空 (无设置程序) 时安装器自动跳过该快捷方式。
    # 这样 wind-installer 新增能力段时只需改 config\app.toml 一处, 无需同步本脚本 (消除双真相漂移)。
    $baseCfg = Join-Path $ProductRoot "config\app.toml"
    if (-not (Test-Path $baseCfg)) { ErrMsg "未找到清单基底: $baseCfg"; throw "缺少 config\app.toml" }
    $base = Get-Content $baseCfg -Raw

    $appSec = @"
[app]
id                = "$id"
display_name      = "$disp"
version           = "$Version"
publisher         = "清风输入法 项目"
description       = "轻量开源输入法"
main_exe          = "$mainExe"
setting_exe       = "$settingExe"
start_menu_folder = "$menu"
window_title      = "$title"
url_protocol      = "$proto"
portable_marker   = "portable_mode"
process_names     = $procs
acl_dlls          = $acl
"@
    $imeSec = @"
[ime]
clsid        = "$clsid"
profile_guid = "$prof"
lang_id      = "0804"
dll_x64      = "$dllX64"
dll_x86      = "$dllX86"
"@
    $pkgSec = @"
[package]
compression = "zstd"
source_dir  = "$srcFwd"
output_name = "$outName"
output_dir  = "$distFwd"
logo        = "$logoFwd"
icon        = "$iconFwd"
"@
    # 砍掉 config 的 [package] 及之后 (打包参数按机器生成), 再替换 [app]/[ime] 段。
    # 用 MatchEvaluator 回调返回字面串, 避免 -replace 把替换文本里的 $ 当分组引用。
    $head = ($base -split '(?m)^\[package\]', 2)[0]
    $head = [regex]::Replace($head, '(?ms)^\[app\]\r?\n.*?(?=^\[)', { param($x) $appSec + "`r`n`r`n" })
    $head = [regex]::Replace($head, '(?ms)^\[ime\]\r?\n.*?(?=^\[)',  { param($x) $imeSec + "`r`n`r`n" })
    $ai = $head.IndexOf("[app]"); if ($ai -gt 0) { $head = $head.Substring($ai) }
    $gen = "# 本文件由 dev.ps1 自动生成 —— $profile 变体; [app]/[ime]/[package] 为变体/机器值, 其余段继承 config\app.toml。请勿手工编辑。`r`n"
    $toml = $gen + $head.TrimEnd() + "`r`n`r`n" + $pkgSec + "`r`n"

    # 无 BOM UTF-8 写出 (Rust toml 解析器对前置 BOM 会报错; PS5.1 的 Set-Content -Encoding UTF8 带 BOM)。
    [System.IO.File]::WriteAllText($cfgPath, $toml, (New-Object System.Text.UTF8Encoding($false)))
}

# ---------- 在线升级元数据 (latest.json / latest-dev.json) ----------
# 供 wind-setting 的在线升级检查读取, 与安装包一并上传 CDN。
# 字段契约见 wind-setting\docs\online-update-plan.md §3.2。要点:
#   · sha256/size 为必填 —— 客户端在缺失或不匹配时拒绝升级, 不退化为"不校验就装"
#     (旧 Go 版官网渠道 size 恒为 0, 导致 %TEMP% 里一个被截断的同名文件会被当成完整包安装)。
#   · channel 与客户端自身变体交叉校验, 防 CDN 缓存串档把 dev 包发给正式版用户。
# 两个变体各写各的文件, 互不干扰; 上传时务必**先传 exe 再传 json** —— json 是开关,
# 反过来会让客户端看到新版本却下载到 404。
function New-UpdateManifest ([string]$profile, [string]$setupPath) {
    $isDev    = ($profile -eq "dev")
    $channel  = if ($isDev) { "dev" } else { "stable" }
    $base     = if ($isDev) { "WindInputDev" } else { "WindInput" }
    $jsonName = if ($isDev) { "latest-dev.json" } else { "latest.json" }

    $item = Get-Item $setupPath
    $sha  = (Get-FileHash -Path $setupPath -Algorithm SHA256).Hash.ToLower()

    # sha256 sidecar (标准 sha256sum 格式), 便于手工核对与 CDN 侧校验
    $shaFile = "$setupPath.sha256"
    [System.IO.File]::WriteAllText($shaFile, "$sha  $($item.Name)`n",
        (New-Object System.Text.UTF8Encoding($false)))

    $manifest = [ordered]@{
        version         = $Version
        tag             = "v$Version"
        channel         = $channel
        exeUrl          = "$CdnBase/$($item.Name)"
        sha256          = $sha
        size            = $item.Length
        releaseNotesUrl = "$CdnBase/$base-$Version-Release.md"
        publishedAt     = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    }

    $out = Join-Path $DistDir $jsonName
    # 无 BOM UTF-8: 客户端按 UTF-8 文本解析, 前置 BOM 会让 serde_json 报错。
    [System.IO.File]::WriteAllText($out, ($manifest | ConvertTo-Json -Depth 3),
        (New-Object System.Text.UTF8Encoding($false)))

    Say "升级元数据: $out"
    Gray "  channel=$channel  version=$Version  size=$($item.Length)"
    Gray "  sha256=$sha"
    Gray "  上传顺序: 先 $($item.Name), 确认可访问后再 $jsonName"
}

# 生成安装包: (除非 skip) 全构建当前变体 → 生成 app.toml → 调 wind-installer\scripts\pack.ps1。
#   pack.ps1 负责: 原生编译 stub/uninstaller/packer → 注入 uninstall.exe 到 source → wind-packer build。
# 打包是纯文件 IO + cargo 构建, 不需管理员 (故未纳入 UAC 提权命令)。
# 便携版压缩包: build[_dev]\ → dist\WindInput[Dev]-Portable-<版本>.zip (+ .sha256)
# 与 dev.sh 的 9/portable-zip 同口径 (同名、同结构), 两边产物可互换。
# 内容依据 Deploy-Portable (便携部署的权威定义): 程序文件 + data\ + 便携标记。
# 【不含 userdata\】—— 那是便携版的用户数据目录 (配置/词频/用户词库), 打进包等于把
# 打包机的个人数据分发给所有人; 同时排除部署残留的 *.old*。
function Do-PortableZip ([string]$profile = "release", [bool]$skipBuild = $false) {
    $outdir = Out-For $profile
    $suffix = if ($profile -eq "dev") { "_dev" } else { "" }
    if (-not $skipBuild) {
        if (-not (Do-Full $profile)) { return $false }
    } elseif (-not (Test-Path "$outdir\wind_input$suffix.exe")) {
        ErrMsg "无 $outdir 产物; 去掉 skip 先全构建, 或运行 '$(if($profile -eq 'dev'){'d1'}else{'1'})'。"
        return $false
    }

    $base  = if ($profile -eq "dev") { "WindInputDev" } else { "WindInput" }
    $name  = "$base-$Version"                       # zip 内顶层目录, 避免解压散落
    $zip   = Join-Path $DistDir "$base-Portable-$Version.zip"
    $stage = Join-Path $DistDir ".portable-stage"

    Say "`n========== 打包便携版 ($profile) → $zip =========="
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Path "$stage\$name" -Force | Out-Null
    Copy-Item "$outdir\*" -Destination "$stage\$name" -Recurse -Force

    $ud = Join-Path "$stage\$name" $PortableDataDir
    if (Test-Path $ud) { Remove-Item $ud -Recurse -Force }   # 用户数据, 绝不入包
    Get-ChildItem "$stage\$name\*.old*" -ErrorAction SilentlyContinue | Remove-Item -Force

    # 便携标记: 复用 Write-PortableMarker, 与 wind-portable 的 ensure_portable_layout 同内容。
    # 缺了它 wind_input.exe 会退化成安装版行为把用户数据写进 %APPDATA%。
    Write-PortableMarker "$stage\$name"

    $hasLauncher = Test-Path "$stage\$name\wind_portable.exe"
    if (Test-Path $zip) { Remove-Item $zip -Force }
    Compress-Archive -Path "$stage\$name" -DestinationPath $zip -CompressionLevel Optimal

    # sha256 sidecar (标准 sha256sum 格式, 与安装包 sidecar 一致)
    $sha = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLower()
    [System.IO.File]::WriteAllText("$zip.sha256", "$sha  $(Split-Path $zip -Leaf)`n",
        (New-Object System.Text.UTF8Encoding($false)))

    Remove-Item $stage -Recurse -Force
    $sz = [math]::Round((Get-Item $zip).Length / 1MB, 1)
    Say "`n便携版打包完成: $zip (${sz}MB)"
    if ($hasLauncher) {
        Gray "使用: 解压后运行 wind_portable.exe (注册组件并拉起服务)"
    } else {
        Gray "使用: 包内无便携启动器 —— 需管理员 regsvr32 注册 wind_tsf.dll"
        Gray "      (x86 版用 %SystemRoot%\SysWOW64\regsvr32.exe), 再手动运行 wind_input.exe"
    }
    return $true
}

function Do-Installer ([string]$profile = "release", [bool]$skipBuild = $false) {
    # 1. 定位 wind-installer 兄弟项目
    $instDir = $InstallerDir
    if (Test-Path $InstallerDir) { $instDir = (Resolve-Path $InstallerDir).Path }
    if (-not (Test-Path $instDir)) {
        ErrMsg "未找到 wind-installer 项目: $instDir"
        ErrMsg "请将 wind-installer 与 WindInput 放在同级目录, 或在 scripts\deploy.local.ps1 设置 `$InstallerDir。"
        return $false
    }
    $packPs1 = Join-Path $instDir "scripts\pack.ps1"
    if (-not (Test-Path $packPs1)) { ErrMsg "缺少打包脚本: $packPs1"; return $false }
    # 品牌资产 (logo.png / installer.ico) 取本仓 assets\, 不取 wind-installer\assets\ ——
    # 后者是通用安装器生成器的中性兜底图 (W 字标), 借用它会让安装界面与 Setup.exe 顶着
    # 别人的标识。且 logo 读不到只警告并回退到 stub 内置默认 (icon 才是硬错误),
    # 一旦路径失效会静默变成 W, 不会打包失败。
    $assetsDir = Join-Path $ProductRoot "assets"

    # 2. 构建产物 (除非 skip)
    $outdir = Out-For $profile
    $suffix = if ($profile -eq "dev") { "_dev" } else { "" }
    if (-not $skipBuild) {
        if (-not (Do-Full $profile)) { return $false }
    } elseif (-not (Test-Path "$outdir\wind_input$suffix.exe")) {
        ErrMsg "无 $outdir 产物; 去掉 skip 先全构建, 或运行 '$(if($profile -eq 'dev'){'d1'}else{'1'})'。"; return $false
    }

    # 3. 生成变体 app.toml → dist\ (在 source 之外)
    New-Item -ItemType Directory -Path $DistDir -Force | Out-Null
    $cfgName = if ($profile -eq "dev") { "WindInputDev.app.toml" } else { "WindInput.app.toml" }
    $cfg = Join-Path $DistDir $cfgName
    New-InstallerConfig $profile $outdir $cfg $assetsDir

    Say "`n========== 生成安装包 ($profile) =========="
    Gray "  安装器: $instDir"
    Gray "  产物:   $outdir"
    Gray "  配置:   $cfg"
    Gray "  输出:   $DistDir"

    # 4. 调 pack.ps1 (编译 stub + 注入卸载器 + packer build)。
    #    skip 模式且 installer 二进制已在 → 透传 -SkipBuild 跳过 stub 重编 (加速反复打包)。
    $instTarget = Get-CargoTargetDir $instDir
    $stub   = Join-Path $instTarget "release\wind-installer.exe"
    $packer = Join-Path $instTarget "release\wind-packer.exe"
    $unins  = Join-Path $instTarget "release\wind-uninstaller.exe"
    $instBuilt = (Test-Path $stub) -and (Test-Path $packer) -and (Test-Path $unins)
    # 哈希表 splat 才能按名绑定 (数组 splat 会把 -Config 当成位置参数的值)。
    $packArgs = @{ Config = $cfg }
    if ($skipBuild -and $instBuilt) { $packArgs['SkipBuild'] = $true }
    & $packPs1 @packArgs
    if ($LASTEXITCODE -ne 0) { ErrMsg "打包失败 (见上方 wind-packer 输出)"; return $false }

    $setup = Join-Path $DistDir "$(if($profile -eq 'dev'){'WindInputDev-Setup'}else{'WindInput-Setup'})-$Version.exe"
    if (Test-Path $setup) {
        $sz = [math]::Round((Get-Item $setup).Length / 1MB, 1)
        Say "`n安装包已生成: $setup (${sz}MB)"
        # 5. 生成在线升级元数据 + sha256 sidecar (供 wind-setting 检查更新)
        New-UpdateManifest $profile $setup
    } else {
        Warn "打包脚本已结束, 但未找到预期输出: $setup"
        Warn "请检查上方 wind-packer 实际输出名 (dist\ 下)。"
    }
    return $true
}

# ---------- 候选 REPL (本机) ----------
function Do-Repl ([string]$data = "") {
    if (-not $data) {
        if (Test-Path "$BuildDevDir\data\schemas\pinyin\cn_dicts\base.dict.yaml") { $data = "$BuildDevDir\data" }
        else { Warn "未找到词库数据; 请先运行 gen-data"; $data = "$BuildDevDir\data" }
    }
    Say "`n启动候选 REPL (data=$data)..."
    Push-Location $ProjectRoot
    try { $env:WIND_DATA = $data; cargo run --release -p wind-repl -- $data } finally { Pop-Location }
}

# ---------- Defender 编译排除项 ----------
# Rust 编译是实时反病毒的最坏负载: 数万个中小文件的高频创建/写入/删除, 每次
# CreateFile/CloseFile 都被 minifilter 同步拦截扫描, 开销直接串进链接关键路径。
# 用 git worktree 并行开发时还要乘以 worktree 个数 (各自独立 target)。
# 具体排除清单与安全取舍见 scripts\defender-exclusions.ps1 的头尾注释。
function Do-Defender ([string]$mode = "apply") {
    $ps1 = Join-Path $ScriptDir "defender-exclusions.ps1"
    if (-not (Test-Path $ps1)) { ErrMsg "未找到 $ps1"; return $false }

    # -Scope Workspace: dev.ps1 的构建范围本就跨兄弟仓 (wind-setting / wind-portable /
    # wind-installer), 排除范围与构建范围对齐才不会漏掉它们的 target。
    $splat = @{ Scope = 'Workspace' }
    switch ($mode) {
        "check"  { $splat['WhatIfOnly'] = $true }
        "remove" { $splat['Remove']     = $true }
    }

    # 子脚本正常路径不调 exit, $LASTEXITCODE 会保留上一条命令的陈旧值 —— 先清零,
    # 否则前一条失败命令的退出码会被误判成本次失败。
    $global:LASTEXITCODE = 0
    try {
        & $ps1 @splat
    } catch {
        ErrMsg "配置 Defender 排除项失败: $($_.Exception.Message)"
        return $false
    }
    return ($LASTEXITCODE -eq 0)
}

# ---------- 新 worktree 初始化 ----------
# 新建 git worktree 后的首次全量构建: 依赖从 sccache 取而不是重编 400 多个 crate。
#
# 分两阶段, 缺一不可 —— 本机实测 (wind_service 及其依赖, 冷 target):
#   今天的做法 (无 sccache, incremental 默认) ......... 366 s
#   阶段1 (sccache + CARGO_INCREMENTAL=0, 95% 命中) ... 155 s
#   阶段2 (切回默认, 只重编 workspace crate) .......... 103 s
#                                              合计 258 s, 省 30%
#
# 阶段2 不是可选的收尾: sccache 不缓存增量编译单元, 必须 CARGO_INCREMENTAL=0,
# 而那会让此后【日常改一行重编】明显变慢 —— 稳态实测约 4.5 s, 关闭后约 6.5~9 s。
# 所以 sccache 只用来过首次构建这一关, 之后必须让位给 incremental。
#
# 幅度刻意不给精确百分比: incremental 缓存有 5~6 轮的预热期, 期间它一边重建缓存一边
# 编译, 反而比不用 incremental 更慢 (实测切换配置后 13.7→17.6→11.2→7.5→6.8→9.7→4.3→5.0,
# 第 6 轮后才收敛)。短程 A/B 在这种系统里不只是噪音大, 而会【系统性指向错误方向】——
# 本机就先后测出过 +61% 与 -21% 两个相反结果。要复测须跑到收敛后再取值。
# 阶段2 只需 103 s 而非再来一次全量, 是因为 CARGO_INCREMENTAL 只影响 workspace 内的
# crate —— 400 多个外部依赖的指纹不含它, 不会重编。
function Do-WorktreeInit {
    $sccache = Get-Command sccache -ErrorAction SilentlyContinue
    if (-not $sccache) {
        ErrMsg "未找到 sccache。"
        Write-Host "  安装: cargo install sccache --locked   (从源码编译, 约 9 分钟)"
        return $false
    }

    Say "`n新 worktree 初始化 (两阶段)"
    Gray "  阶段1: sccache 首次全量构建"
    Gray "  阶段2: 切回 incremental (否则日常迭代慢 61%)"

    Push-Location $ProjectRoot
    # 环境变量必须无条件还原: 菜单模式下本进程长驻, 残留的 RUSTC_WRAPPER 会让
    # 本次会话后续所有构建都带着 sccache 跑, 表现为"今天 dev.ps1 莫名变慢"且难以归因。
    $savedWrapper = $env:RUSTC_WRAPPER
    $savedIncr    = $env:CARGO_INCREMENTAL
    try {
        Say "`n[阶段 1/2] sccache 全量构建..."
        $env:RUSTC_WRAPPER     = "sccache"
        $env:CARGO_INCREMENTAL = "0"
        $sw1 = [System.Diagnostics.Stopwatch]::StartNew()
        cargo build --workspace
        $sw1.Stop()
        if ($LASTEXITCODE -ne 0) { ErrMsg "阶段 1 构建失败"; return $false }
        Say ("  阶段 1 完成: {0:N1} s" -f $sw1.Elapsed.TotalSeconds)

        & sccache --show-stats 2>&1 |
            Select-String -Pattern "Cache hits rate|Compile requests executed" |
            ForEach-Object { Gray "  $_" }

        Say "`n[阶段 2/2] 切回 incremental (只重编 workspace crate)..."
        $env:RUSTC_WRAPPER     = $null
        $env:CARGO_INCREMENTAL = $null
        $sw2 = [System.Diagnostics.Stopwatch]::StartNew()
        cargo build --workspace
        $sw2.Stop()
        if ($LASTEXITCODE -ne 0) { ErrMsg "阶段 2 构建失败"; return $false }
        Say ("  阶段 2 完成: {0:N1} s" -f $sw2.Elapsed.TotalSeconds)

        $total = $sw1.Elapsed.TotalSeconds + $sw2.Elapsed.TotalSeconds
        Say ("`n初始化完成, 合计 {0:N1} s ({1:N1} 分)。此后日常迭代走 incremental, 不受影响。" -f $total, ($total / 60))
        return $true
    } finally {
        $env:RUSTC_WRAPPER     = $savedWrapper
        $env:CARGO_INCREMENTAL = $savedIncr
        Pop-Location
    }
}

# ---------- 菜单 ----------
function Show-Menu {
    Clear-Host
    Write-Host "============================================" -ForegroundColor Cyan
    Write-Host "  WindInput 开发菜单  v$Version  (Windows/MSVC)" -ForegroundColor Cyan
    Write-Host "============================================`n" -ForegroundColor Cyan
    Write-Host "  全构建 (→ build/, 内容 == 部署内容):" -ForegroundColor Yellow
    Write-Host "    1    Release 全构建: wind_input + tsf(x64/x86) + setting + portable + 词库"
    Write-Host "    d1   Dev 全构建 (→ build_dev/)"
    Write-Host "`n  单模块构建 (前缀 d = dev):" -ForegroundColor Yellow
    Write-Host "    m1   仅 tsf (x64+x86)                dm1"
    Write-Host "    m2   仅 wind_input (核心)             dm2"
    Write-Host "    m3   仅 wind_setting (../wind-setting)  dm3"
    Write-Host "    m4   仅 wind_portable (../wind-portable) dm4"
    Write-Host "`n  系统安装 / 卸载 (注册 TSF + 开机自启 + 默认启用, 自动提权):" -ForegroundColor Yellow
    Write-Host "    p1   安装全部 (release)        pd1   安装全部 (dev)"
    Write-Host "    pm1..pm4  安装模块(tsf/核心/setting/portable)   pdm1..pdm4 (dev)"
    Write-Host "    u1/u  卸载全部 (release)        ud1/ud  卸载全部 (dev)"
    Write-Host "      release → $WIND_DIR_RELEASE" -ForegroundColor DarkGray
    Write-Host "      dev     → $WIND_DIR_DEV" -ForegroundColor DarkGray
    Write-Host "`n  便携部署 / 卸载 (纯复制 + 便携标记, 免注册免提权):" -ForegroundColor Yellow
    Write-Host "    pb1  部署全部 (release)        pbd1  部署全部 (dev)"
    Write-Host "    pbm1..pbm4  部署模块(tsf/核心/setting/portable)  pbdm1..pbdm4 (dev)"
    Write-Host "    ub1/ub  卸载 (release)          ubd1/ubd  卸载 (dev)"
    Write-Host "      release → $WIND_DIR_PORTABLE_RELEASE" -ForegroundColor DarkGray
    Write-Host "      dev     → $WIND_DIR_PORTABLE_DEV" -ForegroundColor DarkGray
    Write-Host "      用户数据 <目录>\$PortableDataDir 部署/卸载均保留" -ForegroundColor DarkGray
    Write-Host "`n  安装包 (调用兄弟项目 wind-installer 打包):" -ForegroundColor Yellow
    Write-Host "    8    生成安装包 (release)       d8    生成安装包 (dev)"
    Write-Host "    8s   跳过重建直接打包 (release)  d8s   跳过重建直接打包 (dev)"
    Write-Host "      输出 → $DistDir" -ForegroundColor DarkGray
    Write-Host "`n  便携包 (免安装 zip, 不依赖 wind-installer):" -ForegroundColor Yellow
    Write-Host "    9    生成便携包 (release)       d9    生成便携包 (dev)"
    Write-Host "    9s   跳过重建直接打包 (release)  d9s   跳过重建直接打包 (dev)"
    Write-Host "      输出 → $DistDir\WindInput[Dev]-Portable-$Version.zip" -ForegroundColor DarkGray
    Write-Host "`n  代码质量:" -ForegroundColor Yellow
    Write-Host "    k=check  l=clippy  t=test  f=fmt  ci=fmt+clippy+test"
    Write-Host "`n  数据 / 实测:" -ForegroundColor Yellow
    Write-Host "    gd=gen-data  r=repl(本机)"
    Write-Host "`n  构建环境:" -ForegroundColor Yellow
    Write-Host "    av   配置 Defender 编译排除项 (自动提权)  avc 仅预览  avr 移除"
    Write-Host "      加速 Rust 编译: 免去数万个 target 文件被实时扫描" -ForegroundColor DarkGray
    Write-Host "      进程排除与路径无关, 新建 worktree 自动生效, 无需重跑" -ForegroundColor DarkGray
    Write-Host "    wtinit  新 worktree 首次全量构建 (需 sccache)"
    Write-Host "      实测 366s → 258s (-30%); 完成后自动切回 incremental" -ForegroundColor DarkGray
    if ($WIND_REMOTE_HOST) {
        Write-Host "    runlock  Ctrl+C 中断远程构建后锁卡死时, 强制释放编译机互斥锁"
    }
    Write-Host "`n  杂项:" -ForegroundColor Yellow
    Write-Host "    clean  q=退出"
    Write-Host "============================================" -ForegroundColor Cyan
}

# ---------- 统一分发 (菜单与命令行直调共用; 命令已转小写) ----------
# 返回 127 = 未知命令 (区别于命令执行失败)。
# ---------- 远程构建转发 (仅在配了 build.local.ps1 时生效) ----------
# 走远程的是「吃 CPU 的」命令。刻意排除三类:
#   · 部署/卸载 (p* / u* / pb* / ub*) —— 要注册 COM、写注册表、装字体, 本机才是目标机;
#   · 会改源码的 f/fmt —— cargo fmt 改的是编译机上的源码, 下次 /MIR 同步即被本机覆盖,
#     白改且无声。(ci 走的是 Do-FmtCheck 只读, 故可以远程。)
#   · 本机状态类 (av* / hooks / r / wtinit / clean) —— Defender 排除项、git hooks、交互
#     repl, 以及「清哪台的 target」, 都只在本机有意义。
$RemoteCommands = @(
    "1", "release", "d1", "dev",
    "m1", "m2", "m3", "m4", "dm1", "dm2", "dm3", "dm4",
    "8", "8s", "d8", "d8s", "9", "9s", "d9", "d9s",
    "k", "check", "l", "clippy", "t", "test", "ci", "fmt-check",
    "gd", "gen-data"
)
function Test-RemoteCommand ([string]$cmd) {
    # 临时强制本机: `$env:WIND_NO_REMOTE = "1"` (编译机关机 / 不在内网 / 要和远程做对照)。
    # 这同时是防递归哨兵 —— remote-build.ps1 回落本机时会设上它再回调本脚本, 两边必须同判据。
    if ($env:WIND_NO_REMOTE) { return $false }
    if (-not $WIND_REMOTE_HOST) { return $false }   # 未配置 → 一律本机, 行为与从前一致
    $list = if ($null -ne $WIND_REMOTE_COMMANDS) { $WIND_REMOTE_COMMANDS } else { $RemoteCommands }
    return ($list -contains $cmd)
}

function Dispatch ([string]$cmd, [string]$arg) {
    # 远程转发闸门。未配置时 Test-RemoteCommand 恒 false, 直落下方本机分支; remote-build.ps1
    # 不入库, 新 worktree 里没有它时同样自动降级为本机构建, 不报错。
    if ((Test-RemoteCommand $cmd) -and (Test-Path "$ScriptDir\remote-build.ps1")) {
        & "$ScriptDir\remote-build.ps1" -Command $cmd | Out-Host
        return $LASTEXITCODE
    }
    switch ($cmd) {
        { $_ -in @("1", "release") }        { if (Do-Full release) { 0 } else { 1 }; break }
        { $_ -in @("d1", "dev") }           { if (Do-Full dev)   { 0 } else { 1 }; break }
        "m1"   { if (Build-TsfAll   release) { 0 } else { 1 }; break }
        "dm1"  { if (Build-TsfAll   dev)   { 0 } else { 1 }; break }
        "m2"   { if (Build-Core     release) { 0 } else { 1 }; break }
        "dm2"  { if (Build-Core     dev)   { 0 } else { 1 }; break }
        "m3"   { if (Build-Setting  release) { 0 } else { 1 }; break }
        "dm3"  { if (Build-Setting  dev)   { 0 } else { 1 }; break }
        "m4"   { if (Build-Portable release) { 0 } else { 1 }; break }
        "dm4"  { if (Build-Portable dev)   { 0 } else { 1 }; break }
        "p1"   { if (Deploy-Full release) { 0 } else { 1 }; break }
        "pd1"  { if (Deploy-Full dev)   { 0 } else { 1 }; break }
        "pm1"  { if (Deploy-Module release tsf)      { 0 } else { 1 }; break }
        "pm2"  { if (Deploy-Module release core)     { 0 } else { 1 }; break }
        "pm3"  { if (Deploy-Module release setting)  { 0 } else { 1 }; break }
        "pm4"  { if (Deploy-Module release portable) { 0 } else { 1 }; break }
        "pdm1" { if (Deploy-Module dev tsf)      { 0 } else { 1 }; break }
        "pdm2" { if (Deploy-Module dev core)     { 0 } else { 1 }; break }
        "pdm3" { if (Deploy-Module dev setting)  { 0 } else { 1 }; break }
        "pdm4" { if (Deploy-Module dev portable) { 0 } else { 1 }; break }
        "u"    { if (Uninstall-Full release) { 0 } else { 1 }; break }
        "u1"   { if (Uninstall-Full release) { 0 } else { 1 }; break }
        "ud"   { if (Uninstall-Full dev)   { 0 } else { 1 }; break }
        "ud1"  { if (Uninstall-Full dev)   { 0 } else { 1 }; break }
        # 便携部署 (纯复制, 不提权)
        "pb1"   { if (Deploy-Portable release) { 0 } else { 1 }; break }
        "pbd1"  { if (Deploy-Portable dev)     { 0 } else { 1 }; break }
        "pbm1"  { if (Deploy-PortableModule release tsf)      { 0 } else { 1 }; break }
        "pbm2"  { if (Deploy-PortableModule release core)     { 0 } else { 1 }; break }
        "pbm3"  { if (Deploy-PortableModule release setting)  { 0 } else { 1 }; break }
        "pbm4"  { if (Deploy-PortableModule release portable) { 0 } else { 1 }; break }
        "pbdm1" { if (Deploy-PortableModule dev tsf)      { 0 } else { 1 }; break }
        "pbdm2" { if (Deploy-PortableModule dev core)     { 0 } else { 1 }; break }
        "pbdm3" { if (Deploy-PortableModule dev setting)  { 0 } else { 1 }; break }
        "pbdm4" { if (Deploy-PortableModule dev portable) { 0 } else { 1 }; break }
        "ub"    { if (Uninstall-Portable release) { 0 } else { 1 }; break }
        "ub1"   { if (Uninstall-Portable release) { 0 } else { 1 }; break }
        "ubd"   { if (Uninstall-Portable dev)     { 0 } else { 1 }; break }
        "ubd1"  { if (Uninstall-Portable dev)     { 0 } else { 1 }; break }
        { $_ -in @("8", "installer") }       { if (Do-Installer release $false) { 0 } else { 1 }; break }
        "8s"                                 { if (Do-Installer release $true)  { 0 } else { 1 }; break }
        { $_ -in @("d8", "installer-dev") }  { if (Do-Installer dev $false)   { 0 } else { 1 }; break }
        "d8s"                                { if (Do-Installer dev $true)     { 0 } else { 1 }; break }
        { $_ -in @("9", "portable-zip") }    { if (Do-PortableZip release $false) { 0 } else { 1 }; break }
        "9s"                                 { if (Do-PortableZip release $true)  { 0 } else { 1 }; break }
        { $_ -in @("d9", "portable-zip-dev") } { if (Do-PortableZip dev $false) { 0 } else { 1 }; break }
        "d9s"                                { if (Do-PortableZip dev $true)   { 0 } else { 1 }; break }
        { $_ -in @("k", "check") }   { Do-Check;  $LASTEXITCODE; break }
        { $_ -in @("l", "clippy") }  { Do-Clippy; $LASTEXITCODE; break }
        { $_ -in @("t", "test") }    { Do-Test;   $LASTEXITCODE; break }
        { $_ -in @("f", "fmt") }     { Do-Fmt;    $LASTEXITCODE; break }
        "fmt-check"                  { Do-FmtCheck; $LASTEXITCODE; break }
        "ci"                         { if (Do-Ci) { 0 } else { 1 }; break }
        "hooks"                      { Do-HooksInstall; 0; break }
        "clean"                      { Do-Clean;  $LASTEXITCODE; break }
        { $_ -in @("gd", "gen-data") }  { if (Do-GenData) { 0 } else { 1 }; break }
        { $_ -in @("r", "repl") }       { Do-Repl $arg; 0; break }
        # Defender 编译排除项 (av/avr 经 Invoke-Elevated 自动提权; avc 只读免管理员)
        { $_ -in @("av", "defender") }  { if (Do-Defender apply)  { 0 } else { 1 }; break }
        "avc"                           { if (Do-Defender check)  { 0 } else { 1 }; break }
        "avr"                           { if (Do-Defender remove) { 0 } else { 1 }; break }
        { $_ -in @("wtinit", "worktree-init") } { if (Do-WorktreeInit) { 0 } else { 1 }; break }
        # 强制释放编译机互斥锁 (Ctrl+C 中断构建后卡死时用)。这条命令本身刻意不进 $RemoteCommands
        # 白名单转发表 —— 它不是"要转发去远程跑的构建", 而是直接操作远程锁, 走 remote-build.ps1
        # 自己的 -Unlock 分支。未配置远程编译机时该分支会安静地报"无锁可清"。
        { $_ -in @("runlock", "unlock-remote") } {
            if (Test-Path "$ScriptDir\remote-build.ps1") {
                & "$ScriptDir\remote-build.ps1" -Unlock | Out-Host; $LASTEXITCODE
            } else { ErrMsg "remote-build.ps1 不存在 (未入库或不在本仓)"; 1 }
            break
        }
        default { 127 }
    }
}

function Menu-Loop {
    while ($true) {
        Show-Menu
        $raw = (Read-Host "`n请输入选项").Trim()
        if (-not $raw) { continue }
        if ($raw.ToLower() -eq "q") { return }

        # 支持空格分隔的连续命令: "d1 pd1" → 依次执行, 前者失败则停止
        # @() 强制包装: 单 token 时 Where-Object 返回标量字符串, 索引会取字符而非词
        $tokens = @($raw.ToLower() -split '\s+' | Where-Object { $_ -ne "" })
        $i = 0
        $anyFailed = $false
        $needPause = $false   # UAC 成功时输出已内联显示, 无需额外暂停
        while ($i -lt $tokens.Count -and -not $anyFailed) {
            $choice = $tokens[$i]
            $choiceArg = ""
            # repl 命令后一个 token 为数据路径 (非命令)
            if ($choice -eq "r" -or $choice -eq "repl") {
                $i++
                if ($i -lt $tokens.Count) { $choiceArg = $tokens[$i] }
            }
            $el = Invoke-Elevated $choice $choiceArg
            if ($el -eq "skip") {
                $needPause = $true   # 普通命令在当前窗口产生输出, 需暂停让用户阅读
                $rc = Dispatch $choice $choiceArg
                if ($rc -eq 127) { ErrMsg "无效选项: $choice"; $anyFailed = $true }
                elseif ($rc -ne 0) { ErrMsg "`n命令 '$choice' 失败 (退出码 $rc)"; $anyFailed = $true }
            } elseif ($el -eq "done") {
                $needPause = $true   # UAC 子进程输出已内联显示, 暂停让用户阅读
            } elseif ($el -eq "fail") {
                $needPause = $true   # 提权失败/被取消, 需暂停让用户看到错误
                $anyFailed = $true
            }
            $i++
        }
        if ($needPause) { Write-Host ""; Write-Host "按回车继续..." -NoNewline; Read-Host | Out-Null }
    }
}

# ---------- 入口 ----------
$allCmds = @($Commands | Where-Object { $_ -ne "" })

# 无参数 → 交互菜单
if ($allCmds.Count -eq 0) { Menu-Loop; return }

$firstCmd = $allCmds[0].Trim().ToLower()

# help
if ($firstCmd -eq "-h" -or $firstCmd -eq "--help" -or $firstCmd -eq "help") {
    Get-Content $PSCommandPath | Where-Object { $_ -match '^#' } | ForEach-Object { $_ -replace '^# ?', '' }
    return
}

# menu (显式)
if ($firstCmd -eq "menu") { Menu-Loop; return }

# 按序执行所有命令; repl 后一个参数为数据路径
$i = 0
while ($i -lt $allCmds.Count) {
    $cmd = $allCmds[$i].Trim().ToLower()
    $arg = ""
    if ($cmd -eq "r" -or $cmd -eq "repl") {
        $i++
        if ($i -lt $allCmds.Count) { $arg = $allCmds[$i] }
    }

    $el = Invoke-Elevated $cmd $arg
    if ($el -eq "done") { $i++; continue }
    if ($el -eq "fail") { exit 1 }

    $rc = Dispatch $cmd $arg
    if ($rc -eq 127) {
        ErrMsg "未知命令: $cmd"
        Write-Host "运行 '.\scripts\dev.ps1 --help' 查看可用命令"
        exit 1
    }
    if ($rc -ne 0) { exit $rc }
    $i++
}
exit 0
