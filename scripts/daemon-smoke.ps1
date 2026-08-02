<#
.SYNOPSIS
    Stage-3 gate: the tray daemon does what spec §10 stage 3 says it does.

.DESCRIPTION
    protocol-smoke.ps1 proves the control protocol answers. arm-cycle-leak.ps1
    proves an arm/disarm round trip gives back what it took. This proves the
    things stage 3 added on top: that a clip arrives with all three of its
    files, that the ring reports itself honestly, that the disk ceiling deletes
    the right clips and spares favourites, that autostart really is the
    registry, and that the daemon stays inside §10.1's memory ceilings.

    Everything runs against a SCRATCH clip library and a SCRATCH %APPDATA%.
    That is not tidiness. This gate deliberately drives the disk ceiling until
    it deletes clips, and pointing that at a real library would delete a real
    person's recordings. It also rewrites config.toml, which strips the
    comments out of the reference file shipped in %APPDATA%\trix.

    The autostart check is the one thing that CANNOT be redirected: HKCU\...\Run
    with the value name "Trix" is the production path and is the thing under
    test. It is removed again at the end and the removal is asserted.

    Captures the real screen while armed, for a few seconds. The clips it makes
    live in the scratch library and are deleted, except the one named in the
    receipt, which is copied out for you to play.

    Requires a freshly built workspace:
        cargo build --release --workspace

.PARAMETER KeepArtifacts
    Leave the scratch library in place instead of deleting it. Useful when a
    check fails and you want to look at what it produced.
#>
[CmdletBinding()]
param(
    [string]$DaemonPath,
    [string]$CliPath,
    [int]$ReplaySeconds = 8,
    [switch]$KeepArtifacts
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:NextId = 1
$script:Checks = New-Object System.Collections.Generic.List[object]

# For the cursor nudge (see Start-ArmedWait) and for decoding the thumbnail.
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$RepoRoot = Split-Path -Parent $PSScriptRoot
if (-not $DaemonPath) { $DaemonPath = Join-Path $RepoRoot 'target\release\trix-daemon.exe' }
if (-not $CliPath) { $CliPath = Join-Path $RepoRoot 'target\release\trix.exe' }

$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

# --- Freshness ----------------------------------------------------------------
# Extracted from protocol-smoke.ps1 by AST rather than copied, so the three
# gates cannot drift apart on what "fresh" means. Same technique, and the same
# reason, as arm-cycle-leak.ps1.

$smoke = Join-Path $PSScriptRoot 'protocol-smoke.ps1'
if (-not (Test-Path -LiteralPath $smoke)) { throw "protocol-smoke.ps1 not found next to this script: $smoke" }
$ast = [System.Management.Automation.Language.Parser]::ParseFile($smoke, [ref]$null, [ref]$null)
$fn = $ast.FindAll({ param($n)
    $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $n.Name -eq 'Get-NewestSourceFile' }, $true) | Select-Object -First 1
if (-not $fn) { throw 'Get-NewestSourceFile not found in protocol-smoke.ps1' }
. ([scriptblock]::Create($fn.Extent.Text))

foreach ($exe in @($DaemonPath, $CliPath)) {
    if (-not (Test-Path -LiteralPath $exe)) {
        throw "not built: $exe`nRun: cargo build --release --workspace"
    }
}
# Only the daemon's timestamp is checked, deliberately. trix.exe does not
# depend on trix-daemon, so a daemon-only change leaves cargo with nothing to
# relink and trix.exe legitimately older than the newest source in the repo --
# which failed this gate on its first run for no real reason. A fresh daemon
# proves `cargo build --release --workspace` actually ran, and cargo's own
# dependency tracking covers whether trix.exe needed rebuilding.
$newestSource = Get-NewestSourceFile -RepoRoot $RepoRoot
$daemonWrite = (Get-Item -LiteralPath $DaemonPath).LastWriteTime
if ($daemonWrite -lt $newestSource.LastWriteTime) {
    throw ("trix-daemon.exe ($daemonWrite) is older than $($newestSource.Name) " +
           "($($newestSource.LastWriteTime)).`nRun: cargo build --release --workspace")
}
Write-Host "daemon is fresh (built $daemonWrite, newest source $($newestSource.Name) $($newestSource.LastWriteTime))"

# Refused rather than killed: it may be armed and recording for someone.
$running = @(Get-Process -Name 'trix-daemon' -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    throw ("trix-daemon is already running (PID $($running.Id -join ', ')). " +
           "This gate needs a process it started itself. Stop it and re-run.")
}
if (Get-ItemProperty -Path $RunKey -Name 'Trix' -ErrorAction SilentlyContinue) {
    throw ("HKCU\...\Run already has a Trix value. This gate asserts that it creates " +
           "and removes that value, and will not overwrite one you set yourself.")
}

# --- Checks -------------------------------------------------------------------

function Check {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][bool]$Condition, [string]$Detail = '')
    $status = if ($Condition) { 'PASS' } else { 'FAIL' }
    $line = "  [$status] $Name"
    # Only on FAIL. $Detail is remediation text, so appending it to a PASS
    # produces a healthy receipt that reads like a failure -- which is what
    # plan 2's gate did, and scannability is the whole point of this artefact.
    if ($Detail -and -not $Condition) { $line += " -- $Detail" }
    if ($Condition) { Write-Host $line -ForegroundColor Green } else { Write-Host $line -ForegroundColor Red }
    $script:Checks.Add([PSCustomObject]@{ Name = $Name; Passed = $Condition; Detail = $Detail })
}

# Deliberately NOT fail-fast, unlike protocol-smoke.ps1: this gate arms real
# hardware, writes real clips and touches the registry, so a run that stops at
# the first failure costs a minute to learn one fact and leaves the rest
# unmeasured. Every check runs, the receipt lists all of them, and the cleanup
# in `finally` always happens.

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
    param(
        [Parameter(Mandatory)]$Conn,
        [Parameter(Mandatory)][string]$Cmd,
        [hashtable]$Arguments = @{},
        [int]$TimeoutMs = 60000
    )
    $id = $script:NextId
    $script:NextId++
    $payload = [ordered]@{ id = $id; cmd = $Cmd }
    foreach ($key in $Arguments.Keys) { $payload[$key] = $Arguments[$key] }
    $Conn.Writer.Write(($payload | ConvertTo-Json -Compress -Depth 10))
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

function Invoke-Ok {
    param($Conn, [string]$Cmd, [hashtable]$Arguments = @{}, [int]$TimeoutMs = 60000)
    $r = Send-TrixCommand -Conn $Conn -Cmd $Cmd -Arguments $Arguments -TimeoutMs $TimeoutMs
    if (-not $r.ok) { throw "$Cmd failed: $($r.error)" }
    $r.data
}

# --- Scratch library ----------------------------------------------------------

$scratch = Join-Path $env:TEMP "trix-daemon-smoke-$PID"
$clipDir = Join-Path $scratch 'clips'
New-Item -ItemType Directory -Force (Join-Path $scratch 'trix') | Out-Null
New-Item -ItemType Directory -Force $clipDir | Out-Null

# Three fake clips, old enough to sort before anything this run records, sized
# so the 1 GB ceiling has to delete something. Sparse: NTFS reports the logical
# length that `library::scan` reads, without committing 1.2 GB of real disk.
$fakeIds = @('20200101_000001', '20200101_000002', '20200101_000003')
foreach ($id in $fakeIds) {
    $p = Join-Path $clipDir "$id.mp4"
    [System.IO.File]::Create($p).Close()
    & fsutil sparse setflag $p | Out-Null
    $fs = [System.IO.File]::OpenWrite($p)
    $fs.SetLength(400MB)
    $fs.Close()
}

$outLog = Join-Path $scratch 'daemon.out.log'
$errLog = Join-Path $scratch 'daemon.err.log'

# `max_library_gb = 0` to start: the ceiling is switched on mid-run, once the
# clip checks are done, so it cannot delete the clip they are inspecting.
#
# `clip_hotkey` is deliberately NOT the default. Alt+F10 is owned by the NVIDIA
# overlay on at least one development machine, and a gate that fails because
# somebody else holds a hotkey is measuring the wrong thing.
@"
replay_seconds = $ReplaySeconds
clip_dir = '$clipDir'
max_library_gb = 0
stats_seconds = 1
clip_hotkey = "ctrl+alt+shift+f9"
"@ | Out-File -FilePath (Join-Path $scratch 'trix\config.toml') -Encoding utf8

# `trix replay` gets a config of its own, with a DIFFERENT hotkey. Since Task 7
# the daemon registers the clip hotkey for as long as it runs, and `trix
# replay` treats a failed registration as fatal -- so pointed at the same
# config the two fight over the same combination and the CLI never starts.
$cliScratch = Join-Path $scratch 'cli-appdata'
New-Item -ItemType Directory -Force (Join-Path $cliScratch 'trix') | Out-Null
New-Item -ItemType Directory -Force (Join-Path $cliScratch 'clips') | Out-Null
@"
replay_seconds = $ReplaySeconds
clip_dir = '$(Join-Path $cliScratch 'clips')'
stats_seconds = 1
clip_hotkey = "ctrl+alt+shift+f8"
"@ | Out-File -FilePath (Join-Path $cliScratch 'trix\config.toml') -Encoding utf8

$env:APPDATA = $scratch

# --- Run ----------------------------------------------------------------------

Write-Host "starting $DaemonPath"
Write-Host "scratch library: $clipDir"
''
$daemon = Start-Process -FilePath $DaemonPath -ArgumentList '-v' -PassThru -WindowStyle Hidden `
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

function Get-WorkingSetMB {
    param([int]$DaemonPid)
    $p = Get-Process -Id $DaemonPid
    $p.Refresh()
    [math]::Round($p.WorkingSet64 / 1MB, 1)
}

# Windows Graphics Capture delivers a frame when the desktop composes. On a
# completely still screen it delivers nothing at all, the ring stays empty, and
# `clip` correctly answers "nothing buffered yet" -- which looks like a gate
# failure but is the capture working exactly as designed. After a few seconds
# of that, the cursor is moved one pixel and put straight back, which is enough
# to make the compositor produce frames without disturbing anything.
# A small animated window, shown for as long as the daemon is armed.
#
# Not decoration, and not merely to stop the ring draining. Windows Graphics
# Capture only delivers a frame when the desktop composes, so an idle screen
# produces almost no frames -- and the GOP is a count of FRAMES, not of
# seconds: `gop_size` is `fps`, which is one second only while frames actually
# arrive at `fps`. Measured on a still desktop the daemon saw ~3 fps, making a
# 60-frame GOP span twenty seconds of wall clock and the clip snap back to a
# keyframe far outside its nominal window -- a 10.4 s clip from an 8 s ring,
# with the GOP pin working perfectly. Every duration and frame-count check here
# is therefore only meaningful against a screen that is genuinely changing, and
# this is what guarantees one.
function New-ActivityWindow {
    $form = New-Object System.Windows.Forms.Form
    $form.FormBorderStyle = [System.Windows.Forms.FormBorderStyle]::None
    $form.StartPosition = [System.Windows.Forms.FormStartPosition]::Manual
    $form.Size = New-Object System.Drawing.Size(420, 54)
    $form.Location = New-Object System.Drawing.Point(60, 60)
    $form.TopMost = $true
    $form.ShowInTaskbar = $false
    $form.BackColor = [System.Drawing.Color]::FromArgb(20, 22, 26)
    $label = New-Object System.Windows.Forms.Label
    $label.Dock = [System.Windows.Forms.DockStyle]::Fill
    $label.ForeColor = [System.Drawing.Color]::Gainsboro
    $label.TextAlign = [System.Drawing.ContentAlignment]::MiddleCenter
    $label.Font = New-Object System.Drawing.Font('Consolas', 11)
    $form.Controls.Add($label)
    $form.Show()
    [PSCustomObject]@{ Form = $form; Label = $label; Tick = 0 }
}

function Update-Activity {
    param($Activity)
    if (-not $Activity) { return }
    $Activity.Tick++
    # A moving marquee: the redraw is what dirties the screen, at whatever rate
    # this is called.
    $width = 24
    $at = $Activity.Tick % $width
    $bar = (' ' * $at) + '<##>' + (' ' * ($width - $at))
    $Activity.Label.Text = "stage-3 gate recording  [$bar]"
    $Activity.Label.Refresh()
    [System.Windows.Forms.Application]::DoEvents()
}

# Every wait taken while armed goes through here, not Start-Sleep.
function Start-ArmedWait {
    param([double]$Seconds, $Activity)
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        Update-Activity $Activity
        Start-Sleep -Milliseconds 16
    }
}

function Wait-ForRing {
    param($Conn, [double]$WantSeconds, $Activity, [int]$TimeoutSeconds = 90)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $nextPoll = [DateTime]::UtcNow
    while ((Get-Date) -lt $deadline) {
        # Polled at 400 ms but animated at ~60 Hz: a `status` round trip per
        # frame would itself become the reason frames stop arriving.
        if ([DateTime]::UtcNow -ge $nextPoll) {
            $st = Invoke-Ok $Conn 'status'
            if ($st.ring_seconds_used -ge $WantSeconds) { return $st }
            $nextPoll = [DateTime]::UtcNow.AddMilliseconds(400)
        }
        Update-Activity $Activity
        Start-Sleep -Milliseconds 16
    }
    throw ("the ring never reached $WantSeconds s in $TimeoutSeconds s. No frames are being " +
           "delivered: check the display is not asleep and that monitor_index names a real monitor.")
}

$conn = $null
$receiptClip = $null
$activity = $null
try {
    $conn = Connect-TrixControl

    # --- §10.1 idle ceiling ---------------------------------------------------
    Start-Sleep -Seconds 3
    $idleMB = Get-WorkingSetMB $daemon.Id
    Check "idle working set is under 30 MB (§10.1)  [$idleMB MB]" ($idleMB -lt 30) `
        "idle is where the daemon spends nearly all its life; this is the ceiling that carries the product's promise"

    # --- arm ------------------------------------------------------------------
    $armed = Invoke-Ok $conn 'arm'
    Check 'arm reports an encoder by name' ([bool]$armed.encoder) `
        'arm must not answer with a null encoder'
    Write-Host "        encoder: $($armed.encoder)"

    # The driver settles first, then the ring is confirmed full. Both matter
    # before the sample: a ring still growing and an allocator still ramping
    # both read low, and the clip below needs a full ring to have a duration
    # worth checking.
    $activity = New-ActivityWindow
    Start-ArmedWait 10 $activity
    $st = Wait-ForRing $conn ($ReplaySeconds * 0.95) $activity
    $armedMB = Get-WorkingSetMB $daemon.Id
    Check "armed working set is under 200 MB (§10.1)  [$armedMB MB]" ($armedMB -lt 200) `
        'most of this is the GPU vendor stack, but the ceiling is what catches a leak or a stray buffer'

    # --- ring honesty ---------------------------------------------------------
    Check ("ring_seconds_used <= ring_seconds_total  [$($st.ring_seconds_used) / $($st.ring_seconds_total)]") `
        ($st.ring_seconds_used -le $st.ring_seconds_total) `
        'a progress bar divides one by the other and would render past 100%'

    # --- the clip and all three of its files ----------------------------------
    $clip = Invoke-Ok $conn 'clip'
    $id = $clip.id
    Check 'clip returned an id' ([bool]$id)
    $mp4 = Join-Path $clipDir "$id.mp4"
    $json = Join-Path $clipDir "$id.json"
    $jpg = Join-Path $clipDir "$id.jpg"
    Check 'the .mp4 exists' (Test-Path -LiteralPath $mp4)
    Check 'the .json sidecar exists' (Test-Path -LiteralPath $json)
    Check 'the .jpg thumbnail exists (§5.3)' (Test-Path -LiteralPath $jpg) `
        'a clip with no preview is a hole in the library grid'

    if (Test-Path -LiteralPath $jpg) {
        Add-Type -AssemblyName System.Drawing
        $img = [System.Drawing.Image]::FromFile($jpg)
        $tw = $img.Width; $th = $img.Height
        $img.Dispose()
        Check "the thumbnail decodes as an image  [${tw}x${th}]" (($tw -gt 0) -and ($th -gt 0))
    }

    # --- duration ------------------------------------------------------------
    # NOT a percentage band. The ring evicts whole GOPs because a clip cannot
    # start mid-GOP, so it holds between `replay_seconds` and `replay_seconds`
    # plus one keyframe interval -- an ABSOLUTE overshoot the GOP pin bounds to
    # under a second, not a proportional one. A 10% band happens to fit a 20 s
    # ring and cannot fit an 8 s one, and it also permits a clip that is too
    # SHORT, which is the failure that actually costs the user footage.
    $want = $ReplaySeconds * 1000
    $got = $clip.duration_ms
    $floor = $want - 100
    $ceil = $want + 1200
    Check "clip duration is at least replay_seconds  [$got ms >= $floor ms]" ($got -ge $floor) `
        'a clip shorter than configured has lost footage the user asked for'
    Check "clip duration overshoots by under one GOP  [$got ms <= $ceil ms]" ($got -le $ceil) `
        'the one-second GOP pin is what bounds this; a larger overshoot means it is not being applied'

    # --- paced/dropped against the CLI path -----------------------------------
    # The daemon and `trix replay` share the whole capture path; if the daemon
    # drops frames the CLI does not, the daemon's own wiring is at fault.
    function Get-DaemonCounters {
        $line = (Get-Content $outLog, $errLog -ErrorAction SilentlyContinue |
            Where-Object { $_ -match 'frames=\d+ dropped=\d+ paced=\d+' } | Select-Object -Last 1)
        if ($line -match 'frames=(\d+) dropped=(\d+) paced=(\d+)') {
            return [PSCustomObject]@{ frames = [int]$Matches[1]; dropped = [int]$Matches[2]; paced = [int]$Matches[3] }
        }
        return $null
    }
    $daemonCounters = Get-DaemonCounters
    Check 'the daemon reported frame counters' ([bool]$daemonCounters)

    $null = Invoke-Ok $conn 'disarm'
    Start-Sleep -Seconds 5

    # The CLI takes the single-instance slot, so this can only run disarmed.
    $cliOut = Join-Path $scratch 'cli.log'
    # Windows PowerShell's Start-Process cannot set a child's environment, so
    # the variable is swapped around the launch and put straight back. The
    # daemon inherited its own copy when it started and is unaffected.
    $env:APPDATA = $cliScratch
    try {
        $cliRun = Start-Process -FilePath $CliPath -ArgumentList @('-v', 'replay', '--exit-after', '12') `
            -PassThru -WindowStyle Hidden -RedirectStandardOutput $cliOut -RedirectStandardError "$cliOut.err"
        $cliRun.WaitForExit(60000) | Out-Null
    } finally { $env:APPDATA = $scratch }
    $cliLine = (Get-Content $cliOut, "$cliOut.err" -ErrorAction SilentlyContinue |
        Where-Object { $_ -match 'frames=\d+ dropped=\d+ paced=\d+' } | Select-Object -Last 1)
    if ($cliLine -match 'frames=(\d+) dropped=(\d+) paced=(\d+)') {
        $cliCounters = [PSCustomObject]@{ frames = [int]$Matches[1]; dropped = [int]$Matches[2]; paced = [int]$Matches[3] }
        Write-Host ("        daemon frames={0} dropped={1} paced={2}" -f $daemonCounters.frames, $daemonCounters.dropped, $daemonCounters.paced)
        Write-Host ("        CLI    frames={0} dropped={1} paced={2}" -f $cliCounters.frames, $cliCounters.dropped, $cliCounters.paced)
        # A rate, not a count: the two runs are different lengths.
        $daemonDropRate = $daemonCounters.dropped / [math]::Max($daemonCounters.frames, 1)
        $cliDropRate = $cliCounters.dropped / [math]::Max($cliCounters.frames, 1)
        Check ("the daemon drops no more than the CLI path  [{0:P2} vs {1:P2}]" -f $daemonDropRate, $cliDropRate) `
            ($daemonDropRate -le ($cliDropRate + 0.02)) `
            'the daemon and trix.exe run identical capture code; a gap is the daemon wiring'
    } else {
        $why = (Get-Content "$cliOut.err" -ErrorAction SilentlyContinue | Select-Object -Last 3) -join ' / '
        Check 'the CLI replay path reported frame counters' $false `
            "no stats line in $cliOut ($why)"
    }

    # --- the disk ceiling -----------------------------------------------------
    # Switched on only now, so it cannot have deleted the clip checked above.
    $favId = $fakeIds[0]
    $doomedId = $fakeIds[1]
    $sparedId = $fakeIds[2]
    $null = Invoke-Ok $conn 'library.favorite' @{ clip_id = $favId; favorite = $true }
    $null = Invoke-Ok $conn 'config.set' @{ values = @{ max_library_gb = 1 } }

    $null = Invoke-Ok $conn 'arm'
    # Only enough footage to have something to save; this clip exists to
    # trigger the ceiling, not to be measured.
    $null = Wait-ForRing $conn 1.0 $activity
    $ceilingClip = Invoke-Ok $conn 'clip'
    $null = Invoke-Ok $conn 'disarm'
    Start-Sleep -Seconds 2

    Check "the ceiling deleted the oldest non-favourite  [$doomedId]" `
        (-not (Test-Path -LiteralPath (Join-Path $clipDir "$doomedId.mp4"))) `
        'spec §5.4: over the ceiling, oldest non-favourites go first'
    Check "the ceiling spared the favourite  [$favId]" `
        (Test-Path -LiteralPath (Join-Path $clipDir "$favId.mp4")) `
        'a favourite is the user saying "never delete this"'
    Check "the ceiling stopped once it was under  [$sparedId kept]" `
        (Test-Path -LiteralPath (Join-Path $clipDir "$sparedId.mp4")) `
        'the ceiling must delete the minimum, not empty the library'
    Check 'the clip that triggered the ceiling survived it' `
        (Test-Path -LiteralPath (Join-Path $clipDir "$($ceilingClip.id).mp4")) `
        'pruning must never eat the clip the user just asked for'

    # --- autostart is the registry --------------------------------------------
    $null = Invoke-Ok $conn 'config.set' @{ values = @{ autostart = $true } }
    $runValue = (Get-ItemProperty -Path $RunKey -Name 'Trix' -ErrorAction SilentlyContinue).Trix
    Check 'config.set autostart=true creates the Run value (§7.3)' ([bool]$runValue)
    if ($runValue) {
        Check 'the Run value is quoted and points at trix-daemon.exe' `
            (($runValue -like '"*"') -and ($runValue -like '*trix-daemon.exe*')) `
            "an unquoted path with spaces is parsed as a command plus arguments; got $runValue"
    }
    $cfg = Invoke-Ok $conn 'config.get'
    Check 'config.get reports autostart on' ([bool]$cfg.autostart)

    # The point of the design: the registry decides, not the file.
    Remove-ItemProperty -Path $RunKey -Name 'Trix' -ErrorAction SilentlyContinue
    $cfg = Invoke-Ok $conn 'config.get'
    Check 'deleting the Run value behind the daemon flips config.get to off' (-not $cfg.autostart) `
        'the registry is the source of truth; a settings page must not keep showing the toggle on'
    $null = Invoke-Ok $conn 'config.set' @{ values = @{ autostart = $false } }
    Check 'no Run value is left behind' `
        (-not (Get-ItemProperty -Path $RunKey -Name 'Trix' -ErrorAction SilentlyContinue))

    # --- the clip directory is proved, not assumed ----------------------------
    # `clip_dir` is the one setting whose value can be wrong in a way that
    # checking the *string* would never catch: a typo and an unplugged drive
    # are both perfectly well-formed paths. Accepting one aims every future
    # clip at a folder that swallows it, and the user finds out at the moment
    # they wanted the clip. So the daemon proves the folder by writing to it.
    $moved = Join-Path $scratch 'moved-clips\Trix'
    Check 'the new clips folder does not exist yet' (-not (Test-Path -LiteralPath $moved))
    $set = Invoke-Ok $conn 'config.set' @{ values = @{ clip_dir = $moved } }
    Check 'config.set creates the clip directory it accepted' (Test-Path -LiteralPath $moved) $moved
    Check 'changing clip_dir needs no re-arm' ($set.requires_rearm.Count -eq 0) `
        'clip_dir is read per clip; a UI must not prompt to restart capture for it'
    $probes = @(Get-ChildItem -LiteralPath $moved -Force -ErrorAction SilentlyContinue)
    # Piped rather than `$probes.Name`: `Check`'s detail argument is evaluated
    # on the passing path too, and member enumeration over an empty array is an
    # error under `Set-StrictMode -Version Latest`.
    $probeNames = ($probes | ForEach-Object { $_.Name }) -join ', '
    Check 'the writability probe is cleaned up' ($probes.Count -eq 0) "left behind: $probeNames"

    # A directory whose parent is a *file* can never be created. It is the
    # portable stand-in for the real cases -- an unplugged drive, a folder that
    # needs admin -- neither of which a gate can conjure on demand.
    $blocker = Join-Path $scratch 'blocker'
    'not a directory' | Out-File -FilePath $blocker -Encoding utf8
    $refused = Send-TrixCommand $conn 'config.set' `
        @{ values = @{ clip_dir = (Join-Path $blocker 'clips') } }
    Check 'config.set refuses a clip directory it cannot create' (-not $refused.ok) `
        ($refused | ConvertTo-Json -Compress)
    $cfg = Invoke-Ok $conn 'config.get'
    Check 'a refused clip_dir leaves the working one in force' ($cfg.clip_dir -eq $moved) `
        "config.get reports $($cfg.clip_dir); a half-applied settings write is the bug this guards"

    # Back to the scratch library: the receipt below reads clips out of it.
    $null = Invoke-Ok $conn 'config.set' @{ values = @{ clip_dir = $clipDir } }

    $receiptClip = $clip
}
finally {
    if ($activity) { try { $activity.Form.Close(); $activity.Form.Dispose() } catch { } }
    if ($conn) {
        try { $conn.Reader.Dispose() } catch { }
        try { $conn.Writer.Dispose() } catch { }
        try { $conn.Pipe.Dispose() } catch { }
    }
    if (-not $daemon.HasExited) { $daemon.Kill(); $daemon.WaitForExit(5000) }
    # Belt and braces: the registry is the one thing here that is NOT scratch.
    Remove-ItemProperty -Path $RunKey -Name 'Trix' -ErrorAction SilentlyContinue
}

# --- Receipt ------------------------------------------------------------------

''
$passed = @($script:Checks | Where-Object { $_.Passed }).Count
$failed = @($script:Checks | Where-Object { -not $_.Passed }).Count

if ($receiptClip -and -not $KeepArtifacts) {
    # The clip named in the receipt is copied out before the scratch library
    # goes: a gate that says "play this clip" and then deletes it is not a gate.
    $keep = Join-Path ([Environment]::GetFolderPath('MyVideos')) 'Trix-gate'
    New-Item -ItemType Directory -Force $keep | Out-Null
    foreach ($ext in @('mp4', 'json', 'jpg')) {
        $src = Join-Path $clipDir "$($receiptClip.id).$ext"
        if (Test-Path -LiteralPath $src) { Copy-Item $src $keep -Force }
    }
    Write-Host "PLAY THIS:  $(Join-Path $keep "$($receiptClip.id).mp4")" -ForegroundColor Cyan
    Write-Host "AND OPEN:   $(Join-Path $keep "$($receiptClip.id).jpg")  (should be the moment of the clip, not a black frame)" -ForegroundColor Cyan
}

if ($KeepArtifacts) {
    Write-Host "scratch library kept at $clipDir"
} else {
    Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue
}

''
if ($failed -gt 0) {
    Write-Host "STAGE-3 GATE FAILED  ($passed passed, $failed failed)" -ForegroundColor Red
    foreach ($c in $script:Checks | Where-Object { -not $_.Passed }) {
        Write-Host "  - $($c.Name)" -ForegroundColor Red
    }
    exit 1
}
Write-Host "STAGE-3 GATE PASSED  ($passed checks)" -ForegroundColor Green
Write-Host 'Machine-green is not stage 3: play the clip and open the thumbnail above.'
