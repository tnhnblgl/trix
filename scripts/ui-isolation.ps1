# Spec 3.2: trix-ui must not depend on trix-core, directly or transitively.
#
# This is the only automated thing standing between the published control
# protocol and a first-party UI that quietly grows privileged access. Once the
# UI can call into the engine, every gap in the protocol stops being a bug
# somebody has to fix and starts being an inconvenience somebody can route
# around -- and a third-party UI, which has no such shortcut, becomes a
# second-class client. The rule is cheap to keep and impossible to restore
# after it has been broken for a release.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Push-Location (Join-Path $PSScriptRoot '..')
try {
    # -e no-dev: a dev-dependency on the engine in a test would be a different
    # (and much smaller) problem than the shipped binary linking it.
    $tree = & cargo tree -p trix-ui -e no-dev 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAIL: cargo tree failed"
        $tree | ForEach-Object { Write-Host "  $_" }
        exit 1
    }

    $offenders = @($tree | Where-Object { $_ -match '\btrix-core\b' })
    if ($offenders.Count -gt 0) {
        Write-Host "FAIL: trix-ui depends on trix-core (spec 3.2)"
        # ForEach-Object rather than .Trim() over the array: under
        # Set-StrictMode member enumeration on an empty array is an error, and
        # this branch is one edit away from being reachable with none.
        $offenders | ForEach-Object { Write-Host "  $($_.Trim())" }
        exit 1
    }

    Write-Host "OK: trix-ui does not depend on trix-core"
    exit 0
}
finally {
    Pop-Location
}
