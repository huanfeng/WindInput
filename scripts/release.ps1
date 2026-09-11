# WindInput 多仓库发布脚本 (打 tag + push)
#
# 背景:
#   WindInput 发布依赖几个兄弟仓库, 以往只推主仓, 容易出现"主仓已发布、附属仓库改动
#   还留在本地"的情况。本脚本把它们作为一个整体发布, 且【WindInput 永远最后推】——
#   推 WindInput 的 v* tag 会立刻触发 .github/workflows/release.yml, 而该 workflow 用
#   actions/checkout 拉取各附属仓库时【没有指定 ref】, 取的是它们默认分支的最新提交。
#   所以附属仓库必须先于主仓 tag 到位, 否则 CI 会拿旧代码构建出错误的发布包。
#
# 版本号真源:
#   【tag 才是发布版本的唯一真源】。release.yml 在 tag 触发时执行
#   V="${GITHUB_REF_NAME#v}" 并写入 docs/VERSION, 即 CI 完全以 tag 名为准。
#   docs/VERSION 只用于两处: 本地 dev.ps1 构建, 以及 workflow_dispatch 手动触发时的
#   开发占位版本。因此发布【不需要】为改版本号单独提交 commit ——
#   bump 只是选一个新的 tag 名。发布成功后脚本会把 docs/VERSION 同步为新版本
#   (仅写文件, 不 commit), 让本地构建的产物版本号跟上, 你可以顺手带进下次提交,
#   也可以 git checkout 丢弃。
#
# 用法:
#   .\scripts\release.ps1                  # 交互菜单
#   .\scripts\release.ps1 check            # 完整预检 + push --dry-run, 不做任何改动
#   .\scripts\release.ps1 patch            # 发布 Patch 版 (v0.110.0 → v0.110.1)
#   .\scripts\release.ps1 minor            # 发布 Minor 版 (v0.110.0 → v0.111.0)
#   .\scripts\release.ps1 current          # 用最新 tag 版本号重发布 (需 -Force 覆盖)
#   .\scripts\release.ps1 status           # 只看本地状态, 不联网
#   .\scripts\release.ps1 push             # 五仓同步推送, 不打 tag
#   .\scripts\release.ps1 sign-draft       # 拉 CI 产物 → 本机签名 → 回传草稿 Release
#   .\scripts\release.ps1 sign-draft -Version 0.120.2   # 指定草稿 (缺省取最新的)
#   .\scripts\release.ps1 auto-sign        # 等 CI 跑完 → 自动接 sign-draft (可指定 -Version)
#   .\scripts\release.ps1 patch -AutoSign  # 打完 tag 直接进入等待, 全程无人值守
#   .\scripts\release.ps1 -Version 0.111.0-beta1   # 指定任意版本号发布
#
# 开关: -DryRun 只演练 / -Force 覆盖同名 tag / -Yes 非交互 / -Branch 指定分支
#       -AutoSign 打完 tag 自动等 CI 并签名 / -TimeoutMinutes 等待上限 / -PollMinutes 轮询间隔
#
# 无人值守发布 (打 tag → 等 CI → 签名回传, 约 25 分钟):
#   打完 tag 后脚本会问「是否自动签名」, 答 y 即进入轮询等待; 也可用 -AutoSign 免问。
#   等待期间只有一行状态在原地刷新, 阶段变化时才留一行记录; 按任意键立即查询一次,
#   Ctrl+C 随时中止 —— 中止不会丢任何东西, 事后 `release.ps1 sign-draft` 接着跑即可。
#   ⚠️ 签名会话有效期 2 小时, 而 CI 约 20 分钟: 选自动签名时【现在就把会话开好】,
#      等到 CI 结束时它仍在有效期内。脚本在进入等待前会先体检一次并提示;
#      CI 跑完时若会话仍不可用, 无人值守模式下最多再宽限 10 分钟, 之后停下来等你手动接。
#
# 注意:
#   - 所有仓库必须【处于目标分支上】。repo sync 后会停在游离 HEAD, 此时脚本中断
#     并提示切换命令 —— 游离状态下推送等同盲推, 不适合发布。
#   - 本脚本【不做 commit】。工作区有未提交改动时逐仓提示, 由你决定是否继续。

param(
    # 子命令; 留空进交互菜单
    [Parameter(Position = 0)]
    [ValidateSet("", "menu", "check", "patch", "minor", "current", "status", "push", "sign-draft", "auto-sign")]
    [string]$Command = "",
    # 指定发布版本号 (不含 v 前缀), 给定时忽略子命令的 bump 规则
    [string]$Version,
    # annotated tag 说明; 缺省 "Release v<版本号>"
    [string]$Message,
    # 目标分支; 缺省从 repo manifest 读取
    [string]$Branch,
    # 只演练: push 走 --dry-run, 不打 tag、不写文件
    [switch]$DryRun,
    # 覆盖已存在的同名 tag (本地 -f + 远端 --force)。
    # 远端 tag 已经指向本轮 HEAD 的仓库会被跳过 —— 那种"覆盖"只是换个 tagger 时间戳,
    # 引用一个字节都不变, 不值得冒 force push 的险。代价是不产生新的 release.yml run,
    # 脚本会据此不再提议守 CI (要重跑构建请去 Actions 手动 re-run)。
    [switch]$Force,
    # 非交互: 所有确认自动通过 (预检硬失败仍中止)
    [switch]$Yes,
    # 打完 tag 后不再询问, 直接进入「等 CI → 自动签名回传」
    [switch]$AutoSign,
    # 等 CI 的总时长上限 (分钟); 构建约 20 分钟, 留足重跑余量
    [ValidateRange(1, 1440)]
    [int]$TimeoutMinutes = 60,
    # 构建中的轮询间隔 (分钟); 临近完成时脚本会自动收紧到 1 分钟。
    # 下限必须是 1: 给 0 会变成不带 sleep 的死循环, 满速轮询 GitHub API 直到撞限流。
    [ValidateRange(1, 60)]
    [int]$PollMinutes = 5
)

$ErrorActionPreference = "Stop"
# git 往 stderr 写进度是常态, 不应视为失败; 一律以 $LASTEXITCODE 判定。
if ($PSVersionTable.PSVersion.Major -ge 7) { $PSNativeCommandUseErrorActionPreference = $false }
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }

# ---------- 路径 ----------
# 目录层级: <工作区根>\WindInput\scripts\release.ps1
$ScriptDir   = $PSScriptRoot
$ProductRoot = Split-Path $ScriptDir -Parent     # 主仓 (含 docs\VERSION)
$WorkRoot    = Split-Path $ProductRoot -Parent   # 各仓库平级所在的工作区根
$VersionFile = Join-Path $ProductRoot "docs\VERSION"

# ---------- 参与发布的仓库 ----------
# 顺序即执行顺序: 依赖在前, WindInput 最后 (主仓 tag = 整套代码已就位的信号)。
#   Tag=$true  打版本 tag
#   Tag=$false 只保证推送 —— wind-ui-rust 是 wind-setting 的 path 依赖
#              (wind-setting/Cargo.toml: windui = { path = "../wind-ui-rust" }),
#              CI 会拉它的 main 参与构建; 但它是独立发 crates.io 的开源库,
#              自有版本线, 不应打 WindInput 的版本 tag。
#              (wind-installer 走 crates.io 的 windui = "0.8", 不受此影响)
$Repos = @(
    [pscustomobject]@{ Name = "wind-ui-rust";   Tag = $false }
    [pscustomobject]@{ Name = "wind-setting";   Tag = $true  }
    [pscustomobject]@{ Name = "wind-portable";  Tag = $true  }
    [pscustomobject]@{ Name = "wind-installer"; Tag = $true  }
    [pscustomobject]@{ Name = "WindInput";      Tag = $true  }
)
$MainRepo = "WindInput"

# ---------- 输出辅助 (风格对齐 dev.ps1) ----------
function Say    ([string]$m) { Write-Host $m -ForegroundColor Green }
function Warn   ([string]$m) { Write-Host $m -ForegroundColor Yellow }
function ErrMsg ([string]$m) { Write-Host $m -ForegroundColor Red }
function Gray   ([string]$m) { Write-Host $m -ForegroundColor DarkGray }
function Cyan   ([string]$m) { Write-Host $m -ForegroundColor Cyan }

# ---------- 进度显示辅助 ----------
function Format-Size ([double]$bytes) {
    if ($bytes -ge 1GB) { return ("{0:N2} GB" -f ($bytes / 1GB)) }
    if ($bytes -ge 1MB) { return ("{0:N1} MB" -f ($bytes / 1MB)) }
    if ($bytes -ge 1KB) { return ("{0:N0} KB" -f ($bytes / 1KB)) }
    return ("{0:N0} B" -f $bytes)
}
# 秒 → mm:ss / h:mm:ss; 无法估算时给 --:--, 不要编一个假数字出来
function Format-Span ([double]$seconds) {
    if ($seconds -lt 0 -or [double]::IsNaN($seconds) -or [double]::IsInfinity($seconds) -or $seconds -gt 359999) {
        return "--:--"
    }
    $t = [TimeSpan]::FromSeconds([math]::Round($seconds))
    if ($t.TotalHours -ge 1) { return ("{0}:{1:d2}:{2:d2}" -f [int]$t.TotalHours, $t.Minutes, $t.Seconds) }
    return ("{0:d2}:{1:d2}" -f $t.Minutes, $t.Seconds)
}

# 原地刷新的单行进度。输出被重定向时(管道 / 日志文件)整个退化为静默 ——
# `\r` 在日志里不会回退光标, 只会攒出几千行垃圾; 那种场合由调用方按里程碑打整行。
$script:ProgressInline = $true
try { $script:ProgressInline = -not [Console]::IsOutputRedirected } catch { $script:ProgressInline = $false }

# 终端显示列数, 不是字符数。
# ⚠️ 这两个数在中文行上差很多 ("剩余 00:09" 的 .Length 是 9, 占 11 列)。用 .Length 去
#    截断/补齐, 补出来的行会超出窗口宽度而【自动换行】—— 之后 `\r` 只能退到新行行首,
#    上一帧就永久留在屏幕上了。5 分钟的倒计时能就此攒出几百行, 表现为"刷屏"。
function Get-DisplayWidth ([string]$text) {
    $w = 0
    foreach ($c in $text.ToCharArray()) {
        $u = [int]$c
        if (($u -ge 0x1100 -and $u -le 0x115F) -or   # 韩文字母
            ($u -ge 0x2E80 -and $u -le 0xA4CF) -or   # 部首扩展 ~ 彝文 (含 CJK 统一汉字)
            ($u -ge 0xAC00 -and $u -le 0xD7A3) -or   # 韩文音节
            ($u -ge 0xF900 -and $u -le 0xFAFF) -or   # CJK 兼容汉字
            ($u -ge 0xFE30 -and $u -le 0xFE4F) -or   # CJK 兼容形式
            ($u -ge 0xFF00 -and $u -le 0xFF60) -or   # 全角 ASCII
            ($u -ge 0xFFE0 -and $u -le 0xFFE6)) {    # 全角符号
            $w += 2
        } else {
            $w += 1
        }
    }
    return $w
}

# 按显示列数截断到 $limit 列。
# ⚠️ 按文本单元(grapheme)推进, 不是按 UTF-16 码元 —— BMP 外的字符(emoji 等)由两个码元
#    组成, 按码元截会在中间断开, 留下一个孤立代理字符, 终端显示成乱码方块。
function Limit-DisplayWidth ([string]$text, [int]$limit) {
    if (-not $text) { return "" }
    $w = 0
    $sb = New-Object System.Text.StringBuilder
    $e = [System.Globalization.StringInfo]::GetTextElementEnumerator($text)
    while ($e.MoveNext()) {
        $el = [string]$e.Current
        $cw = Get-DisplayWidth $el
        if ($w + $cw -gt $limit) { break }
        [void]$sb.Append($el); $w += $cw
    }
    return $sb.ToString()
}

function Write-ProgressLine ([string]$text) {
    if (-not $script:ProgressInline) { return }
    $w = 100
    try { $w = [Console]::WindowWidth - 1 } catch { }
    if ($w -lt 20) { $w = 20 }
    $text = Limit-DisplayWidth $text $w
    # 补齐用的是"还差几列", 不是"还差几个字符"
    $pad = $w - (Get-DisplayWidth $text)
    if ($pad -gt 0) { $text = $text + (" " * $pad) }
    Write-Host ("`r" + $text) -NoNewline
    $script:ProgressActive = $true
}
# 结束一段原地进度: 把最后一帧留在屏幕上并换行。
# 没有正在刷新的进度行时什么都不做 —— 否则每次调用都平白多吐一个空行。
$script:ProgressActive = $false
function Close-ProgressLine ([string]$finalText) {
    if (-not $script:ProgressInline) { if ($finalText) { Gray $finalText }; return }
    if ($finalText) { Write-ProgressLine $finalText }
    elseif (-not $script:ProgressActive) { return }
    Write-Host ""
    $script:ProgressActive = $false
}

# ---------- git 封装 ----------
# 统一走 git -C <repo>, 避免 Set-Location 造成状态泄漏。
function Invoke-GitRaw ([string]$repo, [string[]]$gitArgs) {
    $out = & git -C $repo @gitArgs 2>&1
    [pscustomobject]@{
        Code = $LASTEXITCODE
        Out  = (($out | ForEach-Object { $_.ToString() }) -join "`n").Trim()
    }
}
# 探测性查询: 失败返回空串
function Get-GitValue ([string]$repo, [string[]]$gitArgs) {
    $r = Invoke-GitRaw $repo $gitArgs
    if ($r.Code -ne 0) { return "" }
    return $r.Out
}
# 写操作: 失败即抛
function Invoke-GitOrDie ([string]$repo, [string[]]$gitArgs, [string]$what) {
    $r = Invoke-GitRaw $repo $gitArgs
    if ($r.Code -ne 0) { throw "$what 失败:`n  git -C $repo $($gitArgs -join ' ')`n$($r.Out)" }
    return $r.Out
}

# ---------- 交互 ----------
# 无人值守签名期间置位: 让 Invoke-SignDraft 里那些「继续?」「上传?」不再拦人。
# ⚠️ 它【不能】用来跳过签名会话体检 —— 那道闸门问的是「客观上能不能签」,
#    不是「要不要征求同意」。故 Confirm-Step 提供 $ignoreAuto 让它退出自动模式。
$script:AutoYes = $false

function Confirm-Step ([string]$prompt, [bool]$defaultYes = $false, [bool]$ignoreAuto = $false) {
    if ($script:AutoYes -and -not $ignoreAuto) { Gray "$prompt  → (自动签名) 自动确认"; return $true }
    if ($Yes -and -not $ignoreAuto) { Gray "$prompt  → (-Yes) 自动确认"; return $true }
    $hint = if ($defaultYes) { "[Y/n]" } else { "[y/N]" }
    while ($true) {
        try {
            $ans = (Read-Host "$prompt $hint").Trim().ToLower()
        } catch {
            ErrMsg "当前不是交互式终端, 无法确认。非交互运行请加 -Yes。"
            return $false
        }
        if ($ans -eq "") { return $defaultYes }
        if ($ans -in @("y", "yes")) { return $true }
        if ($ans -in @("n", "no"))  { return $false }
    }
}

# ---------- 语义化版本 ----------
function ConvertTo-SemVer ([string]$v) {
    $v = ($v -replace '^v', '').Trim()
    if ($v -notmatch '^(\d+)\.(\d+)\.(\d+)(?:[-+.](.+))?$') { return $null }
    [pscustomobject]@{
        Major = [int]$Matches[1]
        Minor = [int]$Matches[2]
        Patch = [int]$Matches[3]
        Pre   = $Matches[4]           # 预发布后缀, 如 alpha / beta1
        Raw   = $v
    }
}
# 返回 >0 表示 a 更新
function Compare-SemVer ($a, $b) {
    foreach ($f in @("Major", "Minor", "Patch")) {
        if ($a.$f -ne $b.$f) { return $a.$f - $b.$f }
    }
    # 同 X.Y.Z 时: 正式版 > 预发布版 (v1.0.0 比 v1.0.0-beta 新)
    $ap = [bool]$a.Pre; $bp = [bool]$b.Pre
    if ($ap -ne $bp) { if ($ap) { return -1 } else { return 1 } }
    if ($a.Pre -ne $b.Pre) { return [string]::Compare($a.Pre, $b.Pre) }
    return 0
}
function Step-Version ($sv, [string]$level) {
    switch ($level) {
        "patch" { "{0}.{1}.{2}" -f $sv.Major, $sv.Minor, ($sv.Patch + 1) }
        "minor" { "{0}.{1}.0"   -f $sv.Major, ($sv.Minor + 1) }
        "major" { "{0}.0.0"     -f ($sv.Major + 1) }
    }
}

# ---------- 目标分支: 从 repo manifest 读 default revision ----------
function Get-ManifestBranch {
    $manifest = Join-Path $WorkRoot ".repo\manifests\default.xml"
    if (-not (Test-Path $manifest)) { return "main" }
    try {
        $xml = [xml](Get-Content $manifest -Raw)
        $rev = $xml.manifest.default.revision
        if ($rev) { return ($rev -replace '^refs/heads/', '') }
    } catch {
        Warn "解析 $manifest 失败, 回退到 main: $($_.Exception.Message)"
    }
    return "main"
}

# ---------- 查询最新的已发布 tag (远端为准, tag 是版本真源) ----------
function Get-LatestTag ([string]$repoPath, [switch]$LocalOnly) {
    $versions = @()
    if ($LocalOnly) {
        $out = Get-GitValue $repoPath @("tag", "--list", "v*")
        foreach ($line in ($out -split "`n")) {
            $sv = ConvertTo-SemVer $line.Trim()
            if ($sv) { $versions += $sv }
        }
    } else {
        # --refs 过滤掉 ^{} 解引用行
        $out = Get-GitValue $repoPath @("ls-remote", "--tags", "--refs", "origin", "v*")
        foreach ($line in ($out -split "`n")) {
            if ($line -match 'refs/tags/(\S+)$') {
                $sv = ConvertTo-SemVer $Matches[1]
                if ($sv) { $versions += $sv }
            }
        }
    }
    if ($versions.Count -eq 0) { return $null }
    $max = $versions[0]
    foreach ($v in $versions) { if ((Compare-SemVer $v $max) -gt 0) { $max = $v } }
    return $max
}

# ============================================================
# 预检: 采集单个仓库状态 (无副作用)
# ============================================================
function Get-RepoState ([pscustomobject]$repo, [string]$branch, [string]$tag, [bool]$needTag, [bool]$dryRun = $false) {
    $name = $repo.Name
    $path = Join-Path $WorkRoot $name
    $st = [pscustomobject]@{
        Name = $name; Path = $path; Tag = $repo.Tag
        Ok = $false; Errors = @(); Warnings = @(); Dirty = @()
        Head = ""; HeadShort = ""; Subject = ""; Branch = ""
        Ahead = 0; Behind = 0
        LocalTagOnHead = $false; RemoteHasTag = $false
        RemoteTagCommit = ""; TagUpToDate = $false
        TagPushed = $false      # 本轮是否真的把 tag 推上去了(决定 CI 会不会起新 run)
    }

    if (-not (Test-Path (Join-Path $path ".git"))) {
        $st.Errors += "不是 git 仓库或目录不存在: $path"
        return $st
    }

    Gray "  fetch origin $branch ..."
    $fetch = Invoke-GitRaw $path @("fetch", "origin", $branch, "--quiet")
    if ($fetch.Code -ne 0) {
        $st.Errors += "fetch 失败 (检查网络 / SSH 权限):`n      $($fetch.Out)"
        return $st
    }

    $st.Head      = Get-GitValue $path @("rev-parse", "HEAD")
    $st.HeadShort = Get-GitValue $path @("rev-parse", "--short", "HEAD")
    $st.Subject   = Get-GitValue $path @("log", "-1", "--pretty=%s")

    if (-not (Get-GitValue $path @("rev-parse", "origin/$branch"))) {
        $st.Errors += "远端不存在分支 origin/$branch"
        return $st
    }

    # -------- 必须在目标分支上 --------
    # repo sync 会停在游离 HEAD; 游离状态下推送是盲推 (推完本地仍不在分支上,
    # 后续 sync 行为难以预期), 不适合发布。
    $cur = Get-GitValue $path @("symbolic-ref", "--short", "-q", "HEAD")
    if (-not $cur) {
        $st.Errors += "处于游离 HEAD 状态 (detached), 未在任何分支上`n" +
                      "      切换命令:  git -C `"$path`" checkout $branch"
        return $st
    }
    if ($cur -ne $branch) {
        $st.Errors += "当前在分支 '$cur', 而发布目标分支是 '$branch'`n" +
                      "      切换命令:  git -C `"$path`" checkout $branch"
        return $st
    }
    $st.Branch = $cur

    # -------- 快进关系 --------
    $st.Behind = [int](Get-GitValue $path @("rev-list", "--count", "$($st.Head)..origin/$branch"))
    $st.Ahead  = [int](Get-GitValue $path @("rev-list", "--count", "origin/$branch..$($st.Head)"))
    if ($st.Behind -gt 0) {
        $st.Errors += "远端领先本地 $($st.Behind) 个提交 —— 请先 repo sync (或 git pull) 后重试"
    }

    # -------- 工作区 --------
    # WindInput 的 docs/VERSION 是本地版本占位文件(release 末尾会自动写它、常态不提交),
    # 不计入脏工作区确认, 避免每次发布都为它多按一次 y。porcelain 格式为 "XY path",
    # 路径从第 4 个字符起(Substring(3))。
    $porcelain = Get-GitValue $path @("status", "--porcelain")
    if ($porcelain) {
        $st.Dirty = @($porcelain -split "`n" | Where-Object { $_.Trim() } | Where-Object {
            -not ($name -eq $MainRepo -and $_.Length -ge 3 -and $_.Substring(3).Trim() -eq "docs/VERSION")
        })
    }

    # -------- tag 冲突 (仅对需要打 tag 的仓库) --------
    if ($needTag -and $st.Tag) {
        $localTagCommit = Get-GitValue $path @("rev-list", "-n", "1", $tag)
        $st.LocalTagOnHead = ($localTagCommit -eq $st.Head -and $localTagCommit -ne "")
        # ls-remote 的输出是 "<sha>\trefs/tags/<tag>"; 别只取存在性 —— 远端 tag 指向
        # 哪个 commit 决定了 -Force 到底是「危险地移动引用」还是「原地重推」。
        # annotated tag 要看 peeled 行(refs/tags/<tag>^{})才是 commit, 裸的那行是 tag
        # 对象自身的 SHA; lightweight tag 没有 peeled 行, 裸行就是 commit。
        $lsRemote = Get-GitValue $path @("ls-remote", "--tags", "origin", "refs/tags/$tag", "refs/tags/$tag^{}")
        $st.RemoteHasTag = [bool]$lsRemote
        if ($lsRemote) {
            $bare = ""
            foreach ($l in ($lsRemote -split "`n")) {
                if ($l -match '^(\S+)\s+refs/tags/\S+\^\{\}$') { $st.RemoteTagCommit = $Matches[1] }
                elseif ($l -match '^(\S+)\s+refs/tags/')        { $bare = $Matches[1] }
            }
            if (-not $st.RemoteTagCommit) { $st.RemoteTagCommit = $bare }
        }
        # 远端 tag 已经指向本轮要发布的 commit —— 推它不会改变任何引用指向。
        $st.TagUpToDate = ($st.RemoteTagCommit -ne "" -and $st.RemoteTagCommit -eq $st.Head)

        # 演练模式不会真的打 tag, 故 tag 冲突降级为警告, 让健康检查能跑完整流程
        if ($st.RemoteHasTag -and -not $Force) {
            $m = "远端已存在 tag $tag (用 -Force 覆盖, 或换一个版本号)"
            if ($dryRun) { $st.Warnings += $m } else { $st.Errors += $m }
        }
        if ($localTagCommit -and $localTagCommit -ne $st.Head -and -not $Force) {
            $short = Get-GitValue $path @("rev-parse", "--short", $localTagCommit)
            $m = "本地已存在 tag $tag 且指向 $short (非 HEAD); 用 -Force 重打"
            if ($dryRun) { $st.Warnings += $m } else { $st.Errors += $m }
        }
    }

    $st.Ok = ($st.Errors.Count -eq 0)
    return $st
}

# 打印单仓预检结果
function Show-RepoState ($st, [string]$tag, [bool]$needTag) {
    $label = if ($st.Tag) { $st.Name } else { "$($st.Name)  (只推送, 不打 tag)" }
    Write-Host ""
    Write-Host "── $label" -ForegroundColor White
    if ($st.Errors.Count -gt 0 -and -not $st.Head) {
        foreach ($e in $st.Errors) { ErrMsg "  [X] $e" }
        return
    }
    Write-Host "  分支       : $($st.Branch)"
    Write-Host "  HEAD       : $($st.HeadShort)  $($st.Subject)"
    Write-Host "  待推提交   : $($st.Ahead) 个" -NoNewline
    if ($st.Ahead -gt 0) { Warn "  ← 本地领先 origin/$($st.Branch)" } else { Gray "  (与远端一致)" }
    foreach ($e in $st.Errors)   { ErrMsg "  [X] $e" }
    foreach ($w in $st.Warnings) { Warn   "  [!] $w" }
    if ($needTag -and $st.Tag -and $Force -and $st.RemoteHasTag) {
        Warn "  [!] 远端已存在 tag $tag, -Force 将覆盖它"
    }
    if ($st.Dirty.Count -gt 0) {
        Warn "  [!] 工作区有 $($st.Dirty.Count) 处未提交改动:"
        $st.Dirty | Select-Object -First 10 | ForEach-Object { Gray "        $_" }
        if ($st.Dirty.Count -gt 10) { Gray "        ... 其余 $($st.Dirty.Count - 10) 项省略" }
    } else {
        Gray "  工作区     : 干净"
    }
}

# 精简 git push 的输出: 跳过 pre-push hook 的噪音, 只留末尾的推送结果
function Show-PushOutput ([string]$out) {
    $lines = @($out -split "`n" | Where-Object { $_.Trim() })
    if ($lines.Count -eq 0) { return }
    $keep = 3
    if ($lines.Count -gt $keep) {
        Gray "    ... (pre-push hook 输出 $($lines.Count - $keep) 行已省略)"
        $lines = $lines[-$keep..-1]
    }
    foreach ($l in $lines) { Gray "    $($l.TrimEnd())" }
}

# ============================================================
# 主流程: 发布 / 只推送 / 演练
# ============================================================
function Invoke-Release {
    param(
        [string]$tag,        # 形如 v0.111.0; $NoTag 时忽略
        [string]$branch,
        [string]$msg,
        [bool]$noTag  = $false,
        [bool]$dryRun = $false
    )
    $needTag = -not $noTag
    $title = if ($dryRun) { "演练 (DryRun)" } elseif ($noTag) { "同步推送 (不打 tag)" } else { "发布 $tag" }

    Write-Host ""
    Cyan "==================== WindInput 多仓库$title ===================="
    if ($needTag) { Write-Host "  tag        : " -NoNewline; Say $tag }
    Write-Host "  目标分支   : $branch"
    Write-Host "  工作区根   : $WorkRoot"
    Write-Host "  执行顺序   : $(($Repos.Name) -join ' → ')"
    if ($Force) { Warn "  模式       : Force (允许覆盖已存在的 tag)" }
    Cyan "=============================================================="

    # ---------- [1/3] 预检 ----------
    Write-Host ""
    Cyan "[1/3] 预检"
    $states = @()
    foreach ($r in $Repos) {
        $st = Get-RepoState $r $branch $tag $needTag $dryRun
        Show-RepoState $st $tag $needTag
        $states += $st
    }

    if ($states | Where-Object { -not $_.Ok }) {
        Write-Host ""
        ErrMsg "预检未通过, 已中止 —— 未对任何仓库做出改动。"
        return 1
    }

    # ---------- 脏工作区逐仓确认 ----------
    foreach ($st in ($states | Where-Object { $_.Dirty.Count -gt 0 })) {
        Write-Host ""
        $what = if ($needTag) { "不会进入 $tag" } else { "不会被推送" }
        Warn "[!] $($st.Name) 有 $($st.Dirty.Count) 处未提交改动, 这些改动【$what】。"
        if (-not (Confirm-Step "    跳过这些改动并继续?" $false)) {
            ErrMsg "已取消 —— 未对任何仓库做出改动。请先提交或 stash 后重试。"
            return 1
        }
    }

    # ---------- [2/3] 计划 ----------
    Write-Host ""
    Cyan "[2/3] 执行计划"
    Write-Host ""
    Write-Host ("  {0,-16} {1,-10} {2,-6} {3,-6} {4}" -f "仓库", "HEAD", "待推", "脏", "操作")
    Write-Host ("  " + ("-" * 78)) -ForegroundColor DarkGray
    foreach ($st in $states) {
        $ops = @()
        if ($st.Ahead -gt 0) { $ops += "push $branch ($($st.Ahead) 提交)" } else { $ops += "跳过 push(无新提交)" }
        if ($needTag -and $st.Tag) {
            $ops += $(if ($st.TagUpToDate)   { "跳过 $tag(远端已一致)" }
                      elseif ($st.RemoteHasTag) { "覆盖 $tag" }
                      else                   { "打 $tag" })
        }
        Write-Host ("  {0,-16} {1,-10} {2,-6} {3,-6} {4}" -f `
            $st.Name, $st.HeadShort, $st.Ahead, $st.Dirty.Count, ($ops -join " + "))
    }
    if ($needTag) {
        Write-Host ""
        Gray "  $MainRepo 排最后: 推它的 tag 会触发 release.yml, 届时 CI 会拉取各附属仓库的"
        Gray "  $branch 最新提交参与构建, 故附属仓库必须先到位。"
    }

    # 真正危险的只有「远端 tag 指向别的 commit」—— 那才会移动引用。远端已指向本轮
    # HEAD 的仓库会被整个跳过, 不该拿它去吓唬人(见上面计划表里的「跳过」)。
    $moving = @($states | Where-Object { $_.RemoteHasTag -and -not $_.TagUpToDate })
    if ($Force -and $moving) {
        Write-Host ""
        Warn "[!!] -Force 将强制覆盖远端已存在的 tag。若他人已拉取该 tag, 会造成引用不一致。"
        foreach ($m in $moving) {
            $rs = if ($m.RemoteTagCommit) { $m.RemoteTagCommit.Substring(0, 7) } else { "?" }
            Warn ("     {0,-16} {1} → {2}" -f $m.Name, $rs, $m.HeadShort)
        }
        if (-not (Confirm-Step "     确认强制覆盖?" $false)) { ErrMsg "已取消。"; return 1 }
    }

    # 全部仓库的 tag 都已在远端就位: 本轮不会推任何 tag, 也就不会有新的 release.yml
    # run。这一步必须说在执行前 —— 否则用户会守着一个永远等不来的构建。
    $taggedRepos = @($states | Where-Object { $_.Tag })
    if ($needTag -and -not $dryRun -and $taggedRepos -and
        -not ($taggedRepos | Where-Object { -not $_.TagUpToDate })) {
        Write-Host ""
        Warn "[!] 所有 tag 在远端均已指向本轮 HEAD, 本次不会推送任何 tag。"
        Gray "    故 CI 不会起新的构建。要重跑构建请去 GitHub Actions 手动 re-run,"
        Gray "    或用 .\scripts\release.ps1 auto-sign 直接收尾上一轮已成功的构建。"
    }

    Write-Host ""
    if ($dryRun) {
        Warn "演练模式: 只做 git push --dry-run (真实校验远端权限与快进关系), 不打 tag、不推送。"
    } else {
        if (-not (Confirm-Step "确认执行?" $false)) { ErrMsg "已取消 —— 未做任何改动。"; return 1 }
    }

    # ---------- [3/3] 执行 ----------
    Write-Host ""
    Cyan "[3/3] 执行"
    # 主仓 tag 是最后推的, 所以本次 CI run 的 createdAt 必然晚于这一刻。留 2 分钟宽限
    # 抵消本地与 GitHub 的时钟差。用它挡掉「-Force 重发时误认上一轮旧 run」。
    $execStart = (Get-Date).AddMinutes(-2)
    $done = @()
    $createdTags = @()      # 本次新建但尚未推送成功的本地 tag, 失败时回滚
    $failedRepo = $null; $failedMsg = $null

    foreach ($st in $states) {
        Write-Host ""
        Write-Host "── $($st.Name)" -ForegroundColor White
        try {
            # 预检已确保处于目标分支。无新提交(Ahead=0)时跳过 push ——
            # 远端已含 HEAD(Behind 已在预检拦截), 再 push 只是空操作, 却会触发 pre-push
            # hook(如 wind-ui-rust 跑 clippy + 全量测试)白白变慢; 预检已 fetch 过, 无意义。
            if ($st.Ahead -eq 0) {
                Gray "  无新提交, 跳过 push (远端已是最新)"
            } else {
                # 不加 --force: 非快进会被远端拒绝, 这正是我们要的保护。
                $pushArgs = @("push", "origin", $branch)
                if ($dryRun) { $pushArgs += "--dry-run" }
                Gray "  git push origin $branch$(if ($dryRun) { ' --dry-run' })"
                $out = Invoke-GitOrDie $st.Path $pushArgs "推送分支"
                # 仓库可能装有 pre-push hook (如 wind-ui-rust 会跑 clippy + 全量测试),
                # 成功时其输出对发布无价值, 只保留末尾的 git 推送结果行。
                # 失败路径不走这里 —— Invoke-GitOrDie 抛出的异常带完整输出。
                if ($out) { Show-PushOutput $out }
                if ($dryRun) { Say "  [OK] 分支推送校验通过" } else { Say "  [OK] 分支已推送" }
            }

            if ($needTag -and $st.Tag -and -not $dryRun -and $st.TagUpToDate) {
                # 远端 tag 已指向本轮的 HEAD: 重打只会换一个 tagger 时间戳、让 tag 对象
                # 换个 SHA, 引用指向一个字节都不变, 却要冒 force push 的风险。跳过。
                # 代价: 不产生新的 release.yml run —— 由调用方据 TagPushed 决定不等 CI。
                Gray "  远端 tag $tag 已指向 $($st.HeadShort), 跳过打 tag / 推 tag"
            } elseif ($needTag -and $st.Tag -and -not $dryRun) {
                if ($st.LocalTagOnHead -and -not $Force) {
                    Gray "  本地 tag $tag 已指向 HEAD, 跳过创建"
                } else {
                    $tagArgs = if ($Force) { @("tag", "-a", "-f", $tag, "-m", $msg, $st.Head) }
                               else        { @("tag", "-a",     $tag, "-m", $msg, $st.Head) }
                    Invoke-GitOrDie $st.Path $tagArgs "创建 tag" | Out-Null
                    $createdTags += $st.Path
                    Say "  [OK] 已打 tag $tag → $($st.HeadShort)"
                }
                $tagPush = @("push", "origin", "refs/tags/$tag")
                if ($Force) { $tagPush += "--force" }
                Invoke-GitOrDie $st.Path $tagPush "推送 tag" | Out-Null
                $createdTags = @($createdTags | Where-Object { $_ -ne $st.Path })
                $st.TagPushed = $true
                Say "  [OK] tag $tag 已推送"
            } elseif ($needTag -and $st.Tag -and $dryRun) {
                Gray "  (演练) 跳过打 tag / 推 tag"
            }
            $done += $st.Name
        } catch {
            $failedRepo = $st.Name; $failedMsg = $_.Exception.Message
            break
        }
    }

    # ---------- 结果 ----------
    Write-Host ""
    Cyan "=============================================================="
    if ($failedRepo) {
        ErrMsg "中断于: $failedRepo"
        ErrMsg $failedMsg
        Write-Host ""
        if ($done.Count -gt 0) { Warn "已完成 (远端已改变, 不自动回滚): $($done -join ', ')" }
        else { Gray "没有任何仓库完成操作, 远端未改变。" }
        $pending = @($states | Where-Object { $_.Name -notin $done -and $_.Name -ne $failedRepo }).Name
        if ($pending) { Gray "尚未开始: $($pending -join ', ')" }
        # 清理本次创建但未推送成功的本地 tag, 保证重跑幂等
        foreach ($p in $createdTags) {
            if ((Invoke-GitRaw $p @("tag", "-d", $tag)).Code -eq 0) { Gray "已清理未推送的本地 tag: $p → $tag" }
        }
        Write-Host ""
        Warn "修复后重跑即可 —— 已推送的仓库会跳过, tag 幂等。"
        Cyan "=============================================================="
        return 1
    }

    # 触发 release.yml 的是【主仓】的 tag; 它没被推, 后面就没有新 run 可等。
    $mainTagPushed = [bool](@($states | Where-Object { $_.Name -eq $MainRepo -and $_.TagPushed }))

    if ($dryRun) {
        Say "演练完成: $($done.Count) 个仓库校验通过, 未做任何改动。"
    } elseif ($noTag) {
        Say "同步推送完成: $($done -join ', ')"
    } else {
        Say "发布完成: $tag"
        foreach ($st in $states) {
            $mark = if ($st.Tag) { $tag } else { "(未打 tag)" }
            Write-Host ("  {0,-16} {1,-10} {2}" -f $st.Name, $st.HeadShort, $mark)
        }
        # ---- 同步本地 docs/VERSION (仅写文件, 不 commit) ----
        # tag 已是 CI 的版本真源; 这里只是让本地 dev.ps1 构建的产物版本号跟上。
        Sync-LocalVersionFile ($tag -replace '^v', '')
        Write-Host ""
        if ($mainTagPushed) {
            Gray "CI: 推送 $tag 已触发 release.yml, 去 GitHub Actions 查看构建与草稿 Release。"
        } else {
            Warn "CI: 未推送 $MainRepo 的 tag (远端已一致), 本轮【没有】新的 release.yml run。"
        }
    }
    Cyan "=============================================================="

    # ---- 收尾: 等 CI + 自动签名回传 ----
    # 只在「真打了 tag」时提供 —— push/演练都没有可等的构建。
    # -Yes 是「别拿确认打断我」, 不是「替我决定要不要多等 20 分钟」, 故它不隐含自动签名;
    # 要无人值守请显式 -AutoSign。
    # 没推主仓 tag 就没有新 run —— 此时守 CI 只会空等到超时(Wait-ForCiBuild 的 notBefore
    # 会把上一轮的旧 run 一律当作"还没排上队"), 所以连问都不该问。
    if ($needTag -and -not $dryRun -and -not $mainTagPushed) {
        Write-Host ""
        Gray "未推送 tag, 无新构建可等。要收尾上一轮已成功的构建:"
        Gray "  .\scripts\release.ps1 sign-draft   (或 auto-sign)"
    } elseif ($needTag -and -not $dryRun) {
        $auto = $false
        if ($AutoSign) { $auto = $true }
        elseif (-not $Yes) {
            Write-Host ""
            Gray "下一步是等 CI 跑完 (约 20 分钟), 再把签名产物回传到草稿 Release。"
            $auto = Confirm-Step "现在就守着 CI, 跑完自动签名回传?" $true
        }
        if ($auto) { return (Invoke-AutoSign $tag $TimeoutMinutes $PollMinutes $execStart) }
        Gray "稍后手动收尾:  .\scripts\release.ps1 sign-draft   (或 auto-sign 让它自己等)"
    }
    return 0
}

# 把 docs/VERSION 同步为已发布版本 (保留原文件的换行风格; 不 commit)
function Sync-LocalVersionFile ([string]$newVersion) {
    if (-not (Test-Path $VersionFile)) { return }
    $raw = Get-Content $VersionFile -Raw
    $old = $raw.Trim()
    if ($old -eq $newVersion) { return }
    # 只替换版本号本身, 原有尾随换行保持不变
    Set-Content -Path $VersionFile -Value ($raw -replace [regex]::Escape($old), $newVersion) -NoNewline
    Write-Host ""
    Say "已同步 docs\VERSION: $old → $newVersion  (未 commit)"
    Gray "  tag 才是 CI 的版本真源; 此文件只影响本地 dev.ps1 构建的产物版本号。"
    Gray "  可顺手带进下次提交, 或 git -C `"$ProductRoot`" checkout docs/VERSION 丢弃。"
}

# ============================================================
# sign-draft: 拉 CI 产物 → 本机签名 → 回传草稿 Release
# ============================================================
# 发布链路的收尾。CI 建立不了云签名会话(要二次验证、会话只活 2 小时、需要本地客户端
# 进程,托管 runner 上做不到 —— 见 docs\design\code-signing.md 第 3 节),所以
# release.yml 产出的 Windows 包必然未签名,草稿 Release 正文顶着一条未签名横幅。
#
# 本命令补上那一步,且【本机一行代码都不编译】—— 构建环境只有 CI 一套,避免本机与
# CI 的工具链差异产出不同的二进制:
#   1. 拉 CI 的中转产物(build\ 散件 + 安装器三件套)
#   2. dev.ps1 unstage 还原(内含版本号硬校验)
#   3. dev.ps1 sign 8s / sign 9s —— 签名并【重新打包】
#   4. verify-sign 硬校验后才上传
#   5. 带进度地覆盖草稿里的 4 个资产 (见下面 Send-ReleaseAssets)
#   6. 删掉正文的未签名横幅
#
# ⚠️ 第 3 步为什么必须重新打包、不能对 CI 的成品补签外壳:签名夹在打包中间 ——
#    PE 要在封进压缩块之前签。补签只签得到外壳,包内 5 个 PE 仍是全裸的,而
#    signtool verify 验 Setup.exe 照样通过(见 dev.ps1 Do-Installer 第 2.5 步)。
#
# latest.json 不在上传之列 —— 它由 release-published.yml 在 Release 发布后按实际
# 资产重新生成,且带 sha256 与产物比对的硬校验(资产被动过就发布失败)。

# 从 Release 正文里摘掉未签名横幅。返回 $null 表示没找到(已删过, 或本就是签名产物)。
function Remove-UnsignedBanner ([string]$body) {
    $lines = $body -split "`r?`n"
    $start = -1
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -match '^>\s*\[!WARNING\]') { $start = $i; break }
    }
    if ($start -lt 0) { return $null }
    $end = $start
    while ($end + 1 -lt $lines.Count -and $lines[$end + 1] -match '^>') { $end++ }
    # 认特征串而不是"第一个 WARNING 块", 免得误删将来别的告示
    if ((($lines[$start..$end]) -join "`n") -notmatch '未经代码签名') { return $null }
    while ($end + 1 -lt $lines.Count -and $lines[$end + 1].Trim() -eq "") { $end++ }
    $kept = @()
    if ($start -gt 0)             { $kept += $lines[0..($start - 1)] }
    if ($end + 1 -lt $lines.Count) { $kept += $lines[($end + 1)..($lines.Count - 1)] }
    return ($kept -join "`n")
}

# ============================================================
# 带进度的资产上传
# ============================================================
# 为什么不用 `gh release upload`: 它把整个 HTTP 过程包在里面, 只在结束时打一行,
# 4 个 20+MB 的包传下来是好几分钟的纯黑屏 —— 分不清是在传、卡住了, 还是网断了。
# 这里直接打 GitHub 的 uploads 端点, 自己按块喂请求流, 从而拿到真实字节进度。
#
# ⚠️ AllowWriteStreamBuffering 必须关: 默认 .NET 会把整个文件先缓冲进内存再发,
#    那样进度条会在几百毫秒内冲到 100%, 然后对着已完成的进度条干等好几分钟 ——
#    比没有进度更误导人。关掉之后 Write() 才是真的往 socket 上写。
#
# 失败退路: 拿不到 token / uploadUrl 时原样退回 `gh release upload --clobber`,
# 只是没有进度。发布流程的正确性不依赖这段代码。

function Send-FileToUrl {
    param(
        [string]$Url,
        [string]$Token,
        [string]$Path,
        [string]$Prefix        # 进度行前缀, 形如 "  [2/4] WindInput-Portable-0.120.3.zip"
    )
    $total = [long](Get-Item -LiteralPath $Path).Length
    $req = [System.Net.HttpWebRequest]::Create($Url)
    $req.Method                    = "POST"
    $req.ContentType               = "application/octet-stream"
    $req.Accept                    = "application/vnd.github+json"
    $req.UserAgent                 = "WindInput-release.ps1"
    $req.AllowWriteStreamBuffering = $false
    $req.ContentLength             = $total
    # ⚠️ PowerShell 7 的 HttpWebRequest 是 HttpClient 的兼容壳: Timeout 覆盖【整个请求,
    #    含请求体】, 而 ReadWriteTimeout 在这条路径上根本不起作用。实测 Timeout=4s 传
    #    64MB, 写到 4016ms 就被 IOException 掐断。
    #    所以这里【不能】按"建连超时"给 60 秒 —— 那等于要求 26MB 必须跑满 440KB/s,
    #    慢网下 4 个包会全部失败。按体积折算成总上限, 20KB/s 的下限足够宽松,
    #    真卡死时仍会在有限时间内退出去走重试/退路。
    $req.Timeout                   = [int][math]::Min([int]::MaxValue, [math]::Max(600000, ($total / 20KB) * 1000))
    $req.ReadWriteTimeout          = 600000
    $req.Headers.Add("Authorization", "Bearer $Token")
    $req.Headers.Add("X-GitHub-Api-Version", "2022-11-28")

    # HttpWebRequest 只认 IE/系统代理设置, 不认 HTTPS_PROXY 环境变量 —— 而 gh、curl 认它。
    # 不补这一段, 会出现「gh 能传、这里传不出去」的割裂。
    $proxyUrl = if ($env:HTTPS_PROXY) { $env:HTTPS_PROXY } else { $env:https_proxy }
    if ($proxyUrl) {
        try { $req.Proxy = New-Object System.Net.WebProxy($proxyUrl, $true) } catch { }
    }

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $sent = [long]0
    $fs = $null; $rs = $null
    try {
        $rs  = $req.GetRequestStream()
        $fs  = [System.IO.File]::OpenRead($Path)
        $buf = New-Object byte[] 262144
        $lastDrawMs = -1000
        $lastMilestone = -1
        while ($true) {
            $n = $fs.Read($buf, 0, $buf.Length)
            if ($n -le 0) { break }
            $rs.Write($buf, 0, $n)
            $sent += $n
            $pct = if ($total -gt 0) { [int](100 * $sent / $total) } else { 100 }
            if ($script:ProgressInline) {
                # 200ms 一帧: 再密就是在刷终端而不是在传文件
                if ($sw.ElapsedMilliseconds - $lastDrawMs -ge 200) {
                    $lastDrawMs = $sw.ElapsedMilliseconds
                    Write-ProgressLine (Format-UploadLine $Prefix $sent $total $sw.Elapsed.TotalSeconds)
                }
            } elseif ([int]($pct / 20) -gt $lastMilestone) {
                # 非交互: 每 20% 打一整行, 日志里看得出进度又不刷屏
                $lastMilestone = [int]($pct / 20)
                Gray (Format-UploadLine $Prefix $sent $total $sw.Elapsed.TotalSeconds)
            }
        }
        $rs.Close(); $rs = $null
        $fs.Close(); $fs = $null

        # 服务端还要落盘校验, GetResponse() 这一步可能再等几秒
        Write-ProgressLine "$Prefix  等待 GitHub 确认 ..."
        $resp = $req.GetResponse()
        $code = [int]$resp.StatusCode
        $resp.Close()
        Close-ProgressLine (Format-UploadLine $Prefix $total $total $sw.Elapsed.TotalSeconds)
        if ($code -ge 200 -and $code -lt 300) {
            return [pscustomobject]@{ Ok = $true;  Error = "" }
        }
        return [pscustomobject]@{ Ok = $false; Error = "HTTP $code" }
    } catch {
        Close-ProgressLine ""
        # 主动断掉这条请求: 否则 AllowWriteStreamBuffering=$false 且字节没写满时,
        # 连接会半开着等 GC —— 重试 3 次就留 3 条。
        try { $req.Abort() } catch { }
        # ⚠️ PowerShell 把 .NET 方法抛出的异常包一层 MethodInvocationException, 所以
        #    $_.Exception.Response 恒为空 —— 必须顺着 InnerException 找到 WebException,
        #    否则拿不到 GitHub 的错误正文(422 的 already_exists 全在正文里)。
        $ex = $_.Exception
        $web = $null
        while ($ex) {
            if ($ex -is [System.Net.WebException]) { $web = $ex; break }
            $ex = $ex.InnerException
        }
        $msg = if ($web) { $web.Message } else { $_.Exception.Message }
        if ($web -and $web.Response) {
            try {
                $sr = New-Object System.IO.StreamReader($web.Response.GetResponseStream())
                $body = $sr.ReadToEnd(); $sr.Close()
                if ($body) { $msg = "$msg`n      $($body.Trim())" }
            } catch { }   # 连接被对端掐断时读不到正文, 有状态码就够定位了
            finally { try { $web.Response.Dispose() } catch { } }
        }
        return [pscustomobject]@{ Ok = $false; Error = $msg }
    } finally {
        if ($rs) { try { $rs.Dispose() } catch { } }
        if ($fs) { try { $fs.Dispose() } catch { } }
    }
}

function Format-UploadLine ([string]$prefix, [long]$sent, [long]$total, [double]$elapsed) {
    $pct  = if ($total -gt 0) { [int](100 * $sent / $total) } else { 100 }
    $barW = 22
    $fill = if ($total -gt 0) { [int]($barW * $sent / $total) } else { $barW }
    $bar  = ("=" * $fill).PadRight($barW, '.')
    $spd  = if ($elapsed -gt 0.001) { $sent / $elapsed } else { 0 }
    $eta  = if ($spd -gt 1) { ($total - $sent) / $spd } else { -1 }
    return ("{0} [{1}] {2,3}%  {3}/{4}  {5}/s  剩余 {6}" -f `
        $prefix, $bar, $pct, (Format-Size $sent), (Format-Size $total), (Format-Size $spd), (Format-Span $eta))
}

# 退路: gh 自己传。没有进度, 但它有自己的重试与分块逻辑, 且认 HTTPS_PROXY。
function Invoke-GhUpload ([string]$tagName, [string[]]$paths) {
    & gh release upload $tagName $paths --clobber | Out-Host
    return ($LASTEXITCODE -eq 0)
}

# 覆盖式上传一组资产。返回 $true 表示全部成功。
function Send-ReleaseAssets ([string]$tagName, [string[]]$paths) {
    $grand = ($paths | ForEach-Object { (Get-Item -LiteralPath $_).Length } | Measure-Object -Sum).Sum

    $uploadUrl = ((& gh release view $tagName --json uploadUrl -q .uploadUrl 2>$null) -join "").Trim()
    $token     = ((& gh auth token 2>$null) -join "").Trim()
    if (-not $uploadUrl -or -not $token) {
        Warn "拿不到上传地址或 gh token, 退回 gh release upload (无进度显示, 请耐心等)。"
        return (Invoke-GhUpload $tagName $paths)
    }
    # uploadUrl 形如 https://uploads.github.com/repos/O/R/releases/123/assets{?name,label}
    $uploadUrl = ($uploadUrl -split '\{')[0]

    $existing = @((& gh release view $tagName --json assets -q '.assets[].name' 2>$null) |
                  ForEach-Object { $_.Trim() } | Where-Object { $_ })

    Gray ("  共 {0} 个文件 / {1}" -f $paths.Count, (Format-Size $grand))
    $swAll = [System.Diagnostics.Stopwatch]::StartNew()
    $idx = 0
    foreach ($p in $paths) {
        $idx++
        $name   = Split-Path $p -Leaf
        $prefix = "  [{0}/{1}] {2}" -f $idx, $paths.Count, $name
        $url    = $uploadUrl + "?name=" + [uri]::EscapeDataString($name)

        $ok = $false
        for ($attempt = 1; $attempt -le 3; $attempt++) {
            # 同名资产存在就先删 —— GitHub 的上传端点没有 --clobber, 同名会直接 422。
            # 重试时也要删: 上一次失败可能已在服务端留下一个 state=starter 的半成品。
            if ($existing -contains $name -or $attempt -gt 1) {
                $delOut = (& gh release delete-asset $tagName $name -y 2>&1) -join "`n"
                # 删不掉(权限/已发布 Release)时后面 3 次会撞同一个 422, 重试等于空转 ——
                # 说出来, 别让它静默成"网络不好"。
                if ($LASTEXITCODE -ne 0 -and $delOut -notmatch 'not found|no such asset') {
                    Warn "  删除同名旧资产失败: $delOut"
                }
                $existing = @($existing | Where-Object { $_ -ne $name })
            }
            $r = Send-FileToUrl $url $token $p $prefix
            if ($r.Ok) { $ok = $true; break }
            Warn "  上传失败 (第 $attempt/3 次): $($r.Error)"
            if ($attempt -lt 3) { Gray "  10 秒后重试 ..."; Start-Sleep -Seconds 10 }
        }
        if (-not $ok) {
            # ⚠️ 此刻草稿上的同名旧资产【已被删掉】, 直接 return 会留下缺件的 Release。
            #    退回 gh 把整组重传一遍 —— 它的网络栈与这里不同(认 HTTPS_PROXY、自带
            #    重试), 这条新路径失败不代表 gh 也传不上去。
            Write-Host ""
            Warn "自建上传通道失败, 退回 gh release upload 重传全部资产 (无进度, 请耐心等)。"
            return (Invoke-GhUpload $tagName $paths)
        }
    }
    Gray ("  合计用时 {0}, 平均 {1}/s" -f (Format-Span $swAll.Elapsed.TotalSeconds),
                                        (Format-Size ($grand / [math]::Max($swAll.Elapsed.TotalSeconds, 0.001))))
    return $true
}

# ============================================================
# 等 CI: 轮询 release.yml 的 run 直到成功 / 失败 / 超时
# ============================================================
# tag 触发的 run, 其 headBranch 即 tag 名 —— 与 sign-draft 定位构建用的是同一条判据。
function Get-LatestReleaseRun ([string]$tagName) {
    $raw = (& gh run list --workflow release.yml --branch $tagName --limit 20 `
                --json databaseId,status,conclusion,createdAt,url 2>&1) -join "`n"
    if ($LASTEXITCODE -ne 0) { return [pscustomobject]@{ QueryError = $raw } }
    try { $runs = @($raw | ConvertFrom-Json) } catch { return [pscustomobject]@{ QueryError = $raw } }
    if ($runs.Count -eq 0) { return $null }
    return ($runs | Sort-Object { [datetime]$_.createdAt } -Descending)[0]
}

# 可中断的倒计时等待。按任意键立即结束等待 (提前查一次), Ctrl+C 照常中止整个脚本。
# $statusText 是当前 CI 状态, 与倒计时【画在同一行】—— 等待期间屏幕上只有这一行在动。
function Start-CountdownSleep ([int]$seconds, [string]$statusText) {
    if (-not $script:ProgressInline) { Start-Sleep -Seconds $seconds; return }
    $end = (Get-Date).AddSeconds($seconds)
    $canPeek = $true
    try { $canPeek = -not [Console]::IsInputRedirected } catch { $canPeek = $false }
    $lastShown = -1
    while ($true) {
        $left = [int][math]::Ceiling(($end - (Get-Date)).TotalSeconds)
        if ($left -le 0) { break }
        # 按键要跟手, 所以循环跑得密; 但只有秒数真的变了才重画 —— 一秒一帧就够看,
        # 再密只是在刷终端。
        if ($left -ne $lastShown) {
            $lastShown = $left
            Write-ProgressLine ("{0} · 下次查询 {1} [任意键]" -f $statusText, (Format-Span $left))
        }
        if ($canPeek) {
            try {
                if ([Console]::KeyAvailable) { [Console]::ReadKey($true) | Out-Null; break }
            } catch { $canPeek = $false }
        }
        Start-Sleep -Milliseconds 150
    }
}

# 返回 @{ Ok = $bool; Reason = ""; Run = <run 对象或 $null> }
#
# 输出策略: 等待期间屏幕上只有一行在原地刷新; 只有【阶段真的变了】(没排队→排队中→
# 构建中→结束) 才把当前状态定格成一行永久记录。20 分钟下来屏幕上就三四行, 而不是
# 每轮一行地往下堆。输出被重定向时(日志)反过来 —— 无处原地刷新, 每轮打一行才对。
#
# $notBefore: 只认这个时刻之后创建的 run。
#   ⚠️ 没有它就有一个静默的错误路径: `-Force` 重发同一个 tag 时, 推 tag 到新 run 出现
#      有几十秒窗口, 这期间 `gh run list --branch <tag>` 返回的是【上一轮那个已成功的
#      旧 run】。脚本会立刻判定"构建成功"直奔 sign-draft, 拉到上次构建的中转产物 ——
#      而版本号没变, dev.ps1 unstage 的版本号硬校验也拦不住, 于是把上一次的二进制签名
#      后覆盖上去, 全程无报错。
#   刚推完 tag 的调用方必须传它; 独立的 `auto-sign` 子命令传 $null (那里"最新的 run"
#   正是用户想等的那个)。
function Wait-ForCiBuild ([string]$tagName, [int]$timeoutMinutes, [int]$pollMinutes, $notBefore = $null) {
    $start     = Get-Date
    $deadline  = $start.AddMinutes($timeoutMinutes)
    $lastPhase = ""
    $warnedNoRun = $false

    while ($true) {
        $now = Get-Date
        $ts  = $now.ToString("HH:mm:ss")
        $run = Get-LatestReleaseRun $tagName

        # 早于基准时刻的 run 一律当作"还没排上队"继续等
        if ($notBefore -and $run -and -not $run.PSObject.Properties['QueryError'] -and
            [datetime]$run.createdAt -lt $notBefore) {
            $run = $null
        }

        if ($run -and $run.PSObject.Properties['QueryError']) {
            # 查询失败要留痕: 它可能是网络在断断续续, 事后需要看得见
            Close-ProgressLine ""
            Warn ("  [{0}] 查询 CI 失败, 稍后重试:" -f $ts)
            Gray "      $($run.QueryError)"
            $phase = "error"; $lastPhase = "error"   # 已经打过行了, 别再定格一遍
            $status = "  查询失败, 稍后重试"
            $wait   = 60
        } elseif (-not $run) {
            $phase  = "pending"
            $status = "  [{0}] CI 还没排上队 · 已等 {1}" -f $ts, (Format-Span ($now - $start).TotalSeconds)
            # 推 tag 到 run 出现通常几十秒; 超过 5 分钟还没有多半是 workflow 压根没被触发
            if (-not $warnedNoRun -and ($now - $start).TotalMinutes -ge 5) {
                $warnedNoRun = $true
                Close-ProgressLine ""
                Warn "  [!] 推 tag 已 5 分钟仍无 run, 确认一下 release.yml 是否被 tag 触发。"
                $lastPhase = ""      # 强制下面重新定格一行
            }
            $wait = 30
        } elseif ($run.status -ne "completed") {
            $phase = $run.status
            $age   = ((Get-Date) - [datetime]$run.createdAt).TotalMinutes
            $st    = switch ($run.status) {
                "queued"      { "排队中" }
                "in_progress" { "构建中" }
                "waiting"     { "等待审批" }
                default       { $run.status }
            }
            $status = "  [{0}] {1} run {2} · 已跑 {3} / 约 20 分钟" -f $ts, $st, $run.databaseId, (Format-Span ($age * 60))
            # 临近完成时收紧间隔: 5 分钟的粒度会白等最多 5 分钟墙钟
            $wait = if ($age -ge 15) { 60 } else { $pollMinutes * 60 }
        } elseif ($run.conclusion -eq "success") {
            Close-ProgressLine ""
            Say ("  [{0}] CI 构建成功  run {1}" -f $ts, $run.databaseId)
            return @{ Ok = $true; Reason = ""; Run = $run }
        } else {
            Close-ProgressLine ""
            return @{ Ok = $false; Reason = "CI 构建 $($run.conclusion)"; Run = $run }
        }

        # 阶段变了才定格一行; 同阶段内只让那一行原地跳数字
        if ($script:ProgressInline) {
            if ($phase -and $phase -ne $lastPhase) {
                Close-ProgressLine $status
                if ($run -and $run.url) { Gray "      $($run.url)" }
                $lastPhase = $phase
            }
        } else {
            Gray $status
        }

        # ⚠️ 判据是「现在越没越线」, 不是「睡完会不会越线」。用后者的话:
        #    默认值下实际只等 55 分钟却报"超时 (>60 分钟)"; 而 -PollMinutes 大于等于
        #    -TimeoutMinutes 时第一轮就直接返回超时, 一秒都没等。
        if ((Get-Date) -ge $deadline) {
            Close-ProgressLine ""
            return @{ Ok = $false; Reason = "等待超时 (>$timeoutMinutes 分钟)"; Run = $run }
        }
        # 最后一觉不要睡过 deadline
        $wait = [int][math]::Max(1, [math]::Min($wait, ($deadline - (Get-Date)).TotalSeconds))
        Start-CountdownSleep $wait $status
    }
}

# 等 CI → 自动接 sign-draft。中止/失败都不会留下半成品, 事后 sign-draft 可原样重跑。
function Invoke-AutoSign ([string]$tagName, [int]$timeoutMinutes, [int]$pollMinutes, $notBefore = $null) {
    if ($tagName -notmatch '^v') { $tagName = "v$tagName" }
    $version = $tagName -replace '^v', ''
    $devPs1  = Join-Path $ScriptDir "dev.ps1"

    Write-Host ""
    Cyan "============== 等待 CI 构建 $tagName =============="
    Gray "  中止后随时可手动接上:  .\scripts\release.ps1 sign-draft -Version $version"

    # 会话体检前置: 签名会话有效期 2 小时, CI 约 20 分钟 —— 现在开好, 到时候一定还在。
    # 这里【只提示不拦截】: 会话没开也照样等 CI, 反正还有 20 分钟可以去开。
    Write-Host ""
    & $devPs1 sign-status | Out-Host
    if ($LASTEXITCODE -ne 0) {
        Write-Host ""
        Warn "[!] 签名会话当前不可用 —— 请在 CI 跑完前 (约 20 分钟内) 建立会话。"
        Gray "    会话有效期 2 小时, 现在建立即可覆盖整个等待窗口。"
        Gray "    CI 跑完后脚本会再体检一次; 那时仍不可用才会停下来。"
    }

    $r = Wait-ForCiBuild $tagName $timeoutMinutes $pollMinutes $notBefore
    if (-not $r.Ok) {
        Write-Host ""
        ErrMsg "自动签名中止: $($r.Reason)"
        if ($r.Run) { Gray "  run: $($r.Run.url)" }
        Gray "  tag 已推送, 发布本身没有受影响。修好 CI 后:"
        Gray "    gh run rerun <run-id>            # 重跑构建"
        Gray "    .\scripts\release.ps1 sign-draft -Version $version"
        return 1
    }

    $script:AutoYes = $true
    try { return (Invoke-SignDraft $tagName) }
    finally { $script:AutoYes = $false }
}

function Invoke-SignDraft ([string]$tagName) {
    $mainPath = Join-Path $WorkRoot $MainRepo
    $devPs1   = Join-Path $ScriptDir "dev.ps1"
    $distDir  = Join-Path $ProductRoot "dist"

    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
        ErrMsg "未找到 gh (GitHub CLI) —— 本命令靠它拉 CI 产物、改 Release。"
        Gray "  winget install GitHub.cli   然后  gh auth login"
        return 1
    }

    Push-Location $mainPath
    try {
        # ---------- 1. 目标 tag ----------
        if (-not $tagName) {
            Gray "正在查询草稿 Release ..."
            $raw = (& gh release list --limit 20 --json tagName,isDraft 2>&1) -join "`n"
            if ($LASTEXITCODE -ne 0) { ErrMsg "gh release list 失败:`n$raw"; return 1 }
            $drafts = @(($raw | ConvertFrom-Json) | Where-Object { $_.isDraft })
            if ($drafts.Count -eq 0) {
                ErrMsg "没有草稿 Release。先 release.ps1 patch/minor 打 tag, 等 CI 跑完再来。"
                return 1
            }
            $tagName = $drafts[0].tagName
            if ($drafts.Count -gt 1) {
                Warn "有 $($drafts.Count) 个草稿 Release, 取最新的 $tagName"
                Gray "  指定其它: release.ps1 sign-draft -Version <x.y.z>"
            }
        }
        if ($tagName -notmatch '^v') { $tagName = "v$tagName" }
        $version = $tagName -replace '^v', ''

        # -Version 指定的 tag 未必是草稿。覆盖【已发布】Release 的资产会让已下载用户的
        # sha256 对不上, 且 R2 同步(release-published.yml)早按旧文件跑过 —— 那边的
        # latest.json 也已指向旧 hash。故默认拒绝, 要覆盖必须显式 -Force。
        $relRaw = (& gh release view $tagName --json isDraft 2>&1) -join "`n"
        if ($LASTEXITCODE -ne 0) { ErrMsg "找不到 Release ${tagName}:`n$relRaw"; return 1 }
        if (-not ($relRaw | ConvertFrom-Json).isDraft) {
            ErrMsg "$tagName 已经发布, 不是草稿。"
            Gray "  覆盖已发布 Release 的资产会让已下载用户的 sha256 对不上,"
            Gray "  且 R2 上的 latest.json 早已指向旧文件的 hash。"
            if (-not $Force) { Gray "  确要覆盖请加 -Force。"; return 1 }
            Warn "  -Force: 继续覆盖已发布的 $tagName"
        }

        Write-Host ""
        Cyan "============== 签名并回传 $tagName =============="

        # ---------- 2. 签名会话预检 ----------
        # 放在下载之前: 拉完 26MB 才发现会话没开, 是最没必要的等待。
        Write-Host ""
        & $devPs1 sign-status | Out-Host
        $signReady = ($LASTEXITCODE -eq 0)
        # ⚠️ 这道闸门问的是「客观上能不能签」, 不是「要不要征求同意」——
        #    自动签名模式下也【不能】跳过, 否则等 20 分钟只为撞上一个必然失败的 sign。
        #    会话没开时给人补开的机会 (ignoreAuto: 强制真人回答, 免得自动确认转成死循环)。
        if (-not $signReady -and $script:AutoYes) {
            # 无人值守: 人不在场, 问了也没人答 —— `Read-Host` 会把脚本无限期挂住,
            # 正好背叛 -AutoSign 承诺的"全程无人值守"。改成有界轮询, 到点认输。
            $graceMin = 10
            Write-Host ""
            Warn "[!] 签名会话不可用。CI 已跑完, 最多再等 $graceMin 分钟让你建立会话。"
            Gray "    建立后无需操作, 脚本会自己发现。"
            $graceEnd = (Get-Date).AddMinutes($graceMin)
            while (-not $signReady -and (Get-Date) -lt $graceEnd) {
                Start-CountdownSleep 30 ("  等待签名会话 · 剩余 " + (Format-Span ($graceEnd - (Get-Date)).TotalSeconds))
                & $devPs1 sign-status | Out-Null
                $signReady = ($LASTEXITCODE -eq 0)
            }
            Close-ProgressLine ""
            if (-not $signReady) {
                ErrMsg "$graceMin 分钟内签名会话仍不可用, 停在这里。"
                Gray "  tag 与草稿 Release 都还在, 建立会话后接着跑:"
                Gray "    .\scripts\release.ps1 sign-draft -Version $version"
                return 1
            }
            Say "签名会话已就绪。"
        }
        $retry = 0
        while (-not $signReady) {
            Write-Host ""
            Warn "[!] 签名会话不可用 —— 打开 SimplySign 建立会话后再继续 (有效期 2 小时)。"
            $retry++
            if ($retry -gt 5) { ErrMsg "会话仍不可用, 已放弃。"; return 1 }
            if (-not (Confirm-Step "    已建立会话, 重新体检?" $true $true)) {
                Gray "已取消 —— tag 与草稿 Release 都还在, 事后 sign-draft 可重跑。"
                return 1
            }
            & $devPs1 sign-status | Out-Host
            $signReady = ($LASTEXITCODE -eq 0)
        }
        Write-Host ""
        if (-not (Confirm-Step "签名会话已就绪, 继续?" $true)) { Gray "已取消。"; return 0 }

        # ---------- 3. 拉中转产物 ----------
        # tag 触发的 run, 其 headBranch 即 tag 名。
        Gray "`n正在定位 CI 构建 ..."
        $raw = (& gh run list --workflow release.yml --branch $tagName --limit 10 `
                    --json databaseId,conclusion,createdAt 2>&1) -join "`n"
        if ($LASTEXITCODE -ne 0) { ErrMsg "gh run list 失败:`n$raw"; return 1 }
        $runs = @(($raw | ConvertFrom-Json) | Where-Object { $_.conclusion -eq "success" })
        if ($runs.Count -eq 0) {
            ErrMsg "$tagName 没有成功的 release.yml 构建。"
            Gray "  查看: gh run list --workflow release.yml --branch $tagName"
            return 1
        }
        $runId = $runs[0].databaseId

        $tmp = Join-Path $distDir ".stage-download"
        if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
        New-Item -ItemType Directory -Path $tmp -Force | Out-Null
        Say "拉取中转产物 (run $runId) ..."
        & gh run download $runId --name stage-windows --dir $tmp | Out-Host
        if ($LASTEXITCODE -ne 0) {
            ErrMsg "下载 stage-windows 失败。artifact 保留期 14 天, 过期需重跑 CI。"
            return 1
        }
        $stageZip = Get-ChildItem (Join-Path $tmp "WindInput-Stage-*.zip") -ErrorAction SilentlyContinue |
                    Select-Object -First 1
        if (-not $stageZip) { ErrMsg "下载结果里没有中转产物包: $tmp"; return 1 }

        # ---------- 4. 对齐版本号并还原 ----------
        # tag 是版本真源。先对齐 docs\VERSION, 免得被 unstage 自己的守门员拦下 ——
        # 那道校验防的是「中转产物与打包用的版本对不上」, 对齐后它依然有效
        # (拉错 run 时中转产物版本仍会与 tag 不符而被拦)。
        Sync-LocalVersionFile $version

        & $devPs1 unstage $stageZip.FullName | Out-Host
        if ($LASTEXITCODE -ne 0) { ErrMsg "还原中转产物失败"; return 1 }
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue

        # ---------- 5. 签名 + 重新打包 ----------
        Write-Host ""
        Say "==> dev.ps1 sign 8s   (签 5 个 PE → 打包 → 签外壳 → 出 manifest)"
        & $devPs1 sign 8s | Out-Host
        if ($LASTEXITCODE -ne 0) { ErrMsg "出安装包失败"; return 1 }

        Write-Host ""
        Say "==> dev.ps1 sign 9s   (打便携包; 包内 PE 已签, 按指纹跳过, 不再扣配额)"
        & $devPs1 sign 9s | Out-Host
        if ($LASTEXITCODE -ne 0) { ErrMsg "出便携包失败"; return 1 }

        # ---------- 6. 硬校验 ----------
        Write-Host ""
        Say "==> dev.ps1 verify-sign"
        & $devPs1 verify-sign | Out-Host
        if ($LASTEXITCODE -ne 0) { ErrMsg "验签未通过, 不上传。"; return 1 }

        # ---------- 7. 覆盖草稿资产 ----------
        $assets = @(
            "WindInput-Setup-$version.exe"
            "WindInput-Setup-$version.exe.sha256"
            "WindInput-Portable-$version.zip"
            "WindInput-Portable-$version.zip.sha256"
        ) | ForEach-Object { Join-Path $distDir $_ }
        $absent = @($assets | Where-Object { -not (Test-Path $_) })
        if ($absent.Count -gt 0) {
            ErrMsg "以下产物不存在, 无法上传:"
            $absent | ForEach-Object { ErrMsg "  $_" }
            return 1
        }

        Write-Host ""
        Gray "将覆盖 $tagName 的 4 个资产:"
        foreach ($a in $assets) {
            Gray ("  {0}  ({1}MB)" -f (Split-Path $a -Leaf), [math]::Round((Get-Item $a).Length / 1MB, 1))
        }
        if (-not (Confirm-Step "上传?" $true)) { Gray "已取消 (签名产物留在 dist\)。"; return 0 }

        Write-Host ""
        if (-not (Send-ReleaseAssets $tagName $assets)) {
            # 已签名的产物就在 dist\, 补传不需要重签 —— 重跑 sign-draft 会重新拉产物、
            # 重新签一遍(白扣云签名配额), 这里给出直接补传的命令。
            ErrMsg "上传失败。签名产物已在 dist\, 修好网络后直接补传即可:"
            Gray   "  gh release upload $tagName ``"
            foreach ($a in $assets) { Gray ("      `"{0}`" ``" -f $a) }
            Gray   "      --clobber"
            return 1
        }
        Say "已上传 $($assets.Count) 个签名产物。"

        # ---------- 8. 摘掉未签名横幅 ----------
        $body = (& gh release view $tagName --json body -q .body 2>&1) -join "`n"
        if ($LASTEXITCODE -ne 0) {
            Warn "读取 Release 正文失败, 请手动删掉未签名横幅后再发布。"
            return 0
        }
        $cleaned = Remove-UnsignedBanner $body
        if ($null -eq $cleaned) {
            Gray "正文里没有未签名横幅 (已删过, 或本次 CI 判定为已签名)。"
        } else {
            $nf = Join-Path ([System.IO.Path]::GetTempPath()) "windinput-notes-$version.md"
            [System.IO.File]::WriteAllText($nf, $cleaned, (New-Object System.Text.UTF8Encoding($false)))
            & gh release edit $tagName --notes-file $nf | Out-Host
            if ($LASTEXITCODE -ne 0) { Warn "更新正文失败, 请手动删掉未签名横幅。" }
            else { Say "已删除未签名横幅。" }
            Remove-Item $nf -Force -ErrorAction SilentlyContinue
        }

        Write-Host ""
        Cyan "============== 完成 =============="
        $url = (& gh release view $tagName --json url -q .url 2>$null)
        if ($url) { Gray "  $url" }
        Gray "  人工过目 Release Notes 后点 Publish —— 发布会触发 R2 同步与文档仓更新。"
        return 0
    } finally {
        Pop-Location
    }
}

# ============================================================
# status: 只看本地状态, 不联网
# ============================================================
function Show-Status ([string]$branch) {
    Write-Host ""
    Cyan "==================== 本地状态 (未联网) ===================="
    Write-Host ""
    Write-Host ("  {0,-16} {1,-12} {2,-10} {3,-8} {4}" -f "仓库", "分支", "HEAD", "脏文件", "最新本地 tag")
    Write-Host ("  " + ("-" * 74)) -ForegroundColor DarkGray
    foreach ($r in $Repos) {
        $path = Join-Path $WorkRoot $r.Name
        if (-not (Test-Path (Join-Path $path ".git"))) {
            Write-Host ("  {0,-16} " -f $r.Name) -NoNewline; ErrMsg "缺失"
            continue
        }
        $cur = Get-GitValue $path @("symbolic-ref", "--short", "-q", "HEAD")
        if (-not $cur) { $cur = "(游离 HEAD)" }
        $short = Get-GitValue $path @("rev-parse", "--short", "HEAD")
        $dirty = @(Get-GitValue $path @("status", "--porcelain") -split "`n" | Where-Object { $_.Trim() }).Count
        $lt = if ($r.Tag) { $t = Get-LatestTag $path -LocalOnly; if ($t) { "v$($t.Raw)" } else { "-" } } else { "(不打 tag)" }
        $line = "  {0,-16} {1,-12} {2,-10} {3,-8} {4}" -f $r.Name, $cur, $short, $dirty, $lt
        if ($cur -ne $branch -or $dirty -gt 0) { Warn $line } else { Write-Host $line }
    }
    Write-Host ""
    $fileVer = if (Test-Path $VersionFile) { (Get-Content $VersionFile -Raw).Trim() } else { "?" }
    Gray "  docs\VERSION = $fileVer   (本地构建占位; CI 以 tag 为版本真源)"
    Gray "  黄色行 = 不在目标分支 '$branch' 或工作区不干净"
    Cyan "==========================================================="
}

# ============================================================
# 交互菜单
# ============================================================
function Show-Menu ([string]$branch) {
    $mainPath = Join-Path $WorkRoot $MainRepo
    if (-not (Test-Path (Join-Path $mainPath ".git"))) { ErrMsg "找不到主仓: $mainPath"; return 1 }

    Write-Host ""
    Gray "正在查询远端 tag ..."
    $latest = Get-LatestTag $mainPath
    $fileVer = if (Test-Path $VersionFile) { (Get-Content $VersionFile -Raw).Trim() } else { "" }

    # bump 基准: 以远端最新 tag 为准 (tag 是版本真源)。
    # 若本地 docs/VERSION 更大, 说明手工提前 bump 过, 取较大者以免回退。
    $base = $latest
    $fileSv = ConvertTo-SemVer $fileVer
    if ($fileSv -and (-not $base -or (Compare-SemVer $fileSv $base) -gt 0)) { $base = $fileSv }
    if (-not $base) { $base = ConvertTo-SemVer "0.0.0" }

    $ahead = [int](Get-GitValue $mainPath @("rev-list", "--count", "origin/$branch..HEAD"))

    Write-Host ""
    Cyan "==================== WindInput 发布 ===================="
    Write-Host "  最新已发布 tag : " -NoNewline
    if ($latest) { Say "v$($latest.Raw)" } else { Warn "(无)" }
    Write-Host "  docs\VERSION   : $fileVer" -NoNewline; Gray "   (本地构建占位, 非版本真源)"
    Write-Host "  主仓待推提交   : $ahead 个"
    Cyan "========================================================"
    Write-Host ""
    Write-Host "  [1] 检查        " -NoNewline -ForegroundColor White
    Gray "完整预检 + push --dry-run, 不做任何改动"
    Write-Host "  [2] 发布 Patch  " -NoNewline -ForegroundColor White
    Say ("v{0}  →  v{1}" -f $base.Raw, (Step-Version $base "patch"))
    Write-Host "  [3] 发布 Minor  " -NoNewline -ForegroundColor White
    Say ("v{0}  →  v{1}" -f $base.Raw, (Step-Version $base "minor"))
    Write-Host "  [4] 重发当前版  " -NoNewline -ForegroundColor White
    Write-Host ("v{0}" -f $base.Raw) -NoNewline
    if ($latest -and (Compare-SemVer $base $latest) -eq 0) { Warn "  (远端已存在, 将强制覆盖)" } else { Gray "  (远端尚无此 tag)" }
    Write-Host "  [5] 查看状态    " -NoNewline -ForegroundColor White
    Gray "各仓库分支 / HEAD / 脏文件, 不联网"
    Write-Host "  [6] 只推送      " -NoNewline -ForegroundColor White
    Gray "五仓同步 push, 不打 tag (日常同步用)"
    Write-Host "  [7] 签名回传    " -NoNewline -ForegroundColor White
    Gray "CI 已跑完: 拉产物 → 本机签名打包 → 覆盖草稿 Release (需签名会话)"
    Write-Host "  [8] 等CI+签名   " -NoNewline -ForegroundColor White
    Gray "tag 已推出: 守着 CI 跑完 (约 20 分钟), 再自动执行 [7]"
    Write-Host "  [q] 退出" -ForegroundColor White
    Write-Host ""

    # 菜单要求交互式终端; 非交互环境 (CI / 管道) 请改用子命令
    try {
        $choice = (Read-Host "请选择").Trim().ToLower()
    } catch {
        Write-Host ""
        ErrMsg "当前不是交互式终端, 无法显示菜单。"
        Gray "  请改用子命令: release.ps1 check|patch|minor|current|status|push|sign-draft|auto-sign  (可加 -Yes)"
        return 1
    }
    switch ($choice) {
        # [1] 用下一个 patch 版本演练 (拿已发布的版本号演练会必然撞 tag 冲突)
        "1" { $v = Step-Version $base "patch"; return Invoke-Release "v$v" $branch "Release v$v" $false $true }
        "2" { $v = Step-Version $base "patch"; return Invoke-Release "v$v" $branch "Release v$v" $false $false }
        "3" { $v = Step-Version $base "minor"; return Invoke-Release "v$v" $branch "Release v$v" $false $false }
        "4" {
            if ($latest -and (Compare-SemVer $base $latest) -eq 0) {
                Warn ""
                Warn "远端已存在 v$($base.Raw), 重发需要强制覆盖该 tag。"
                if (-not (Confirm-Step "确认以 -Force 模式继续?" $false)) { Gray "已取消。"; return 0 }
                $script:Force = $true
            }
            return Invoke-Release ("v" + $base.Raw) $branch "Release v$($base.Raw)" $false $false
        }
        "5" { Show-Status $branch; return 0 }
        "6" { return Invoke-Release "" $branch "" $true $false }
        "7" { return Invoke-SignDraft "" }
        # [8] 守的是【最新已发布 tag】—— 菜单场景下它就是刚推出去的那个
        "8" {
            if (-not $latest) { ErrMsg "远端没有 v* tag, 没有可等的构建。"; return 1 }
            return Invoke-AutoSign ("v" + $latest.Raw) $TimeoutMinutes $PollMinutes
        }
        "q" { Gray "已退出。"; return 0 }
        default { ErrMsg "无效选择: $choice"; return 1 }
    }
}

# ============================================================
# 入口分发
# ============================================================
if (-not $Branch) { $Branch = Get-ManifestBranch }

# sign-draft 不打 tag、不推送, 与发布流程正交; 且 -Version 在这里的语义是「签哪个草稿」
# 而不是「发布哪个版本」—— 故必须排在下面的 -Version 分支之前, 否则
# `release.ps1 sign-draft -Version 0.120.2` 会被当成"发布 0.120.2"直接打 tag。
if ($Command -eq "sign-draft") { exit (Invoke-SignDraft $Version) }

# auto-sign 同理: -Version 在这里是「守哪个 tag」, 不是「发布哪个版本」。
# 缺省守远端最新 tag —— 刚 release.ps1 patch 推出去的那个。
if ($Command -eq "auto-sign") {
    $t = $Version
    if (-not $t) {
        Gray "正在查询远端最新 tag ..."
        $lt = Get-LatestTag (Join-Path $WorkRoot $MainRepo)
        if (-not $lt) { ErrMsg "远端没有 v* tag, 没有可等的构建。"; exit 1 }
        $t = $lt.Raw
        Gray "  守: v$t  (指定其它: release.ps1 auto-sign -Version <x.y.z>)"
    }
    exit (Invoke-AutoSign $t $TimeoutMinutes $PollMinutes)
}

# -Version 优先于子命令的 bump 规则
if ($Version) {
    $sv = ConvertTo-SemVer $Version
    if (-not $sv) { ErrMsg "版本号格式不合法: '$Version' (期望 x.y.z 或 x.y.z-suffix)"; exit 1 }
    $tag = "v$($sv.Raw)"
    if (-not $Message) { $Message = "Release $tag" }
    exit (Invoke-Release $tag $Branch $Message $false ([bool]$DryRun))
}

switch ($Command) {
    "status" { Show-Status $Branch; exit 0 }
    "push"   { exit (Invoke-Release "" $Branch "" $true ([bool]$DryRun)) }
    { $_ -in @("", "menu") } { exit (Show-Menu $Branch) }
}

# 以下子命令需要先确定基准版本 (远端最新 tag)
$mainPath = Join-Path $WorkRoot $MainRepo
Gray "正在查询远端 tag ..."
$latest = Get-LatestTag $mainPath
$fileSv = if (Test-Path $VersionFile) { ConvertTo-SemVer ((Get-Content $VersionFile -Raw).Trim()) } else { $null }
$base = $latest
if ($fileSv -and (-not $base -or (Compare-SemVer $fileSv $base) -gt 0)) { $base = $fileSv }
if (-not $base) { ErrMsg "无法确定基准版本: 远端无 v* tag, docs\VERSION 也不可用"; exit 1 }

$newVer = switch ($Command) {
    # check 只做健康检查, 用下一个 patch 版本作演练目标 —— 拿已发布的版本号演练
    # 会必然撞 tag 冲突, 没有意义。
    "check"   { Step-Version $base "patch" }
    "current" { $base.Raw }
    "patch"   { Step-Version $base "patch" }
    "minor"   { Step-Version $base "minor" }
}
$tag = "v$newVer"
if (-not $Message) { $Message = "Release $tag" }
$isDry = ($Command -eq "check") -or $DryRun

exit (Invoke-Release $tag $Branch $Message $false $isDry)
