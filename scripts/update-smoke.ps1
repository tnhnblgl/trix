<#
.SYNOPSIS
    Proves, against a real running trix-daemon.exe, the one Windows
    filesystem behaviour the whole update swap depends on.

.DESCRIPTION
    swap_in (crates\trix-ui\src\update\swap.rs) renames each binary aside
    rather than deleting or overwriting it, because Windows will not let a
    running executable be deleted, but will let one be renamed. That is not
    an implementation detail -- it is the entire reason the swap is shaped
    the way it is -- and nothing in the unit tests exercises it: they inject
    a rename function and move files that were never running, so a real OS
    refusal and a false assumption that merely compiles look identical to
    them.

    This script cannot call swap_in itself and does not pretend to. trix-ui
    is a binary-only crate with no lib.rs, so there is no code path here for
    a PowerShell script to reach. An earlier version of this test worked
    around that by re-implementing the moves with Move-Item and asserting
    that PowerShell had done what PowerShell had just done -- which would
    stay green forever regardless of what swap_in, or Windows, actually
    does. This one does not touch trix-ui.exe at all.

    Instead it copies the real trix-daemon.exe into a scratch install
    directory, starts it there against a scratch config, and against that
    live, running process:

      * renames trix-daemon.exe to trix-daemon.exe.old, and asserts that
        succeeds;
      * asserts the daemon is still running afterwards and still answers on
        its control pipe -- proof that Windows kept the image mapped under
        its new name instead of tearing the process down, which is what
        makes the rename safe to perform under a live daemon;
      * places a new file at the name the rename just vacated, the way the
        incoming payload would be moved into place;
      * restores the running binary's original name, then attempts to
        DELETE it, and asserts the delete is refused. This is the negative
        that explains why the design renames instead of overwriting: if
        this delete ever starts succeeding, the .old dance is unnecessary,
        and if the rename above ever starts failing, the updater is broken.

    Everything runs under a scratch %APPDATA% AND a scratch clip_dir, never
    the developer's real ones -- an empty clip_dir resolves to
    %USERPROFILE%\Videos\Trix, so a scratch %APPDATA% alone is not
    isolation. This follows daemon-smoke.ps1's isolation setup.

    Requires a built release daemon:
        cargo build --release -p trix-daemon
#>
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$relDir = Join-Path $RepoRoot 'target\release'
$realDaemonExe = Join-Path $relDir 'trix-daemon.exe'

# Only trix-daemon.exe is checked, unlike ship-zip.ps1's full three-binary
# build: this script never touches trix.exe or trix-ui.exe, so requiring a
# full `cargo tauri build` before it can run would be a needless gate on
# something it does not use.
if (-not (Test-Path -LiteralPath $realDaemonExe)) {
    throw "trix-daemon.exe is not built. Run: cargo build --release -p trix-daemon"
}

# Refused rather than silently talking to the wrong process: this script
# drives commands over the well-known 'trix-control' pipe name, and a
# daemon already running under that name -- the developer's own -- would
# answer instead of the scratch one this script starts.
$alreadyRunning = @(Get-Process -Name 'trix-daemon' -ErrorAction SilentlyContinue)
if ($alreadyRunning.Count -gt 0) {
    throw ("trix-daemon is already running (PID $($alreadyRunning.Id -join ', ')). " +
           "This script needs to control a daemon it started itself, against a scratch " +
           "config. Stop the running one and re-run.")
}

$failures = 0
function Check {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][bool]$Condition, [string]$Detail = '')
    $status = 'FAIL'
    if ($Condition) { $status = 'PASS' }
    $line = "  [$status] $Name"
    if ($Detail -and -not $Condition) { $line += " -- $Detail" }
    if ($Condition) { Write-Host $line -ForegroundColor Green } else { Write-Host $line -ForegroundColor Red }
    if (-not $Condition) { $script:failures++ }
}

# Runs an action and turns a thrown error into a plain result instead of
# stopping the script -- every assertion below still has to run and print
# even when an earlier one failed, or a single bad rename would silence the
# rest of the report.
function Invoke-Guarded {
    param([Parameter(Mandatory)][scriptblock]$Action)
    try {
        & $Action
        [PSCustomObject]@{ Ok = $true; Error = $null }
    } catch {
        [PSCustomObject]@{ Ok = $false; Error = $_.Exception.Message }
    }
}

# --- Isolation, following daemon-smoke.ps1 exactly -----------------------------
# Both a scratch %APPDATA% and a scratch clip_dir are required. A scratch
# %APPDATA% alone still leaves clip_dir empty, which resolves to the real
# %USERPROFILE%\Videos\Trix -- so clip_dir is set explicitly even though this
# script never arms and never writes a clip.

$root = Join-Path $env:TEMP ("trix-update-smoke-" + [guid]::NewGuid().ToString('N'))
$install = Join-Path $root 'install'
$appData = Join-Path $root 'appdata'
$clipDir = Join-Path $root 'clips'
New-Item -ItemType Directory -Force $install | Out-Null
New-Item -ItemType Directory -Force (Join-Path $appData 'trix') | Out-Null
New-Item -ItemType Directory -Force $clipDir | Out-Null

@"
clip_dir = '$clipDir'
max_library_gb = 0
stats_seconds = 1
clip_hotkey = "ctrl+alt+shift+f9"
"@ | Out-File -FilePath (Join-Path $appData 'trix\config.toml') -Encoding utf8

$liveExe = Join-Path $install 'trix-daemon.exe'
$oldExe = "$liveExe.old"
$outLog = Join-Path $root 'daemon.out.log'
$errLog = Join-Path $root 'daemon.err.log'
Copy-Item -LiteralPath $realDaemonExe -Destination $liveExe -Force

$prevAppData = $env:APPDATA
$env:APPDATA = $appData

$daemon = $null
try {
    Write-Host ''
    Write-Host "starting $liveExe" -ForegroundColor Cyan
    Write-Host "scratch appdata: $appData"
    Write-Host "scratch clip_dir: $clipDir"
    Write-Host ''

    $daemon = Start-Process -FilePath $liveExe -ArgumentList '-v' -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $outLog -RedirectStandardError $errLog

    $ready = $false
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline) {
        try {
            $probe = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'trix-control', [System.IO.Pipes.PipeDirection]::InOut)
            $probe.Connect(250); $probe.Dispose(); $ready = $true; break
        } catch { Start-Sleep -Milliseconds 200 }
    }
    if (-not $ready) { throw "daemon never opened the control pipe; see $errLog" }
    Write-Host "daemon is up (PID $($daemon.Id))" -ForegroundColor Cyan

    function Connect-TrixControl {
        param([int]$TimeoutMs = 5000)
        $pipe = New-Object System.IO.Pipes.NamedPipeClientStream(
            '.', 'trix-control',
            [System.IO.Pipes.PipeDirection]::InOut,
            [System.IO.Pipes.PipeOptions]::Asynchronous)
        $pipe.Connect($TimeoutMs)
        $utf8 = New-Object System.Text.UTF8Encoding($false)
        $reader = New-Object System.IO.StreamReader($pipe, $utf8, $false, 4096, $true)
        $writer = New-Object System.IO.StreamWriter($pipe, $utf8, 4096, $true)
        $writer.AutoFlush = $true
        [PSCustomObject]@{ Pipe = $pipe; Reader = $reader; Writer = $writer }
    }

    $script:NextId = 1
    function Send-TrixCommand {
        param([Parameter(Mandatory)]$Conn, [Parameter(Mandatory)][string]$Cmd, [int]$TimeoutMs = 5000)
        $id = $script:NextId
        $script:NextId++
        $payload = [ordered]@{ id = $id; cmd = $Cmd }
        $Conn.Writer.Write(($payload | ConvertTo-Json -Compress))
        $Conn.Writer.Write("`n")
        $Conn.Writer.Flush()

        $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
        while ($true) {
            $remainingMs = [int](($deadline - [DateTime]::UtcNow).TotalMilliseconds)
            if ($remainingMs -le 0) { throw "timed out waiting for a response to id=$id ($Cmd)" }
            $task = $Conn.Reader.ReadLineAsync()
            if (-not $task.Wait($remainingMs)) { throw "read timed out for id=$id ($Cmd)" }
            $line = $task.Result
            if ($null -eq $line) { throw "control pipe closed while waiting for id=$id ($Cmd)" }
            if (-not $line.Trim()) { continue }
            $obj = $line | ConvertFrom-Json
            if ($obj.PSObject.Properties.Name -notcontains 'id') { continue }
            if ($obj.PSObject.Properties.Name -notcontains 'ok') { continue }
            if ($obj.id -ne $id) { continue }
            return $obj
        }
    }

    # A single round trip against the control pipe: enough to prove the
    # daemon is alive and responsive, which is all this check needs.
    function Test-DaemonAnswers {
        try {
            $conn = Connect-TrixControl
            try {
                $r = Send-TrixCommand -Conn $conn -Cmd 'status'
                [PSCustomObject]@{ Ok = [bool]$r.ok; Error = $null }
            } finally {
                try { $conn.Reader.Dispose() } catch { }
                try { $conn.Writer.Dispose() } catch { }
                try { $conn.Pipe.Dispose() } catch { }
            }
        } catch {
            [PSCustomObject]@{ Ok = $false; Error = $_.Exception.Message }
        }
    }

    Write-Host ''
    Write-Host 'Renaming while running' -ForegroundColor Cyan

    $rename1 = Invoke-Guarded { Move-Item -LiteralPath $liveExe -Destination $oldExe -ErrorAction Stop }
    Check 'renaming trix-daemon.exe to trix-daemon.exe.old succeeds while it is running' `
        $rename1.Ok $rename1.Error

    $proc = Get-Process -Id $daemon.Id -ErrorAction SilentlyContinue
    $stillAlive = ($null -ne $proc) -and (-not $proc.HasExited)
    Check 'the daemon process is still alive after the rename' $stillAlive `
        'Windows tore the process down when its backing file was renamed -- the loader did not keep the running image mapped under its new name, which is the assumption the entire swap design rests on'

    $answer = Test-DaemonAnswers
    Check 'the daemon still answers on its control pipe after the rename' $answer.Ok $answer.Error

    $place = Invoke-Guarded {
        Set-Content -LiteralPath $liveExe -Value 'NEW trix-daemon.exe payload placeholder' -Encoding utf8 -ErrorAction Stop
    }
    $placed = $place.Ok -and (Test-Path -LiteralPath $liveExe)
    Check 'a new file can be placed at the name the rename vacated' $placed $place.Error

    Write-Host ''
    Write-Host 'Refusing to delete the running image' -ForegroundColor Cyan

    # The placeholder was only there to prove placement works; it has to
    # come out of the way before the running binary's original name can be
    # restored.
    Remove-Item -LiteralPath $liveExe -Force -ErrorAction SilentlyContinue

    $rename2 = Invoke-Guarded { Move-Item -LiteralPath $oldExe -Destination $liveExe -ErrorAction Stop }
    Check "the running binary's original name can be restored" $rename2.Ok $rename2.Error

    $delete = Invoke-Guarded { Remove-Item -LiteralPath $liveExe -Force -ErrorAction Stop }
    $deleteRefused = -not $delete.Ok
    $deleteDetail = 'Remove-Item deleted a file that is the running image of trix-daemon.exe -- Windows let a running executable be deleted, which means swap_in''s rename-then-move design is solving a problem that no longer exists on this machine, and the whole assumption needs re-checking'
    Check 'deleting the running image is refused' $deleteRefused $deleteDetail
} finally {
    if ($daemon -and -not $daemon.HasExited) {
        try { $daemon.Kill() } catch { }
        $daemon.WaitForExit(5000) | Out-Null
    }
    $env:APPDATA = $prevAppData
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}

Write-Host ''
if ($failures -gt 0) {
    Write-Host "UPDATE SMOKE FAILED ($failures)" -ForegroundColor Red
    exit 1
}
Write-Host 'UPDATE SMOKE PASSED' -ForegroundColor Green
