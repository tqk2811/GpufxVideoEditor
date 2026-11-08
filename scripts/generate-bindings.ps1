# Regenerates crates/poc/src/ffi/bindings.rs from the headers in third_party/ffmpeg.
# Needs bindgen-cli and a libclang.dll; neither is installed by this repository.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$BindgenExe,
    [Parameter(Mandatory = $true)][string]$LibclangDir,
    [Parameter(Mandatory = $true)][string]$MsvcInclude,
    [Parameter(Mandatory = $true)][string]$UcrtInclude
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$bindgenDir = Join-Path $repoRoot 'crates\poc\bindgen'
$output = Join-Path $repoRoot 'crates\poc\src\ffi\bindings.rs'
$ffmpegInclude = Join-Path $repoRoot 'third_party\ffmpeg\include'

$functions = @(
    'av_buffer_ref', 'av_buffer_unref', 'av_find_best_stream', 'av_frame_alloc', 'av_frame_free',
    'av_frame_unref', 'av_get_pix_fmt_name', 'av_hwdevice_ctx_alloc', 'av_hwdevice_ctx_init',
    'av_hwframe_transfer_data', 'av_log_set_level', 'av_packet_alloc', 'av_packet_free',
    'av_packet_unref', 'av_read_frame', 'av_seek_frame', 'av_strerror',
    'avcodec_alloc_context3', 'avcodec_find_decoder', 'avcodec_flush_buffers', 'avcodec_free_context',
    'avcodec_open2', 'avcodec_parameters_to_context', 'avcodec_receive_frame', 'avcodec_send_packet',
    'avformat_close_input', 'avformat_find_stream_info', 'avformat_open_input'
)
$types = @(
    'AVD3D12VADeviceContext', 'AVD3D12VAFrame', 'AVD3D12VAFramesContext', 'AVHWDeviceContext',
    'AVHWFramesContext'
)
$variables = @(
    'AV_HWDEVICE_TYPE_D3D12VA', 'AV_LOG_WARNING', 'AV_PIX_FMT_D3D12', 'AV_PIX_FMT_NONE', 'AV_PIX_FMT_NV12',
    'AV_D3D12VA_FRAME_FLAG_TEXTURE_ARRAY', 'AVMEDIA_TYPE_VIDEO', 'AVSEEK_FLAG_BACKWARD'
)

$arguments = @(
    (Join-Path $bindgenDir 'wrapper.h'), '-o', $output,
    '--default-enum-style', 'consts', '--no-prepend-enum-name', '--no-layout-tests',
    '--no-doc-comments', '--use-core', '--ctypes-prefix', 'core::ffi'
)
foreach ($name in $functions) { $arguments += @('--allowlist-function', "^$name$") }
foreach ($name in $types) { $arguments += @('--allowlist-type', "^$name$") }
foreach ($name in $variables) { $arguments += @('--allowlist-var', "^$name$") }
$arguments += @(
    '--', '--target=x86_64-pc-windows-msvc',
    '-I', (Join-Path $bindgenDir 'stubs'), '-I', $ffmpegInclude, '-I', $MsvcInclude, '-I', $UcrtInclude
)

$env:LIBCLANG_PATH = $LibclangDir
& $BindgenExe @arguments
if ($LASTEXITCODE -ne 0) { throw "bindgen failed ($LASTEXITCODE)" }
Write-Host "Wrote $output"
