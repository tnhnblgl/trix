<#
.SYNOPSIS
    Stage 2 gate for the Trix control protocol (spec §10, item 2 and 3):
    drives a scripted client through arm -> status -> clip -> library.list ->
    disarm over the \\.\pipe\trix-control named pipe, then confirms the CLI's
    single-instance slot comes back. Exits nonzero on any failure.

.DESCRIPTION
    This script does not build anything and does not hand-verify playback —
    it only proves the wire protocol and the on-disk artifacts it produces.
    Run `cargo build --release --workspace` first. The `--workspace` is not
    decoration: without it cargo once skipped trix-daemon entirely, and this
    gate would then run fully green against whatever trix-daemon.exe happened
    to be lying in target\release. Step 2 no longer takes that on trust -- it
    compares the binary's write time against the newest file under crates\
    and fails if the binary is the older of the two.

    Playing the resulting clip back (video and audio in sync) is a separate,
    manual step this script cannot perform.

.PARAMETER DaemonPath
    Path to trix-daemon.exe. Defaults to target\release\trix-daemon.exe next
    to this script's repo root.

.PARAMETER TrixPath
    Path to trix.exe. Defaults to target\release\trix.exe next to this
    script's repo root.
#>
param(
    [string]$DaemonPath,
    [string]$TrixPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# --- Paths ------------------------------------------------------------------

$RepoRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrEmpty($DaemonPath)) {
    $DaemonPath = Join-Path $RepoRoot 'target\release\trix-daemon.exe'
}
if ([string]::IsNullOrEmpty($TrixPath)) {
    $TrixPath = Join-Path $RepoRoot 'target\release\trix.exe'
}

# --- Bookkeeping --------------------------------------------------------------

$script:Checks = New-Object System.Collections.Generic.List[Object]
$script:NextId = 1

function Get-JsonProp {
    # Safe optional-property read for a ConvertFrom-Json PSCustomObject.
    # Response.error and Response.data are omitted from the wire entirely
    # (serde's skip_serializing_if) rather than sent as null, so a plain
    # `$obj.error` throws under Set-StrictMode whenever the property was
    # never there -- this checks the PSObject property collection instead
    # of dot-accessing the possibly-missing member.
    param($Obj, [string]$Name)
    if ($null -eq $Obj) { return $null }
    $prop = $Obj.PSObject.Properties[$Name]
    if ($null -eq $prop) { return $null }
    return $prop.Value
}

function Confirm-Check {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][bool]$Condition,
        [string]$Detail = ''
    )
    $status = if ($Condition) { 'PASS' } else { 'FAIL' }
    $line = "  [$status] $Name"
    # Only on FAIL. $Detail is written as remediation text ("unlock the session
    # and re-run"), so appending it to a PASS produced receipts that read
    # "[PASS] session is not locked ... -- unlock the session and re-run" --
    # scannability is the entire point of this artefact.
    if ($Detail -and -not $Condition) { $line += " -- $Detail" }
    Write-Host $line
    $script:Checks.Add([PSCustomObject]@{ Name = $Name; Passed = $Condition; Detail = $Detail })
    if (-not $Condition) {
        throw "gate check failed: $Name -- $Detail"
    }
}

# There is deliberately no version check here any more.
#
# What used to live at this spot compared the live `status` response's
# `version` field against `cargo metadata`'s trix-daemon version, and was
# described as one of two independent stale-daemon defenses. It was not a
# defense at all: both sides read the inherited workspace version, so it was
# `0.1.0 -eq 0.1.0` forever and could not fail under any circumstance --
# including the exact one it was written for. Worse, it *read* as protection,
# which is how the gate came to have no working staleness check while appearing
# to have two. (It also ran `cargo metadata ... 2>$null` under
# $ErrorActionPreference = 'Stop', so a cargo that wrote "Blocking waiting for
# file lock" to stderr while exiting 0 would abort the whole gate at step 2.)
#
# Its job is now done by Get-NewestSourceFile below, which compares real
# timestamps and can actually be wrong. Giving the version check teeth instead
# would mean stamping a build identity (a build.rs GIT_SHA surfaced in
# `status`); that is a real option, but a defense that exists is worth more
# than a defense that reads well.

function Get-NewestSourceFile {
    # The most recently written .rs / Cargo.toml under crates\. Compared
    # against trix-daemon.exe's own LastWriteTime in Step 2, which is what
    # proves the gate is exercising the code that is checked out rather than
    # asking the operator to promise it.
    #
    # Deliberately a timestamp comparison and not a version string: the failure
    # this catches is "you edited source and forgot to rebuild", or "you ran
    # plain `cargo build --release`, which used to skip trix-daemon entirely".
    # Neither of those changes any version anywhere.
    #
    # Pulled out as its own function so the self-test can drive it, and so the
    # $null case (an empty crates\ tree) is handled in one place rather than at
    # the call site under Set-StrictMode.
    param([Parameter(Mandatory)][string]$RepoRoot)
    $crates = Join-Path $RepoRoot 'crates'
    if (-not (Test-Path -LiteralPath $crates)) {
        throw "no crates directory under $RepoRoot -- cannot check the daemon binary's freshness"
    }
    $newest = Get-ChildItem -Path $crates -Recurse -Include *.rs, Cargo.toml -File |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($null -eq $newest) {
        throw "no .rs or Cargo.toml files under $crates -- cannot check the daemon binary's freshness"
    }
    return $newest
}

function Get-StaleDaemonMismatches {
    # Returns a human-readable mismatch description for every process in
    # $Procs whose exe path isn't $DaemonPath, or whose StartTime is not after
    # $BinaryWriteTime (i.e. it was already running before the binary being
    # gated was even built). An empty array means every process matches, so
    # it's safe to reuse the daemon they represent. Pulled out as its own
    # function so it can be exercised directly with synthetic process objects
    # in isolation, without needing a real stale/fresh daemon pair on disk.
    param(
        [Parameter(Mandatory)]$Procs,
        [Parameter(Mandatory)][string]$DaemonPath,
        [Parameter(Mandatory)][datetime]$BinaryWriteTime
    )
    $mismatches = @()
    foreach ($proc in @($Procs)) {
        $procPath = $null
        try { $procPath = $proc.Path } catch { }
        if ($procPath -ne $DaemonPath) {
            $mismatches += "PID $($proc.Id): path is '$procPath', expected '$DaemonPath'"
        } elseif ($proc.StartTime -le $BinaryWriteTime) {
            $mismatches += "PID $($proc.Id): started $($proc.StartTime), which is not after the binary's last write time ($BinaryWriteTime)"
        }
    }
    # The leading comma is required, not decorative: PowerShell unrolls a
    # plain `return $mismatches` one level, so a *zero*-mismatch array (the
    # common, healthy case) would emit nothing and the caller would receive
    # $null instead of an empty array -- which then throws under
    # Set-StrictMode the moment it does `$mismatches.Count`. The comma
    # operator forces the whole array through as a single object.
    return ,$mismatches
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

function Disconnect-TrixControl {
    param($Conn)
    if ($null -eq $Conn) { return }
    try { $Conn.Reader.Dispose() } catch { }
    try { $Conn.Writer.Dispose() } catch { }
    try { $Conn.Pipe.Dispose() } catch { }
}

# Sends one request and waits for *its* response, correlated by id. Lines
# with no id/ok (events: armed, disarmed, clip_saved, stats, ...) and lines
# answering some other in-flight id are skipped, not assumed to be the
# answer -- responses are broadcast on the same stream events arrive on.
function Send-TrixCommand {
    param(
        [Parameter(Mandatory)]$Conn,
        [Parameter(Mandatory)][string]$Cmd,
        [hashtable]$Arguments = @{},
        [int]$TimeoutMs = 10000
    )
    $id = $script:NextId
    $script:NextId++

    $payload = [ordered]@{ id = $id; cmd = $Cmd }
    foreach ($key in $Arguments.Keys) { $payload[$key] = $Arguments[$key] }
    $json = ($payload | ConvertTo-Json -Compress -Depth 10)

    $Conn.Writer.Write($json)
    $Conn.Writer.Write("`n")
    $Conn.Writer.Flush()

    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
    while ($true) {
        $remainingMs = [int](($deadline - [DateTime]::UtcNow).TotalMilliseconds)
        if ($remainingMs -le 0) {
            throw "timed out waiting for a response to id=$id ($Cmd)"
        }
        $readTask = $Conn.Reader.ReadLineAsync()
        if (-not $readTask.Wait($remainingMs)) {
            throw "timed out waiting for a response to id=$id ($Cmd)"
        }
        $line = $readTask.Result
        if ($null -eq $line) {
            throw "the pipe closed while waiting for a response to id=$id ($Cmd)"
        }
        if ([string]::IsNullOrWhiteSpace($line)) { continue }

        $msg = $null
        try { $msg = $line | ConvertFrom-Json -ErrorAction Stop } catch { continue }

        $props = @($msg.PSObject.Properties.Name)
        if (($props -contains 'id') -and ($props -contains 'ok')) {
            if ([int64]$msg.id -eq $id) {
                return $msg
            }
            # a response to some other in-flight request -- keep waiting
            continue
        }
        # an event line (armed / disarmed / clip_saved / stats / ...): not a
        # reply to anything, keep reading for the real answer.
    }
}

# --- Gate state, restored/cleaned up no matter how the run ends --------------

$startedDaemon = $false
$daemonProcess = $null
$conn = $null
$didDisarm = $false
$mp4Path = $null
$exitCode = 1

try {
    Write-Host "=== Trix stage 2 protocol gate ==="

    # Step 1 -- refuse to run against a locked session. A locked desktop makes
    # Windows Graphics Capture silently stop delivering frames while the run
    # still exits 0 with a one-frame clip -- this is not optional, so it goes
    # through Confirm-Check like every other check rather than a bare throw,
    # so it shows up in the SUMMARY receipt too.
    Write-Host "[1/10] Checking the session is not locked..."
    $logonUiRunning = [bool](Get-Process -Name 'LogonUI' -ErrorAction SilentlyContinue)
    Confirm-Check "session is not locked (LogonUI is not running)" (-not $logonUiRunning) `
        "Windows Graphics Capture stops delivering frames on a locked desktop and this run would silently produce a bogus one-frame clip -- unlock the session and re-run"

    # Step 2 -- start the daemon if nothing is already serving the pipe.
    #
    # Two separate staleness questions, and they are not the same question:
    #   (a) is the BINARY on disk older than the source it is supposed to have
    #       been built from? -- the freshness check just below.
    #   (b) is the PROCESS older than the binary, i.e. left running from an
    #       earlier build? -- Get-StaleDaemonMismatches further down.
    # Only (b) used to be asked. (a) is the one that catches plain
    # `cargo build --release` having skipped trix-daemon: a week-old binary,
    # freshly launched, passes (b) with room to spare.
    Write-Host "[2/10] Ensuring trix-daemon is running..."
    if (-not (Test-Path $DaemonPath)) { throw "daemon binary not found: $DaemonPath" }
    if (-not (Test-Path $TrixPath)) { throw "trix binary not found: $TrixPath" }

    $newestSource = Get-NewestSourceFile -RepoRoot $RepoRoot
    $daemonWrite = (Get-Item $DaemonPath).LastWriteTime
    Confirm-Check "trix-daemon.exe ($daemonWrite) is newer than the newest source file ($($newestSource.Name), $($newestSource.LastWriteTime))" `
        ($daemonWrite -gt $newestSource.LastWriteTime) `
        "rebuild with 'cargo build --release --workspace' -- this gate would otherwise report on a binary that is not the checked-out code"

    $existing = Get-Process -Name 'trix-daemon' -ErrorAction SilentlyContinue
    if ($existing) {
        $procs = @($existing)
        $pids = ($procs | ForEach-Object { $_.Id }) -join ','
        $binaryWriteTime = $daemonWrite

        # No @() wrap here. Get-StaleDaemonMismatches already returns its array
        # as a single object via `return ,$mismatches`; wrapping that in @()
        # collects the array into a *new* one-element array, so .Count is 1 for
        # every input -- healthy and stale alike -- and `-join` renders the
        # nested array as the literal text "System.Object[]". The two idioms
        # each solve the unrolling problem alone and cancel each other when
        # combined: the comma is the one that survives.
        $mismatches = Get-StaleDaemonMismatches -Procs $procs -DaemonPath $DaemonPath -BinaryWriteTime $binaryWriteTime
        $staleDetail = if ($mismatches.Count -gt 0) {
            "$($mismatches -join '; ') -- stop the stale daemon (Stop-Process -Name trix-daemon -Force) and re-run so the gate tests the binary that was just built"
        } else { '' }
        Confirm-Check "running trix-daemon (PID $pids) is the freshly built binary (path and start time match $DaemonPath)" `
            ($mismatches.Count -eq 0) $staleDetail

        Write-Host "  daemon already running (PID $pids) -- path and start time match, using it, will not stop it on exit"
    } else {
        Write-Host "  starting $DaemonPath"
        $outLog = Join-Path $env:TEMP 'trix-daemon-smoke.out.log'
        $errLog = Join-Path $env:TEMP 'trix-daemon-smoke.err.log'
        if (Test-Path $outLog) { Remove-Item $outLog -Force }
        if (Test-Path $errLog) { Remove-Item $errLog -Force }
        $daemonProcess = Start-Process -FilePath $DaemonPath -PassThru -WindowStyle Hidden `
            -RedirectStandardOutput $outLog -RedirectStandardError $errLog
        $startedDaemon = $true

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
            } catch {
                Start-Sleep -Milliseconds 200
            }
        }
        if (-not $ready) {
            throw "trix-daemon did not start listening on the control pipe within 10s (see $outLog / $errLog)"
        }
        Write-Host "  daemon started (PID $($daemonProcess.Id)), pipe is up"
    }

    # Step 3 -- connect, arm, assert ok:true and a non-empty encoder name.
    Write-Host "[3/10] Connecting and arming..."
    $conn = Connect-TrixControl
    $armTime = Get-Date
    $armResp = Send-TrixCommand -Conn $conn -Cmd 'arm' -TimeoutMs 15000
    Confirm-Check "arm returns ok:true" ([bool]$armResp.ok) (Get-JsonProp $armResp 'error')

    $clipDir = $armResp.data.clip_dir
    $ringSecondsTotal = [int]$armResp.data.ring_seconds_total
    $encoder = $armResp.data.encoder

    if ([string]::IsNullOrEmpty($encoder)) {
        # Documented race in trix-daemon/src/state.rs: for up to one 250ms
        # engine tick after arm() returns, the encoder name may not have been
        # published yet. Poll status briefly rather than fail on that window.
        Write-Host "  encoder not yet published on arm's own response, polling status..."
        $graceDeadline = (Get-Date).AddSeconds(3)
        while ([string]::IsNullOrEmpty($encoder) -and (Get-Date) -lt $graceDeadline) {
            Start-Sleep -Milliseconds 150
            $pollResp = Send-TrixCommand -Conn $conn -Cmd 'status'
            $encoder = $pollResp.data.encoder
        }
    }
    Confirm-Check "arm/status reports a non-empty encoder name" `
        (-not [string]::IsNullOrEmpty($encoder)) "encoder=$encoder"
    Write-Host "  armed. encoder = $encoder, clip_dir = $clipDir, ring_seconds_total = $ringSecondsTotal"

    # Step 4 -- wait for the ring to fill, then check status.
    Write-Host "[4/10] Waiting 10s for the replay ring to fill..."
    Start-Sleep -Seconds 10
    $statusResp = Send-TrixCommand -Conn $conn -Cmd 'status'
    Confirm-Check "status returns ok:true" ([bool]$statusResp.ok) (Get-JsonProp $statusResp 'error')

    # clip_dir and ring_seconds_total come back from status directly -- take
    # the freshest values rather than hardcoding a path or 15 seconds.
    $clipDir = $statusResp.data.clip_dir
    $ringSecondsTotal = [int]$statusResp.data.ring_seconds_total
    $ringUsed = [double]$statusResp.data.ring_seconds_used
    Confirm-Check "ring_seconds_used ($ringUsed) is between 5 and ring_seconds_total ($ringSecondsTotal)" `
        (($ringUsed -ge 5) -and ($ringUsed -le $ringSecondsTotal))

    # Step 6 checks duration_ms against the *full* ring_seconds_total, not
    # against however much had buffered at the 10s mark above -- so if the
    # configured replay length is longer than 10s, keep waiting until the
    # ring is (near enough) full before asking for a clip. This is derived
    # from ring_seconds_total itself, never a hardcoded default.
    $elapsedSinceArm = ((Get-Date) - $armTime).TotalSeconds
    $stillNeeded = $ringSecondsTotal - $elapsedSinceArm
    if ($stillNeeded -gt 0) {
        $extraWaitSecs = [Math]::Ceiling($stillNeeded) + 1
        Write-Host "  ring at $([Math]::Round($elapsedSinceArm, 1))s of $ringSecondsTotal s -- waiting $extraWaitSecs more second(s) so the clip reflects a full ring"
        Start-Sleep -Seconds $extraWaitSecs
        $statusResp = Send-TrixCommand -Conn $conn -Cmd 'status'
        Confirm-Check "status returns ok:true (after full-ring wait)" ([bool]$statusResp.ok) (Get-JsonProp $statusResp 'error')
        $clipDir = $statusResp.data.clip_dir
        $ringSecondsTotal = [int]$statusResp.data.ring_seconds_total
        $ringUsed = [double]$statusResp.data.ring_seconds_used
        Write-Host "  ring now at $ringUsed / $ringSecondsTotal seconds"
    }

    # Hoisted out of the `if` above, deliberately. This assertion used to live
    # inside it, which meant it silently did not run at all for any
    # replay_seconds short enough that the 10 s wait had already filled the ring
    # (~11 s or less) -- an assertion whose presence depends on configuration is
    # an assertion nobody can rely on, and it is the same failure class as the
    # stale-binary hole in Step 2. Out here it runs after whatever wait
    # occurred, short or long, and the claim is the same either way: by the time
    # a clip is requested the ring is essentially full, so Step 6's duration_ms
    # comparison against the *whole* ring is a fair one. Without it, an
    # insufficient wait surfaces two steps later as a confusing duration
    # mismatch.
    Confirm-Check "ring_seconds_used ($ringUsed) reached at least 90% of ring_seconds_total ($ringSecondsTotal) before clipping" `
        ($ringUsed -ge ($ringSecondsTotal * 0.9))

    # Step 5 -- clip.
    Write-Host "[5/10] Requesting a clip..."
    $clipResp = Send-TrixCommand -Conn $conn -Cmd 'clip' -TimeoutMs 15000
    Confirm-Check "clip returns ok:true" ([bool]$clipResp.ok) (Get-JsonProp $clipResp 'error')
    $clipId = $clipResp.data.id
    Confirm-Check "clip response carries a non-empty id" (-not [string]::IsNullOrEmpty($clipId)) "id=$clipId"
    Write-Host "  clip id = $clipId"

    # Step 6 -- the clip on disk: mp4 size, sidecar keys, duration, encoder.
    Write-Host "[6/10] Verifying the clip on disk..."
    $mp4Path = Join-Path $clipDir "$clipId.mp4"
    $jsonPath = Join-Path $clipDir "$clipId.json"

    Confirm-Check "clip mp4 exists at $mp4Path" (Test-Path $mp4Path)
    $mp4Size = (Get-Item $mp4Path).Length
    Confirm-Check "clip mp4 is over 100 KB" ($mp4Size -gt 100KB) "size=$mp4Size bytes"

    Confirm-Check "clip sidecar json exists at $jsonPath" (Test-Path $jsonPath)
    # -Encoding UTF8 is required: the daemon writes UTF-8 without a BOM, and
    # Get-Content's auto-detection falls back to the system ANSI codepage
    # without a BOM, which mangles non-ASCII encoder names (e.g. "Intel(R)").
    $sidecar = Get-Content -Raw -Encoding UTF8 -Path $jsonPath | ConvertFrom-Json

    $expectedKeys = @('id', 'title', 'created', 'duration_ms', 'bytes', 'width', 'height', `
            'fps', 'encoder', 'has_audio', 'favorite')
    $actualKeys = @($sidecar.PSObject.Properties.Name)
    $missing = @($expectedKeys | Where-Object { $actualKeys -notcontains $_ })
    $extra = @($actualKeys | Where-Object { $expectedKeys -notcontains $_ })
    Confirm-Check "sidecar has exactly the eleven expected keys" `
        (($missing.Count -eq 0) -and ($extra.Count -eq 0)) `
        "missing=[$($missing -join ',')] extra=[$($extra -join ',')]"

    $durationMs = [double]$sidecar.duration_ms
    $expectedMs = $ringSecondsTotal * 1000
    $lowerBound = $expectedMs * 0.8
    $upperBound = $expectedMs * 1.2
    Confirm-Check "duration_ms ($durationMs) within 20% of ring_seconds_total*1000 ($expectedMs)" `
        (($durationMs -ge $lowerBound) -and ($durationMs -le $upperBound))

    Confirm-Check "sidecar encoder ($($sidecar.encoder)) matches what arm reported ($encoder)" `
        ($sidecar.encoder -eq $encoder)

    # Step 7 -- library.list: the new clip present and first (newest-first).
    Write-Host "[7/10] Checking library.list..."
    $listResp = Send-TrixCommand -Conn $conn -Cmd 'library.list' -Arguments @{ offset = 0; limit = 50 }
    Confirm-Check "library.list returns ok:true" ([bool]$listResp.ok) (Get-JsonProp $listResp 'error')
    $clips = @($listResp.data.clips)
    Confirm-Check "library.list returned at least one clip" ($clips.Count -gt 0)
    $firstId = $clips[0].id
    Confirm-Check "the new clip is first in library.list" ($firstId -eq $clipId) "first=$firstId expected=$clipId"

    # Step 8 -- the negative half of the single-instance check, taken while the
    # daemon is still armed.
    #
    # Step 10 below ("replay exits 0 once the daemon has disarmed") was for a
    # long time the ONLY check anywhere for the coupling that `Armed { engine,
    # _slot }` and the drop ordering in state.rs::disarm exist to provide -- and
    # on its own it could not fail. `trix.exe replay --exit-after 3` exits 0
    # whether or not the daemon ever held the single-instance mutex, so ripping
    # `_slot` out of `Armed` entirely left the gate fully green. That is a
    # paraphrase of the coupling, not the coupling.
    #
    # This is the other direction, and it is the one with teeth: while the
    # daemon is armed it MUST be holding the slot, so the CLI must be refused,
    # by name. Delete the slot from `Armed` and this check fails immediately.
    # With both halves the pair is falsifiable in both directions, and it costs
    # about a second.
    Write-Host "[8/10] Confirming the armed daemon holds the single-instance slot..."
    $blocked = $null
    $blockedExit = 0
    try {
        # 'Continue', scoped to this one call, is required rather than tidy. In
        # Windows PowerShell 5.1 a native command's stderr redirected with 2>&1
        # arrives as ErrorRecords, and under the script-wide
        # $ErrorActionPreference = 'Stop' that is a *terminating* error. This
        # command is expected to fail and expected to write to stderr -- that is
        # the whole assertion -- so without this the gate would abort here with a
        # raw RemoteException instead of recording the pass. Verified, not
        # assumed: the unscoped form throws. Same trap the deleted
        # `cargo metadata ... 2>$null` version check used to spring.
        $ErrorActionPreference = 'Continue'
        $blocked = & $TrixPath replay --exit-after 1 2>&1
        $blockedExit = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = 'Stop'
    }
    $blocked | ForEach-Object { Write-Host "  | $_" }
    Confirm-Check "trix.exe replay is refused while the daemon is armed" `
        (($blockedExit -ne 0) -and ("$blocked" -match 'already running')) `
        "exit=$blockedExit output=$blocked"

    # Step 9 -- disarm.
    Write-Host "[9/10] Disarming..."
    $disarmResp = Send-TrixCommand -Conn $conn -Cmd 'disarm'
    Confirm-Check "disarm returns ok:true" ([bool]$disarmResp.ok) (Get-JsonProp $disarmResp 'error')
    $didDisarm = $true

    Disconnect-TrixControl -Conn $conn
    $conn = $null

    # Step 10 -- the single-instance slot must be free again. Meaningful only
    # because Step 8 proved it was held a moment ago: on its own this is an
    # exit-0 that would also hold for a daemon that never took the slot at all.
    Write-Host "[10/10] Confirming the single-instance slot is free again (trix.exe replay --exit-after 3)..."
    $replayOutput = & $TrixPath replay --exit-after 3
    $replayExit = $LASTEXITCODE
    $replayOutput | ForEach-Object { Write-Host "  | $_" }
    Confirm-Check "trix.exe replay --exit-after 3 exits 0 now that the daemon has disarmed" `
        ($replayExit -eq 0) "exit code=$replayExit"

    $exitCode = 0
}
catch {
    Write-Host ""
    Write-Host "FAIL: $($_.Exception.Message)" -ForegroundColor Red
    $exitCode = 1
}
finally {
    # Best-effort cleanup: if we're still armed (an earlier step threw before
    # Step 8's disarm ran), disarm before tearing anything else down so the
    # engine finalizes instead of being killed mid-write.
    if ($conn -and -not $didDisarm) {
        try {
            Write-Host "cleanup: disarming before exit..."
            Send-TrixCommand -Conn $conn -Cmd 'disarm' -TimeoutMs 5000 | Out-Null
        } catch {
            Write-Host "cleanup: disarm attempt failed: $($_.Exception.Message)"
        }
    }
    Disconnect-TrixControl -Conn $conn

    if ($startedDaemon -and $daemonProcess) {
        try {
            if (-not $daemonProcess.HasExited) {
                Write-Host "cleanup: stopping the daemon this script started (PID $($daemonProcess.Id))"
                Stop-Process -Id $daemonProcess.Id -Force -ErrorAction SilentlyContinue
            }
        } catch { }
    }

    Write-Host ""
    Write-Host "=== SUMMARY ==="
    foreach ($check in $script:Checks) {
        $status = if ($check.Passed) { 'PASS' } else { 'FAIL' }
        $line = "  [$status] $($check.Name)"
        if ($check.Detail -and -not $check.Passed) { $line += " -- $($check.Detail)" }
        Write-Host $line
    }
    if ($exitCode -eq 0) {
        Write-Host ""
        Write-Host "PASS -- stage 2 protocol gate is green."
        Write-Host "Clip written to: $mp4Path"
        Write-Host "Play it and confirm video and audio are present and in sync -- that hand check is still outstanding."
    } else {
        Write-Host ""
        Write-Host "FAIL -- see the check above that failed."
    }
}

exit $exitCode
