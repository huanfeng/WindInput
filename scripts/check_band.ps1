# check_band.ps1 - DWM Window Band diagnostic tool
# Usage:
#   .\check_band.ps1              # Scan after 8s countdown (open Start Menu first)
#   .\check_band.ps1 -Loop        # Continuous monitoring (Ctrl+C to stop)
#   .\check_band.ps1 -Now         # Scan immediately
#   .\check_band.ps1 -All         # Show all windows (including Band=0/1)

param(
    [switch]$Loop,
    [switch]$Now,
    [switch]$All
)

if (-not ([System.Management.Automation.PSTypeName]'WinBandTool2').Type) {
Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public class WinBandTool2 {
    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumCallback cb, IntPtr lp);
    [DllImport("user32.dll")]
    public static extern bool EnumThreadWindows(uint tid, EnumCallback cb, IntPtr lp);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")]
    public static extern int GetWindowLong(IntPtr h, int i);
    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool GetWindowBand(IntPtr h, out uint b);
    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr h, out RECT r);

    public delegate bool EnumCallback(IntPtr h, IntPtr lp);

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    public struct WinInfo {
        public IntPtr Hwnd;
        public uint Band;
        public uint Pid;
        public string ClassName;
        public string Title;
        public int ExStyle;
        public RECT Rect;
        public bool Visible;
    }

    private static HashSet<IntPtr> collected = new HashSet<IntPtr>();
    public static List<WinInfo> Results = new List<WinInfo>();

    private static WinInfo Collect(IntPtr h) {
        uint pid = 0;
        GetWindowThreadProcessId(h, out pid);
        uint band = 0;
        GetWindowBand(h, out band);
        StringBuilder cls = new StringBuilder(260);
        GetClassName(h, cls, 260);
        StringBuilder title = new StringBuilder(260);
        GetWindowTextW(h, title, 260);
        int ex = GetWindowLong(h, -20);
        RECT rc;
        GetWindowRect(h, out rc);
        bool vis = IsWindowVisible(h);
        return new WinInfo {
            Hwnd = h, Band = band, Pid = pid,
            ClassName = cls.ToString(), Title = title.ToString(),
            ExStyle = ex, Rect = rc, Visible = vis
        };
    }

    public static bool TopHandler(IntPtr h, IntPtr lp) {
        if (!collected.Contains(h)) {
            collected.Add(h);
            Results.Add(Collect(h));
        }
        return true;
    }

    public static bool ThreadHandler(IntPtr h, IntPtr lp) {
        if (!collected.Contains(h)) {
            collected.Add(h);
            Results.Add(Collect(h));
        }
        return true;
    }

    public static void Scan(uint[] threadIds) {
        Results.Clear();
        collected.Clear();
        EnumWindows(TopHandler, IntPtr.Zero);
        if (threadIds != null) {
            foreach (uint tid in threadIds) {
                EnumThreadWindows(tid, ThreadHandler, IntPtr.Zero);
            }
        }
    }
}
"@
}

$bandNames = @{
    0  = "DEFAULT";        1  = "DESKTOP";        2  = "UIACCESS"
    3  = "IMM_IHM";        4  = "IMM_NOTIF";      5  = "IMM_APPCHROME"
    6  = "IMM_MOGO";       7  = "IMM_EDGY";       8  = "IMM_INACT_MOB"
    9  = "IMM_INACT_DOCK"; 10 = "IMM_ACT_MOB";    11 = "IMM_ACT_DOCK"
    12 = "IMM_BG";         13 = "IMM_SEARCH";     14 = "GENUINE_WIN"
    15 = "IMM_RESTRICTED"; 16 = "SYSTEM_TOOLS";   17 = "LOCK"
    18 = "ABOVELOCK_UX"
}

$immersiveProcs = 'SearchHost', 'StartMenuExperienceHost', 'SearchUI', 'TextInputHost', 'InputApp', 'ShellExperienceHost'

$highlightProcs = 'wind_input', 'WeaselServer', 'SearchHost', 'StartMenuExperienceHost', 'SearchUI', 'TextInputHost', 'InputApp', 'ctfmon', 'explorer', 'wetype_renderer', 'ChsIME', 'ShellExperienceHost'

function Get-ImmersiveThreadIds {
    $tids = @()
    foreach ($name in $immersiveProcs) {
        Get-Process -Name $name -EA SilentlyContinue | ForEach-Object {
            $_.Threads | ForEach-Object { $tids += [uint32]$_.Id }
        }
    }
    return $tids
}

function Is-Highlight($procName) {
    foreach ($hp in $highlightProcs) {
        if ($procName -like "*$hp*") { return $true }
    }
    return $false
}

function Format-WindowInfo($win, $procName) {
    $bn = $bandNames[[int]$win.Band]
    if (-not $bn) { $bn = "UNKNOWN" }
    $top = if (($win.ExStyle -band 0x8) -ne 0) { "TOP" } else { "   " }
    $vis = if ($win.Visible) { " " } else { "H" }
    $w = $win.Rect.Right - $win.Rect.Left
    $h = $win.Rect.Bottom - $win.Rect.Top
    $sz = "${w}x${h}"
    $hw = "0x{0:X}" -f [int64]$win.Hwnd
    $pfx = if (Is-Highlight $procName) { ">>>" } else { "   " }
    return ("{0} Band={1,-2} ({2,-14}) {3}{4} PID={5,-6} [{6,-22}] {7,-35} {8,-12} {9}" -f $pfx, $win.Band, $bn, $top, $vis, $win.Pid, $procName, $win.ClassName, $sz, $hw)
}

function Do-Scan([bool]$showAll) {
    $procMap = @{}
    Get-Process -EA SilentlyContinue | ForEach-Object { $procMap[$_.Id] = $_.ProcessName }

    $tids = Get-ImmersiveThreadIds
    if ($tids.Count -gt 0) { [WinBandTool2]::Scan([uint32[]]$tids) }
    else { [WinBandTool2]::Scan($null) }

    $grouped = @{}
    foreach ($win in [WinBandTool2]::Results) {
        $pn = $procMap[[int]$win.Pid]
        if (-not $pn) { $pn = "?" }
        $show = $showAll -or ($win.Band -gt 1) -or (Is-Highlight $pn)
        if ($show) {
            $line = Format-WindowInfo $win $pn
            $b = [int]$win.Band
            if (-not $grouped.ContainsKey($b)) { $grouped[$b] = @() }
            $grouped[$b] += $line
        }
    }

    Write-Host ""
    Write-Host "=== Window Band Scan $(Get-Date -Format 'HH:mm:ss') ==="
    Write-Host "    (>>> = IME/StartMenu related, H = hidden, TOP = topmost)"
    Write-Host ""

    foreach ($band in ($grouped.Keys | Sort-Object -Descending)) {
        $bn = $bandNames[$band]
        if (-not $bn) { $bn = "UNKNOWN" }
        Write-Host "--- Band $band ($bn) ---" -ForegroundColor Cyan
        foreach ($line in $grouped[$band]) {
            if ($line.StartsWith(">>>")) { Write-Host $line -ForegroundColor Yellow }
            else { Write-Host $line }
        }
        Write-Host ""
    }

    $total = ([WinBandTool2]::Results).Count
    $shown = 0
    foreach ($v in $grouped.Values) { $shown += $v.Count }
    Write-Host "Total: $total windows scanned, $shown shown (use -All to show all)"
}

# === Main ===

if ($Loop) {
    Write-Host "=== Band Monitor (Ctrl+C to stop) ===" -ForegroundColor Green
    Write-Host "Monitoring for new IME/high-Band windows..."
    Write-Host ""

    $seen = @{}
    while ($true) {
        $procMap = @{}
        Get-Process -EA SilentlyContinue | ForEach-Object { $procMap[$_.Id] = $_.ProcessName }

        $tids = Get-ImmersiveThreadIds
        if ($tids.Count -gt 0) { [WinBandTool2]::Scan([uint32[]]$tids) }
    else { [WinBandTool2]::Scan($null) }

        foreach ($win in [WinBandTool2]::Results) {
            $pn = $procMap[[int]$win.Pid]
            if (-not $pn) { $pn = "?" }
            $show = ($win.Band -gt 1) -or (Is-Highlight $pn)
            if ($show) {
                $key = "$($win.Pid)-$($win.ClassName)-$($win.Band)"
                if (-not $seen.ContainsKey($key)) {
                    $seen[$key] = $true
                    $line = Format-WindowInfo $win $pn
                    if ($line.StartsWith(">>>")) { Write-Host $line -ForegroundColor Yellow }
                    else { Write-Host $line }
                }
            }
        }
        Start-Sleep -Milliseconds 200
    }
} else {
    if (-not $Now) {
        Write-Host "Open Start Menu and activate IME within 8 seconds..." -ForegroundColor Green
        for ($i = 8; $i -gt 0; $i--) {
            Write-Host "`r  $i seconds..." -NoNewline
            Start-Sleep -Seconds 1
        }
        Write-Host "`r  Scanning...   "
    }
    Do-Scan $All
}
