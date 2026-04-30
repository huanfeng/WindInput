# lint_agents_md.ps1
#
# 扫描仓库内所有 AGENTS.md（排除 .claude/ 副本和 node_modules/build/ 等产物目录），
# 校验其中的 Markdown 链接 [text](path) 与代码内/反引号包围的相对路径引用是否真实存在。
# 输出悬空引用列表（[MISSING] 标记），便于在 PR 前修复。
#
# 用法：
#   pwsh ./scripts/lint_agents_md.ps1
#   pwsh ./scripts/lint_agents_md.ps1 -Verbose   # 同时打印 OK 项
#
# 退出码：
#   0 - 所有引用都有效
#   1 - 发现悬空引用

[CmdletBinding()]
param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
)

$ErrorActionPreference = 'Stop'

# 仓库内排除的目录前缀（相对仓库根，使用正斜杠）
$ExcludeDirs = @(
    '.claude/',
    '.git/',
    'node_modules/',
    'build/',
    'wind_tsf/build/',
    'wind_tsf/build_debug/',
    'wind_tsf/build_x86/',
    'wind_tsf/build_debug_x86/',
    'wind_setting/build/',
    'wind_setting/frontend/dist/',
    'wind_setting/frontend/node_modules/'
)

function Should-Exclude($relPath) {
    $norm = $relPath -replace '\\', '/'
    foreach ($p in $ExcludeDirs) {
        if ($norm.StartsWith($p)) { return $true }
    }
    return $false
}

# 找到所有 AGENTS.md
$agentsFiles = Get-ChildItem -Path $RepoRoot -Recurse -Filter 'AGENTS.md' -File |
    Where-Object {
        $rel = [System.IO.Path]::GetRelativePath($RepoRoot, $_.FullName)
        -not (Should-Exclude $rel)
    }

Write-Host "Scanning $($agentsFiles.Count) AGENTS.md files under $RepoRoot" -ForegroundColor Cyan

# 正则：[text](relative/path) — 跳过 http(s):// 和 #anchor
$reMdLink = [regex]'\[(?<text>[^\]]+)\]\((?<url>[^)\s]+)\)'

# 反引号包围的疑似仓库内相对路径：必须以 ./ 或 ../ 开头，或显式包含目录分隔且带扩展名
# 故意排除"裸包路径"如 `internal/dict`、`pkg/theme`、`encoding/json` —— 这些可能是 Go 包导入路径
# 而非文件引用，启发式无法准确判断，应通过 Markdown 链接显式标注
$reBacktickPath = [regex]'`(?<path>(?:\./|\.\./)[A-Za-z0-9_./\-]+)`'

$totalRefs = 0
$missing = @()

foreach ($file in $agentsFiles) {
    $rel = [System.IO.Path]::GetRelativePath($RepoRoot, $file.FullName)
    $dir = Split-Path $file.FullName -Parent
    $content = Get-Content $file.FullName -Raw

    # Markdown 链接
    foreach ($m in $reMdLink.Matches($content)) {
        $url = $m.Groups['url'].Value
        if ($url -match '^(https?:|mailto:|#)') { continue }
        # 去除 #anchor 段
        $cleanUrl = ($url -split '#', 2)[0]
        if ([string]::IsNullOrWhiteSpace($cleanUrl)) { continue }
        # 绝对路径（以 / 开头）= 相对仓库根
        if ($cleanUrl.StartsWith('/')) {
            $target = Join-Path $RepoRoot ($cleanUrl.TrimStart('/'))
        } else {
            $target = Join-Path $dir $cleanUrl
        }
        $totalRefs++
        if (-not (Test-Path -LiteralPath $target)) {
            $missing += [PSCustomObject]@{
                File = $rel
                Kind = 'mdlink'
                Ref  = $url
            }
        } elseif ($VerbosePreference -eq 'Continue') {
            Write-Verbose "[OK] $rel -> $url"
        }
    }

    # 反引号路径（启发式校验：仅检查含 / 或 . 的，避免命中代码 token）
    foreach ($m in $reBacktickPath.Matches($content)) {
        $p = $m.Groups['path'].Value
        # 跳过明显是代码符号或包名的情况
        if ($p -match '^(go|pnpm|npm|cmake|wails|github\.com|golang\.org|gopkg\.in|@\w+|\w+\.\w+\(\))') { continue }
        # 必须含 / 或落在仓库内的扩展名才检
        if (-not ($p.Contains('/'))) { continue }
        # 跳过 URL 片段
        if ($p -match '^https?:') { continue }

        # 解析：先尝试相对当前目录
        $candidates = @(
            (Join-Path $dir $p),
            (Join-Path $RepoRoot $p)
        )
        $found = $false
        foreach ($c in $candidates) {
            if (Test-Path -LiteralPath $c) { $found = $true; break }
        }
        $totalRefs++
        if (-not $found) {
            $missing += [PSCustomObject]@{
                File = $rel
                Kind = 'backtick'
                Ref  = $p
            }
        }
    }
}

Write-Host ""
Write-Host "Total references checked: $totalRefs" -ForegroundColor Cyan
Write-Host "Missing references:        $($missing.Count)" -ForegroundColor $(if ($missing.Count -eq 0) { 'Green' } else { 'Yellow' })
Write-Host ""

if ($missing.Count -gt 0) {
    $missing | Sort-Object File, Ref | Format-Table -AutoSize
    Write-Host ""
    Write-Host "Note: 'backtick' kind is heuristic and may have false positives (e.g. TS module imports without extension, wailsjs generated paths, runtime build artifacts)." -ForegroundColor DarkGray
    Write-Host "      Review manually before fixing. 'mdlink' kind should be 100% real." -ForegroundColor DarkGray
    exit 1
}

Write-Host "All references look valid." -ForegroundColor Green
exit 0
