# Self-test for protocol-smoke.ps1's stale-daemon check.
#
# Why this exists: the gate's stale-daemon check shipped broken once already.
# `Get-StaleDaemonMismatches` ends with `return ,$mismatches` to stop PowerShell
# from unrolling a 0- or 1-element array; a fix pass then ALSO wrapped the call
# site in @(...). Each idiom solves the unrolling problem alone, but together
# they cancel: @() collects the already-protected array into a new 1-element
# array, so .Count was 1 for every input -- the gate aborted against a perfectly
# fresh daemon, and the detail text degraded to the literal "System.Object[]".
#
# The isolation test written alongside that fix passed, because it called the
# helper by direct assignment instead of through the shipped call-site
# expression. It tested a paraphrase of the call path, not the call path.
#
# So this test extracts the real function from the real script by AST and drives
# it through the real call-site expression. It cannot pass while the shipped
# composition is broken, and it cannot drift from the script, because it reads
# the file that ships.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script = Join-Path $PSScriptRoot 'protocol-smoke.ps1'
if (-not (Test-Path -LiteralPath $script)) { throw "protocol-smoke.ps1 not found next to this test: $script" }

$ast = [System.Management.Automation.Language.Parser]::ParseFile($script, [ref]$null, [ref]$null)
$fn = $ast.FindAll({ param($n)
    $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $n.Name -eq 'Get-StaleDaemonMismatches' }, $true) | Select-Object -First 1
if (-not $fn) { throw 'Get-StaleDaemonMismatches not found in protocol-smoke.ps1' }
. ([scriptblock]::Create($fn.Extent.Text))

$expected  = 'C:\repo\target\release\trix-daemon.exe'
$buildTime = [datetime]'2026-07-30 18:00:00'

function New-FakeProc($id, $path, $start) {
    # StartTime/Path on a real Process are read-only, so model them as a
    # PSCustomObject exposing the same property names the helper reads.
    [PSCustomObject]@{ Id = $id; Path = $path; StartTime = $start }
}

# Mixed expectations on purpose: a helper that always returns "reuse" and one
# that always returns "stale" each fail at least one case, so the suite has real
# negative controls in both directions.
$cases = @(
    @{ Name = 'fresh daemon, correct path, started after build'; Procs = @(New-FakeProc 1 $expected ([datetime]'2026-07-30 18:05:00')); ExpectReuse = $true }
    @{ Name = 'wrong path (stale binary elsewhere)';             Procs = @(New-FakeProc 2 'C:\old\trix-daemon.exe' ([datetime]'2026-07-30 18:05:00')); ExpectReuse = $false }
    @{ Name = 'right path but started BEFORE the build';         Procs = @(New-FakeProc 3 $expected ([datetime]'2026-07-30 17:00:00')); ExpectReuse = $false }
    @{ Name = 'started exactly at build time (boundary)';        Procs = @(New-FakeProc 4 $expected $buildTime); ExpectReuse = $false }
    @{ Name = 'two procs, one fresh one stale';                  Procs = @((New-FakeProc 5 $expected ([datetime]'2026-07-30 18:05:00')), (New-FakeProc 6 'C:\old\trix-daemon.exe' ([datetime]'2026-07-30 18:05:00'))); ExpectReuse = $false }
)

$fail = 0
foreach ($c in $cases) {
    # The shipped call-site expression, verbatim. Do not add @() here -- that is
    # the exact defect this test exists to catch.
    $mismatches = Get-StaleDaemonMismatches -Procs $c.Procs -DaemonPath $expected -BinaryWriteTime $buildTime
    $reuse  = ($mismatches.Count -eq 0)
    $detail = if ($mismatches.Count -gt 0) { $mismatches -join '; ' } else { '' }

    $ok = ($reuse -eq $c.ExpectReuse)
    # A non-empty, non-degenerate detail string is part of the contract: the
    # previous defect surfaced as the literal text "System.Object[]".
    if (-not $reuse -and ($detail -match 'System\.Object\[\]' -or -not $detail)) { $ok = $false }
    if (-not $ok) { $fail++ }

    '{0}  {1,-52} Count={2} reuse={3}' -f $(if ($ok) { '[PASS]' } else { '[FAIL]' }), $c.Name, $mismatches.Count, $reuse
    if (-not $reuse) { '         detail: {0}' -f $detail }
}

''
if ($fail -eq 0) { "ALL $($cases.Count) CASES PASS" } else { "$fail of $($cases.Count) CASES FAILED"; exit 1 }
