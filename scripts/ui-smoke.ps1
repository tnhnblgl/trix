<#
.SYNOPSIS
    Stage-4 machine gate: the desktop app builds, stays off the engine, and
    finds the daemon on its own.

.DESCRIPTION
    This gate cannot see a webview, and does not pretend to. What it proves is
    the half that is mechanical: that trix-ui still does not depend on
    trix-core, that both test suites pass, that the two binaries land in one
    directory, that the app survives a missing daemon instead of dying on it,
    that it connects by itself once one appears, and that a second launch
    focuses rather than duplicates.

    Everything a person has to look at -- the grid filling within a second of
    the hotkey, playback, seeking, the settings round trip -- is in the plan's
    hand-verification checklist and is NOT in here. A green run of this script
    is not stage 4.

    Runs the app against a SCRATCH %APPDATA%, so it can neither read nor
    rewrite the developer's real config.toml or clip library.

    Requires:
        cargo build --release --workspace
        cargo tauri build --no-bundle --config crates/trix-ui/tauri.conf.json
#>
[CmdletBinding()]
param(
    [string]$UiPath,
    [string]$DaemonPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:Checks = New-Object System.Collections.Generic.List[object]

$RepoRoot = Split-Path -Parent $PSScriptRoot
if (-not $UiPath) { $UiPath = Join-Path $RepoRoot 'target\release\trix-ui.exe' }
if (-not $DaemonPath) { $DaemonPath = Join-Path $RepoRoot 'target\release\trix-daemon.exe' }

function Check {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][bool]$Condition, [string]$Detail = '')
    $status = if ($Condition) { 'PASS' } else { 'FAIL' }
    $line = "  [$status] $Name"
    if ($Detail -and -not $Condition) { $line += " -- $Detail" }
    if ($Condition) { Write-Host $line -ForegroundColor Green } else { Write-Host $line -ForegroundColor Red }
    $script:Checks.Add([PSCustomObject]@{ Name = $Name; Passed = $Condition; Detail = $Detail })
}

# Not fail-fast, for daemon-smoke.ps1's reason: this builds a Tauri app, so a
# run that stops at the first failure costs minutes to learn one fact.

$scratch = Join-Path $env:TEMP ("trix-ui-smoke-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $scratch | Out-Null
# tracing_subscriber::fmt()'s default writer is stdout, not stderr -- checked
# empirically before this gate was written by running trix-daemon.exe with
# stdout and stderr redirected to separate files and diffing them; every line
# it prints, including "client connected", landed in stdout. Both streams are
# still captured and searched below so this check does not silently start
# failing if a future change to init_tracing (crates/trix-daemon/src/main.rs)
# moves the writer.
$daemonOutLog = Join-Path $scratch 'daemon.out.log'
$daemonErrLog = Join-Path $scratch 'daemon.err.log'
$started = New-Object System.Collections.Generic.List[object]

try {
    # --- 1. The rule ----------------------------------------------------------
    & powershell -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'ui-isolation.ps1') | Out-Null
    Check 'trix-ui does not depend on trix-core' ($LASTEXITCODE -eq 0) 'see scripts/ui-isolation.ps1'

    # --- 2. The suites --------------------------------------------------------
    Push-Location $RepoRoot
    try {
        & cargo test --workspace --quiet 2>&1 | Out-Null
        Check 'cargo test --workspace passes' ($LASTEXITCODE -eq 0)

        & npm --prefix crates/trix-ui/web test 2>&1 | Out-Null
        Check 'the frontend unit tests pass' ($LASTEXITCODE -eq 0) 'npm --prefix crates/trix-ui/web test'
    }
    finally {
        Pop-Location
    }

    # --- 3. The layout both launch paths assume -------------------------------
    Check 'trix-ui.exe was built' (Test-Path -LiteralPath $UiPath) $UiPath
    Check 'trix-daemon.exe was built' (Test-Path -LiteralPath $DaemonPath) $DaemonPath
    Check 'both binaries share one directory' `
        ((Split-Path -Parent $UiPath) -eq (Split-Path -Parent $DaemonPath)) `
        'the tray launches the app, and the app launches the daemon, by looking beside itself'

    # --- 4. A missing daemon is a panel, not a crash --------------------------
    # $UiPath's existence is checked (not assumed) before every Start-Process
    # below: `Start-Process -FilePath` on a path that does not exist throws a
    # terminating .NET exception rather than returning a failed process, and
    # under `$ErrorActionPreference = 'Stop'` that unwinds straight past every
    # remaining check to the `finally` block -- which is exactly the
    # fail-fast behaviour this gate's own comment says it does not have.
    # Caught by actually renaming trix-daemon.exe aside and running this
    # script: it raised "Sistem belirtilen dosyayi bulamiyor" out of
    # Start-Process and skipped straight to cleanup, never printing the
    # PASSED/FAILED summary at all. Guarding each launch keeps every later
    # check reporting a real PASS or FAIL instead.
    $uiBuilt = Test-Path -LiteralPath $UiPath
    $env:APPDATA = $scratch
    if ($uiBuilt) {
        $ui = Start-Process -FilePath $UiPath -PassThru
        $started.Add($ui)
        Start-Sleep -Seconds 5
        Check 'the app survives having no daemon' (-not $ui.HasExited) `
            'it should show the "Trix isn''t running" panel and keep retrying'
    }
    else {
        Check 'the app survives having no daemon' $false "trix-ui.exe was not built; see above"
    }

    # --- 5. It finds a daemon that appears later ------------------------------
    if (Test-Path -LiteralPath $DaemonPath) {
        $daemon = Start-Process -FilePath $DaemonPath -ArgumentList '-v' -PassThru `
            -RedirectStandardOutput $daemonOutLog -RedirectStandardError $daemonErrLog -WindowStyle Hidden
        $started.Add($daemon)

        $deadline = (Get-Date).AddSeconds(10)
        $connected = $false
        while ((Get-Date) -lt $deadline -and -not $connected) {
            $logs = @($daemonOutLog, $daemonErrLog) | Where-Object { Test-Path -LiteralPath $_ }
            if ($logs) {
                $connected = (Select-String -LiteralPath $logs -Pattern 'client connected' -Quiet) -eq $true
            }
            Start-Sleep -Milliseconds 250
        }
        Check 'the app reconnects on its own once a daemon exists' $connected `
            "no 'client connected' in $daemonOutLog or $daemonErrLog within 10s -- the backoff loop in daemon.rs"
    }
    else {
        Check 'the app reconnects on its own once a daemon exists' $false "trix-daemon.exe was not built; see above"
    }

    # --- 6. One window, not two ----------------------------------------------
    if ($uiBuilt) {
        $second = Start-Process -FilePath $UiPath -PassThru
        $started.Add($second)
        $exited = $second.WaitForExit(5000)
        Check 'a second launch focuses the first instead of opening a window' $exited `
            'tauri-plugin-single-instance should make the second copy hand over focus and exit'
    }
    else {
        Check 'a second launch focuses the first instead of opening a window' $false "trix-ui.exe was not built; see above"
    }
}
finally {
    foreach ($p in $started) {
        # -ErrorAction on Stop-Process suppresses the message but still fails
        # the tool, so the whole thing is wrapped: a process that already
        # exited is the normal case here, not a problem.
        try { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction Stop } } catch {}
    }
    Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue
}

$passed = @($script:Checks | Where-Object { $_.Passed }).Count
$failed = @($script:Checks | Where-Object { -not $_.Passed }).Count

''
if ($failed -gt 0) {
    Write-Host "STAGE-4 MACHINE GATE FAILED  ($passed passed, $failed failed)" -ForegroundColor Red
    # ForEach-Object, not .Name: under Set-StrictMode member enumeration over
    # an empty collection throws, and this list is empty on every green run.
    $script:Checks | Where-Object { -not $_.Passed } | ForEach-Object {
        Write-Host "  - $($_.Name)" -ForegroundColor Red
    }
    exit 1
}
Write-Host "STAGE-4 MACHINE GATE PASSED  ($passed checks)" -ForegroundColor Green
Write-Host 'This gate cannot see a webview. Stage 4 is the hand-verification checklist in the plan.'
