# Builds gpufx-render and gpufx-editor against both FFmpeg variants and runs the tests of each.
# Output: target\release (LGPL) and target\gpl\release (GPL). Each variant needs its own target
# directory because the FFmpeg DLLs are copied next to the executables.
[CmdletBinding()]
param(
    [switch]$SkipTests
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$packages = @('-p', 'engine', '-p', 'd3d12-backend', '-p', 'gpufx-render', '-p', 'gpufx-editor')

function Invoke-Cargo {
    param([string[]]$CargoArguments)

    Write-Host "cargo $($CargoArguments -join ' ')"
    & cargo @CargoArguments
    if ($LASTEXITCODE -ne 0) { throw "cargo failed ($LASTEXITCODE)" }
}

function Build-Variant {
    param(
        [string]$TargetDir,
        [string[]]$Features
    )

    $env:CARGO_TARGET_DIR = $TargetDir
    if (-not $SkipTests) {
        Invoke-Cargo (@('test') + $packages + $Features)
    }
    Invoke-Cargo (@('build', '--release', '-p', 'gpufx-render', '-p', 'gpufx-editor') + $Features)
}

Push-Location $repoRoot
try {
    Build-Variant -TargetDir (Join-Path $repoRoot 'target') -Features @()
    Build-Variant -TargetDir (Join-Path $repoRoot 'target\gpl') -Features @('--features', 'gpufx-render/ffmpeg-gpl,gpufx-editor/ffmpeg-gpl')
}
finally {
    Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    Pop-Location
}
