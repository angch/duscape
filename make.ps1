# `make` for Windows without make: reads the Makefile beside this script and runs its targets,
# each recipe line in Git's bash, so there is one copy of every recipe and it is the Makefile's.
#
#   .\make [-n] [-s] [-f FILE] [TARGET...] [VAR=value...]
#
# Interprets the subset of make this Makefile uses: targets with prerequisites (phony ones run;
# file targets are built when the file is missing), `:=` and `?=` variables with `$(VAR)`,
# `$(CURDIR)`, `$(HOME)` and `$(shell ...)`, backslash continuations, `@` for a silent line, `-`
# for one whose failure is ignored, `$$` for a shell `$`, and `VAR=value` on the command line
# (exported to the recipes as well, as make does). Nothing else — no patterns, conditionals or
# includes — and it says so if it meets one. Needs bash: Git for Windows's (found on its own),
# or whatever `MAKE_SHELL` names. `-n` prints the commands instead of running them.
[CmdletBinding(PositionalBinding = $false)]
param(
    [Alias("n")][switch]$DryRun,
    [Alias("s")][switch]$Silent,
    [Alias("f")][string]$Makefile = "",
    [Parameter(Position = 0, ValueFromRemainingArguments = $true)][string[]]$Goals
)
$ErrorActionPreference = "Stop"

# make's own voice for an error: the line on stderr and the exit code, nothing else.
function Fail([string]$message, [int]$code = 2) {
    [Console]::Error.WriteLine($message)
    exit $code
}

$here = $PSScriptRoot
if (-not $Makefile) { $Makefile = Join-Path $here "Makefile" }
if (-not (Test-Path $Makefile)) { Fail "make: ${Makefile}: No such file" 2 }

function Find-Bash {
    if ($env:MAKE_SHELL) { return $env:MAKE_SHELL }
    $candidates = @(
        "$env:ProgramFiles\Git\bin\bash.exe",
        "${env:ProgramFiles(x86)}\Git\bin\bash.exe",
        "$env:LOCALAPPDATA\Programs\Git\bin\bash.exe"
    )
    foreach ($c in $candidates) { if (Test-Path $c) { return $c } }
    # Not the WSL launcher in System32, which runs a Linux, not this checkout.
    $onPath = Get-Command bash -ErrorAction SilentlyContinue | Where-Object { $_.Source -notmatch 'System32' } | Select-Object -First 1
    if ($onPath) { return $onPath.Source }
    Fail "make: no bash: install Git for Windows, or set MAKE_SHELL to a bash"
}
$bash = Find-Bash

# A path as bash and cargo both take it: forward slashes.
$curdir = ($here -replace '\\', '/')
$home_ = if ($env:HOME) { $env:HOME } else { $env:USERPROFILE }
$vars = @{ "CURDIR" = $curdir; "HOME" = ($home_ -replace '\\', '/') }

# Command-line assignments first: they beat `?=` and `:=` alike, and reach the recipes.
$targets = @()
foreach ($goal in $Goals) {
    if ($goal -match '^([A-Za-z_][A-Za-z0-9_]*)=(.*)$') {
        $vars[$Matches[1]] = $Matches[2]
        Set-Item -Path "env:$($Matches[1])" -Value $Matches[2]
    } else { $targets += $goal }
}
$commandLine = @($vars.Keys)

# Run `script` in bash, in the Makefile's directory. Its output flows on (a PowerShell function
# returns everything it emits, so the status comes back in `$LastStatus` instead); with
# `-Capture`, its stdout is returned, for `$(shell ...)`. The text goes through a file, never an
# argument list, which PowerShell would re-quote. Windows PowerShell treats a native command's
# stderr as an error under `Stop`, so that is relaxed for the call.
$script:LastStatus = 0
function Run-Shell([string]$script, [switch]$Capture) {
    $file = [IO.Path]::GetTempFileName() + ".sh"
    [IO.File]::WriteAllText($file, "cd `"$curdir`" || exit 1`n" + $script + "`n", (New-Object Text.UTF8Encoding($false)))
    $was = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        if ($Capture) {
            $out = & $bash $file 2>$null
            $script:LastStatus = $LASTEXITCODE
            return ($out -join "`n")
        }
        & $bash $file
        $script:LastStatus = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $was
        Remove-Item $file -ErrorAction SilentlyContinue
    }
}

# `$(VAR)`, `$(shell ...)` and `$$` in `text`, left to right, parentheses balanced.
function Expand([string]$text) {
    $out = New-Object Text.StringBuilder
    $i = 0
    while ($i -lt $text.Length) {
        $c = $text[$i]
        if ($c -ne '$') { [void]$out.Append($c); $i++; continue }
        if ($i + 1 -ge $text.Length) { [void]$out.Append('$'); break }
        $next = $text[$i + 1]
        if ($next -eq '$') { [void]$out.Append('$'); $i += 2; continue }
        if ($next -ne '(') {
            # `$@`, `$<` and friends: not used here; kept as the shell would see them.
            [void]$out.Append('$'); $i++; continue
        }
        $depth = 0; $j = $i + 1
        do {
            if ($text[$j] -eq '(') { $depth++ } elseif ($text[$j] -eq ')') { $depth-- }
            $j++
        } while ($depth -gt 0 -and $j -lt $text.Length)
        $inner = $text.Substring($i + 2, $j - $i - 3)
        if ($inner -match '^shell\s+(.*)$') {
            [void]$out.Append((Run-Shell (Expand $Matches[1]) -Capture).Trim())
        } elseif ($inner -match '^[A-Za-z_][A-Za-z0-9_]*$') {
            $value = if ($vars.ContainsKey($inner)) { $vars[$inner] }
                     elseif (Test-Path "env:$inner") { (Get-Item "env:$inner").Value }
                     else { "" }
            [void]$out.Append($value)
        } else {
            Fail "make: ${inner}: a make function this does not know (see make.ps1)" 2
        }
        $i = $j
    }
    $out.ToString()
}

# --- read the Makefile: continuations joined, variables set, targets collected
$lines = [IO.File]::ReadAllLines($Makefile)
$logical = @()   # (text, isRecipe)
$i = 0
while ($i -lt $lines.Length) {
    $line = $lines[$i]; $i++
    $isRecipe = $line.StartsWith("`t")
    while ($line.EndsWith('\') -and $i -lt $lines.Length) {
        $more = $lines[$i]; $i++
        if ($isRecipe) {
            # As make hands it to the shell: the backslash-newline stays (the shell joins),
            # one leading tab goes.
            if ($more.StartsWith("`t")) { $more = $more.Substring(1) }
            $line = $line.Substring(0, $line.Length - 1) + "\`n" + $more
        } else {
            $line = $line.Substring(0, $line.Length - 1).TrimEnd() + " " + $more.Trim()
        }
    }
    $logical += , @($line, $isRecipe)
}

$rules = [ordered]@{}   # name -> @{ Prereqs = @(); Recipe = @() }
$phony = @{}
$current = $null
foreach ($entry in $logical) {
    $text, $isRecipe = $entry
    if ($isRecipe) {
        if ($null -eq $current) { Fail "make: recipe commences before first target" 2 }
        $rules[$current].Recipe += $text.Substring(1)
        continue
    }
    $t = $text.Trim()
    if ($t -eq "" -or $t.StartsWith("#")) { continue }
    if ($t -match '^(ifeq|ifneq|ifdef|ifndef|include|-include|define|export|unexport)\b' -or $t -match '%') {
        Fail "make: line `"$t`": a make construct this does not know (see make.ps1)" 2
    }
    if ($t -match '^([A-Za-z_][A-Za-z0-9_]*)\s*(:=|\?=|=)\s*(.*)$') {
        $name, $op, $value = $Matches[1], $Matches[2], $Matches[3]
        if ($commandLine -contains $name) { continue }
        if ($op -eq '?=' -and ($vars.ContainsKey($name) -or (Test-Path "env:$name"))) { continue }
        $vars[$name] = Expand $value
        continue
    }
    if ($t -match '^([^:=]+):(.*)$') {
        $names = (Expand $Matches[1].Trim()) -split '\s+' | Where-Object { $_ }
        $prereqs = @((Expand $Matches[2].Trim()) -split '\s+' | Where-Object { $_ })
        foreach ($name in $names) {
            if ($name -eq ".PHONY") { foreach ($p in $prereqs) { $phony[$p] = $true }; continue }
            if (-not $rules.Contains($name)) { $rules[$name] = @{ Prereqs = @(); Recipe = @() } }
            $rules[$name].Prereqs += $prereqs
            $current = $name
        }
        continue
    }
    Fail "make: line `"$t`": not understood (see make.ps1)" 2
}

# --- run
$done = @{}
function Build([string]$name, [int]$depth) {
    if ($done.ContainsKey($name)) { return }
    if (-not $rules.Contains($name)) {
        if (Test-Path (Join-Path $here $name)) { $done[$name] = $true; return }
        Fail "make: *** No rule to make target '$name'.  Stop." 2
    }
    $rule = $rules[$name]
    foreach ($p in $rule.Prereqs) { Build $p ($depth + 1) }
    $done[$name] = $true
    # A file target that exists is up to date; a phony one, or a missing file, is made.
    if (-not $phony.ContainsKey($name) -and (Test-Path (Join-Path $here $name))) { return }
    if ($rule.Recipe.Count -eq 0 -and $depth -eq 0 -and $rule.Prereqs.Count -eq 0) {
        Write-Host "make: Nothing to be done for '$name'."
    }
    foreach ($raw in $rule.Recipe) {
        $cmd = Expand $raw
        $quiet = $Silent; $ignore = $false
        while ($cmd.Length -gt 0 -and ($cmd[0] -eq '@' -or $cmd[0] -eq '-')) {
            if ($cmd[0] -eq '@') { $quiet = $true } else { $ignore = $true }
            $cmd = $cmd.Substring(1)
        }
        if ($cmd.Trim() -eq "") { continue }
        if (-not $quiet -or $DryRun) { Write-Host $cmd }
        if ($DryRun) { continue }
        Run-Shell $cmd
        $status = $script:LastStatus
        if ($status -ne 0 -and -not $ignore) {
            [Console]::Error.WriteLine("make: *** [$name] Error $status")
            exit $status
        }
    }
}

if ($targets.Count -eq 0) { $targets = @(@($rules.Keys)[0]) }
foreach ($target in $targets) { Build $target 0 }
