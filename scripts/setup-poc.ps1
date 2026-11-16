# Initialises the FFmpegGpuFilter submodule and its packages and the GpufxVideoEditor-Assets submodule, downloads the LGPL and GPL FFmpeg builds
# into third_party/ffmpeg and third_party/ffmpeg-gpl, adds dxcompiler.dll (DXC) to third_party/ffmpeg/bin,
# and generates the POC test clips.
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

# DirectX Shader Compiler from NuGet, the same version and hash as FFmpegGpuFilter's scripts/fetch-dxc.sh.
# wgpu needs it on D3D12 for the gpufx-nn shaders of AI background removal, which FXC cannot compile.
# The build scripts copy every runtime DLL of third_party/ffmpeg/bin next to the executables.
function Install-Dxc {
    $bin = Join-Path $ffmpegRoot 'bin'
    if ((Test-Path (Join-Path $bin 'dxcompiler.dll')) -and -not $Force) {
        Write-Host "dxcompiler.dll already present in $bin"
        return
    }

    $version = '1.9.2609.5'
    $sha256 = '73cfa082245918c2cc1ec6740250eb7377e83a757b3b3a3e16e0d71535bfbfc3'
    New-Item -ItemType Directory -Force $toolsDir | Out-Null
    $package = Join-Path $toolsDir "microsoft.direct3d.dxc.$version.nupkg"
    if (-not (Test-Path $package) -or (Get-FileHash -Algorithm SHA256 $package).Hash.ToLowerInvariant() -ne $sha256) {
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri "https://api.nuget.org/v3-flatcontainer/microsoft.direct3d.dxc/$version/microsoft.direct3d.dxc.$version.nupkg" -OutFile $package
    }
    $actual = (Get-FileHash -Algorithm SHA256 $package).Hash.ToLowerInvariant()
    if ($actual -ne $sha256) { throw "SHA-256 mismatch for DXC: expected $sha256, got $actual" }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($package)
    try {
        # The package is LLVM (University of Illinois/NCSA) licensed, with MIT parts and Microsoft terms
        # for the binaries; all three texts go next to the DLL.
        $members = [ordered]@{
            'build/native/bin/x64/dxcompiler.dll' = 'dxcompiler.dll'
            'LICENSE-LLVM.txt' = 'LICENSE-dxcompiler-LLVM.txt'
            'LICENCE-MIT.txt' = 'LICENSE-dxcompiler-MIT.txt'
            'LICENSE-MS.txt' = 'LICENSE-dxcompiler-MS.txt'
        }
        foreach ($member in $members.Keys) {
            $entry = $zip.GetEntry($member)
            if (-not $entry) { throw "DXC package has no $member" }
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $bin $members[$member]), $true)
        }
    } finally {
        $zip.Dispose()
    }
    Write-Host "DXC $version extracted to $bin"
}

function New-TestClip {
    param(
        [string]$Name,
        [string]$Source,
        # Frames between keyframes; scene cuts never add one.
        [int]$Gop = 30
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
        -g $Gop -keyint_min $Gop -sc_threshold 0 -bf 2 -crf 18 -movflags +faststart $output
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed for $Name ($LASTEXITCODE)" }
    Write-Host "Generated $output"
}

Initialize-Submodules
Install-FFmpeg
Install-GplFFmpeg
Install-Dxc

New-Item -ItemType Directory -Force $clipsDir | Out-Null
New-TestClip -Name 'clip-a.mp4' -Source 'testsrc2=size=1920x1080:rate=30'
New-TestClip -Name 'clip-b.mp4' -Source 'smptehdbars=size=1920x1080:rate=30,hue=H=2*PI*t/10'
New-TestClip -Name 'clip-c.mp4' -Source 'mandelbrot=size=1920x1080:rate=30'
# Long-GOP copies (keyframes 8.3 s apart) for the seek tests, e.g. crates/gpufx-editor/tests/preview_gpu.rs.
New-TestClip -Name 'gop250-a.mp4' -Source 'testsrc2=size=1920x1080:rate=30' -Gop 250
New-TestClip -Name 'gop250-b.mp4' -Source 'smptehdbars=size=1920x1080:rate=30,hue=H=2*PI*t/10' -Gop 250
New-TestClip -Name 'gop250-c.mp4' -Source 'mandelbrot=size=1920x1080:rate=30' -Gop 250
