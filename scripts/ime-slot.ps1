# ime-slot.ps1 —— 查「哪个构建正占着输入法的 CLSID 槽位」。
#
# 为什么需要它: TSF 输入法必须在全局注册表占一个 COM 类才能被系统加载, 而本产品只有
# 两个变体 (wind-portable\src\variant.rs 的 `is_dev ? WindInputDev : WindInput`), 即
# **全机只有两个槽位**。便携部署看着像沙箱, 但它走的同样是 regsvr32 + InstallLayoutOrTip
# (wind-portable\src\registration.rs), 隔离的只是文件位置 / userdata / 自启项, **不隔离
# CLSID** —— 谁后部署谁占用, 且会顶掉同变体的系统安装。
#
# 多个 worktree 同时开发时, dev 槽位是公共资源, 需交替使用。本脚本不做锁也不做仲裁
# (单人本地场景, 简单的手动逃生口胜过自动机制), 只回答一个问题: **现在是谁占着**。
#
# ★ 判据取自注册表而非我们自己维护的标记文件: 那是系统实际加载 DLL 时读的同一个键,
#   不存在"记的账与实际状态不同步"这种失效模式。
#
# 用法: pwsh -File scripts\ime-slot.ps1

$ErrorActionPreference = "Stop"

$variants = @(
    @{ Name = "release"; Clsid = "{99C2EE30-5C57-45A2-9C63-FB54B34FD90A}"; Note = "日常在用 + 对比验证, 不要占用" },
    @{ Name = "dev    "; Clsid = "{99C2DEB0-5C57-45A2-9C63-FB54B34FD90A}"; Note = "公共测试槽, 各 worktree 交替使用" }
)

function Get-Inproc ([string]$clsid, [bool]$wow) {
    $node = if ($wow) { "WOW6432Node\CLSID" } else { "CLSID" }
    $p = "HKLM:\SOFTWARE\Classes\$node\$clsid\InprocServer32"
    if (Test-Path $p) { (Get-ItemProperty $p).'(default)' } else { $null }
}

# DLL 路径 → 人话。便携目录名各 worktree 自取, 故只能按前缀粗分。
function Describe ([string]$dll) {
    if (-not $dll) { return "(未注册)" }
    $dir = Split-Path $dll -Parent
    if ($dir -like "C:\Program Files\*") { return "$dir  [系统安装]" }
    return "$dir  [便携]"
}

Write-Host "`n输入法 CLSID 槽位占用情况" -ForegroundColor Cyan
Write-Host ("=" * 68)
foreach ($v in $variants) {
    $x64 = Get-Inproc $v.Clsid $false
    $x86 = Get-Inproc $v.Clsid $true
    Write-Host ("  {0}  {1}" -f $v.Name, (Describe $x64))
    # x86 与 x64 分开注册, 只换一半会让 32 位宿主用着旧 DLL —— 那是最难查的一类不一致。
    if ($x64 -and $x86 -and (Split-Path $x64 -Parent) -ne (Split-Path $x86 -Parent)) {
        Write-Host ("           ⚠ x86 指向别处: {0}" -f (Split-Path $x86 -Parent)) -ForegroundColor Red
    }
    Write-Host ("           {0}" -f $v.Note) -ForegroundColor DarkGray
}
Write-Host ("=" * 68)
Write-Host @"
占用 dev 槽 : 本 worktree 里跑 dev.ps1 的 pbd1 (便携部署 dev, userdata 独立)
归还        : ubd (便携卸载) 或从占用方仓库重新部署
⚠ 部署后务必确认二进制确实是本 worktree 的产物 —— 路径对不代表内容新,
   构建失败时部署链仍会报"完成"(见 AGENTS.md 与 reference_windows_build_dev_ps1)
"@ -ForegroundColor DarkGray
