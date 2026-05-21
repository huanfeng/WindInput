# 实时探测当前前台窗口的 IME 中英文模式（外部 / IMM32 视角）。
#
# 目的：模拟 KBLSwitch 这类第三方工具的探测路径，验证我们 IME 的中英文状态
# 是否能被外部进程读到。
#
# 用法:
#   pwsh scripts/probe_ime_mode.ps1                # 默认 200ms 轮询
#   pwsh scripts/probe_ime_mode.ps1 -IntervalMs 100
#
# 原理:
#   ImmGetDefaultIMEWnd(前台窗口) -> 拿到 IMM32 default IME 窗口句柄
#   SendMessageTimeout(WM_IME_CONTROL, IMC_GETCONVERSIONMODE) 跨线程查询
#   IME_CMODE_NATIVE 位 = 1 即中文模式
#
# 输出列含义:
#   Mode    CN/EN/OFF/NO-IMEWND（NO-IMEWND 表示 IMM32 桥不可用，几乎可断定
#           前台是 TSF-only 客户端，如 Win11 新版记事本 / Edge / 部分 UWP）
#   open    IMC_GETOPENSTATUS 返回值（IME 是否打开）
#   conv    IMC_GETCONVERSIONMODE 返回值（含 NATIVE/FULLSHAPE/SYMBOL 等位）
#   imeWnd  IMM32 default IME window 句柄，0 = CUAS 没建窗口（TSF-only）
#   pid     前台窗口所在进程 ID
#   proc    前台进程名
#   win     窗口标题（截断 40 字符）
#
# 说明:
#   1. 输出仅在状态变化时刷新，避免刷屏。
#   2. TSF compartment 是进程内状态，**任何**外部 probe 都无法跨进程直接读取；
#      对 TSF-only 应用，外部观察者只能依赖 CUAS 把 TSF 状态镜像到 IMM HIMC，
#      若 CUAS 没建 HIMC（imeWnd=0），就是物理上不可读。这种情况下 KBLSwitch
#      之类的工具也读不到，验证只能靠功能行为（实际锁定是否生效）。

[CmdletBinding()]
param(
    [int]$IntervalMs = 200
)

Add-Type -Namespace W -Name Ime -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll")]
public static extern System.IntPtr GetForegroundWindow();
[System.Runtime.InteropServices.DllImport("user32.dll")]
public static extern uint GetWindowThreadProcessId(System.IntPtr hWnd, out uint lpdwProcessId);
[System.Runtime.InteropServices.DllImport("imm32.dll")]
public static extern System.IntPtr ImmGetDefaultIMEWnd(System.IntPtr hWnd);
[System.Runtime.InteropServices.DllImport("user32.dll", CharSet=System.Runtime.InteropServices.CharSet.Auto)]
public static extern int GetWindowText(System.IntPtr hWnd, System.Text.StringBuilder s, int n);
[System.Runtime.InteropServices.DllImport("user32.dll", CharSet=System.Runtime.InteropServices.CharSet.Auto)]
public static extern System.IntPtr SendMessageTimeout(System.IntPtr hWnd, uint msg, System.IntPtr wp, System.IntPtr lp, uint flags, uint timeoutMs, out System.IntPtr result);
'@

$WM_IME_CONTROL        = 0x0283
$IMC_GETCONVERSIONMODE = 0x0001
$IMC_GETOPENSTATUS     = 0x0005
$IME_CMODE_NATIVE      = 0x0001
$SMTO_ABORTIFHUNG      = 0x0002

function Get-ImmView {
    $hwnd = [W.Ime]::GetForegroundWindow()
    if ($hwnd -eq [IntPtr]::Zero) {
        return [pscustomobject]@{ Mode='?'; Open=0; Conv=0; ImeWnd=[IntPtr]::Zero; Pid=0; Proc=''; Title='' }
    }

    $sb = New-Object System.Text.StringBuilder 256
    [W.Ime]::GetWindowText($hwnd, $sb, 256) | Out-Null
    $title = $sb.ToString()
    if ($title.Length -gt 40) { $title = $title.Substring(0, 40) }

    $procId = 0
    [W.Ime]::GetWindowThreadProcessId($hwnd, [ref]$procId) | Out-Null
    $procName = ''
    try {
        $procName = (Get-Process -Id $procId -ErrorAction Stop).ProcessName
    } catch {}

    $imeWnd = [W.Ime]::ImmGetDefaultIMEWnd($hwnd)
    $open = 0; $conv = 0
    if ($imeWnd -ne [IntPtr]::Zero) {
        # SendMessageTimeout 避免目标线程阻塞时主循环卡死。
        $result = [IntPtr]::Zero
        if ([W.Ime]::SendMessageTimeout($imeWnd, $WM_IME_CONTROL, [IntPtr]$IMC_GETOPENSTATUS, [IntPtr]::Zero, $SMTO_ABORTIFHUNG, 200, [ref]$result) -ne [IntPtr]::Zero) {
            $open = $result.ToInt32()
        }
        if ([W.Ime]::SendMessageTimeout($imeWnd, $WM_IME_CONTROL, [IntPtr]$IMC_GETCONVERSIONMODE, [IntPtr]::Zero, $SMTO_ABORTIFHUNG, 200, [ref]$result) -ne [IntPtr]::Zero) {
            $conv = $result.ToInt32()
        }
    }

    $mode = if ($imeWnd -eq [IntPtr]::Zero) {
        'NO-IMEWND'
    } elseif (-not $open) {
        'OFF'
    } elseif ($conv -band $IME_CMODE_NATIVE) {
        'CN'
    } else {
        'EN'
    }

    return [pscustomobject]@{
        Mode = $mode; Open = $open; Conv = $conv; ImeWnd = $imeWnd
        Pid = $procId; Proc = $procName; Title = $title
    }
}

Write-Host ("Probing IME mode via IMM32 path (interval={0}ms)" -f $IntervalMs) -ForegroundColor Cyan
Write-Host "Ctrl+C 退出。状态变化时才输出，不刷屏。" -ForegroundColor Cyan
Write-Host ""

$lastSignature = $null
while ($true) {
    $stamp = (Get-Date).ToString('HH:mm:ss.fff')
    $v = Get-ImmView

    $signature = "{0}|{1}|{2}|{3}|{4}" -f $v.Mode, $v.Conv, $v.Open, $v.Pid, $v.Title
    if ($signature -ne $lastSignature) {
        $color = switch ($v.Mode) {
            'CN'        { 'Green' }
            'EN'        { 'Yellow' }
            'OFF'       { 'DarkGray' }
            'NO-IMEWND' { 'Magenta' }
            default     { 'White' }
        }
        $line = "{0}  {1,-9} open={2} conv=0x{3:X4} imeWnd=0x{4:X} pid={5,-6} proc={6,-20} win=[{7}]" -f `
            $stamp, $v.Mode, $v.Open, $v.Conv, $v.ImeWnd.ToInt64(), $v.Pid, $v.Proc, $v.Title
        Write-Host $line -ForegroundColor $color
        $lastSignature = $signature
    }

    Start-Sleep -Milliseconds $IntervalMs
}
