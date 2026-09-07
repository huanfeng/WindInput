# WindInput 远程构建 —— 把 cargo / MSVC 编译放到另一台 Windows 机器执行, 产物回传本机部署。
#
# ⚠️ 本脚本默认完全不生效。只有 scripts\build.local.ps1 存在且设了 $WIND_REMOTE_HOST 时,
#    dev.ps1 才会把构建类命令转发到这里。未配置的开发者 clone 下来, dev.ps1 行为逐字不变;
#    直接调用本脚本也不会报错, 而是把命令交回本机执行 (见下方「回落本机」)。
#
#    配置模板: scripts\build.local.ps1.example。真正的 build.local.ps1 **不入库**
#    (含内网地址与账号), 每台开发机自行照模板填。
#
# ── 临时回落本机 ──────────────────────────────────────────────────────────
#   $env:WIND_NO_REMOTE = "1"     # 本会话内一律本机编 (编译机关机 / 不在内网 / 要做对照)
#   $env:WIND_NO_REMOTE = $null   # 恢复
#   已配置时想单次走本机, 也可以直接 `dev.ps1` 加这个环境变量前缀。
#
# ── 为什么编译机必须是 Windows + 原生 MSVC ────────────────────────────────────
# clang / cargo-xwin 交叉编译出的 wind_tsf.dll 在带安全加固的宿主 (企业微信 / TIM /
# QQ 部分进程 / UU浏览器) 里 COM 激活失败, 同 commit 的本机 MSVC 版正常 —— 已由 A/B 实测
# 把唯一变量锁定在工具链上 (6dbc8595 因此把发布链从 ubuntu 交叉编译改回 windows-latest)。
# 由此:
#   · 编译机不能是 Linux / WSL —— 那等于把已否决的交叉编译链请回来;
#   · sccache-dist 也不适用 —— 它的 build server 官方只支持 Linux, Windows 只能当客户端,
#     任务仍会被分发到 Linux 上交叉编译。
# 能走的只剩「整机远程构建 + 产物回传」, 也就是本脚本。
#
# ── 两种用法 ──────────────────────────────────────────────────────────────
#   -Command <dev.ps1 子命令>    经 dev.ps1 的构建 (dm2 / d1 / 8 ...), 完事回传产物
#   -Raw     <任意命令>          在编译机的 wind_input\ 下直接跑, 不回传产物
#                                (细粒度 cargo: cargo test -p wind-coordinator 等)
#
# ── 通道: 一律 SSH ────────────────────────────────────────────────────────
# 源码同步、命令执行、产物回传全部走同一套 SSH 密钥认证, 不引入第二套凭据。
# 曾考虑 SMB + robocopy 增量, 但那需要额外的编译机密码; 实测 tar 打包整个工作树只有
# 5.3 MB / 1 秒 (排除 target 后), 全量传输与增量同步的耗时差已无实际意义。
#   同步 : 本地 tar.gz  →  scp  →  远程解压   (~5 MB, 2~3 秒)
#   回传 : 按命令只取该模块的产物文件; 全构建才整目录打包
#
# ⚠️ 同步是【镜像】: 解压后会清掉编译机上多余的文件 (prune), 否则本机删掉的 tests\*.rs
#    会继续参与编译 —— cargo 自动发现测试目标, 不需要任何引用。清理范围严格等于同步范围,
#    target\ / build[_dev]\ / dist\ / .cache\ 一律不碰。逃生口: -NoPrune。

[CmdletBinding(DefaultParameterSetName = 'Dev')]
param(
    [Parameter(ParameterSetName = 'Dev', Mandatory, Position = 0)] [string]$Command,
    [Parameter(ParameterSetName = 'Raw', Mandatory)] [string]$Raw,
    # 强制清除编译机上的互斥锁 (锁被 Ctrl+C 中断卡死、或已确认持有者已死时的手动逃生口)。
    # 不做任何构建, 只删那个锁文件。见下方「编译机互斥锁」一节。
    [Parameter(ParameterSetName = 'Unlock', Mandatory)] [switch]$Unlock,
    # 跳过源码同步 (上次同步后没改代码时可省 2~3 秒)
    [switch]$NoSync,
    # 只编译不回传
    [switch]$NoFetch,
    # 不清理编译机上的残留文件 (逃生口; 正常情况下不该用, 见 Sync-Tree 里的说明)
    [switch]$NoPrune,
    # 不抢编译机互斥锁。只用于【只读】的 -Raw 查询 (看文件、看进程) —— 那种命令不写任何
    # 东西, 没必要排在一次长构建后面等二十分钟。凡是会写 target\ 或 build\ 的命令都别加。
    [switch]$NoLock,
    # 先删除编译机上的整个仓库目录再全量同步。⚠️ 连 target\ 一起删, 下次要全量重编 (~9 分钟)
    [switch]$Clean
)

$ErrorActionPreference = "Stop"
# 原生命令 (scp/ssh/tar/robocopy) 的非零退出码在这里靠 $LASTEXITCODE 自行判读,
# 不要让 PowerShell 7.3+ 把它们升级成终止错误。
$PSNativeCommandUseErrorActionPreference = $false

$ScriptDir   = $PSScriptRoot
$ProductRoot = Split-Path $ScriptDir -Parent

function Say    ([string]$m) { Write-Host $m -ForegroundColor Green }
function Warn   ([string]$m) { Write-Host $m -ForegroundColor Yellow }
function ErrMsg ([string]$m) { Write-Host $m -ForegroundColor Red }
function Gray   ([string]$m) { Write-Host $m -ForegroundColor DarkGray }

# 独立调用时自行载入配置; 经 dev.ps1 转发时它已载入过, 重复 dot-source 无副作用。
$buildCfg = "$ScriptDir\build.local.ps1"
if (Test-Path $buildCfg) { . $buildCfg }

# ---------- 未配置 / 临时禁用 → 回落本机执行 ----------
# 本脚本入库后, 绝大多数 clone 都不会配 build.local.ps1。它必须在那些环境里「安静地做对的
# 事」——把命令交回本机跑完, 而不是报错退出。三种回落情形:
#   1. 没有 build.local.ps1, 或缺 HOST/USER/ROOT 任一项 —— 从没配过
#   2. $env:WIND_NO_REMOTE 非空 —— 临时强制本机 (编译机关机 / 不在内网 / 要和远程做对照)
#   3. 上层已在回落中 —— 见下方哨兵, 防 dev.ps1 ⇄ remote-build.ps1 互调成死循环
function Test-RemoteReady {
    if ($env:WIND_NO_REMOTE) { return $false }
    foreach ($req in @("WIND_REMOTE_HOST", "WIND_REMOTE_USER", "WIND_REMOTE_ROOT")) {
        if (-not (Get-Variable $req -ValueOnly -ErrorAction SilentlyContinue)) { return $false }
    }
    return $true
}

if (-not (Test-RemoteReady)) {
    if ($env:WIND_NO_REMOTE) {
        Gray "WIND_NO_REMOTE 已设 —— 本次在本机执行。"
    } else {
        Gray "未配置远程编译机 —— 本机执行。(要启用见 scripts\build.local.ps1.example)"
    }
    if ($PSCmdlet.ParameterSetName -eq 'Unlock') {
        Gray "没有远程编译机可清 —— 本机执行不涉及锁。"
        exit 0
    }
    # ⚠️ 哨兵必须在调 dev.ps1 之前设上: dev.ps1 的转发闸门看的正是这个环境变量, 不设它就会
    #    dev.ps1 → remote-build.ps1 → dev.ps1 无限递归。finally 还原, 免得污染调用者的会话。
    $prevNoRemote = $env:WIND_NO_REMOTE
    $env:WIND_NO_REMOTE = "1"
    try {
        if ($PSCmdlet.ParameterSetName -eq 'Raw') {
            # -Raw 的本机等价物: 在 wind_input\ 下原样执行。远程侧也是把整个字符串交给 pwsh,
            # 故这里同样不拆词 —— 拆了会丢掉引号内的空格 (rc.ps1 会给含空格的参数加引号)。
            Push-Location "$ProductRoot\wind_input"
            try { Invoke-Expression $Raw } finally { Pop-Location }
        } else {
            & "$ScriptDir\dev.ps1" $Command
        }
        $rc = $LASTEXITCODE
    } finally { $env:WIND_NO_REMOTE = $prevNoRemote }
    if ($null -eq $rc) { $rc = 0 }
    exit $rc
}
$Target   = "$WIND_REMOTE_USER@$WIND_REMOTE_HOST"
$remotePs = if ($WIND_REMOTE_PS) { $WIND_REMOTE_PS } else { "pwsh" }
# 远程仓库路径的两种写法: Windows 原生 (给 pwsh/dev.ps1) 与正斜杠 (给 scp)
$RRoot    = $WIND_REMOTE_ROOT.TrimEnd('\')

# ---------- worktree 槽位: 换掉【父目录】, 而不是主仓的目录名 ----------
# 多个 worktree 共用一台编译机时, 若都同步到 C:\build\WindInput 就会互相覆盖, 且是无声的:
# 后一次 tar 解压盖掉前一次的源码, 产物属于哪个分支全看谁最后跑完。
#
# ⚠️ 为什么是换父目录, 而不是把 root 改成 C:\build\WindInput-fx:
#    伴生仓与主仓平级, 而 wind-setting\Cargo.toml 里写死了三条相对 path 依赖
#    (wind-ipc / wind-rpc / wind-config 均为 ../WindInput/wind_input/crates/...)。
#    只改主仓目录名, wind-setting 仍会去 ..\WindInput 取那三个 crate —— 取到的是主树代码,
#    编译照样成功, 错得毫无提示。整个父目录换槽位, 相对结构才保持自洽:
#        默认     C:\build\{WindInput, wind-setting, wind-ui-rust, ...}
#        槽位 fx  C:\build-fx\{WindInput, wind-setting, wind-ui-rust, ...}
#
# ⚠️ 每个槽位各有一份 target\ (几十 GB)。开完记得 `remote-build.ps1 -Command <x> -Clean`
#    或直接删掉编译机上的整个槽位目录。
$slot = $env:WIND_REMOTE_SLOT
if ($slot -and $slot -in @('0', 'none', 'off')) {
    $slot = $null          # 显式关掉: worktree 也共用默认目录 (串台风险自负)
} elseif (-not $slot) {
    # 没显式指定就按 worktree 目录名自动派生 ——「忘了设 → 静默互相覆盖」这类坑本仓踩过
    # 太多次, 默认值必须是安全的那个。主树的 git-dir 与 git-common-dir 相同, 派生不出
    # 槽位, 行为与从前逐字不变。
    try {
        $gitDir = & git -C $ProductRoot rev-parse --git-dir 2>$null
        $common = & git -C $ProductRoot rev-parse --git-common-dir 2>$null
        if ($LASTEXITCODE -eq 0 -and $gitDir -and $common -and $gitDir -ne $common) {
            $slot = Split-Path $ProductRoot -Leaf
        }
    } catch { }   # 没装 git / 不是仓库: 当作主树处理
}
if ($slot) {
    # 分支名里的 / : 之类会把路径打断, 非安全字符一律折成 '-'
    $slot  = ($slot -replace '[^A-Za-z0-9._-]', '-').Trim('-')
    $RRoot = "{0}-{1}\{2}" -f (Split-Path $RRoot -Parent), $slot, (Split-Path $RRoot -Leaf)
}
$RRootFwd = $RRoot -replace '\\', '/'
$SshOpts  = @('-o', 'BatchMode=yes', '-o', 'ConnectTimeout=15',
              '-o', 'ServerAliveInterval=30', '-o', 'ServerAliveCountMax=40')

# ---------- 远程执行 ----------
# 命令经 -EncodedCommand (UTF-16LE base64) 下发。理由: Windows OpenSSH 的默认 shell 是
# cmd.exe, 而命令里必然出现引号/分号/反斜杠 (路径、cargo 参数), 逐层转义既难写, 又会随
# 「编译机默认 shell 被改成 powershell」而静默失效。base64 把整条命令变成无特殊字符的 token。
# ⚠️ 上限: cmd.exe 命令行 8191 字符, base64 膨胀约 2.7 倍 ⇒ 内层脚本须短于 ~3 KB。
#    成篇的脚本要先 scp 落盘再执行, 别塞进这里 (实测踩过「命令行太长」)。
function Invoke-RemotePs ([string]$innerScript, [switch]$Quiet) {
    # 远端先切 UTF-8 再干活: dev.ps1 / cargo 的中文输出若以 GBK 出栈, 经 SSH 传回本机按
    # UTF-8 解码必然全乱 —— 而乱码会让编译错误变得不可读, 排错时等于没有输出。
    # $ProgressPreference: pwsh 作为子进程且输出被重定向时, 会把 Progress 流以 CLIXML 编码
    # 写进 stderr, 于是 `#< CLIXML` 与整段 <Objs ...> 报文糊在构建输出里 (实测)。抑制掉即可。
    $prelude = 'try{[Console]::OutputEncoding=[Text.Encoding]::UTF8;$OutputEncoding=[Text.Encoding]::UTF8}catch{}; ' +
               '$ProgressPreference=''SilentlyContinue''; '
    $bytes = [Text.Encoding]::Unicode.GetBytes($prelude + $innerScript)
    if ($bytes.Length * 4 / 3 -gt 7000) {
        ErrMsg "  内层命令过长 ($($bytes.Length) 字节), 会撞上 cmd.exe 的 8191 字符上限。"
        return $false
    }
    $enc = [Convert]::ToBase64String($bytes)
    # ⚠️ 这里【不能】写 `& ssh ... 2>&1`: PowerShell 会对被重定向 stderr 的原生命令尝试按
    #    CLIXML 解析错误流, 于是每行 stderr 都刷一条「Cannot process the XML from the
    #    'Error' stream of ssh.exe」, 且正常输出被打印两遍 (实测)。
    #    cargo/rustc 的错误确实全走 stderr, 但合并要在【远端】做 —— 见调用处 inner 里的 2>&1,
    #    那是远程 PowerShell 内部的流合并, 到 ssh 时已经只剩 stdout 一条流。
    # | Out-Host 不可省: PowerShell 函数的返回值是「所有未被消费的输出」, 少了它, 几十行
    # 远程编译输出会和末尾的 bool 一起组成返回值数组, 调用方的 if 判断必然错乱。
    if ($Quiet) { & ssh @SshOpts $Target "$remotePs -NoProfile -EncodedCommand $enc" | Out-Null }
    else        { & ssh @SshOpts $Target "$remotePs -NoProfile -EncodedCommand $enc" | Out-Host }
    return ($LASTEXITCODE -eq 0)
}

# 取远程一行输出 (用于探测状态), 失败返回 $null
function Get-RemoteValue ([string]$expr) {
    $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($expr))
    $v = & ssh @SshOpts $Target "$remotePs -NoProfile -EncodedCommand $enc" 2>$null
    if ($LASTEXITCODE -ne 0) { return $null }
    return ($v | Select-Object -Last 1)
}

# ---------- 编译机互斥锁 ----------
# 同一个槽位同时只允许一个构建在跑。理由不是效率而是【正确性】: dev.ps1 的 Do-Full 开头会
# `Remove-Item -Recurse -Force $outdir` 清空 build[_dev]\, 两个构建并发时后者会把前者的产物
# 整个删掉 —— 而前者浑然不觉, 继续走到打包, 于是打出一个空安装包并报告「打包完成」。
# 2026-08-11 实测踩中: 本该 20 MB 的安装包产出 2.3 MB, 全程零报错。
#
# 锁文件放在槽位目录【旁边】(不在仓库内), 因此既不会被同步覆盖, 也不会被 prune 清掉;
# 路径含槽位名 ⇒ 不同 worktree 各锁各的, 互不排队。
$LockFile = "$RRoot.buildlock"
$LockHolder = "$env:COMPUTERNAME/$env:USERNAME/pid$PID"
$script:LockOwned = $false

# 陈旧判据: 锁文件创建时刻起 30 分钟未变 (一次冷全构建的量级)。
#
# ⚠️ 2026-08-21 教训: Ctrl+C/断线中断构建时, 锁可能要卡到这 30 分钟耗尽才会自愈——试过让
# 【本地】在退出前抢一次 SSH 通知远程删锁 (CancelKeyPress/TreatControlCAsInput), 也试过让
# 【远程】开一个后台心跳持续续期、把陈旧阈值砍到 90 秒——机制本身实测有效, 但用户反馈
# "主要只有我自己用, 这套太复杂了", 于是撤回, 改回最简单的方案: 卡住了就手动
# `.\scripts\remote-build.ps1 -Unlock` (或 `dev.ps1 runlock`) 清一下, 不搞自动化。
# 复杂版的设计与实测记录留在项目记忆 project_remote_build_machine.md, 以后若换成多人共用
# 这台编译机、手动清锁不再够用时, 可以按那份记录原样抄回来。
function Enter-RemoteLock {
    # 最多等 20 分钟 (一次冷全构建的量级); 超时宁可报错也不硬闯, 免得两个构建互相拆台。
    foreach ($try in 1..120) {
        # CreateNew + FileShare.None 是原子的: 文件已存在时直接抛异常, 不存在时独占创建。
        # 用 New-Item / Test-Path 判断则有检查与创建之间的竞态窗口。
        $r = Get-RemoteValue @"
try {
  `$fs = [IO.File]::Open('$LockFile', 'CreateNew', 'Write', 'None')
  `$sw = New-Object IO.StreamWriter(`$fs); `$sw.Write('$LockHolder'); `$sw.Dispose(); 'OK'
} catch {
  `$age = ((Get-Date) - (Get-Item '$LockFile' -EA SilentlyContinue).LastWriteTime).TotalMinutes
  if (`$age -gt 30) { Remove-Item '$LockFile' -Force -EA SilentlyContinue; 'STALE' }
  else { 'BUSY:' + ((Get-Content '$LockFile' -Raw -EA SilentlyContinue) -replace '\s','') }
}
"@
        if ($r -eq 'OK') { $script:LockOwned = $true; return $true }
        if ($r -eq 'STALE') { continue }        # 陈旧锁已清除, 立刻重试抢占
        if ($null -eq $r)   { return $true }    # 连不上时不因为锁而卡死, 后续步骤自会报错
        if ($try -eq 1) { Warn "  [锁] 编译机正被另一个构建占用 ($($r -replace '^BUSY:','')), 等待..." }
        Start-Sleep -Seconds 10
    }
    ErrMsg "  [锁] 等待编译机超过 20 分钟仍未空闲, 已放弃。"
    ErrMsg "       若确认对方已死: .\scripts\remote-build.ps1 -Unlock"
    return $false
}

function Exit-RemoteLock {
    if (-not $script:LockOwned) { return }
    $script:LockOwned = $false
    Invoke-RemotePs "Remove-Item -LiteralPath '$LockFile' -Force -EA SilentlyContinue" -Quiet | Out-Null
}

# ---------- -Unlock: 手动逃生口, 不做任何构建 ----------
# 旧文档曾让用户"确认对方已死后去编译机上手动删文件" —— 大多数人根本没法/不敢直接 SSH
# 上去删文件。这里给一条安全的等价操作: 只删锁文件, 不碰源码/产物, 走已有的 SSH 通道。
if ($PSCmdlet.ParameterSetName -eq 'Unlock') {
    Say "`n========== 强制释放编译机锁 ($WIND_REMOTE_HOST) =========="
    if ($slot) { Gray "  槽位 $slot → $RRoot" }
    $r = Get-RemoteValue @"
if (Test-Path -LiteralPath '$LockFile') {
  `$holder = (Get-Content -LiteralPath '$LockFile' -Raw -EA SilentlyContinue)
  Remove-Item -LiteralPath '$LockFile' -Force; "REMOVED:`$holder"
} else { 'NONE' }
"@
    if ($null -eq $r) { ErrMsg "连接编译机失败 —— 无法确认或清除锁。"; exit 1 }
    elseif ($r -eq 'NONE') { Gray "锁文件不存在, 无需处理: $LockFile" }
    else { Say "已删除锁文件 (原持有者: $($r -replace '^REMOVED:','')): $LockFile" }
    exit 0
}

# ---------- 源码同步 ----------
# 排除清单的理由:
#   target\      50 GB / 7.8 万文件 —— 同步它等于把远程构建的收益全部倒赔进去
#   build*\      产物目录, 回传方向相反; 传过去会用本机旧产物覆盖编译机刚产出的
#   .git\        编译机只要工作树, 不要历史
#   .cache\      词库下载缓存, 编译机首次全构建时自行下载即可
#   .claude\ 等  AI 工具的会话历史与运行时状态 —— 与编译无关, 却占 5481 文件 / 40 MB
#   *.local.ps1  两机配置不同, 传过去会让编译机拿本机的部署目标办事;
#                sign.local.ps1 更是不该出编译机的门 —— 签名是本机的事, 编译机既没有
#                签名会话也不该有。
#                ⚠️ 这里【必须】用通配而不是逐个列举: 原先写死 deploy/build 两个名字,
#                   新增 sign.local.ps1 时就漏了, 它被同步到了编译机 (实测发现)。
#                   注释一直写的是 *.local.ps1, 是实现没跟上。
$ExcludeDirs  = @("target", "build", "build_dev", "build_mac", "build_debug", "dist", ".git",
                  "node_modules", ".cache", ".claude", ".remember", ".omc", ".omx", ".vscode", ".idea")
$ExcludeFiles = @("*.log", "*.pdb", "*.local.ps1")

# scp 带退避重试。
# 为什么需要: Windows OpenSSH 不支持 ControlMaster 连接复用, 一次远程构建要连开 4~6 条
# SSH 会话 (上传 / 解压 / 执行 / 回传), 短时间密集建连会偶发被 sshd 拒。判据是「手动逐条
# 跑必成功、脚本里却间歇失败」—— 那指向密集建连而非配置错误, 故退避重试而不是改 sshd。
function Invoke-Scp ([string]$from, [string]$to, [string]$what) {
    foreach ($try in 1..3) {
        & scp @SshOpts -q $from $to
        if ($LASTEXITCODE -eq 0) { return $true }
        if ($try -lt 3) {
            Warn "  scp $what 第 $try 次失败 (rc=$LASTEXITCODE), 重试..."
            Start-Sleep -Milliseconds (500 * $try)
        }
    }
    return $false
}

function Sync-Tree ([string]$src, [string]$remoteDir, [string]$label) {
    if (-not (Test-Path $src)) { Gray "  - 跳过 $label (本机不存在)"; return $true }
    $rnd  = -join ((48..57) + (97..122) | Get-Random -Count 6 | ForEach-Object { [char]$_ })
    $tgz  = Join-Path $env:TEMP "wi-src-$rnd.tar.gz"
    $rTgz = "C:/Windows/Temp/wi-src-$rnd.tar.gz"
    try {
        # bsdtar 的 --exclude 按【归档内路径】做 fnmatch。以 -C <root> . 打包时路径形如
        # ./wind_input/..., 故顶层与嵌套两种模式都要给 —— 只写 --exclude=target 不保险。
        $ex = @()
        foreach ($d in $ExcludeDirs)  { $ex += "--exclude=./$d"; $ex += "--exclude=*/$d" }
        foreach ($f in $ExcludeFiles) { $ex += "--exclude=$f" }
        # gnutar 格式没有 ustar 的 255 字符路径与 8 GB 大小限制, 对本仓是安全的默认替换。
        # (2026-08-18 更正: 曾以为 --format=gnutar 能绕开下面这个「拒绝覆盖归档自身」的
        # 报错, 判断依据错了 —— 详见本函数下方解压那段的说明, 真正的绕法在解压那一侧。)
        Push-Location $src
        try { & tar -czf $tgz --format=gnutar @ex . } finally { Pop-Location }
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path $tgz)) { ErrMsg "  打包 $label 失败 (tar rc=$LASTEXITCODE)"; return $false }
        $mb = [math]::Round((Get-Item $tgz).Length / 1MB, 2)

        if (-not (Invoke-Scp $tgz "${Target}:$rTgz" "上传 $label")) {
            ErrMsg "  scp 上传 $label 失败 (已重试 3 次)"; return $false
        }

        # ---- 清理残留 (prune) ----
        # tar 解压是【叠加】不是镜像: 只创建和覆盖, 从不删除。本机删掉的文件会永远留在编译机上。
        # 对 src\ 多半无害 (不被 mod 引用就不参与编译), 但 cargo 会把 tests\ benches\ examples\
        # 下的每个 .rs **自动发现**为独立编译目标 —— 不需要任何引用就参与构建。于是早已删除的
        # 测试文件带着对已删字段的引用一起炸, 而报错指向一个 git 和工作区里都找不到的文件。
        #
        # 做法: 拿 tar 包自身的清单当「应当存在的集合」, 剪枝遍历远程目录删掉集合外的文件。
        # ★ 清单直接来自刚传上去的那个包 (tar -tzf), 与实际传输内容【必然】一致 —— 不引入
        #   第二份需要人工保持同步的规则。
        # ★ 清理作用域 == 同步作用域, 同一个 $ExcludeDirs/$ExcludeFiles 说了算: 我们从不同步
        #   的东西, 也就不管它的生死。target\ build[_dev]\ dist\ .cache\ (CMake 缓存) 因此
        #   全部原样保留 —— 剪枝时遇到这些目录名直接不进去, 连遍历成本都没有。
        # ★ 只删文件不删目录: 空目录无害, 且天然堵死「误删整个 target」这类不可逆后果。
        $prune = ""
        if (-not $NoPrune) {
            $edLit = ($ExcludeDirs  | ForEach-Object { "'$_'" }) -join ','
            $efLit = ($ExcludeFiles | ForEach-Object { "'$_'" }) -join ','
            # 两道自保闸门, 缺一不可:
            #   $c -eq 0      解压失败时清单不可信, 绝不动手;
            #   $k.Count -gt 0 清单为空 (tar -tzf 失败) 时会把整棵树判成残留 —— 必须挡住。
            $prune =
                "if (`$c -eq 0) { " +
                  "`$k=[Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase); " +
                  "tar -tzf '$rTgz' | ForEach-Object { `$p=`$_ -replace '^\./','' -replace '/','\'; " +
                    "if (`$p -and -not `$p.EndsWith('\')) { [void]`$k.Add(`$p) } }; " +
                  "if (`$k.Count -gt 0) { " +
                    "`$ed=@($edLit); `$ef=@($efLit); " +
                    "`$rl='$remoteDir'.TrimEnd('\').Length+1; " +
                    "`$pr=[Collections.Generic.List[string]]::new(); " +
                    "`$st=[Collections.Stack]::new(); `$st.Push('$remoteDir'); " +
                    "while (`$st.Count) { `$d=`$st.Pop(); " +
                      "foreach (`$e in (Get-ChildItem -LiteralPath `$d -Force -EA SilentlyContinue)) { " +
                        "if (`$e.PSIsContainer) { if (`$ed -notcontains `$e.Name) { [void]`$st.Push(`$e.FullName) } } " +
                        "else { `$sk=`$false; foreach (`$m in `$ef) { if (`$e.Name -like `$m) { `$sk=`$true; break } }; " +
                          "if (-not `$sk) { `$r=`$e.FullName.Substring(`$rl); " +
                            "if (-not `$k.Contains(`$r)) { Remove-Item -LiteralPath `$e.FullName -Force -EA SilentlyContinue; `$pr.Add(`$r) } } } } }; " +
                    "if (`$pr.Count) { `"    [$label] 清理残留 `$(`$pr.Count) 个:`"; " +
                      "`$pr | Select-Object -First 10 | ForEach-Object { `"      `$_`" }; " +
                      "if (`$pr.Count -gt 10) { `"      ... 另 `$(`$pr.Count-10) 个`" } } } } "
        }

        # 不加 -Quiet: tar 正常解压本就静默, 一旦出错 (占用/权限/损坏) 那几行才是唯一线索。
        # 静默吞掉它等于把「解压失败」变成一个无法排查的黑盒。
        #
        # ⚠️ 解压必须走【管道】喂给 tar 的 stdin, 不能 `tar -xzf <命名文件>`——
        #    后者会稳定复现 "<某个源文件>: Refusing to overwrite archive: No error"。
        #    真实机制 (libarchive/archive_read_open_filename.c 的 S_ISREG 分支, 已读源码确认):
        #    bsdtar 打开【具名】归档文件时会 stat() 它自己, 把这份 dev+ino 记成
        #    "skip_file" —— 解压时任何一个目标路径的 dev+ino 撞上它就报这个错, 本意是防止
        #    「解压目录里正好包含归档自身」时把还在读的归档文件覆盖掉。
        #    这两个数在我们的场景里纯属巧合碰撞: temp 目录 (C:\Windows\Temp) 文件churn 高,
        #    NTFS 的 MFT 记录被快速复用, 归档文件的 dev+ino 偶尔会和 $remoteDir 下某个【已存在】
        #    的旧文件相同 —— 与文件名/内容无关, 每次撞上的文件都不一样。
        #    (2026-08-18 更正: 上面 --format=gnutar 的注释把这个错误归因到 SCHILY.dev/ino 这份
        #    完全不相关的 pax 硬链接元数据上, 已用 libarchive 源码证伪, 那次「修好了」是巧合。)
        #    绕法: 把归档从 stdin 喂给 tar, 而不是让它按文件名自己打开。stdin 是管道
        #    (S_ISFIFO) 不是具名文件 (S_ISREG), skip_file 检查的前置条件就不成立。
        #    PowerShell 里唯一能拿到【真管道】而非「重定向到同一个文件」的写法, 是让管道
        #    两端都是【原生命令】(cmd.exe 与 tar.exe) —— cmdlet(如 Get-Content) 在中间会把
        #    字节流按对象/文本重新编码, 达不到效果。
        # cmd.exe 内置的 type 命令不认正斜杠路径 (跟 echo/dir 等基于 Win32 API 的命令不同,
        # 会直接报 "The syntax of the command is incorrect." 且不产出任何字节) —— 而 $rTgz
        # 全篇都是正斜杠 (scp/ssh 目标路径的写法), 这里必须单独转成反斜杠再喂给 type。
        # ⚠️ 实测踩过: type 失败时管道另一端的 tar 拿到空 stdin 照样 exit 0 (0 字节的合法
        #    gzip? 不合法但 tar 未必硬失败), $LASTEXITCODE 只反映管道最后一个命令 tar 的退出码,
        #    于是"远程解压成功"是假的——目标目录里的旧文件原封不动, 且没有任何报错浮出来。
        $rTgzBS = $rTgz -replace '/', '\'
        $inner = "New-Item -ItemType Directory -Force -Path '$remoteDir' | Out-Null; " +
                 "cmd /c type '$rTgzBS' | tar -xz -C '$remoteDir'; `$c=`$LASTEXITCODE; " +
                 $prune +
                 "Remove-Item '$rTgz' -Force -EA SilentlyContinue; exit `$c"
        if (-not (Invoke-RemotePs $inner)) { ErrMsg "  远程解压 $label 失败 (退出码见上)"; return $false }
        Gray "  - $label 已同步 ($mb MB)"
        return $true
    }
    finally { Remove-Item $tgz -Force -ErrorAction SilentlyContinue }
}

# ---------- 产物回传 ----------
# 按命令只取该模块产出的文件 —— dm2 只需 1 个 exe (20 MB), 没必要连 data\ 22 MB 一起拉。
# 全构建 (1/d1/8/9) 才整目录打包回传。
$ArtifactMap = @{
    'm1'  = @('wind_tsf.dll', 'wind_tsf_x86.dll');       'dm1' = @('wind_tsf_dev.dll', 'wind_tsf_x86_dev.dll')
    'm2'  = @('wind_input.exe', 'wind_cli.bat');         'dm2' = @('wind_input_dev.exe', 'wind_cli.bat')
    'm3'  = @('wind_setting.exe');                       'dm3' = @('wind_setting_dev.exe')
    'm4'  = @('wind_portable.exe');                      'dm4' = @('wind_portable.exe')
}

# 命令 → 只回传 build[_dev]\ 下的这个子目录。gen-data 只产出 data\, 若照全构建那样整目录回传,
# 会把编译机上的 exe 一并盖到本机 —— 那些可能比本机刚编的还旧, 正是上面那条注释警告的
# 「改了没生效」成因, 而且没有任何提示。
$SubdirMap = @{ 'gd' = 'data'; 'gen-data' = 'data' }

# 只做校验、不产出任何 build[_dev]\ 内容的命令。不登记的话会落到下面的「全构建」整目录回传
# 分支, 把编译机的 build\ 拉回来盖掉本机产物 —— 一次远程 test 就能让本机的 exe 悄悄变成编译
# 机上不知哪一版, 而这几个命令恰恰是用得最频繁的。dev.ps1 转发时不传 -NoFetch, 拦不住。
$NoArtifactCmds = @('k', 'check', 'l', 'clippy', 't', 'test', 'ci', 'fmt-check')

function Receive-Artifacts ([string]$cmd, [string]$outName, [string]$localOut) {
    if (-not (Test-Path $localOut)) { New-Item -ItemType Directory -Path $localOut -Force | Out-Null }
    $files = $ArtifactMap[$cmd]

    if ($files) {
        $got = @(); $missed = @()
        foreach ($f in $files) {
            if (Invoke-Scp "${Target}:$RRootFwd/$outName/$f" "$localOut\$f" "回传 $f") { $got += $f } else { $missed += $f }
        }
        if (-not $got.Count) { ErrMsg "  未取回任何产物 —— 编译机上 $outName\ 里没有 $($files -join ', ')"; return $false }
        Gray ("  - " + ($got -join ", "))
        # 部分缺失必须说出来: 静默跳过会让本机留着【旧】产物, 而你以为部署的是刚编的那份 ——
        # 这正是「改了没生效」类问题最难查的成因。
        if ($missed.Count) { Warn "  ! 未取回: $($missed -join ', ') —— 本机这些文件仍是上一次的版本" }
        return $true
    }

    # 全构建: 远程打包整个 build[_dev]\ 再拉回 (含 data\, 首次约 30 MB)。
    # 只产出子目录的命令 (见 $SubdirMap) 按子目录收窄, 不碰同目录下的其它产物。
    $sub      = $SubdirMap[$cmd]
    $packPath = if ($sub) { "$outName/$sub" } else { $outName }
    $label    = if ($sub) { "$outName\$sub\" } else { "整目录 $outName\" }
    $rnd  = -join ((48..57) + (97..122) | Get-Random -Count 6 | ForEach-Object { [char]$_ })
    $rTgz = "C:/Windows/Temp/wi-out-$rnd.tar.gz"
    $tgz  = Join-Path $env:TEMP "wi-out-$rnd.tar.gz"
    try {
        $inner = "tar -czf '$rTgz' -C '$RRoot' '$packPath'; exit `$LASTEXITCODE"
        if (-not (Invoke-RemotePs $inner -Quiet)) { ErrMsg "  远程打包产物失败"; return $false }
        & scp @SshOpts -q "${Target}:$rTgz" $tgz
        if ($LASTEXITCODE -ne 0) { ErrMsg "  scp 下载产物失败"; return $false }
        # 同上 (Sync-Tree 里的说明): 具名文件解压会偶发撞上 bsdtar 的 skip_file 误判,
        # 改走 cmd/tar 两个原生命令之间的管道, stdin 是 S_ISFIFO 而非 S_ISREG。
        cmd /c type $tgz | tar -xz -C $ProductRoot
        if ($LASTEXITCODE -ne 0) { ErrMsg "  本地解压产物失败"; return $false }
        Gray "  - $label 已回传 ($([math]::Round((Get-Item $tgz).Length/1MB,2)) MB)"
        Invoke-RemotePs "Remove-Item '$rTgz' -Force -EA SilentlyContinue" -Quiet | Out-Null
        return $true
    }
    finally { Remove-Item $tgz -Force -ErrorAction SilentlyContinue }
}

# ---------- 测试假绿闸门 ----------
# 依赖词库的测试在 build_dev\data 缺失时【静默跳过且计数照绿】—— 编译机若没跑过全构建
# 就没有 data\, 远程 test 会给出一份漂亮的假绿。判据是耗时不是通过数, 但那要人去看;
# 这里改成开跑前直接拦住, 让缺失变成一次明确失败, 而不是一次虚假成功。
function Test-RemoteDataReady ([string]$cmdText) {
    if ($cmdText -notmatch '(^|\s)(t|test|ci)(\s|$)' -and $cmdText -notmatch 'cargo\s+test') { return $true }
    $n = Get-RemoteValue "@(Get-ChildItem '$RRoot\build_dev\data' -Recurse -File -EA SilentlyContinue).Count"
    if (-not $n -or [int]$n -eq 0) {
        ErrMsg "`n拒绝在编译机上跑测试: $RRoot\build_dev\data 不存在或为空。"
        ErrMsg "依赖词库的测试会因此【静默跳过且计数照绿】, 给出假绿。"
        ErrMsg "请先跑一次全构建:  dev.ps1 d1"
        return $false
    }
    if ([int]$n -lt 10) { Warn "  ! 编译机 build_dev\data 只有 $n 个文件 (本机通常 50+), 词库测试可能不完整。" }
    return $true
}

# 命令 → 产物目录名。这个函数回答的是「产物落在 build 还是 build_dev」, **不是**「构建哪个
# profile」—— 两者绝大多数时候一致 (dev 类命令一律以 d 开头: d1/dm1/dm2/d8/d9), 但 gen-data
# 是例外: 它不以 d 开头, 而 dev.ps1 的 Do-GenData 默认 outdir 就是 $BuildDevDir。
#
# ⚠️ 照搬「d 开头 = dev」的拼写规律曾让远程 gen-data 静默失效: 去编译机的 build\ 取产物,
# 而它刚写的是 build_dev\data ——【回传的是编译机上一次全构建留下的旧 build\, 还整个盖回
# 本机】, 且全程 exit 0、日志照打「✓ 完成」。唯一破绽是产出路径与回传路径两行对不上。
# ★ 判据必须与 dev.ps1 的实际写入目录对齐, 不能与命令的拼写规律对齐。
function Get-OutDirFor ([string]$cmd) {
    if ($cmd -match '^d' -or $cmd -in @('gd', 'gen-data')) { "build_dev" } else { "build" }
}

# 命令 → 真正需要的伴生仓。dm1/dm2 (最常用) 和细粒度 cargo 完全用不到它们, 每次都同步
# 三个仓纯属浪费; 只有构建 setting/portable 或打包时才需要。
#
# ⚠️ wind-setting 必须连 wind-ui-rust 一起同步: 它是 **path 依赖**
# (windui = { path = "../wind-ui-rust" }), 仓不在就直接 "系统找不到指定的路径 (os error 3)"。
# wind-portable / wind-installer 用的是 crates.io 版本 (均 0.11), 不需要本地仓。
function Get-NeededSiblings ([string]$cmd) {
    if (-not $WIND_REMOTE_SIBLINGS) { return @() }
    $need = switch -Regex ($cmd) {
        '^(1|release|d1|dev)$' { @('wind-setting', 'wind-ui-rust', 'wind-portable'); break }
        '^(m3|dm3)$'           { @('wind-setting', 'wind-ui-rust'); break }
        '^(m4|dm4)$'           { @('wind-portable'); break }
        '^(8|8s|d8|d8s)$'      { @('wind-setting', 'wind-ui-rust', 'wind-portable', 'wind-installer'); break }
        '^(9|9s|d9|d9s)$'      { @('wind-setting', 'wind-ui-rust', 'wind-portable'); break }
        default                { @() }
    }
    # 与用户配置的清单取交集 —— 用户可能只列了其中一部分
    return @($WIND_REMOTE_SIBLINGS | Where-Object { $need -contains $_ })
}

# ============================== 主流程 ==============================
$isRaw   = ($PSCmdlet.ParameterSetName -eq 'Raw')
$cmdText = if ($isRaw) { $Raw } else { $Command }
$sw      = [System.Diagnostics.Stopwatch]::StartNew()
if ($isRaw) { $outName = $null; $localOut = $null }
else {
    $outName  = Get-OutDirFor $Command
    $localOut = "$ProductRoot\$outName"
}

Say "`n========== 远程执行 ($cmdText → $WIND_REMOTE_HOST) =========="
# 槽位必须显式打出来: 不可见的路径切换本身就是下一个坑 —— 看到产物不对时, 第一眼就能
# 分清「编错了」还是「编在别的槽位里」。
if ($slot) { Gray "  槽位 $slot → $RRoot" }
if (-not (Test-RemoteDataReady $cmdText)) { exit 1 }

# --- [1/3] 同步源码 ---
if ($Clean) {
    Warn "[0/3] -Clean: 删除编译机上的 $RRoot (含 target\, 下次将全量重编) ..."
    if (-not (Invoke-RemotePs "Remove-Item -LiteralPath '$RRoot' -Recurse -Force -EA SilentlyContinue" -Quiet)) {
        ErrMsg "清理失败"; exit 1
    }
}
# 取锁必须【早于同步】: 同步会覆盖对方正在编译的源码, 危害不比清空产物目录小。
if (-not $NoLock) { if (-not (Enter-RemoteLock)) { exit 1 } }

# ↓↓↓ 从这里到文件末尾的 finally 都在锁内。内部刻意不缩进 —— 这段六十行注释密集,
#     整体缩进会让 diff 淹没真正的改动。exit 在 try 内同样会触发 finally, 锁不会漏放。
try {

# 分段计时: 优化前先知道时间花在哪一段。只测量、不改变行为 —— 曾凭「感觉编译慢」去调
# 并行度, 实测才发现编译只占总时长的一小半, 同步与回传才是大头。
$tSync = [System.Diagnostics.Stopwatch]::StartNew()
if ($NoSync) { Gray "[1/3] 跳过源码同步 (-NoSync)" }
else {
    Say "[1/3] 同步源码到编译机..."
    # 伴生仓与产品仓平级, 但只在真正需要时同步 (见 Get-NeededSiblings)
    $siblings   = if ($isRaw) { @() } else { Get-NeededSiblings $Command }
    $rRootParent = Split-Path $RRoot -Parent
    $ok = Sync-Tree $ProductRoot $RRoot "主仓"
    foreach ($s in $siblings) {
        if (-not $ok) { break }
        $ok = Sync-Tree ([System.IO.Path]::GetFullPath("$ProductRoot\..\$s")) "$rRootParent\$s" $s
    }
    if (-not $ok) { ErrMsg "`n源码同步失败, 已中止 —— 未触发远程执行。"; exit 1 }
}
$tSync.Stop()

# --- [2/3] 远程执行 ---
# 流的处理 (三次踩坑后的定稿, 别再动):
#   6>&1  把 Information 流 (dev.ps1 的 Write-Host) 并进 stdout。不并的话 pwsh 作为子进程
#         会把它以 CLIXML 编码塞进 stderr, 输出里就会出现 `#< CLIXML` + 整段 <Objs ...>。
#   不写 2>&1  —— 错误流保持独立: cargo / rustc 的 stderr 经 ssh 自己的 stderr 通道原样传回,
#         本机照常可见 (实测 "error: package ID specification ... did not match" 清晰可读)。
#         合并它反而会把上面那套 CLIXML 序列化一起拖进来。
#   exit $LASTEXITCODE —— 不可省, 否则 ssh 只反映 pwsh 是否启动成功, 编译失败会被吞成绿灯。
if ($isRaw) {
    Say "[2/3] 在编译机 wind_input\ 下执行: $Raw"
    # ⚠️ 必须包进 &{ } 再重定向: 6>&1 只能作用于【命令】。直接写 "$Raw 6>&1" 时, 若 $Raw 以
    #    语句块收尾 (if/foreach/try ...), PowerShell 会把 6>&1 解析成下一条命令 —— 报
    #    "The term '6>&1' is not recognized"。简单命令 (cargo test -p x) 看不出问题, 复合
    #    命令才炸, 属于放着必然咬人的那类。
    $inner = "Set-Location -LiteralPath '$RRoot\wind_input'; & { $Raw } 6>&1; exit `$LASTEXITCODE"
} else {
    Say "[2/3] 在编译机上执行 dev.ps1 $Command ..."
    # 编译机是专用机器, 没有「别把机器压卡」的顾虑, 于是让全构建的四步并行 (见 dev.ps1 的
    # Invoke-BuildStagesParallel)。开关只在这里注入 —— 本机直接跑 dev.ps1 时行为不变。
    $inner = "`$env:WIND_PARALLEL_BUILD='1'; & '$RRoot\scripts\dev.ps1' $Command 6>&1; exit `$LASTEXITCODE"
}
$tExec = [System.Diagnostics.Stopwatch]::StartNew()
if (-not (Invoke-RemotePs $inner)) {
    ErrMsg "`n远程执行失败$(if($localOut){" —— 本机 $outName\ 保持原样, 未回传任何产物。"})"
    exit 1
}
$tExec.Stop()

# --- [3/3] 回传产物 ---
$tFetch = [System.Diagnostics.Stopwatch]::StartNew()
$noArtifact = (-not $isRaw) -and ($Command -in $NoArtifactCmds)
if ($isRaw -or $NoFetch -or $noArtifact) {
    $why = if ($isRaw) { ' (-Raw 模式不产出 build\ 内容)' }
           elseif ($noArtifact) { " ($Command 只做校验, 不产出 build[_dev]\ 内容)" }
           else { ' (-NoFetch)' }
    Gray "[3/3] 跳过产物回传$why"
} else {
    Say "[3/3] 回传产物 → $localOut ..."
    if (-not (Receive-Artifacts $Command $outName $localOut)) { exit 1 }
}

$tFetch.Stop()
$sw.Stop()
Say "`n完成 ($cmdText), 耗时 $([math]::Round($sw.Elapsed.TotalSeconds,1)) s"
# 三段耗时: 想提速时先看这行, 别对着占比最小的那段调参数。
Gray ("  同步 {0:N1}s  ·  远程执行 {1:N1}s  ·  回传 {2:N1}s" -f `
      $tSync.Elapsed.TotalSeconds, $tExec.Elapsed.TotalSeconds, $tFetch.Elapsed.TotalSeconds)
if ($localOut -and -not $noArtifact) { Gray "  产物已在本机 $outName\, 可直接用部署命令 (pdm1 / pdm2 / pd1 ...) 安装。" }

}
finally { Exit-RemoteLock }   # ↑↑↑ 锁区结束。失败退出、Ctrl-C、正常收尾都会走到这里
# ⚠️ Ctrl+C/断线中断时, 上面这次释放不一定跑得到 (本地进程如何退出不受这段代码控制),
# 卡住了就手动 .\scripts\remote-build.ps1 -Unlock (或 dev.ps1 runlock) 清掉, 见该函数定义。
exit 0
