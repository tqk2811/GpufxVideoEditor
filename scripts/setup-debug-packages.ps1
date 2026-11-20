# Fills target\debug\packages so a debug build of the editor starts with every package:
# - packages\gpufx\<group>\<package>.zip: every effect and transition package of FFmpegGpuFilter and
#   GpufxVideoEditor-Assets, packed by `gpufx pack` like a release (scripts\pack-packages.ps1 of FFmpegGpuFilter
#   for its own packages, the gpufx CLI it builds for the rest).
# - packages\gpufx_ai_<filter>\<model>.zip: the AI models of FFmpegGpuFilter, fetched and converted by its
#   pack-packages.ps1 -Models (needs network; they are not in git).
# - packages\gpufx\capcut\<package>.zip: the packages of a shallow clone of FFmpegGpuFilter-packages-capcut.
# The effect packages are zips, not links to the checkouts: editing a shader or manifest there needs a rerun of
# this script (cargo is needed to build the gpufx CLI).
# - packages\sfx, music, sticker, clip-animation, font: the packs of the GpufxVideoEditor-Assets releases, unpacked
#   the way the editor installs them, with the translations (i18n\) of the GpufxVideoEditor-Assets checkout,
#   which may be newer than the releases.
# - the private CapCut music, sticker and text style packs, copied from a GpufxVideoEditor-Assets-capcut-private
#   checkout next to this repository (or -CapcutPrivate) when there is one.
# The editor uses the packages next to its executable before those of the checkout, so once this folder
# exists it must hold everything. Run it again after cargo clean, or with -Force to fetch the packs again.
[CmdletBinding()]
param(
    [string]$Target = 'debug',
    [string]$CapcutPrivate = '',
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$assetsRepo = 'tqk2811/GpufxVideoEditor-Assets'
$releases = @('audio-v0.1.0', 'stickers-v0.1.0', 'animations-v0.1.0')
$capcutRepos = @(
    @{ Repo = 'tqk2811/FFmpegGpuFilter-packages-capcut'; Group = 'capcut' }
)
$outDir = Join-Path $repoRoot "target\$Target"
$packagesDir = Join-Path $outDir 'packages'

if (-not (Test-Path -LiteralPath (Join-Path $repoRoot 'external\FFmpegGpuFilter\packages\gpufx') -PathType Container)) {
    throw 'external\FFmpegGpuFilter\packages is empty: run scripts\setup-poc.ps1 first'
}

# Removes a junction without touching the folder it points to.
function Remove-Junction([string]$Path) {
    if (Test-Path -LiteralPath $Path) {
        [System.IO.Directory]::Delete($Path, $false)
    }
}

# Clears what an earlier run left in $Path: junctions are unlinked (the target stays), real folders deleted.
function Clear-Packages([string]$Path) {
    foreach ($item in Get-ChildItem -LiteralPath $Path -Force) {
        if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
            Remove-Junction $item.FullName
        } elseif ($item.PSIsContainer) {
            Remove-Item -LiteralPath $item.FullName -Recurse -Force
        } else {
            Remove-Item -LiteralPath $item.FullName -Force
        }
    }
}

$gpufxDir = Join-Path $packagesDir 'gpufx'
New-Item -ItemType Directory -Force -Path $gpufxDir | Out-Null
Clear-Packages $gpufxDir
foreach ($old in Get-ChildItem -LiteralPath $packagesDir -Force -Directory -Filter 'gpufx_ai_*') {
    if ($old.Attributes -band [System.IO.FileAttributes]::ReparsePoint) { Remove-Junction $old.FullName }
    else { Remove-Item -LiteralPath $old.FullName -Recurse -Force }
}

# FFmpegGpuFilter: its own script packs packages\gpufx -> gpufx\<group>\<name>.zip and the models -> gpufx_ai_*.
& (Join-Path $repoRoot 'external\FFmpegGpuFilter\scripts\pack-packages.ps1') -Out $gpufxDir -Models
if (-not $?) { throw 'pack-packages.ps1 of FFmpegGpuFilter failed' }
$gpufxCli = Join-Path $repoRoot 'external\FFmpegGpuFilter\target\release\gpufx.exe'
if (-not (Test-Path -LiteralPath $gpufxCli)) { throw "$gpufxCli missing after pack-packages.ps1" }

# Packs every folder with a manifest.json under $Source into $Out\<relative path>.zip.
function Pack-Folders([string]$Source, [string]$Out) {
    $count = 0
    foreach ($manifestFile in Get-ChildItem -LiteralPath $Source -Recurse -File -Filter 'manifest.json') {
        $dir = $manifestFile.Directory.FullName
        $rel = $dir.Substring($Source.TrimEnd('').Length + 1)
        $zip = Join-Path $Out "$rel.zip"
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $zip) | Out-Null
        # Out-Null: the CLI's "wrote ..." line would otherwise become part of the returned count.
        & $gpufxCli pack $dir $zip | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "packing $dir failed ($LASTEXITCODE)" }
        $count++
    }
    return $count
}

$assetsGpufx = Join-Path $repoRoot 'external\GpufxVideoEditor-Assets\packages\gpufx'
if (Test-Path -LiteralPath $assetsGpufx -PathType Container) {
    $count = Pack-Folders $assetsGpufx $gpufxDir
    Write-Host "packed $count packages of GpufxVideoEditor-Assets -> $gpufxDir"
}

$download = Join-Path $outDir 'package-downloads'
if ($Force -and (Test-Path -LiteralPath $download)) {
    Remove-Item -LiteralPath $download -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $download | Out-Null

# The CapCut recreations live in repositories of their own that are never submodules here. A debug build
# still gets them, packed from a shallow clone of each, so every package can be tried locally.
foreach ($capcut in $capcutRepos) {
    $clone = Join-Path $download ($capcut.Repo -replace '^.*/', '')
    if (-not (Test-Path -LiteralPath $clone)) {
        & git -c core.longpaths=true clone -q --depth 1 "https://github.com/$($capcut.Repo).git" $clone
        if ($LASTEXITCODE -ne 0) { throw "cloning $($capcut.Repo) failed ($LASTEXITCODE)" }
    } else {
        # The clone is only read, so it simply follows the repository.
        & git -C $clone fetch -q --depth 1 origin HEAD
        if ($LASTEXITCODE -ne 0) { throw "fetching $($capcut.Repo) failed ($LASTEXITCODE)" }
        & git -C $clone reset -q --hard FETCH_HEAD
        if ($LASTEXITCODE -ne 0) { throw "updating the clone of $($capcut.Repo) failed ($LASTEXITCODE)" }
    }
    $groupDir = Join-Path $gpufxDir $capcut.Group
    New-Item -ItemType Directory -Force -Path $groupDir | Out-Null
    # The packages sit in src\ of the repository; an older clone kept them at its root.
    $source = Join-Path $clone 'src'
    if (-not (Test-Path -LiteralPath $source)) { $source = $clone }
    $count = Pack-Folders $source $groupDir
    Write-Host "packed $count packages of $($capcut.Repo) -> $groupDir"
}
foreach ($release in $releases) {
    $folder = Join-Path $download $release
    if (-not (Test-Path -LiteralPath $folder)) {
        & gh release download $release -R $assetsRepo -D $folder -p '*.zip'
        if ($LASTEXITCODE -ne 0) { throw "downloading release $release failed ($LASTEXITCODE)" }
    }
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
foreach ($zip in Get-ChildItem -LiteralPath $download -Recurse -Filter '*.zip') {
    # audio-packs.zip only bundles the pack zips of the same release for a single download.
    if ($zip.Name -eq 'audio-packs.zip') { continue }
    $archive = [System.IO.Compression.ZipFile]::OpenRead($zip.FullName)
    try {
        $entry = $archive.GetEntry('manifest.json')
        if ($null -eq $entry) { throw "$($zip.Name) has no manifest.json" }
        $reader = New-Object System.IO.StreamReader($entry.Open())
        try { $manifest = $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
    } finally {
        $archive.Dispose()
    }
    if ($manifest.type -notin @('sfx', 'music', 'sticker', 'clip-animation', 'text-animation', 'font')) { throw "$($zip.Name) has an unknown type '$($manifest.type)'" }
    # text-animation is the older name of clip-animation; the editor installs both to clip-animation.
    $typeFolder = if ($manifest.type -eq 'text-animation') { 'clip-animation' } else { $manifest.type }
    $packDir = Join-Path $packagesDir "$typeFolder\$($manifest.id.ToLowerInvariant())"
    if (Test-Path -LiteralPath $packDir) {
        Remove-Item -LiteralPath $packDir -Recurse -Force
    }
    [System.IO.Compression.ZipFile]::ExtractToDirectory($zip.FullName, $packDir)
    Write-Host "unpacked $($zip.Name) -> $packDir"
}

# The translations of the assets checkout, by pack id: a folder with a manifest.json or pack.json is named by
# its id, one holding only i18n\ (audio, stickers) by the folder name.
$assetsRoot = Join-Path $repoRoot 'external\GpufxVideoEditor-Assets'
$translations = @{}
foreach ($kind in @('audio', 'stickers', 'animations', 'text-styles')) {
    $kindDir = Join-Path $assetsRoot $kind
    if (-not (Test-Path -LiteralPath $kindDir)) { continue }
    foreach ($pack in Get-ChildItem -LiteralPath $kindDir -Directory) {
        $i18n = Join-Path $pack.FullName 'i18n'
        if (-not (Test-Path -LiteralPath $i18n)) { continue }
        $id = $pack.Name
        foreach ($name in @('manifest.json', 'pack.json')) {
            $file = Join-Path $pack.FullName $name
            if (Test-Path -LiteralPath $file) { $id = (Get-Content -LiteralPath $file -Raw -Encoding UTF8 | ConvertFrom-Json).id; break }
        }
        $translations[$id.ToLowerInvariant()] = $i18n
    }
}
foreach ($typeFolder in @('sfx', 'music', 'sticker', 'clip-animation', 'text-style')) {
    $typeDir = Join-Path $packagesDir $typeFolder
    if (-not (Test-Path -LiteralPath $typeDir)) { continue }
    foreach ($packDir in Get-ChildItem -LiteralPath $typeDir -Directory) {
        $i18n = $translations[$packDir.Name.ToLowerInvariant()]
        if ($null -eq $i18n) { continue }
        $target = Join-Path $packDir.FullName 'i18n'
        if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Recurse -Force }
        Copy-Item -LiteralPath $i18n -Destination $target -Recurse
        Write-Host "translations of $($packDir.Name) <- $i18n"
    }
}

# The private CapCut packs: folders of a checkout, copied as they are.
if ($CapcutPrivate -eq '') { $CapcutPrivate = Join-Path (Split-Path -Parent $repoRoot) 'GpufxVideoEditor-Assets-capcut-private' }
if (Test-Path -LiteralPath $CapcutPrivate -PathType Container) {
    foreach ($kind in @(@{ Source = 'music'; Type = 'music' }, @{ Source = 'sticker'; Type = 'sticker' }, @{ Source = 'text-style'; Type = 'text-style' })) {
        $kindDir = Join-Path $CapcutPrivate $kind.Source
        if (-not (Test-Path -LiteralPath $kindDir)) { continue }
        foreach ($pack in Get-ChildItem -LiteralPath $kindDir -Directory) {
            $manifest = Join-Path $pack.FullName 'manifest.json'
            if (-not (Test-Path -LiteralPath $manifest)) { continue }
            $id = (Get-Content -LiteralPath $manifest -Raw -Encoding UTF8 | ConvertFrom-Json).id.ToLowerInvariant()
            $packDir = Join-Path $packagesDir "$($kind.Type)\$id"
            if (Test-Path -LiteralPath $packDir) { Remove-Item -LiteralPath $packDir -Recurse -Force }
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $packDir) | Out-Null
            Copy-Item -LiteralPath $pack.FullName -Destination $packDir -Recurse
            Write-Host "copied $($pack.FullName) -> $packDir"
        }
    }
} else {
    Write-Host "no CapCut private checkout at $CapcutPrivate, skipped"
}

Write-Host "packages ready in $packagesDir"
