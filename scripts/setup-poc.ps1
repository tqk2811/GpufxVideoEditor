# Initialises the FFmpegGpuFilter submodule and its packages and the GpufxVideoEditor-Assets submodule, downloads the LGPL and GPL FFmpeg builds
# into third_party/ffmpeg and third_party/ffmpeg-gpl, and generates the POC test clips.
# The clips are encoded by the ffmpeg on PATH (libx264 required). They are local test data
# and are never distributed, so a GPL build is fine for this step only.
[CmdletBinding()]
param(
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$releaseTag = 'ffmpeg-N-128691-gcc583787bb'
$assetName = "$releaseTag-win64-lgpl-shared.zip"
$releaseRepo = 'tqk2811/FFmpegGpuFilter'
$ffmpegRoot = Join-Path $repoRoot 'third_party\ffmpeg'
$gplRoot = Join-Path $repoRoot 'third_party\ffmpeg-gpl'
$gplAssetName = "$releaseTag-win64-gpl-shared.zip"
# The same FFmpeg commit from upstream. Upstream removes old autobuilds, so this URL can expire.
$gplUrl = "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-11-08-12-35/$gplAssetName"
$gplSha256 = '8e6372ef8382b9b0acc74ceddfebcff5627442d6d4330ab9c2ce8a8695d217e0'
$toolsDir = Join-Path $repoRoot '.tools'
$clipsDir = Join-Path $repoRoot 'test-clips'

# Not --recursive: that would also fetch the FFmpeg sources of FFmpegGpuFilter, which the editor does not use.
function Initialize-Submodules {
    & git -C $repoRoot submodule update --init external/FFmpegGpuFilter
    if ($LASTEXITCODE -ne 0) { throw "git submodule update failed ($LASTEXITCODE)" }
    & git -C (Join-Path $repoRoot 'external\FFmpegGpuFilter') submodule update --init packages
    if ($LASTEXITCODE -ne 0) { throw "git submodule update failed ($LASTEXITCODE)" }
    & git -C (Join-Path $repoRoot 'external\FFmpegGpuFilter\packages') submodule update --init gpufx/capcut
    if ($LASTEXITCODE -ne 0) { throw "git submodule update failed ($LASTEXITCODE)" }
    & git -C $repoRoot submodule update --init external/GpufxVideoEditor-Assets
    if ($LASTEXITCODE -ne 0) { throw "git submodule update failed ($LASTEXITCODE)" }
}

function Install-FFmpeg {
    if ((Test-Path (Join-Path $ffmpegRoot 'include\libavcodec\avcodec.h')) -and -not $Force) {
        Write-Host "FFmpeg already present at $ffmpegRoot"
        return
    }

    New-Item -ItemType Directory -Force $toolsDir | Out-Null
    & gh release download $releaseTag -R $releaseRepo -D $toolsDir --clobber -p $assetName -p sums.txt
    if ($LASTEXITCODE -ne 0) { throw "gh release download failed ($LASTEXITCODE)" }

    $archive = Join-Path $toolsDir $assetName
    $expected = $null
    foreach ($line in Get-Content (Join-Path $toolsDir 'sums.txt')) {
        $parts = $line.Trim() -split '\s+', 2
        if ($parts.Count -eq 2 -and $parts[1].TrimStart('*').EndsWith($assetName)) {
            $expected = $parts[0].ToLowerInvariant()
        }
    }
    if (-not $expected) { throw "$assetName is not listed in sums.txt" }

    $actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw "SHA-256 mismatch: expected $expected, got $actual" }
    Write-Host "SHA-256 verified: $actual"

    Expand-FFmpeg -Archive $archive -Destination $ffmpegRoot
}

function Install-GplFFmpeg {
    if ((Test-Path (Join-Path $gplRoot 'include\libavcodec\avcodec.h')) -and -not $Force) {
        Write-Host "FFmpeg already present at $gplRoot"
        return
    }

    New-Item -ItemType Directory -Force $toolsDir | Out-Null
    $archive = Join-Path $toolsDir $gplAssetName
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri $gplUrl -OutFile $archive

    $actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
    if ($actual -ne $gplSha256) { throw "SHA-256 mismatch: expected $gplSha256, got $actual" }
    Write-Host "SHA-256 verified: $actual"

    Expand-FFmpeg -Archive $archive -Destination $gplRoot
}

function Expand-FFmpeg {
    param(
        [string]$Archive,
        [string]$Destination
    )

    $extractDir = Join-Path $toolsDir 'ffmpeg-extract'
    if (Test-Path $extractDir) { Remove-Item -LiteralPath $extractDir -Recurse -Force }
    Expand-Archive -LiteralPath $Archive -DestinationPath $extractDir

    $inner = Get-ChildItem -LiteralPath $extractDir -Directory | Select-Object -First 1
    if (-not $inner -or -not (Test-Path (Join-Path $inner.FullName 'include'))) {
        throw 'Unexpected archive layout: no include directory'
    }

    if (Test-Path $Destination) { Remove-Item -LiteralPath $Destination -Recurse -Force }
    New-Item -ItemType Directory -Force (Split-Path -Parent $Destination) | Out-Null
    Move-Item -LiteralPath $inner.FullName -Destination $Destination
    Remove-Item -LiteralPath $extractDir -Recurse -Force
    Write-Host "FFmpeg extracted to $Destination"
}

function New-TestClip {
    param(
        [string]$Name,
        [string]$Source
    )

    $output = Join-Path $clipsDir $Name
    if ((Test-Path $output) -and -not $Force) {
        Write-Host "Clip already present: $output"
        return
    }

    # Burnt-in frame index: a 16-cell binary bar along the top edge, white = 1, MSB on the left.
    $cellWidth = 120
    $boxes = 0..15 | ForEach-Object {
        $shift = 15 - $_
        $x = $_ * $cellWidth
        "drawbox=x=${x}:y=0:w=${cellWidth}:h=64:color=white:t=fill:enable='mod(floor(n/pow(2,$shift)),2)'"
    }
    $background = 'drawbox=x=0:y=0:w=1920:h=64:color=black:t=fill'
    $filter = "format=yuv420p,$background," + ($boxes -join ',')

    & ffmpeg -hide_banner -loglevel warning -y -f lavfi -i $Source -t 20 -vf $filter `
        -an -c:v libx264 -preset veryfast -profile:v high -pix_fmt yuv420p `
        -g 30 -keyint_min 30 -sc_threshold 0 -bf 2 -crf 18 -movflags +faststart $output
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed for $Name ($LASTEXITCODE)" }
    Write-Host "Generated $output"
}

Initialize-Submodules
Install-FFmpeg
Install-GplFFmpeg

New-Item -ItemType Directory -Force $clipsDir | Out-Null
New-TestClip -Name 'clip-a.mp4' -Source 'testsrc2=size=1920x1080:rate=30'
New-TestClip -Name 'clip-b.mp4' -Source 'smptehdbars=size=1920x1080:rate=30,hue=H=2*PI*t/10'
New-TestClip -Name 'clip-c.mp4' -Source 'mandelbrot=size=1920x1080:rate=30'
