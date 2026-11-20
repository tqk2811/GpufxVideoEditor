# Fills target\debug\packages so a debug build of the editor starts with every package:
# - packages\gpufx\<group>\<name>\: every effect and transition package of FFmpegGpuFilter, GpufxVideoEditor-Assets
#   and the CapCut repository, as the editor's wrapper pack unpacked one level: manifest.json, i18n\<code>.json
#   and <name>.zip (the inner package, packed by `gpufx pack` like a release). The inner zips are staged in
#   package-downloads\inner\gpufx (scripts\pack-packages.ps1 of FFmpegGpuFilter for its own packages, the gpufx
#   CLI it builds for the rest), `py scripts\pack.py --wrappers` of GpufxVideoEditor-Assets wraps them using its
#   gpufx\<group>\<name>\ sources, and each wrapper zip is extracted. An inner package with no wrapper source is
#   listed and not installed.
# - packages\gpufx_ai_<filter>\<model>.zip: the AI models of FFmpegGpuFilter, fetched and converted by its
#   pack-packages.ps1 -Models (needs network; they are not in git).
# - the packages of a shallow clone of FFmpegGpuFilter-packages-capcut go through the same staging, group capcut.
# The effect packages are zips, not links to the checkouts: editing a shader or manifest there needs a rerun of
# this script (cargo is needed to build the gpufx CLI, py for the wrappers).
# - packages\sfx, music, sticker, clip-animation, text-style, subtitle-style: the packs of the
#   GpufxVideoEditor-Assets releases ($releases below), one item per pack. Only the aggregate of each release
#   (*-packs.zip, a stored zip of the per-item pack zips) is downloaded and unpacked two levels: aggregate ->
#   pack zips -> pack folders, the way the editor installs them, with the translations (i18n\) of the
#   GpufxVideoEditor-Assets checkout for a pack whose id it has, which may be newer than the releases. These
#   type folders are emptied first, so packs of an older layout (one pack per set) do not show twice.
# - the private CapCut packs (one folder per item), copied from a GpufxVideoEditor-Assets-capcut-private
#   checkout next to this repository (or -CapcutPrivate) when there is one.
# The editor uses the packages next to its executable before those of the checkout, so once this folder
# exists it must hold everything. Run it again after cargo clean, or with -Force to fetch the packs again.
# -SkipModels keeps the gpufx_ai_* zips already in packages instead of fetching the models again.
[CmdletBinding()]
param(
    [string]$Target = 'debug',
    [string]$CapcutPrivate = '',
    [switch]$Force,
    [switch]$SkipModels
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$assetsRepo = 'tqk2811/GpufxVideoEditor-Assets'
# The GpufxVideoEditor-Assets releases to install, one tag per line (provisional tags until they are published).
$releases = @(
    'audio-v0.2.0'
    'stickers-v0.2.0'
    'animations-v0.2.0'
    'text-styles-v0.1.0'
)
# The type folders the releases and the CapCut private packs fill; emptied before they are filled again.
$contentTypes = @('sfx', 'music', 'sticker', 'clip-animation', 'text-style', 'subtitle-style')
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

# packages\gpufx is built in a staging folder and swapped in only once every step succeeded, so a failure keeps
# the previous one instead of leaving it empty (tests needing effects would then skip themselves).
$gpufxDir = Join-Path $packagesDir 'gpufx'
New-Item -ItemType Directory -Force -Path $packagesDir | Out-Null
$existingModels = @(Get-ChildItem -LiteralPath $packagesDir -Force -Directory -Filter 'gpufx_ai_*')
if ($SkipModels -and $existingModels.Count -eq 0) { throw '-SkipModels: there is no gpufx_ai_* folder in packages to keep' }
if (-not $SkipModels) {
    foreach ($old in $existingModels) {
        if ($old.Attributes -band [System.IO.FileAttributes]::ReparsePoint) { Remove-Junction $old.FullName }
        else { Remove-Item -LiteralPath $old.FullName -Recurse -Force }
    }
}

# The inner packages are staged here, laid out <innerRoot>\gpufx\<group>\<name>.zip (+ gpufx_ai_* with the models).
$download = Join-Path $outDir 'package-downloads'
if ($Force -and (Test-Path -LiteralPath $download)) {
    Remove-Item -LiteralPath $download -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $download | Out-Null
$innerRoot = Join-Path $download 'inner'
$wrapperRoot = Join-Path $download 'wrappers'
foreach ($stale in @($innerRoot, $wrapperRoot)) {
    if (Test-Path -LiteralPath $stale) { Remove-Item -LiteralPath $stale -Recurse -Force }
}
$innerGpufx = Join-Path $innerRoot 'gpufx'
New-Item -ItemType Directory -Force -Path $innerGpufx | Out-Null
$gpufxStaging = Join-Path $download 'gpufx-staging'
if (Test-Path -LiteralPath $gpufxStaging) { Remove-Item -LiteralPath $gpufxStaging -Recurse -Force }
New-Item -ItemType Directory -Force -Path $gpufxStaging | Out-Null

# FFmpegGpuFilter: its own script packs packages\gpufx -> gpufx\<group>\<name>.zip and the models -> gpufx_ai_*.
if ($SkipModels) {
    & (Join-Path $repoRoot 'external\FFmpegGpuFilter\scripts\pack-packages.ps1') -Out $innerGpufx
} else {
    & (Join-Path $repoRoot 'external\FFmpegGpuFilter\scripts\pack-packages.ps1') -Out $innerGpufx -Models
}
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
    $count = Pack-Folders $assetsGpufx $innerGpufx
    Write-Host "packed $count packages of GpufxVideoEditor-Assets -> $innerGpufx"
}

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
    $groupDir = Join-Path $innerGpufx $capcut.Group
    New-Item -ItemType Directory -Force -Path $groupDir | Out-Null
    # The packages sit in src\ of the repository; an older clone kept them at its root.
    $source = Join-Path $clone 'src'
    if (-not (Test-Path -LiteralPath $source)) { $source = $clone }
    $count = Pack-Folders $source $groupDir
    Write-Host "packed $count packages of $($capcut.Repo) -> $groupDir"
}
# Wraps every inner package with the sources of GpufxVideoEditor-Assets (gpufx\<group>\<name>\) and unpacks each
# wrapper zip one level: packages\gpufx\<group>\<name>\{manifest.json, i18n\<code>.json, <name>.zip}.
$packScript = Join-Path $repoRoot 'external\GpufxVideoEditor-Assets\scripts\pack.py'
& py $packScript --wrappers $innerGpufx -o $wrapperRoot
if ($LASTEXITCODE -ne 0) { throw "pack.py --wrappers failed ($LASTEXITCODE)" }
Add-Type -AssemblyName System.IO.Compression.FileSystem
$wrapperGpufx = Join-Path $wrapperRoot 'gpufx'
$wrapped = @{}
foreach ($wrapperZip in Get-ChildItem -LiteralPath $wrapperGpufx -Recurse -File -Filter '*.zip.gpufxpack') {
    $rel = $wrapperZip.FullName.Substring($wrapperGpufx.Length + 1)
    $rel = $rel -replace '\.zip\.gpufxpack$', ''
    $wrapped["$rel.zip".ToLowerInvariant()] = $true
    $packDir = Join-Path $gpufxStaging $rel
    if (Test-Path -LiteralPath $packDir) { Remove-Item -LiteralPath $packDir -Recurse -Force }
    [System.IO.Compression.ZipFile]::ExtractToDirectory($wrapperZip.FullName, $packDir)
}
if ($wrapped.Count -eq 0) { throw "pack.py --wrappers produced no wrapper pack" }
# Every effect package is ready: swap the new tree in, then drop the old one.
$gpufxOld = Join-Path $download 'gpufx-old'
if (Test-Path -LiteralPath $gpufxOld) { Clear-Packages $gpufxOld; Remove-Item -LiteralPath $gpufxOld -Recurse -Force }
if (Test-Path -LiteralPath $gpufxDir) { Move-Item -LiteralPath $gpufxDir -Destination $gpufxOld }
Move-Item -LiteralPath $gpufxStaging -Destination $gpufxDir
if (Test-Path -LiteralPath $gpufxOld) { Clear-Packages $gpufxOld; Remove-Item -LiteralPath $gpufxOld -Recurse -Force }
Write-Host "unpacked $($wrapped.Count) wrapper packs -> $gpufxDir"
$unwrapped = @()
foreach ($innerZip in Get-ChildItem -LiteralPath $innerGpufx -Recurse -File -Filter '*.zip') {
    $rel = $innerZip.FullName.Substring($innerGpufx.Length + 1)
    if (-not $wrapped.ContainsKey($rel.ToLowerInvariant())) { $unwrapped += $rel }
}
if ($unwrapped.Count -gt 0) {
    Write-Warning "$($unwrapped.Count) inner packages have no wrapper source in GpufxVideoEditor-Assets\gpufx and are not installed:"
    foreach ($rel in $unwrapped) { Write-Warning "  $rel" }
}
# The models go next to gpufx, as in a release.
if (-not $SkipModels) {
    foreach ($models in Get-ChildItem -LiteralPath $innerRoot -Directory -Filter 'gpufx_ai_*') {
        Move-Item -LiteralPath $models.FullName -Destination (Join-Path $packagesDir $models.Name)
    }
}

foreach ($release in $releases) {
    $folder = Join-Path $download $release
    if (-not (Test-Path -LiteralPath $folder) -or @(Get-ChildItem -LiteralPath $folder -File -Filter '*-packs.zip').Count -eq 0) {
        & gh release download $release -R $assetsRepo -D $folder -p '*-packs.zip' --clobber
        if ($LASTEXITCODE -ne 0) { throw "downloading release $release failed ($LASTEXITCODE)" }
    }
}

# Unpacks one pack zip (manifest.json at its root) into packages\<type>\<id lower-cased>.
function Install-PackZip([string]$ZipPath) {
    $archive = [System.IO.Compression.ZipFile]::OpenRead($ZipPath)
    try {
        $entry = $archive.GetEntry('manifest.json')
        if ($null -eq $entry) { throw "$ZipPath has no manifest.json" }
        $reader = New-Object System.IO.StreamReader($entry.Open())
        try { $manifest = $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
    } finally {
        $archive.Dispose()
    }
    if ($manifest.type -notin @('sfx', 'music', 'sticker', 'clip-animation', 'text-animation', 'text-style', 'subtitle-style', 'font')) { throw "$ZipPath has an unknown type '$($manifest.type)'" }
    # text-animation is the older name of clip-animation; the editor installs both to clip-animation.
    $typeFolder = if ($manifest.type -eq 'text-animation') { 'clip-animation' } else { $manifest.type }
    $packDir = Join-Path $packagesDir "$typeFolder\$($manifest.id.ToLowerInvariant())"
    if (Test-Path -LiteralPath $packDir) {
        Remove-Item -LiteralPath $packDir -Recurse -Force
    }
    [System.IO.Compression.ZipFile]::ExtractToDirectory($ZipPath, $packDir)
}

foreach ($typeFolder in $contentTypes) {
    $typeDir = Join-Path $packagesDir $typeFolder
    if (Test-Path -LiteralPath $typeDir) { Clear-Packages $typeDir }
}
foreach ($release in $releases) {
    $folder = Join-Path $download $release
    foreach ($aggregate in Get-ChildItem -LiteralPath $folder -File -Filter '*-packs.zip') {
        # First level: the aggregate holds the per-item pack zips, flat.
        $unpacked = Join-Path $folder ('unpacked\' + $aggregate.BaseName)
        if (Test-Path -LiteralPath $unpacked) { Remove-Item -LiteralPath $unpacked -Recurse -Force }
        [System.IO.Compression.ZipFile]::ExtractToDirectory($aggregate.FullName, $unpacked)
        # Second level: each pack zip becomes a pack folder.
        $count = 0
        foreach ($zip in Get-ChildItem -LiteralPath $unpacked -Recurse -File -Filter '*.zip') {
            Install-PackZip $zip.FullName
            $count++
        }
        Write-Host "unpacked $count packs of $($aggregate.Name) -> $packagesDir"
    }
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
foreach ($typeFolder in $contentTypes) {
    $typeDir = Join-Path $packagesDir $typeFolder
    if (-not (Test-Path -LiteralPath $typeDir)) { continue }
    foreach ($packDir in Get-ChildItem -LiteralPath $typeDir -Directory) {
        $i18n = $translations[$packDir.Name.ToLowerInvariant()]
        if ($null -eq $i18n) { continue }
        $target = Join-Path $packDir.FullName 'i18n'
        if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Recurse -Force }
        Copy-Item -LiteralPath $i18n -Destination $target -Recurse
        }
}

# The private CapCut packs: one folder per item (its manifest names the collection), copied as they are. Their
# type folders were emptied above, so no pack of the older one-pack-per-set layout is left beside them.
if ($CapcutPrivate -eq '') { $CapcutPrivate = Join-Path (Split-Path -Parent $repoRoot) 'GpufxVideoEditor-Assets-capcut-private' }
if (Test-Path -LiteralPath $CapcutPrivate -PathType Container) {
    foreach ($kind in $contentTypes) {
        $kindDir = Join-Path $CapcutPrivate $kind
        if (-not (Test-Path -LiteralPath $kindDir)) { continue }
        $copied = 0
        foreach ($pack in Get-ChildItem -LiteralPath $kindDir -Directory) {
            $manifest = Join-Path $pack.FullName 'manifest.json'
            if (-not (Test-Path -LiteralPath $manifest)) { continue }
            $id = (Get-Content -LiteralPath $manifest -Raw -Encoding UTF8 | ConvertFrom-Json).id.ToLowerInvariant()
            $packDir = Join-Path $packagesDir "$kind\$id"
            if (Test-Path -LiteralPath $packDir) { Remove-Item -LiteralPath $packDir -Recurse -Force }
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $packDir) | Out-Null
            Copy-Item -LiteralPath $pack.FullName -Destination $packDir -Recurse
            $copied++
        }
        Write-Host "copied $copied CapCut private $kind packs -> $(Join-Path $packagesDir $kind)"
    }
} else {
    Write-Host "no CapCut private checkout at $CapcutPrivate, skipped"
}

Write-Host "packages ready in $packagesDir"
