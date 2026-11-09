# Builds gpufx-render, gpufx-editor and gpufx-mcp against both FFmpeg variants and runs the tests of each.
# Output: target\release (LGPL) and target\gpl\release (GPL). Each variant needs its own target
# directory because the FFmpeg DLLs are copied next to the executables.
[CmdletBinding()]
param(
    [switch]$SkipTests
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$packages = @('-p', 'engine', '-p', 'd3d12-backend', '-p', 'gpufx-render', '-p', 'gpufx-editor', '-p', 'gpufx-mcp')

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
    Invoke-Cargo (@('build', '--release', '-p', 'gpufx-render', '-p', 'gpufx-editor', '-p', 'gpufx-mcp') + $Features)
    # The editor runs gpufx-render.exe from its own folder to export.
    foreach ($name in 'gpufx-render.exe', 'gpufx-editor.exe', 'gpufx-mcp.exe') {
        $built = Join-Path $TargetDir "release\$name"
        if (-not (Test-Path -LiteralPath $built -PathType Leaf)) { throw "$built was not built" }
    }
}

Push-Location $repoRoot
try {
    Build-Variant -TargetDir (Join-Path $repoRoot 'target') -Features @()
    Build-Variant -TargetDir (Join-Path $repoRoot 'target\gpl') -Features @('--features', 'gpufx-render/ffmpeg-gpl,gpufx-editor/ffmpeg-gpl,gpufx-mcp/ffmpeg-gpl')
}
finally {
    Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    Pop-Location
}
