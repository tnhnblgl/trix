<#
.SYNOPSIS
    Fails if an arm/disarm cycle does not give back what it took.

.DESCRIPTION
    Trix is meant to sit armed through a whole gaming session on a low-end PC,
    so memory that an arm/disarm round trip never returns is a product defect,
    not a tidiness issue. This gate arms and disarms repeatedly and measures the
    daemon between cycles.

    Every sample is taken while DISARMED and settled. Memory a live ring is
    holding is not a leak; memory still held with nothing armed is.

    "Settled" is load-bearing and was measured, not guessed. After a disarm the
    Intel graphics/Media Foundation stack holds its allocations on a plateau for
    5-10 seconds and then releases them in one step. Sampling inside that
    plateau charges ~76 MB of driver memory to Trix: at a 2 s settle this daemon
    reads 105 MB private, and at 12 s the same daemon reads 19 MB. The default
    below therefore sits past the cliff. Anyone shortening it is measuring the
    driver, not a leak -- which is exactly what the numbers recorded on
    2026-08-01 (97-101 MB disarmed) turned out to be.

    What it caught when it was written: the hardware encoder MFT was created
    with IMFActivate::ActivateObject and never shut down with ShutdownObject.
    Releasing the IMFTransform is not enough -- the MFT kept its D3D device
    reference, its Media Foundation work-queue thread and its driver handles,
    so every cycle leaked ~50 MB of GPU memory and ~73 MB of private bytes,
    for the life of the process. Six cycles cost half a gigabyte.

    Threads are measured alongside bytes deliberately: that leak showed up as
    +2 threads per cycle long before the byte count was conclusive, because
    thread counts do not oscillate the way working set does.

    Handles are gated loosely, and the reason matters. Measured 2026-08-01, an
    arm/disarm cycle leaks ~6 handles that never come back, and almost none of
    them are ours to give back:

      ~3.0/cycle  activating the hardware encoder MFT and shutting it down
                  again -- with no D3D manager, no media types, no streaming
                  messages and no frames attached. Proven by the ignored test
                  `bare_mft_activation_cycles` in encode/h264.rs, which does
                  nothing but ActivateObject/ShutdownObject in a loop and still
                  climbs by exactly 3 every cycle. This is the Intel Quick Sync
                  driver's activation cost; ShutdownObject is already called.
      ~2.3/cycle  ALPC ports from one WGC capture session. `windows-capture`
                  does remove its FrameArrived/Closed handlers and Close() both
                  the frame pool and the session, and RoUninitialize()s its
                  thread, so these are the platform's side of the DWM channel.
      ~1.0/cycle  the remainder of the arm path (device manager, ring setup).
      ~0.1/cycle  audio -- flat within noise across 8 cycles.

    Attribution came from `crates/trix-core/tests/handle_leak.rs`, which cycles
    one subsystem at a time in-process. Do not re-derive it by staring at
    HandleCount: a process-wide number cannot say which half of an arm leaks,
    and the object-type histogram (ALPC Port / Event / WaitCompletionPacket /
    Key) is what made the split obvious.

    The one change that would reclaim these is keeping the encoder and capture
    session alive across a disarm -- which is exactly the thing whose removal
    gave back ~50 MB of GPU memory and ~73 MB of private bytes per cycle. Trix
    is a tool that sits idle at ~10 MB waiting for a game; paying 130 MB of
    permanent idle footprint to avoid 6 handles is the wrong trade, so the
    floor stays and this gate guards the ceiling above it instead.

    The limit therefore sits well above the platform floor: it exists to catch
    a *new* leak of Trix's own making (the ShutdownObject bug was ~37/cycle),
    not to fail on the ~6 the driver stack keeps. A different GPU vendor will
    have a different floor -- if this fails on an AMD or NVIDIA machine, verify
    with the tests above before believing it.

    Requires a freshly built daemon:
        cargo build --release --workspace
    The binary's timestamp is checked against the newest source file, for the
    same reason protocol-smoke.ps1 checks it -- a gate that runs green against
    a stale binary proves nothing.

    Captures the real screen while armed. Saves no clips and writes no config.

.PARAMETER Cycles
    Arm/disarm round trips. Fewer than 4 cannot establish a slope.

.PARAMETER MaxMbPerCycle
    Private-bytes growth per cycle that fails the gate. Default 20 MB, against
    a measured post-fix figure of ~3 MB/cycle and a pre-fix figure of ~73.

.PARAMETER MaxThreadsPerCycle
    Thread growth per cycle that fails the gate. Default 1.0, against a
    measured post-fix figure of ~0 and a pre-fix figure of 2.

.PARAMETER MaxHandlesPerCycle
    Handle growth per cycle that fails the gate. Default 15, against a measured
    platform floor of ~6 (see above) and the ~37 the pre-ShutdownObject leak
    produced. Chosen to sit between the two rather than snugly above the floor,
    so the gate is not a tripwire for driver-version noise.
#>
[CmdletBinding()]
param(
    [ValidateRange(4, 200)][int]$Cycles = 10,
    [int]$ArmSeconds = 2,
    [int]$SettleSeconds = 12,
    [string]$DaemonPath,
    [double]$MaxMbPerCycle = 20,
    [double]$MaxThreadsPerCycle = 1.0,
    [double]$MaxHandlesPerCycle = 15
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:NextId = 1

$RepoRoot = Split-Path -Parent $PSScriptRoot
if (-not $DaemonPath) { $DaemonPath = Join-Path $RepoRoot 'target\release\trix-daemon.exe' }

# --- Freshness ----------------------------------------------------------------
# Get-NewestSourceFile is extracted from protocol-smoke.ps1 by AST rather than
# copied, so the two gates cannot drift apart on what "fresh" means. Same
# technique protocol-smoke-selftest.ps1 uses, for the same reason.

$smoke = Join-Path $PSScriptRoot 'protocol-smoke.ps1'
if (-not (Test-Path -LiteralPath $smoke)) { throw "protocol-smoke.ps1 not found next to this script: $smoke" }
$ast = [System.Management.Automation.Language.Parser]::ParseFile($smoke, [ref]$null, [ref]$null)
$fn = $ast.FindAll({ param($n)
    $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $n.Name -eq 'Get-NewestSourceFile' }, $true) | Select-Object -First 1
if (-not $fn) { throw 'Get-NewestSourceFile not found in protocol-smoke.ps1' }
. ([scriptblock]::Create($fn.Extent.Text))

if (-not (Test-Path -LiteralPath $DaemonPath)) {
    throw "daemon not built: $DaemonPath`nRun: cargo build --release --workspace"
}
$newestSource = Get-NewestSourceFile -RepoRoot $RepoRoot
$daemonWrite = (Get-Item -LiteralPath $DaemonPath).LastWriteTime
if ($daemonWrite -lt $newestSource.LastWriteTime) {
    throw ("trix-daemon.exe ($daemonWrite) is older than $($newestSource.Name) " +
           "($($newestSource.LastWriteTime)).`nRun: cargo build --release --workspace")
}
Write-Host "daemon is fresh (built $daemonWrite, newest source $($newestSource.Name) $($newestSource.LastWriteTime))"

# A daemon already running would make the baseline meaningless, and it may be
# armed and recording for someone. Refuse rather than kill it.
$running = @(Get-Process -Name 'trix-daemon' -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    throw ("trix-daemon is already running (PID $($running.Id -join ', ')). " +
           "This gate needs a process it started itself. Stop it and re-run.")
}

# --- Pipe client --------------------------------------------------------------

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

# Correlated by id: responses share the stream with events, so a line without
# our id is skipped rather than mistaken for the answer.
function Send-TrixCommand {
    param([Parameter(Mandatory)]$Conn, [Parameter(Mandatory)][string]$Cmd, [int]$TimeoutMs = 30000)
    $id = $script:NextId
    $script:NextId++
    $Conn.Writer.Write((@{ id = $id; cmd = $Cmd } | ConvertTo-Json -Compress))
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

function Get-Sample {
    param([Parameter(Mandatory)][int]$DaemonPid, [Parameter(Mandatory)][string]$Phase)
    $p = Get-Process -Id $DaemonPid
    $p.Refresh()
    [PSCustomObject]@{
        Phase     = $Phase
        PrivateMB = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
        WorkingMB = [math]::Round($p.WorkingSet64 / 1MB, 1)
        Handles   = $p.HandleCount
        Threads   = $p.Threads.Count
    }
}

# --- Run ----------------------------------------------------------------------

$outLog = Join-Path $env:TEMP 'trix-arm-cycle-leak.out.log'
$errLog = Join-Path $env:TEMP 'trix-arm-cycle-leak.err.log'
foreach ($f in @($outLog, $errLog)) { if (Test-Path $f) { Remove-Item $f -Force } }

Write-Host "starting $DaemonPath"
$daemon = Start-Process -FilePath $DaemonPath -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput $outLog -RedirectStandardError $errLog

$ready = $false
$deadline = (Get-Date).AddSeconds(10)
while ((Get-Date) -lt $deadline) {
    try {
        $probe = New-Object System.IO.Pipes.NamedPipeClientStream(
            '.', 'trix-control', [System.IO.Pipes.PipeDirection]::InOut)
        $probe.Connect(250)
        $probe.Dispose()
        $ready = $true
        break
    } catch { Start-Sleep -Milliseconds 200 }
}
if (-not $ready) { throw "daemon never opened the control pipe; see $errLog" }

$conn = $null
$samples = New-Object System.Collections.Generic.List[object]
try {
    $conn = Connect-TrixControl
    Start-Sleep -Seconds 2
    $samples.Add((Get-Sample -DaemonPid $daemon.Id -Phase 'before any arm'))

    for ($i = 1; $i -le $Cycles; $i++) {
        $armed = Send-TrixCommand -Conn $conn -Cmd 'arm'
        if (-not $armed.ok) { throw "arm failed on cycle ${i}: $($armed.error)" }
        Start-Sleep -Seconds $ArmSeconds

        $off = Send-TrixCommand -Conn $conn -Cmd 'disarm'
        if (-not $off.ok) { throw "disarm failed on cycle ${i}: $($off.error)" }
        Start-Sleep -Seconds $SettleSeconds

        $s = Get-Sample -DaemonPid $daemon.Id -Phase "after cycle $i"
        $samples.Add($s)
        Write-Host ("  cycle {0,-3} private {1,7} MB   handles {2,5}   threads {3,3}" -f `
            $i, $s.PrivateMB, $s.Handles, $s.Threads)
    }
}
finally {
    if ($conn) {
        try { $conn.Reader.Dispose() } catch { }
        try { $conn.Writer.Dispose() } catch { }
        try { $conn.Pipe.Dispose() } catch { }
    }
    if (-not $daemon.HasExited) { $daemon.Kill(); $daemon.WaitForExit(5000) }
}

# --- Verdict ------------------------------------------------------------------
# The slope is measured from "after cycle 1", not from startup: the first arm
# pays one-time costs (encoder DLLs, driver init) that are not per-cycle and
# would otherwise be charged to the leak.

''
$samples | Format-Table -AutoSize | Out-String -Width 200

$first = $samples[1]
$last = $samples[$samples.Count - 1]
$span = $Cycles - 1
$mbPerCycle = [math]::Round(($last.PrivateMB - $first.PrivateMB) / $span, 2)
$threadsPerCycle = [math]::Round(($last.Threads - $first.Threads) / $span, 2)
$handlesPerCycle = [math]::Round(($last.Handles - $first.Handles) / $span, 2)

"measured over $span cycles (excluding the first):"
"  private bytes  {0,8} MB/cycle   (limit {1})" -f $mbPerCycle, $MaxMbPerCycle
"  threads        {0,8} /cycle     (limit {1})" -f $threadsPerCycle, $MaxThreadsPerCycle
"  handles        {0,8} /cycle     (limit {1}, platform floor ~6)" -f $handlesPerCycle, $MaxHandlesPerCycle
''

$failures = New-Object System.Collections.Generic.List[string]
if ($mbPerCycle -gt $MaxMbPerCycle) {
    $failures.Add("private bytes grew $mbPerCycle MB/cycle (limit $MaxMbPerCycle)")
}
if ($threadsPerCycle -gt $MaxThreadsPerCycle) {
    $failures.Add("thread count grew $threadsPerCycle /cycle (limit $MaxThreadsPerCycle)")
}
if ($handlesPerCycle -gt $MaxHandlesPerCycle) {
    $failures.Add(("handle count grew $handlesPerCycle /cycle (limit $MaxHandlesPerCycle). " +
                   'The ~6/cycle floor is the driver stack; this is above it, so suspect ' +
                   'Trix. crates/trix-core/tests/handle_leak.rs attributes it by subsystem.'))
}

if ($failures.Count -gt 0) {
    Write-Host 'LEAK GATE FAILED' -ForegroundColor Red
    foreach ($f in $failures) { Write-Host "  - $f" -ForegroundColor Red }
    "daemon log: $outLog"
    exit 1
}

Write-Host 'LEAK GATE PASSED — an arm/disarm cycle returns what it took' -ForegroundColor Green
"daemon log: $outLog"
