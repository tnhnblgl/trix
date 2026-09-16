<#
.SYNOPSIS
    Builds the release zip that gets attached to a GitHub release, and refuses
    to build one that cannot be trusted.

.DESCRIPTION
    Produces trix-v<version>-win-x64.zip containing the three binaries and
    the user-facing docs\ship\README.txt -- nothing else, by decision. The
    one licence that has to travel with every copy, the icon set's, is
    appended to the end of that README rather than shipped as a file of its
    own (see THIRD-PARTY-NOTICES.txt). There is no installer
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

      * trix.exe reports the expected version when run from the STAGED folder,
        and trix-ui.exe's embedded version resource matches. Staged, not
        target\release, so what gets checked is the file that goes in the zip.

        This is where the freshness question is answered, and it is worth
        saying why it is not answered with a timestamp comparison the way
        protocol-smoke.ps1 and arm-cycle-leak.ps1 do. Those scripts consume a
        binary somebody else built, so mtimes are all they have. This one runs
        cargo itself, which makes cargo's fingerprint the authority -- it
        hashes inputs, flags and features rather than trusting a clock. See
        the long comment in Step 4; an mtime check there gave two different
        false failures before it was removed.

        trix-daemon.exe rests on cargo's fingerprint alone: it carries no
        version resource, and running it to ask is precisely the mistake this
        script refuses to make (see below).

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

    SPLITTING THE RUN, FOR SIGNING IN CI:

    A code-signing service signs the binaries after they are built and before
    they are zipped, so the run has to come apart at that seam. -StageOnly
    stops once the staging folder is filled and leaves it behind; -FromStage
    picks that folder up and packages it. Neither weakens a gate: the staged
    binaries are verified in both halves, because signing rewrites them, and
    the zip is only ever built from what is in the staging folder at that
    moment.

      scripts\ship-zip.ps1 -StageOnly -StageDir C:\work
      (sign C:\work\trix-v<version>-win-x64\*.exe)
      scripts\ship-zip.ps1 -FromStage -StageDir C:\work -OutDir C:\out

    The staging half writes release.json beside the staged folder, so the
    packaging half knows the version, the folder name and whether the first
    half skipped anything, rather than being told again on the command line
    and believing it.

    Requires the tauri CLI (`cargo tauri`), and npm on PATH for the frontend
    build and tests.

.PARAMETER OutDir
    Where the zip is written. Defaults to the directory above the repo, so the
    artifact lands beside the project folder rather than inside it -- a zip
    inside the repo is one `git add .` away from being committed.

.PARAMETER StageDir
    The work folder the zip's contents are staged in, holding a folder named
    after the release plus release.json. Defaults to a temporary folder that is
    deleted when the zip is written; pass one to keep it. Required with
    -FromStage, which is how the second half finds the first half's work.

.PARAMETER StageOnly
    Builds and stages, then stops without writing a zip, leaving the staging
    folder for a signing step. Deleting that folder afterwards is the caller's
    job.

.PARAMETER FromStage
    Skips straight to verifying and zipping what is already in -StageDir. For
    the second half of a signed build; the binaries it packages are whatever
    the signing step left there.

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
    [string]$StageDir,
    [switch]$StageOnly,
    [switch]$FromStage,
    [switch]$SkipTests,
    [switch]$AllowDirty
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Split-Path -Parent $RepoRoot }

if ($StageOnly -and $FromStage) {
    throw '-StageOnly and -FromStage are the two halves of one run. Pass one of them, or neither.'
}
if ($FromStage -and -not $StageDir) {
    throw '-FromStage needs -StageDir: the folder the staging half was told to leave behind.'
}

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

# Both halves run this. The staging half, so a build that came out wrong is
# caught before a signing service is asked to put its name on it; the packaging
# half, because signing rewrites the very files the first half checked, and the
# zip is built from the rewritten ones.
function Test-StagedBinaries {
    param([Parameter(Mandatory)][string]$Stage, [Parameter(Mandatory)][string]$Version)

    # Run the CLI from the staged copy rather than from target\release, so what
    # is checked is the file that actually goes in the zip.
    $stagedCli = Join-Path $Stage 'trix.exe'
    $reported = (& $stagedCli --version) -join ''
    Require -Name "trix.exe reports $Version" -Condition ($reported.Trim() -eq "trix $Version") `
        -Detail "reported '$reported'"

    # trix-ui.exe is the only one with a version resource -- tauri-build writes
    # it from tauri.conf.json. It is what Explorer's Properties tab shows.
    $vi = (Get-Item -LiteralPath (Join-Path $Stage 'trix-ui.exe')).VersionInfo
    Require -Name "trix-ui.exe version resource says $Version" -Condition ($vi.FileVersion -eq $Version) `
        -Detail "FileVersion is '$($vi.FileVersion)'"
}

# The packaging half: everything from a filled staging folder to the two files
# that get attached to a release. A function rather than the tail of the script
# because -FromStage enters here having skipped every step above, on a folder a
# signing step has been through.
function Complete-Package {
    param(
        [Parameter(Mandatory)][string]$Stage,
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$OutDir,
        [Parameter(Mandatory)][bool]$SkippedTests
    )

    Write-Host ''
    Write-Host 'Verify the staged binaries and package' -ForegroundColor Cyan

    Test-StagedBinaries -Stage $Stage -Version $Version

    if (-not (Test-Path -LiteralPath $OutDir)) { New-Item -ItemType Directory -Force $OutDir | Out-Null }
    $zipPath = Join-Path $OutDir "$Name.zip"
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }

    # ZipFile.CreateFromDirectory rather than Compress-Archive: the .NET API
    # writes forward slashes in entry names, as the zip format specifies, while
    # Compress-Archive on PowerShell 5.1 writes backslashes. Both unpack on
    # Windows; only one unpacks cleanly everywhere else. The $true is
    # includeBaseDirectory, which puts everything under one folder so unzipping
    # into Downloads does not scatter four files across it.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $Stage, $zipPath, [System.IO.Compression.CompressionLevel]::Optimal, $true)

    $zip = Get-Item -LiteralPath $zipPath
    $sha = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash

    # The updater refuses a zip it cannot check, so this file is not optional --
    # a release with the zip alone reads to every installed Trix as "no update
    # available", silently. Written beside the zip, in the `<hash>  <name>`
    # format sha256sum uses, because the updater parses it and people paste it.
    $sumsPath = Join-Path $OutDir 'SHA256SUMS.txt'
    $sumsLine = "$($sha.ToLower())  $($zip.Name)"
    [System.IO.File]::WriteAllText($sumsPath, "$sumsLine`n", (New-Object System.Text.UTF8Encoding($false)))

    # Every gate above is a Require, so reaching this line means all of them
    # passed -- a failure threw and printed its red line on the way out. The
    # count is here so a green run says how much was actually checked, rather
    # than only that nothing blew up.
    $passed = @($script:Checks | Where-Object { $_.Passed }).Count

    Write-Host ''
    Write-Host ("  " + $zip.FullName)
    Write-Host ("  " + [math]::Round($zip.Length / 1MB, 2) + " MB")
    Write-Host ("  SHA256  " + $sha)
    Write-Host ''

    if ($SkippedTests) {
        Write-Host "SHIP ZIP BUILT WITHOUT TESTS  ($passed checks) -- NOT A RELEASE ARTIFACT" -ForegroundColor Yellow
    } else {
        Write-Host "SHIP ZIP BUILT  ($passed checks)" -ForegroundColor Green
    }
    Write-Host 'Nothing was committed, tagged or pushed. Creating the GitHub release is what makes the tag.'
    Write-Host ''
    Write-Host 'Attach BOTH files to the release:' -ForegroundColor Cyan
    Write-Host "  $($zip.FullName)"
    Write-Host "  $sumsPath"
    Write-Host 'A release with only the zip is invisible to the in-app updater, which will not' -ForegroundColor Yellow
    Write-Host 'offer an update it cannot verify.' -ForegroundColor Yellow
}

# -FromStage is the packaging half on its own. Everything it needs to know about
# the build it is packaging comes out of release.json rather than off the
# command line, so a mistyped version cannot rename somebody else's build.
if ($FromStage) {
    $manifestPath = Join-Path $StageDir 'release.json'
    if (-not (Test-Path -LiteralPath $manifestPath)) {
        throw "no release.json in $StageDir -- that is not a folder a -StageOnly run left behind."
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json

    Write-Host ''
    Write-Host "Packaging the staged build in $StageDir" -ForegroundColor Cyan
    Write-Host "         version: $($manifest.version)"

    Complete-Package -Stage (Join-Path $StageDir $manifest.name) -Name $manifest.name `
        -Version $manifest.version -OutDir $OutDir -SkippedTests ([bool]$manifest.skippedTests)
    exit 0
}

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

# Appended to the README in the zip. Lucide's ISC licence requires its notice
# in every copy of the icons, and trix-ui.exe is one, so a zip without it is
# not one this script will build.
$noticesSrc = Join-Path $RepoRoot 'THIRD-PARTY-NOTICES.txt'
Require -Name 'THIRD-PARTY-NOTICES.txt exists' -Condition (Test-Path -LiteralPath $noticesSrc) -Detail $noticesSrc
$noticesText = Get-Content -LiteralPath $noticesSrc -Raw

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

# NO TIMESTAMP FRESHNESS CHECK HERE, deliberately, and the asymmetry with
# protocol-smoke.ps1 and arm-cycle-leak.ps1 is the point. Those scripts do not
# build: they consume a binary somebody else made, so "is this newer than the
# source" is the only question they can ask, and asking it is what stops a gate
# running green against a stale exe.
#
# This script ran cargo itself, four lines up. That makes cargo's fingerprint
# the authority, and it is a far stronger one than an mtime heuristic -- it
# hashes inputs, flags and features rather than trusting a clock. An mtime
# comparison here is not merely redundant, it is WRONG, and both wrong answers
# showed up on the first two runs of this script:
#
#   * `cargo tauri build` rewrites crates\trix-ui\Cargo.toml on every single
#     invocation (line endings only -- `git diff` on it comes back empty), so
#     a source file is always newer than the binaries the same run produced.
#   * Even sampled before the build, the check still fails honestly-built
#     binaries: cargo correctly does not relink trix.exe when nothing that
#     feeds trix-cli changed, so a touched file anywhere under crates\ leaves
#     an up-to-date trix.exe looking stale. Nothing is wrong; the question is.
#
# What replaces it is a claim that can actually be checked: the staged trix.exe
# reports the version this script read out of Cargo.toml, and trix-ui.exe's
# version resource agrees. Both are below, and both run against the staged
# copies rather than target\release.

Write-Host ''
Write-Host 'Step 5: stage' -ForegroundColor Cyan

$suffix = ''
if ($SkipTests) { $suffix += '-untested' }
if ($AllowDirty -and -not $isClean) { $suffix += '-dirty' }
$name = "trix-v$Version-win-x64$suffix"

$work = $StageDir
if (-not $work) { $work = Join-Path $env:TEMP ("trix-ship-" + [guid]::NewGuid().ToString('N')) }

# Only a work folder this script invented, on a run that packages what it
# staged, is this script's to delete. A caller who named one keeps it, and so
# does a -StageOnly run, whose whole purpose is to leave the folder behind.
$ownsWork = (-not $StageDir) -and (-not $StageOnly)

$stage = Join-Path $work $name
New-Item -ItemType Directory -Force $stage | Out-Null
try {
    foreach ($b in $binaries) { Copy-Item (Join-Path $relDir $b) (Join-Path $stage $b) -Force }
    # The three binaries and README.txt are the whole zip. No LICENSE: the
    # licence notices that must ship go inside the README, below.

    # CRLF and no BOM. This file is opened in Notepad by people who just
    # unzipped it, and it is the first thing they read. The notices go last,
    # after everything a reader came for.
    $shipped = $readmeText.TrimEnd() + "`n`n`n" + $noticesText
    $crlf = ($shipped -replace "`r`n", "`n") -replace "`n", "`r`n"
    [System.IO.File]::WriteAllText((Join-Path $stage 'README.txt'), $crlf, (New-Object System.Text.UTF8Encoding($false)))

    # What the packaging half reads instead of being told again. Beside the
    # staged folder, not inside it: everything inside goes in the zip.
    $manifest = [PSCustomObject]@{
        version      = $Version
        name         = $name
        skippedTests = [bool]$SkipTests
    }
    [System.IO.File]::WriteAllText((Join-Path $work 'release.json'),
        ($manifest | ConvertTo-Json), (New-Object System.Text.UTF8Encoding($false)))

    # The tree was clean at Step 1 and the build dirtied it. Said out loud
    # rather than quietly repaired: this script does not run `git checkout` on
    # the user's files. The one file it happens to is crates\trix-ui\Cargo.toml,
    # rewritten by cargo tauri build with different line endings and identical
    # content.
    Push-Location $RepoRoot
    try { $dirtyAfter = @(git status --porcelain) } finally { Pop-Location }
    if ($dirtyAfter.Count -gt 0 -and $isClean) {
        Write-Host ''
        Write-Host 'Note: the build left the working tree dirty. Expected -- cargo tauri build' -ForegroundColor Yellow
        Write-Host '      rewrites crates\trix-ui\Cargo.toml with different line endings and the' -ForegroundColor Yellow
        Write-Host '      same content. Restore it with: git checkout -- crates/trix-ui/Cargo.toml' -ForegroundColor Yellow
    }

    if ($StageOnly) {
        Test-StagedBinaries -Stage $stage -Version $Version
        $passed = @($script:Checks | Where-Object { $_.Passed }).Count

        Write-Host ''
        Write-Host ("  " + $stage)
        Write-Host ''
        Write-Host "STAGED, NOT PACKAGED  ($passed checks)" -ForegroundColor Green
        Write-Host 'Sign the three .exe files in that folder, then package them with:' -ForegroundColor Cyan
        Write-Host "  scripts\ship-zip.ps1 -FromStage -StageDir $work -OutDir <dir>"
        Write-Host 'Deleting that folder afterwards is the caller''s job; this run left it alone.'
        exit 0
    }

    Complete-Package -Stage $stage -Name $name -Version $Version `
        -OutDir $OutDir -SkippedTests ([bool]$SkipTests)
} finally {
    if ($ownsWork) { Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue }
}

