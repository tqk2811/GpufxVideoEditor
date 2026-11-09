//! FFmpeg bindings: the generated bindings of crate `poc`, plus declarations of the encoding and
//! muxing calls those bindings do not cover. The file is reused by path so its 2200 generated
//! lines are not duplicated and `poc` stays unchanged; regenerate it with
//! scripts/generate-bindings.ps1 when the headers change.

#![allow(dead_code)]

#[path = "../../poc/src/ffi/mod.rs"]
mod poc_ffi;

pub use poc_ffi::*;

/// `AVFMT_GLOBALHEADER`
pub const AVFMT_GLOBALHEADER: i32 = 0x0040;
/// `AV_CODEC_FLAG_GLOBAL_HEADER`
pub const AV_CODEC_FLAG_GLOBAL_HEADER: i32 = 1 << 22;
/// `AVFMT_NOFILE`
pub const AVFMT_NOFILE: i32 = 0x0001;
/// `AVIO_FLAG_WRITE`
pub const AVIO_FLAG_WRITE: i32 = 2;
/// `AV_DISPOSITION_ATTACHED_PIC`
pub const AV_DISPOSITION_ATTACHED_PIC: i32 = 0x0400;
/// `AVERROR_STREAM_NOT_FOUND`, that is `FFERRTAG(0xF8, 'S', 'T', 'R')`.
pub const AVERROR_STREAM_NOT_FOUND: i32 = -(0xF8 | (0x53 << 8) | (0x54 << 16) | (0x52 << 24));
/// `AVERROR_DECODER_NOT_FOUND`, that is `FFERRTAG(0xF8, 'D', 'E', 'C')`.
pub const AVERROR_DECODER_NOT_FOUND: i32 = -(0xF8 | (0x44 << 8) | (0x45 << 16) | (0x43 << 24));

/// libswresample's opaque context.
#[repr(C)]
pub struct SwrContext {
    _unused: [u8; 0],
}

unsafe extern "C" {
    pub fn avcodec_get_name(id: AVCodecID) -> *const core::ffi::c_char;
    pub fn swr_alloc_set_opts2(
        context: *mut *mut SwrContext,
        out_layout: *const AVChannelLayout,
        out_format: AVSampleFormat,
        out_rate: core::ffi::c_int,
        in_layout: *const AVChannelLayout,
        in_format: AVSampleFormat,
        in_rate: core::ffi::c_int,
        log_offset: core::ffi::c_int,
        log_context: *mut core::ffi::c_void,
    ) -> core::ffi::c_int;
    pub fn swr_init(context: *mut SwrContext) -> core::ffi::c_int;
    pub fn swr_free(context: *mut *mut SwrContext);
    pub fn swr_convert(context: *mut SwrContext, out: *const *mut u8, out_count: core::ffi::c_int, input: *const *const u8, in_count: core::ffi::c_int) -> core::ffi::c_int;
    pub fn swr_get_delay(context: *mut SwrContext, base: i64) -> i64;
    pub fn swr_get_out_samples(context: *mut SwrContext, in_samples: core::ffi::c_int) -> core::ffi::c_int;
    pub fn av_channel_layout_default(layout: *mut AVChannelLayout, channels: core::ffi::c_int);
    pub fn av_channel_layout_compare(first: *const AVChannelLayout, second: *const AVChannelLayout) -> core::ffi::c_int;
    pub fn av_channel_layout_copy(destination: *mut AVChannelLayout, source: *const AVChannelLayout) -> core::ffi::c_int;
    pub fn av_channel_layout_uninit(layout: *mut AVChannelLayout);
}

unsafe extern "C" {
    pub fn avcodec_find_encoder_by_name(name: *const core::ffi::c_char) -> *const AVCodec;
    pub fn avcodec_send_frame(context: *mut AVCodecContext, frame: *const AVFrame) -> core::ffi::c_int;
    pub fn avcodec_receive_packet(context: *mut AVCodecContext, packet: *mut AVPacket) -> core::ffi::c_int;
    pub fn avcodec_parameters_from_context(parameters: *mut AVCodecParameters, context: *const AVCodecContext) -> core::ffi::c_int;
    pub fn avformat_alloc_output_context2(
        context: *mut *mut AVFormatContext,
        format: *const AVOutputFormat,
        name: *const core::ffi::c_char,
        file: *const core::ffi::c_char,
    ) -> core::ffi::c_int;
    pub fn avformat_new_stream(context: *mut AVFormatContext, codec: *const AVCodec) -> *mut AVStream;
    pub fn avformat_free_context(context: *mut AVFormatContext);
    pub fn avformat_write_header(context: *mut AVFormatContext, options: *mut *mut core::ffi::c_void) -> core::ffi::c_int;
    pub fn av_interleaved_write_frame(context: *mut AVFormatContext, packet: *mut AVPacket) -> core::ffi::c_int;
    pub fn av_write_trailer(context: *mut AVFormatContext) -> core::ffi::c_int;
    pub fn avio_open(context: *mut *mut AVIOContext, url: *const core::ffi::c_char, flags: core::ffi::c_int) -> core::ffi::c_int;
    pub fn avio_closep(context: *mut *mut AVIOContext) -> core::ffi::c_int;
    pub fn av_frame_get_buffer(frame: *mut AVFrame, align: core::ffi::c_int) -> core::ffi::c_int;
    pub fn av_frame_make_writable(frame: *mut AVFrame) -> core::ffi::c_int;
    pub fn av_packet_rescale_ts(packet: *mut AVPacket, source: AVRational, destination: AVRational);
    pub fn av_opt_set(object: *mut core::ffi::c_void, name: *const core::ffi::c_char, value: *const core::ffi::c_char, flags: core::ffi::c_int) -> core::ffi::c_int;
}

/// Turns an FFmpeg error code into text.
pub fn describe_error(code: i32) -> String {
    let mut buffer = [0 as core::ffi::c_char; 256];
    // SAFETY: the buffer outlives the call and its length is passed.
    let status = unsafe { av_strerror(code, buffer.as_mut_ptr(), buffer.len()) };
    if status < 0 {
        return format!("FFmpeg error {code}");
    }
    // SAFETY: av_strerror NUL-terminates what it writes.
    let text = unsafe { core::ffi::CStr::from_ptr(buffer.as_ptr()) };
    format!("{} ({code})", text.to_string_lossy())
}

/// `Err` naming `action` when `code` is an FFmpeg error.
pub fn check(code: i32, action: &str) -> Result<i32, String> {
    if code < 0 {
        Err(format!("{action} failed: {}", describe_error(code)))
    } else {
        Ok(code)
    }
}
