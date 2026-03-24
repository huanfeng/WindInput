param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Version,

    [string]$Preid = "alpha"
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Definition
$RootDir = (Resolve-Path (Join-Path $ScriptDir "..")).Path
$VersionFile = Join-Path $RootDir "VERSION"

# --- Parse current version from VERSION file ---
function Parse-SemVer([string]$v) {
    $v = $v.Trim()
    $pre = ""
    if ($v -match '^(\d+\.\d+\.\d+)(-(.+))?$') {
        $core = $Matches[1]
        if ($Matches[3]) { $pre = $Matches[3] }
    } else {
        throw "Invalid semver: $v"
    }
    $parts = $core -split '\.'
    return @{
        Major = [int]$parts[0]
        Minor = [int]$parts[1]
        Patch = [int]$parts[2]
        Pre   = $pre
    }
}

function Format-SemVer($sv) {
    $s = "$($sv.Major).$($sv.Minor).$($sv.Patch)"
    if ($sv.Pre) { $s += "-$($sv.Pre)" }
    return $s
}

# --- Resolve target version ---
$keywords = @("major", "minor", "patch", "premajor", "preminor", "prepatch", "prerelease")

if ($Version -in $keywords) {
    if (-not (Test-Path $VersionFile)) {
        throw "VERSION file not found. Cannot use keyword bump without existing version."
    }
    $currentRaw = (Get-Content $VersionFile -Raw).Trim()
    $cur = Parse-SemVer $currentRaw

    switch ($Version) {
        "major" {
            $cur.Major++; $cur.Minor = 0; $cur.Patch = 0; $cur.Pre = ""
        }
        "minor" {
            $cur.Minor++; $cur.Patch = 0; $cur.Pre = ""
        }
        "patch" {
            $cur.Patch++; $cur.Pre = ""
        }
        "premajor" {
            $cur.Major++; $cur.Minor = 0; $cur.Patch = 0; $cur.Pre = "$Preid.0"
        }
        "preminor" {
            $cur.Minor++; $cur.Patch = 0; $cur.Pre = "$Preid.0"
        }
        "prepatch" {
            $cur.Patch++; $cur.Pre = "$Preid.0"
        }
        "prerelease" {
            if ($cur.Pre) {
                # Increment pre-release number: alpha.0 -> alpha.1
                if ($cur.Pre -match '^(.+)\.(\d+)$') {
                    $preTag = $Matches[1]
                    $preNum = [int]$Matches[2] + 1
                    $cur.Pre = "$preTag.$preNum"
                } else {
                    # Pre-release without number: alpha -> alpha.0
                    $cur.Pre = "$($cur.Pre).0"
                }
            } else {
                # Stable -> prerelease: bump patch and add pre tag
                $cur.Patch++
                $cur.Pre = "$Preid.0"
            }
        }
    }

    $TargetVersion = Format-SemVer $cur
    Write-Host "$currentRaw -> $TargetVersion  ($Version)"
} else {
    # Explicit version string provided
    $null = Parse-SemVer $Version  # validate format
    $TargetVersion = $Version.Trim()
    if (Test-Path $VersionFile) {
        $currentRaw = (Get-Content $VersionFile -Raw).Trim()
        Write-Host "$currentRaw -> $TargetVersion"
    } else {
        Write-Host "-> $TargetVersion"
    }
}

Write-Host ""

# --- Apply version to all files ---

# 1. VERSION file
Set-Content -Path $VersionFile -Value $TargetVersion -NoNewline -Encoding UTF8
Write-Host "  [OK] VERSION"

# 2. README.md badge
$ReadmePath = Join-Path $RootDir "README.md"
if (Test-Path $ReadmePath) {
    $content = Get-Content $ReadmePath -Raw -Encoding UTF8
    $escapedVersion = $TargetVersion -replace '-', '--'
    $updated = $content -replace 'version-[^-]+--(alpha|beta|rc\d*|dev)(\.\d+)?-blue', "version-$escapedVersion-blue"
    $updated = $updated -replace 'version-[\d.]+(--.+?)?-blue', "version-$escapedVersion-blue"
    Set-Content -Path $ReadmePath -Value $updated -NoNewline -Encoding UTF8
    Write-Host "  [OK] README.md badge"
}

# 3. CHANGELOG.md header
$ChangelogPath = Join-Path $RootDir "CHANGELOG.md"
if (Test-Path $ChangelogPath) {
    $bytes = [System.IO.File]::ReadAllBytes($ChangelogPath)
    $content = [System.Text.Encoding]::UTF8.GetString($bytes)
    $pattern = '\[\d+\.\d+\.\d+(-[^\]]+)?\]'
    if ($content -match $pattern) {
        $updated = $content -replace $pattern, "[$TargetVersion]"
        [System.IO.File]::WriteAllText($ChangelogPath, $updated, [System.Text.UTF8Encoding]::new($false))
        Write-Host "  [OK] CHANGELOG.md"
    } else {
        Write-Host "  [SKIP] CHANGELOG.md (no matching version header)"
    }
}

Write-Host ""
Write-Host "Done: v$TargetVersion"
