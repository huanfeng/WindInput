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
#   .\scripts\release.ps1 -Version 0.111.0-beta1   # 指定任意版本号发布
#
# 开关: -DryRun 只演练 / -Force 覆盖同名 tag / -Yes 非交互 / -Branch 指定分支
#
# 注意:
#   - 所有仓库必须【处于目标分支上】。repo sync 后会停在游离 HEAD, 此时脚本中断
#     并提示切换命令 —— 游离状态下推送等同盲推, 不适合发布。
#   - 本脚本【不做 commit】。工作区有未提交改动时逐仓提示, 由你决定是否继续。

param(
    # 子命令; 留空进交互菜单
    [Parameter(Position = 0)]
    [ValidateSet("", "menu", "check", "patch", "minor", "current", "status", "push", "sign-draft")]
    [string]$Command = "",
    # 指定发布版本号 (不含 v 前缀), 给定时忽略子命令的 bump 规则
    [string]$Version,
    # annotated tag 说明; 缺省 "Release v<版本号>"
    [string]$Message,
    # 目标分支; 缺省从 repo manifest 读取
    [string]$Branch,
    # 只演练: push 走 --dry-run, 不打 tag、不写文件
    [switch]$DryRun,
    # 覆盖已存在的同名 tag (本地 -f + 远端 --force)
    [switch]$Force,
    # 非交互: 所有确认自动通过 (预检硬失败仍中止)
    [switch]$Yes
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
function Confirm-Step ([string]$prompt, [bool]$defaultYes = $false) {
    if ($Yes) { Gray "$prompt  → (-Yes) 自动确认"; return $true }
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
        $st.RemoteHasTag = [bool](Get-GitValue $path @("ls-remote", "--tags", "origin", "refs/tags/$tag"))

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
        if ($needTag -and $st.Tag) { $ops += $(if ($st.RemoteHasTag) { "覆盖 $tag" } else { "打 $tag" }) }
        Write-Host ("  {0,-16} {1,-10} {2,-6} {3,-6} {4}" -f `
            $st.Name, $st.HeadShort, $st.Ahead, $st.Dirty.Count, ($ops -join " + "))
    }
    if ($needTag) {
        Write-Host ""
        Gray "  $MainRepo 排最后: 推它的 tag 会触发 release.yml, 届时 CI 会拉取各附属仓库的"
        Gray "  $branch 最新提交参与构建, 故附属仓库必须先到位。"
    }

    if ($Force -and ($states | Where-Object { $_.RemoteHasTag })) {
        Write-Host ""
        Warn "[!!] -Force 将强制覆盖远端已存在的 tag。若他人已拉取该 tag, 会造成引用不一致。"
        if (-not (Confirm-Step "     确认强制覆盖?" $false)) { ErrMsg "已取消。"; return 1 }
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

            if ($needTag -and $st.Tag -and -not $dryRun) {
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
        Gray "CI: 推送 $tag 已触发 release.yml, 去 GitHub Actions 查看构建与草稿 Release。"
    }
    Cyan "=============================================================="
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
#   5. gh release upload --clobber 覆盖草稿里的 4 个资产
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

        & gh release upload $tagName $assets --clobber | Out-Host
        if ($LASTEXITCODE -ne 0) { ErrMsg "上传失败"; return 1 }
        Say "已上传 4 个签名产物。"

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
    Gray "拉 CI 产物 → 本机签名打包 → 覆盖草稿 Release (需签名会话)"
    Write-Host "  [q] 退出" -ForegroundColor White
    Write-Host ""

    # 菜单要求交互式终端; 非交互环境 (CI / 管道) 请改用子命令
    try {
        $choice = (Read-Host "请选择").Trim().ToLower()
    } catch {
        Write-Host ""
        ErrMsg "当前不是交互式终端, 无法显示菜单。"
        Gray "  请改用子命令: release.ps1 check|patch|minor|current|status|push  (可加 -Yes)"
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
