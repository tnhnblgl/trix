<#
.SYNOPSIS
    Builds the release zip that gets attached to a GitHub release, and refuses
    to build one that cannot be trusted.

.DESCRIPTION
    Produces trix-v<version>-win-x64.zip containing the three binaries, the
    LICENSE, and the user-facing docs\ship\README.txt. There is no installer
    yet, so this zip IS the product: whatever it contains is what a stranger
    downloads and runs.

    That is why most of this script is gates rather than packaging. Packaging
    is six lines. The rest exists because a release artifact is the one build
    nobody re-checks -- it goes straight from here to a download page, and a
    mistake in it is discovered by users.

    What it proves before it will write a zip:

      * The working tree is clean. A zip built from uncommitted edits cannot be
        reproduced from the tag it ships under, and there is no way to tell
        afterwards which is which. -AllowDirty overrides this for a trial run;
        the zip is then named with a -dirty suffix so it cannot be mistaken for
        a real one.

      * The version in Cargo.toml and the version in tauri.conf.json agree.
        They are two hand-edited files and nothing else compares them. When
        they drift, trix-ui.exe's Properties dialog in Explorer reports one
        number while the daemon reports another over the control socket.

      * docs\ship\README.txt names the version being built. That file carries
        the "what's new" section, and a release whose README still describes
        the previous one is worse than a release with no README: it actively
        misinforms. This catches the forgotten edit.

      * Both test suites pass -- Rust and frontend.

      * Every binary is newer than the newest source file, using the same
        Get-NewestSourceFile that protocol-smoke.ps1 and arm-cycle-leak.ps1
        use. This catches the build that silently did not run, which is not
        hypothetical: on 2026-08-06 the memory soak was nearly run against a
        three-day-old trix-daemon.exe that predated the entire audio feature,
        because only a debug build had been made. A gate that runs against a
        stale binary proves nothing at all.

      * trix.exe reports the expected version when run from the staged folder,
        and trix-ui.exe's embedded version resource matches.

    ON RUNNING THE TESTS: neither test command is piped anywhere. In PowerShell
    as in POSIX shells, the exit code of `cmd | tail` is tail's, so a piped
    test run reports success no matter how the tests went. That mistake was
    made while building the 0.4.0 zip by hand, and it turned the whole test
    gate into decoration -- it "passed" while only the last 16 of 191 tests
    were even visible. Do not add a pipe here for tidier output.

    WHAT THIS SCRIPT DELIBERATELY DOES NOT DO:

      * It never runs trix-daemon.exe. The daemon has no --help; passing it
        one starts a real daemon against the developer's real config.toml and
        real clip library, which is exactly the isolation rule the rest of the
        repo's scripts go to such lengths to respect. If a daemon smoke test
        is wanted, daemon-smoke.ps1 already does it properly, with a scratch
        %APPDATA% and a scratch clip_dir.

      * It never commits, tags, or pushes. The user creates the GitHub release
        by hand, and creating it is what makes the tag.

    Requires the tauri CLI (`cargo tauri`), and npm on PATH for the frontend
    build and tests.

.PARAMETER OutDir
    Where the zip is written. Defaults to the directory above the repo, so the
    artifact lands beside the project folder rather than inside it -- a zip
    inside the repo is one `git add .` away from being committed.

.PARAMETER SkipTests
    Skips both test suites. For iterating on the packaging itself. The summary
    line then says so in full, and the zip is named with a -untested suffix,
    because a skipped check that reads as a pass is how bad artifacts ship.

.PARAMETER AllowDirty
    Permits a build from a dirty working tree, naming the zip -dirty.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\ship-zip.ps1

    The normal case. Takes a few minutes; the trix-ui link is the slow part,
    because the release profile is fat LTO at codegen-units = 1.
#>
[CmdletBinding()]
param(
    [string]$OutDir,
    [switch]$SkipTests,
    [switch]$AllowDirty
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Split-Path -Parent $RepoRoot }

$script:Checks = New-Object System.Collections.Generic.List[object]

function Check {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][bool]$Condition, [string]$Detail = '')
    $status = 'FAIL'
    if ($Condition) { $status = 'PASS' }
    $line = "  [$status] $Name"
    if ($Detail -and -not $Condition) { $line += " -- $Detail" }
    if ($Condition) { Write-Host $line -ForegroundColor Green } else { Write-Host $line -ForegroundColor Red }
    $script:Checks.Add([PSCustomObject]@{ Name = $Name; Passed = $Condition; Detail = $Detail })
}

# Fail-fast, unlike ui-smoke.ps1. That gate accumulates failures because each
# one is an independent fact worth learning in a single run. Here the stages
# are strictly sequential -- there is no point building binaries whose tests
# failed, and no point verifying binaries that did not build -- so a stage
# that fails throws, and only the post-build verifications accumulate.
function Require {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][bool]$Condition, [string]$Detail = '')
    Check -Name $Name -Condition $Condition -Detail $Detail
    if (-not $Condition) { throw "$Name -- $Detail" }
}

# Same AST extraction arm-cycle-leak.ps1 uses, for the same reason: copying
# the function would let two gates drift apart on what "fresh" means.
$smoke = Join-Path $PSScriptRoot 'protocol-smoke.ps1'
if (-not (Test-Path -LiteralPath $smoke)) { throw "protocol-smoke.ps1 not found next to this script: $smoke" }
$ast = [System.Management.Automation.Language.Parser]::ParseFile($smoke, [ref]$null, [ref]$null)
$fn = $ast.FindAll({ param($n)
    $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $n.Name -eq 'Get-NewestSourceFile' }, $true) | Select-Object -First 1
if (-not $fn) { throw 'Get-NewestSourceFile not found in protocol-smoke.ps1' }
. ([scriptblock]::Create($fn.Extent.Text))

Write-Host ''
Write-Host 'Step 1: version and working tree' -ForegroundColor Cyan

# The workspace manifest is the single source of truth for the version. Nothing
# here takes a -Version parameter: a hand-typed version is how a zip ends up
# named for a build it does not contain.
$cargoToml = Join-Path $RepoRoot 'Cargo.toml'
$cargoText = Get-Content -LiteralPath $cargoToml -Raw
$m = [regex]::Match($cargoText, '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"')
Require -Name 'Cargo.toml declares a workspace version' -Condition $m.Success `
    -Detail 'no [workspace.package] version = "..." found'
$Version = $m.Groups[1].Value
Write-Host "         version: $Version"

$tauriConfPath = Join-Path $RepoRoot 'crates\trix-ui\tauri.conf.json'
$tauriConf = Get-Content -LiteralPath $tauriConfPath -Raw | ConvertFrom-Json
Require -Name 'tauri.conf.json version matches Cargo.toml' -Condition ($tauriConf.version -eq $Version) `
    -Detail "tauri.conf.json says $($tauriConf.version), Cargo.toml says $Version"

$readmeSrc = Join-Path $RepoRoot 'docs\ship\README.txt'
Require -Name 'docs\ship\README.txt exists' -Condition (Test-Path -LiteralPath $readmeSrc) -Detail $readmeSrc
$readmeText = Get-Content -LiteralPath $readmeSrc -Raw
Require -Name 'the shipping README names this version' -Condition ($readmeText -match [regex]::Escape($Version)) `
    -Detail "docs\ship\README.txt never mentions $Version -- it probably still describes the previous release"

Push-Location $RepoRoot
try { $dirty = @(git status --porcelain) } finally { Pop-Location }
$isClean = ($dirty.Count -eq 0)
if ($AllowDirty) {
    Check -Name 'working tree is clean (overridden by -AllowDirty)' -Condition $true
    if (-not $isClean) { Write-Host "         $($dirty.Count) uncommitted change(s); the zip will be marked -dirty" -ForegroundColor Yellow }
} else {
    Require -Name 'working tree is clean' -Condition $isClean `
        -Detail "$($dirty.Count) uncommitted change(s). Commit them, or pass -AllowDirty for a trial build."
}

Write-Host ''
Write-Host 'Step 2: tests' -ForegroundColor Cyan

if ($SkipTests) {
    Write-Host '  [SKIP] both test suites -- -SkipTests was passed' -ForegroundColor Yellow
} else {
    # No pipe. See the note in .DESCRIPTION: piping makes $LASTEXITCODE the
    # pipe's, and the gate stops being a gate.
    Push-Location $RepoRoot
    try { cargo test --workspace } finally { Pop-Location }
    Require -Name 'cargo test --workspace' -Condition ($LASTEXITCODE -eq 0) -Detail "exit code $LASTEXITCODE"

    Push-Location (Join-Path $RepoRoot 'crates\trix-ui\web')
    try { npm test -- --run } finally { Pop-Location }
    Require -Name 'npm test (frontend)' -Condition ($LASTEXITCODE -eq 0) -Detail "exit code $LASTEXITCODE"
}

Write-Host ''
Write-Host 'Step 3: release build' -ForegroundColor Cyan

Push-Location $RepoRoot
try { cargo build --release -p trix-cli -p trix-daemon } finally { Pop-Location }
Require -Name 'cargo build --release (cli, daemon)' -Condition ($LASTEXITCODE -eq 0) -Detail "exit code $LASTEXITCODE"

# From crates\trix-ui, never from the repo root with --config: tauri-cli
# resolves the frontend directory by walking down from its working directory,
# and from the root it settles on crates\ and dies there running "npm run
# build". This also runs the frontend build, so web\dist is fresh.
Push-Location (Join-Path $RepoRoot 'crates\trix-ui')
try { cargo tauri build --no-bundle } finally { Pop-Location }
Require -Name 'cargo tauri build --no-bundle' -Condition ($LASTEXITCODE -eq 0) -Detail "exit code $LASTEXITCODE"

Write-Host ''
Write-Host 'Step 4: verify the binaries' -ForegroundColor Cyan

$relDir = Join-Path $RepoRoot 'target\release'
$binaries = @('trix.exe', 'trix-daemon.exe', 'trix-ui.exe')
foreach ($b in $binaries) {
    Require -Name "$b exists" -Condition (Test-Path -LiteralPath (Join-Path $relDir $b)) -Detail (Join-Path $relDir $b)
}

$newestSource = Get-NewestSourceFile -RepoRoot $RepoRoot
foreach ($b in $binaries) {
    $built = (Get-Item -LiteralPath (Join-Path $relDir $b)).LastWriteTime
    Require -Name "$b is newer than the newest source file" -Condition ($built -ge $newestSource.LastWriteTime) `
        -Detail "$b built $built, but $($newestSource.Name) was written $($newestSource.LastWriteTime) -- the build did not run"
}

Write-Host ''
Write-Host 'Step 5: stage and package' -ForegroundColor Cyan

$suffix = ''
if ($SkipTests) { $suffix += '-untested' }
if ($AllowDirty -and -not $isClean) { $suffix += '-dirty' }
$name = "trix-v$Version-win-x64$suffix"

$work = Join-Path $env:TEMP ("trix-ship-" + [guid]::NewGuid().ToString('N'))
$stage = Join-Path $work $name
New-Item -ItemType Directory -Force $stage | Out-Null
try {
    foreach ($b in $binaries) { Copy-Item (Join-Path $relDir $b) (Join-Path $stage $b) -Force }
    Copy-Item (Join-Path $RepoRoot 'LICENSE') (Join-Path $stage 'LICENSE') -Force

    # CRLF and no BOM. This file is opened in Notepad by people who just
    # unzipped it, and it is the first thing they read.
    $crlf = ($readmeText -replace "`r`n", "`n") -replace "`n", "`r`n"
    [System.IO.File]::WriteAllText((Join-Path $stage 'README.txt'), $crlf, (New-Object System.Text.UTF8Encoding($false)))

    # Run the CLI from the staged copy rather than from target\release, so what
    # is checked is the file that actually goes in the zip.
    $stagedCli = Join-Path $stage 'trix.exe'
    $reported = (& $stagedCli --version) -join ''
    Require -Name "trix.exe reports $Version" -Condition ($reported.Trim() -eq "trix $Version") `
        -Detail "reported '$reported'"

    # trix-ui.exe is the only one with a version resource -- tauri-build writes
    # it from tauri.conf.json. It is what Explorer's Properties tab shows.
    $vi = (Get-Item -LiteralPath (Join-Path $stage 'trix-ui.exe')).VersionInfo
    Require -Name "trix-ui.exe version resource says $Version" -Condition ($vi.FileVersion -eq $Version) `
        -Detail "FileVersion is '$($vi.FileVersion)'"

    if (-not (Test-Path -LiteralPath $OutDir)) { New-Item -ItemType Directory -Force $OutDir | Out-Null }
    $zipPath = Join-Path $OutDir "$name.zip"
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }

    # ZipFile.CreateFromDirectory rather than Compress-Archive: the .NET API
    # writes forward slashes in entry names, as the zip format specifies, while
    # Compress-Archive on PowerShell 5.1 writes backslashes. Both unpack on
    # Windows; only one unpacks cleanly everywhere else. The $true is
    # includeBaseDirectory, which puts everything under one folder so unzipping
    # into Downloads does not scatter five files across it.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $stage, $zipPath, [System.IO.Compression.CompressionLevel]::Optimal, $true)
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

$zip = Get-Item -LiteralPath $zipPath
$sha = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash

# Every gate above is a Require, so reaching this line means all of them
# passed -- a failure threw and printed its red line on the way out. The count
# is here so a green run says how much was actually checked, rather than only
# that nothing blew up.
$passed = @($script:Checks | Where-Object { $_.Passed }).Count

Write-Host ''
Write-Host ("  " + $zip.FullName)
Write-Host ("  " + [math]::Round($zip.Length / 1MB, 2) + " MB")
Write-Host ("  SHA256  " + $sha)
Write-Host ''
if ($SkipTests) {
    Write-Host "SHIP ZIP BUILT WITHOUT TESTS  ($passed checks) -- NOT A RELEASE ARTIFACT" -ForegroundColor Yellow
} else {
    Write-Host "SHIP ZIP BUILT  ($passed checks)" -ForegroundColor Green
}
Write-Host 'Nothing was committed, tagged or pushed. Creating the GitHub release is what makes the tag.'
