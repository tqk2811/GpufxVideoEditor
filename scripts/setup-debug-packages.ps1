# Fills target\debug\packages so a debug build of the editor starts with every package:
# - packages\gpufx: junctions to the effect and transition groups of FFmpegGpuFilter and GpufxVideoEditor-Assets.
# - packages\gpufx_ai_*: junctions to the AI packages of FFmpegGpuFilter.
# - packages\sfx, music, sticker: the audio and sticker packs of the GpufxVideoEditor-Assets releases, unpacked
#   the way the editor installs them.
# The editor uses the packages next to its executable before those of the checkout, so once this folder
# exists it must hold everything. Run it again after cargo clean, or with -Force to fetch the packs again.
[CmdletBinding()]
param(
    [string]$Target = 'debug',
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$assetsRepo = 'tqk2811/GpufxVideoEditor-Assets'
$releases = @('audio-v1', 'stickers-v1')
$outDir = Join-Path $repoRoot "target\$Target"
$packagesDir = Join-Path $outDir 'packages'
$shaderSources = @(
    (Join-Path $repoRoot 'external\FFmpegGpuFilter\packages'),
    (Join-Path $repoRoot 'external\GpufxVideoEditor-Assets\packages')
)

if (-not (Test-Path -LiteralPath (Join-Path $shaderSources[0] 'gpufx') -PathType Container)) {
    throw 'external\FFmpegGpuFilter\packages is empty: run scripts\setup-poc.ps1 first'
}

# Removes a junction without touching the folder it points to.
function Remove-Junction([string]$Path) {
    if (Test-Path -LiteralPath $Path) {
        [System.IO.Directory]::Delete($Path, $false)
    }
}

function New-Junction([string]$Path, [string]$To) {
    Remove-Junction $Path
    New-Item -ItemType Junction -Path $Path -Target $To | Out-Null
}

New-Item -ItemType Directory -Force -Path (Join-Path $packagesDir 'gpufx') | Out-Null

foreach ($source in $shaderSources) {
    $gpufx = Join-Path $source 'gpufx'
    if (-not (Test-Path -LiteralPath $gpufx -PathType Container)) { continue }
    foreach ($group in Get-ChildItem -LiteralPath $gpufx -Directory) {
        New-Junction (Join-Path $packagesDir "gpufx\$($group.Name)") $group.FullName
    }
    foreach ($ai in Get-ChildItem -LiteralPath $source -Directory -Filter 'gpufx_*') {
        New-Junction (Join-Path $packagesDir $ai.Name) $ai.FullName
    }
}

$download = Join-Path $outDir 'package-downloads'
if ($Force -and (Test-Path -LiteralPath $download)) {
    Remove-Item -LiteralPath $download -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $download | Out-Null
foreach ($release in $releases) {
    $folder = Join-Path $download $release
    if (-not (Test-Path -LiteralPath $folder)) {
        & gh release download $release -R $assetsRepo -D $folder -p '*.zip'
        if ($LASTEXITCODE -ne 0) { throw "downloading release $release failed ($LASTEXITCODE)" }
    }
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
foreach ($zip in Get-ChildItem -LiteralPath $download -Recurse -Filter '*.zip') {
    $archive = [System.IO.Compression.ZipFile]::OpenRead($zip.FullName)
    try {
        $entry = $archive.GetEntry('manifest.json')
        if ($null -eq $entry) { throw "$($zip.Name) has no manifest.json" }
        $reader = New-Object System.IO.StreamReader($entry.Open())
        try { $manifest = $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
    } finally {
        $archive.Dispose()
    }
    if ($manifest.type -notin @('sfx', 'music', 'sticker')) { throw "$($zip.Name) has an unknown type '$($manifest.type)'" }
    $packDir = Join-Path $packagesDir "$($manifest.type)\$($manifest.id.ToLowerInvariant())"
    if (Test-Path -LiteralPath $packDir) {
        Remove-Item -LiteralPath $packDir -Recurse -Force
    }
    [System.IO.Compression.ZipFile]::ExtractToDirectory($zip.FullName, $packDir)
    Write-Host "unpacked $($zip.Name) -> $packDir"
}

Write-Host "packages ready in $packagesDir"
