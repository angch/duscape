# The standard measurement of docs/probes/bench-matrix.sh, for Windows: the machine, the
# filesystem and the disk, then warm timings of diskonaut against diskus and WizTree on the trees
# given, and the build profile. Writes docs/benchmarks/<host>-<date>.md in the same format;
# commit it.
#
#   docs/probes/bench-matrix.ps1 [-Runs N] [-Tag WORD] TREE...
#
# `-Tag` names the file `<host>-<date>-<tag>.md`, for a second run on the same day beside the
# baseline. `diskus` and `hyperfine` come from `cargo install`; WizTree from wiztreefree.com. It
# is timed in its export mode (`/export`, folders only, `/admin=0`), which scans, writes a CSV and
# exits — unelevated it walks the directories as everyone else does; from an elevated shell it
# reads the MFT, and so does diskonaut read the volume's metadata files, which is what the
# "elevated runs" line records; elevated, diskonaut reads the master file table, and a row with
# `--no-device-read` beside it walks the directories at the same privilege. Cold rows need the
# file cache emptied before each run
# (`drop-cache.ps1`, beside this script), which an elevated shell can do; unelevated there are
# none, and the file says so rather than silently skipping them.
# Only the trees are positional: without this, PowerShell binds the first tree to `-Tag`.
[CmdletBinding(PositionalBinding = $false)]
param(
    [int]$Runs = 3,
    [ValidatePattern('^[\w.-]*$')][string]$Tag = "",
    [Parameter(Position = 0, ValueFromRemainingArguments = $true)][string[]]$Trees
)
$ErrorActionPreference = "Stop"
if (-not $Trees) { Write-Error "usage: bench-matrix.ps1 [-Runs N] [-Tag WORD] TREE..."; exit 2 }

$here = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$bin = if ($env:DISKONAUT) { $env:DISKONAUT } else { Join-Path $here "target\release\diskonaut.exe" }
if (-not (Test-Path $bin)) { Write-Error "missing: $bin (cargo build --release -p diskonaut-angch)"; exit 1 }
function Find-Tool([string]$name) {
    $found = Get-Command $name -ErrorAction SilentlyContinue
    if ($found) { return $found.Source }
    $cargo = Join-Path $env:USERPROFILE ".cargo\bin\$name.exe"
    if (Test-Path $cargo) { return $cargo }
    return $null
}
$hyperfine = Find-Tool hyperfine
if (-not $hyperfine) { Write-Error "missing: hyperfine (cargo install hyperfine)"; exit 1 }
$diskus = Find-Tool diskus
$wiztree = @("C:\Program Files\WizTree\WizTree64.exe", "C:\Program Files (x86)\WizTree\WizTree64.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$elevated = (New-Object Security.Principal.WindowsPrincipal $identity).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
$dropCache = Join-Path $PSScriptRoot "drop-cache.ps1"
$canDrop = $elevated -and (Test-Path $dropCache)
$host_ = $env:COMPUTERNAME.ToLower()
$date = Get-Date
$out = Join-Path $here ("docs\benchmarks\{0}-{1}{2}.md" -f $host_, $date.ToString("yyyyMMdd"), $(if ($Tag) { "-$Tag" } else { "" }))
$wizcsv = Join-Path $env:TEMP "bench-matrix-wiztree.csv"
# Created now, so a path that cannot be written fails before the measurements, not after them.
[IO.File]::WriteAllText($out, "", (New-Object Text.UTF8Encoding($false)))

# diskonaut's `--benchmark` output, stdout and stderr together as lines. Windows PowerShell 5.1
# makes a native command's stderr a terminating error under `Stop`, so that is relaxed here.
function Benchmark-Lines([string[]]$arguments) {
    $was = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try { return @(& $bin @arguments 2>&1 | ForEach-Object { [string]$_ }) }
    finally { $ErrorActionPreference = $was }
}

# An argument for a command line cmd.exe hands to a program: quoted when it has spaces, with a
# trailing backslash doubled so the quote after it is not taken as escaped.
function Arg([string]$value) {
    if ($value -notmatch '[ &]') { return $value }
    if ($value.EndsWith("\")) { $value += "\" }
    return "`"$value`""
}

# A program's path as the first word of a cmd.exe line: its 8.3 name when the real one has
# spaces, since a line that starts with a quote and has more quotes in it loses its first and
# last quote to cmd's parsing.
function Program([string]$path) {
    if ($path -notmatch ' ') { return $path }
    return (New-Object -ComObject Scripting.FileSystemObject).GetFile($path).ShortPath
}

# --- the machine ---
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$os = Get-CimInstance Win32_OperatingSystem
$system = Get-CimInstance Win32_ComputerSystem
$memGiB = [math]::Round($os.TotalVisibleMemorySize / 1MB)
$virt = if ($system.HypervisorPresent) { "hypervisor present (a Hyper-V or WSL2 host, most likely)" } else { "none" }
$commit = (git -C $here rev-parse --short HEAD).Trim()
$dirty = if ((git -C $here status --porcelain -uno | Measure-Object).Count -gt 0) { "with local changes" } else { "clean" }
$diskusVersion = if ($diskus) { (& $diskus --version).Trim() } else { "not installed" }
$wiztreeVersion = if ($wiztree) { "WizTree " + (Get-Item $wiztree).VersionInfo.FileVersion } else { "not installed" }

$lines = New-Object System.Collections.Generic.List[string]
# The dash as a code point: Windows PowerShell reads a script without a BOM in the system's
# code page, and this file has none.
$lines.Add("# $host_ $([char]0x2014) $($date.ToString('yyyy-MM-dd'))")
$lines.Add("")
$lines.Add("| | |")
$lines.Add("| --- | --- |")
$lines.Add("| machine | $($cpu.Name.Trim()), $($cpu.NumberOfLogicalProcessors) cores, $memGiB GiB, virtualisation: $virt |")
$lines.Add("| system | $($os.Caption.Trim()) $($os.Version) |")
$lines.Add("| diskonaut | $commit ($dirty), release profile |")
$lines.Add("| diskus | $diskusVersion |")
$lines.Add("| WizTree | $wiztreeVersion, timed in export mode (folders only) |")
$lines.Add("| cold runs | $(if ($canDrop) { 'yes (the file cache emptied before each: drop-cache.ps1)' } else { 'no: emptying the file cache (drop-cache.ps1) needs an elevated shell' }) |")
$lines.Add("| elevated runs | $(if ($elevated) { 'yes: this shell is elevated (every row)' } else { 'no: run from an elevated shell for them' }) |")
$lines.Add("| runs per cell | $Runs |")
$lines.Add("")
$lines.Add("## Trees")
$lines.Add("")
$lines.Add("| tree | filesystem | device | disk | entries | size |")
$lines.Add("| --- | --- | --- | --- | --- | --- |")

$resolved = @()
$wizFigures = @()
foreach ($tree in $Trees) {
    $path = (Resolve-Path $tree).Path
    $resolved += $path
    $letter = $path.Substring(0, 1)
    $volume = Get-Volume -DriveLetter $letter -ErrorAction SilentlyContinue
    $fs = if ($volume) { $volume.FileSystem } else { "unknown" }
    $partition = Get-Partition -DriveLetter $letter -ErrorAction SilentlyContinue
    $disk = if ($partition) { Get-PhysicalDisk | Where-Object { [int]$_.DeviceId -eq $partition.DiskNumber } | Select-Object -First 1 }
    $diskWords = if ($disk) { "disk $($disk.DeviceId) ($($disk.FriendlyName.Trim())) $($disk.MediaType), $($disk.BusType)" } else { "unknown" }
    $line = Benchmark-Lines @("--benchmark", "--bench-stage", "sharded", $path) | Where-Object { $_ -match '^sharded' } | Select-Object -First 1
    $entries = ""; $size = ""; $hl = ""
    if ($line -match '^sharded\s+\S+\s+(\d+) entries') { $entries = $Matches[1] }
    if ($line -match '([\d.]+ [KMGT]?i?B) \(') { $size = $Matches[1] }
    if ($line -match '(\d+ hard-linked)') { $hl = ", " + $Matches[1] }
    $lines.Add("| ``$path`` | $fs | ${letter}: | $diskWords | $entries$hl | $size |")
    if ($wiztree) {
        # WizTree's own count of the same tree, for the sizes cross-check: its CSV's first data
        # row is the tree itself — Size, Allocated, ..., Files, Folders.
        Remove-Item $wizcsv -ErrorAction SilentlyContinue
        $p = Start-Process -FilePath $wiztree -ArgumentList @((Arg $path), "/export=`"$wizcsv`"", "/admin=0", "/exportfolders=1", "/exportfiles=0") -PassThru -Wait
        if (Test-Path $wizcsv) {
            $row = Get-Content $wizcsv | Select-Object -Skip 2 -First 1
            $cells = $row -split ','
            if ($cells.Count -ge 7) {
                $allocated = [int64]$cells[2]
                $human = if ($allocated -ge 1GB) { "$([math]::Round($allocated / 1GB, 1)) GiB" }
                    elseif ($allocated -ge 1MB) { "$([math]::Round($allocated / 1MB, 1)) MiB" }
                    else { "$([math]::Round($allocated / 1KB, 1)) KiB" }
                $wizFigures += "| ``$path`` | $([int64]$cells[5] + [int64]$cells[6]) | $human ($allocated B) |"
            }
        }
    }
}
if ($wizFigures) {
    $lines.Add("")
    $lines.Add("WizTree's figures for the same trees (files and folders, allocated):")
    $lines.Add("")
    $lines.Add("| tree | entries | allocated |")
    $lines.Add("| --- | --- | --- |")
    $wizFigures | ForEach-Object { $lines.Add($_) }
}

function Bench([string]$title, [string]$dir, [string[]]$hyperfineArgs) {
    $md = Join-Path $env:TEMP "bench-matrix-hyperfine.md"
    $cmds = @()
    if ($diskus) { $cmds += @("-n", "diskus", "$(Program $diskus) --directories excluded $(Arg $dir) >NUL 2>&1") }
    $cmds += @("-n", "diskonaut sharded", "$(Program $bin) --benchmark --bench-stage sharded $(Arg $dir) >NUL")
    $cmds += @("-n", "diskonaut refined", "$(Program $bin) --benchmark --bench-stage refined $(Arg $dir) >NUL")
    if ($elevated) {
        # Elevated, the scan reads the volume's master file table; the walk through the
        # filesystem at the same privilege separates the table read from the rest.
        $cmds += @("-n", "diskonaut sharded, kernel walk", "$(Program $bin) --benchmark --bench-stage sharded --no-device-read $(Arg $dir) >NUL")
    }
    if ($wiztree) { $cmds += @("-n", "WizTree export", "$(Program $wiztree) $(Arg $dir) /export=`"$wizcsv`" /admin=0 /exportfolders=0 /exportfiles=0") }
    # hyperfine's warnings (outliers, say) go to stderr, which under `Stop` would end the run;
    # a cell that fails is recorded with what hyperfine said, and the run goes on.
    $was = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try { $said = @(& $hyperfine --runs $Runs @hyperfineArgs --export-markdown $md @cmds 2>&1 | ForEach-Object { [string]$_ }) }
    finally { $ErrorActionPreference = $was }
    $lines.Add("### ${title}: $dir")
    $lines.Add("")
    if (Test-Path $md) {
        Get-Content $md -Encoding UTF8 | ForEach-Object { $lines.Add($_) }
        Remove-Item $md
    } else {
        $lines.Add("hyperfine failed:")
        $lines.Add("")
        $lines.Add('```')
        $said | Where-Object { $_.Trim() } | ForEach-Object { $lines.Add($_) }
        $lines.Add('```')
    }
    $lines.Add("")
}

$lines.Add("")
$lines.Add("## Timings")
$lines.Add("")
foreach ($path in $resolved) {
    Write-Host "== warm $path"
    Bench "warm" $path @("--warmup", "1")
    if ($canDrop) {
        Write-Host "== cold $path"
        Bench "cold" $path @("--prepare", "powershell -NoProfile -ExecutionPolicy Bypass -File $(Arg $dropCache)")
    }
}

$lines.Add("## Build profile (tree-only, warm)")
$lines.Add("")
foreach ($path in $resolved) {
    $lines.Add("### $path")
    $lines.Add("")
    $lines.Add('```')
    # The profile (stderr) before the stage's line (stdout), as the terminal shows them; merged
    # streams do not keep that order.
    $profile = Benchmark-Lines @("--benchmark", "--bench-stage", "tree-only", "--bench-profile", $path) |
        Where-Object { ($_ -match '^  ' -or $_ -match '^tree-only') -and $_ -notmatch 'threads:' }
    $profile | Where-Object { $_ -match '^  ' } | ForEach-Object { $lines.Add($_) }
    $profile | Where-Object { $_ -match '^tree-only' } | ForEach-Object { $lines.Add($_) }
    $lines.Add('```')
    $lines.Add("")
}
Remove-Item $wizcsv -ErrorAction SilentlyContinue
[IO.File]::WriteAllText($out, ($lines -join "`n") + "`n", (New-Object Text.UTF8Encoding($false)))
Write-Host "wrote $out"
