//! Writes finished RGBA frames to an H.264 or HEVC MP4 with libav.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::{Mutex, OnceLock};

use engine::render::{FrameSink, RenderError};
use engine::{AudioOutput, Output, TICKS_PER_SECOND};

use crate::ffi::{self, check, describe_error};

/// The video codec of the exported file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VideoCodec {
    #[default]
    H264,
    Hevc,
}

/// H.264 encoders in [`VideoCodec::auto_order`].
const H264_ENCODERS: [&str; 6] = ["h264_nvenc", "h264_qsv", "h264_amf", "h264_mf", "libx264", "libopenh264"];
/// HEVC encoders in [`VideoCodec::auto_order`].
const HEVC_ENCODERS: [&str; 6] = ["hevc_nvenc", "hevc_qsv", "hevc_amf", "hevc_mf", "libx265", "libkvazaar"];

impl VideoCodec {
    pub const ALL: [VideoCodec; 2] = [VideoCodec::H264, VideoCodec::Hevc];

    /// The name the CLI and the agent tools use: `h264` or `hevc`.
    pub fn id(self) -> &'static str {
        match self {
            VideoCodec::H264 => "h264",
            VideoCodec::Hevc => "hevc",
        }
    }

    /// Reads [`Self::id`]; `h265` is taken for HEVC too.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "h264" | "avc" => Some(VideoCodec::H264),
            "hevc" | "h265" => Some(VideoCodec::Hevc),
            _ => None,
        }
    }

    /// The encoders [`EncoderChoice::Auto`] tries, in order. The order is fixed, so the same
    /// machine and build always pick the same encoder:
    ///
    /// 1. Hardware first, as it is many times faster than software at a similar bit rate: NVENC
    ///    (NVIDIA), Quick Sync (Intel), AMF (AMD), then Media Foundation, which is set to use a
    ///    hardware MFT only. NVENC comes before Quick Sync because on a laptop with both, the
    ///    discrete GPU is the stronger encoder; AMF comes after them only so the order is total.
    /// 2. Then software: libx264 / libx265, which only the GPL FFmpeg has, then the encoders of
    ///    the LGPL FFmpeg, libopenh264 / libkvazaar, which are slower or weaker but always open
    ///    (libkvazaar only for sizes that are multiples of 8).
    ///
    /// An encoder counts only when [`encoder_available`] says so.
    pub fn auto_order(self) -> &'static [&'static str] {
        match self {
            VideoCodec::H264 => &H264_ENCODERS,
            VideoCodec::Hevc => &HEVC_ENCODERS,
        }
    }

    /// The codec of a known encoder name, if it is one of [`Self::auto_order`].
    pub fn of_encoder(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|codec| codec.auto_order().contains(&name))
    }
}

/// The first encoder of `codec`'s [`VideoCodec::auto_order`] that `available` accepts.
pub fn auto_select(codec: VideoCodec, available: impl Fn(&str) -> bool) -> Option<&'static str> {
    codec.auto_order().iter().copied().find(|name| available(name))
}

/// Which encoder to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncoderChoice {
    /// The first available encoder of the codec's [`VideoCodec::auto_order`].
    Auto(VideoCodec),
    /// This encoder only; the codec is the encoder's.
    Named(String),
}

impl Default for EncoderChoice {
    fn default() -> Self {
        EncoderChoice::Auto(VideoCodec::H264)
    }
}

impl EncoderChoice {
    /// Reads the `--codec` and `--encoder` values of the CLI (and the agent tools). No encoder or
    /// `auto` is [`EncoderChoice::Auto`] of the codec, H.264 when none is given. A known encoder of
    /// another codec than the one given is refused.
    pub fn parse(codec: Option<&str>, encoder: Option<&str>) -> Result<Self, String> {
        let codec = codec.map(|text| VideoCodec::parse(text).ok_or_else(|| format!("unknown codec {text:?}: use h264 or hevc"))).transpose()?;
        match encoder {
            None | Some("auto") => Ok(EncoderChoice::Auto(codec.unwrap_or_default())),
            Some(name) => match (codec, VideoCodec::of_encoder(name)) {
                (Some(asked), Some(actual)) if asked != actual => Err(format!("{name} encodes {}, not {}", actual.id(), asked.id())),
                _ => Ok(EncoderChoice::Named(name.to_owned())),
            },
        }
    }

    /// The `--codec` and `--encoder` values that [`Self::parse`] reads back to this choice.
    pub fn to_args(&self) -> (Option<&'static str>, String) {
        match self {
            EncoderChoice::Auto(codec) => (Some(codec.id()), "auto".to_owned()),
            EncoderChoice::Named(name) => (None, name.clone()),
        }
    }
}

/// Why an encoder this FFmpeg lacks is not there. Only the GPL FFmpeg has libx264 and libx265.
fn missing_encoder_reason(name: &str, gpl: bool) -> String {
    if needs_gpl(name) && !gpl {
        return format!("this build links the LGPL FFmpeg, which has no {name} (use Auto, a hardware encoder or the GPL build)");
    }
    "this FFmpeg does not have it".to_owned()
}

/// Whether only the GPL FFmpeg has the encoder `name`.
pub fn needs_gpl(name: &str) -> bool {
    matches!(name, "libx264" | "libx265")
}

/// Whether this FFmpeg build has an encoder called `name`. A hardware encoder can still fail to open;
/// [`encoder_available`] tells.
pub fn encoder_exists(name: &str) -> bool {
    let Ok(name) = CString::new(name) else { return false };
    // SAFETY: `name` is a valid C string for the duration of the call.
    !unsafe { ffi::avcodec_find_encoder_by_name(name.as_ptr()) }.is_null()
}

/// Width and height of the context [`encoder_available`] opens.
const PROBE_SIZE: (i32, i32) = (256, 144);

/// Whether the encoder `name` opens on this machine: `avcodec_open2` succeeds on a small context
/// with the pixel format and options an export gives it. A success is kept for the process, so a
/// hardware encoder that opens is probed once; a failure is not kept, as it can be transient (an
/// NVENC session limit, a driver not ready yet) and the next question probes again. The first probe of a hardware encoder can take a few hundred
/// milliseconds while its driver loads.
pub fn encoder_available(name: &str) -> bool {
    static PROBES: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let probes = PROBES.get_or_init(Mutex::default);
    if probes.lock().unwrap_or_else(|p| p.into_inner()).get(name).copied().unwrap_or(false) {
        return true;
    }
    // Probed outside the lock, so a slow driver does not hold up questions about other encoders.
    let available = probe_encoder(name).is_ok();
    if available {
        probes.lock().unwrap_or_else(|p| p.into_inner()).insert(name.to_owned(), true);
    }
    available
}

/// Opens `name` on a [`PROBE_SIZE`] context and frees it again.
pub fn probe_encoder(name: &str) -> Result<(), String> {
    let c_name = CString::new(name).map_err(|_| "the encoder name contains NUL")?;
    // SAFETY: the context is freed before returning, whatever happens.
    unsafe {
        let codec = ffi::avcodec_find_encoder_by_name(c_name.as_ptr());
        if codec.is_null() {
            return Err(missing_encoder_reason(name, cfg!(feature = "ffmpeg-gpl")));
        }
        let mut context = ffi::avcodec_alloc_context3(codec);
        if context.is_null() {
            return Err("avcodec_alloc_context3 failed".to_owned());
        }
        let time_base = ffi::AVRational { num: 1, den: 30 };
        let bit_rate = i64::from(PROBE_SIZE.0) * i64::from(PROBE_SIZE.1) * 30 / 10;
        let result = pixel_format_for(codec).and_then(|format| {
            configure_video(&mut *context, name, PROBE_SIZE.0, PROBE_SIZE.1, format, time_base, bit_rate, 60, true);
            check(ffi::avcodec_open2(context, codec, ptr::null_mut()), "avcodec_open2").map(|_| ())
        });
        ffi::avcodec_free_context(&mut context);
        result
    }
}

/// The frame format to give `codec`: NV12, which the GPU encoders take as is, or planar YUV 4:2:0
/// for the software encoders that take only that (libopenh264, libx265, libkvazaar).
unsafe fn pixel_format_for(codec: *const ffi::AVCodec) -> Result<ffi::AVPixelFormat, String> {
    let mut list: *const core::ffi::c_void = ptr::null();
    let mut count: core::ffi::c_int = 0;
    check(ffi::avcodec_get_supported_config(ptr::null(), codec, ffi::AV_CODEC_CONFIG_PIX_FORMAT, 0, &mut list, &mut count), "avcodec_get_supported_config")?;
    if list.is_null() {
        return Ok(ffi::AV_PIX_FMT_NV12);
    }
    let formats = std::slice::from_raw_parts(list.cast::<ffi::AVPixelFormat>(), count.max(0) as usize);
    choose_pixel_format(formats).ok_or_else(|| "it takes neither NV12 nor YUV 4:2:0 frames from memory".to_owned())
}

/// NV12 when offered, else planar YUV 4:2:0.
fn choose_pixel_format(formats: &[ffi::AVPixelFormat]) -> Option<ffi::AVPixelFormat> {
    [ffi::AV_PIX_FMT_NV12, ffi::AV_PIX_FMT_YUV420P].into_iter().find(|format| formats.contains(format))
}

/// One `key=value` private option of an encoder.
type EncoderOption = (&'static str, String);

/// The private options that set an encoder's speed and quality. Every encoder gets `bit_rate` as
/// its target; the hardware ones are variable rate with a peak of twice that, and the ones with a
/// constant-quality mode use it, so simple scenes take fewer bits than the target.
fn quality_options(name: &str) -> Vec<EncoderOption> {
    let option = |key: &'static str, value: &str| (key, value.to_owned());
    match name {
        "libx264" => vec![option("crf", "18")],
        "libx265" => vec![option("crf", "20"), option("x265-params", "log-level=error")],
        // p5 is the slower of the high-quality presets; VBR with a CQ floor of quality.
        "h264_nvenc" => vec![option("preset", "p5"), option("tune", "hq"), option("rc", "vbr"), option("cq", "19")],
        "hevc_nvenc" => vec![option("preset", "p5"), option("tune", "hq"), option("rc", "vbr"), option("cq", "21")],
        "h264_qsv" | "hevc_qsv" => vec![option("preset", "slow")],
        "h264_amf" | "hevc_amf" => vec![option("quality", "quality"), option("rc", "vbr_peak")],
        // Without hw_encoding a machine with no hardware MFT gets Windows' software one, which
        // is worse than the software encoders after it in the auto order.
        "h264_mf" | "hevc_mf" => vec![option("hw_encoding", "1"), option("rate_control", "u_vbr"), option("scenario", "archive")],
        _ => Vec::new(),
    }
}

/// Whether `name` is rate controlled with a peak: the hardware encoders.
fn has_peak_rate(name: &str) -> bool {
    ["_nvenc", "_qsv", "_amf", "_mf"].iter().any(|suffix| name.ends_with(suffix))
}

/// Sets what an export and a probe both need on a fresh context of encoder `name`: size, format,
/// timing, BT.709 limited-range colour, rate and the [`quality_options`].
#[allow(clippy::too_many_arguments)]
unsafe fn configure_video(context: &mut ffi::AVCodecContext, name: &str, width: i32, height: i32, format: ffi::AVPixelFormat, time_base: ffi::AVRational, bit_rate: i64, gop: i32, global_header: bool) {
    context.width = width;
    context.height = height;
    context.pix_fmt = format;
    context.time_base = time_base;
    context.framerate = ffi::AVRational { num: time_base.den, den: time_base.num };
    context.colorspace = ffi::AVCOL_SPC_BT709;
    context.color_range = ffi::AVCOL_RANGE_MPEG;
    context.color_primaries = ffi::AVCOL_PRI_BT709;
    context.color_trc = ffi::AVCOL_TRC_BT709;
    context.bit_rate = bit_rate;
    if has_peak_rate(name) {
        context.rc_max_rate = bit_rate * 2;
        context.rc_buffer_size = (bit_rate * 2).min(i64::from(i32::MAX)) as i32;
    }
    context.gop_size = gop;
    context.max_b_frames = 0;
    if global_header {
        context.flags |= ffi::AV_CODEC_FLAG_GLOBAL_HEADER;
    }
    for (key, value) in quality_options(name) {
        let (Ok(c_key), Ok(c_value)) = (CString::new(key), CString::new(value.as_str())) else { continue };
        // An option this FFmpeg's encoder lacks is skipped, as the defaults still encode; it is logged.
        let status = ffi::av_opt_set(context.priv_data, c_key.as_ptr(), c_value.as_ptr(), 0);
        if status < 0 {
            eprintln!("encoder: {name}: option {key}={value} was not set ({}), the default applies", describe_error(status));
        }
    }
}

/// Encodes frames as they arrive; [`Mp4Encoder::finish`] writes the trailer.
pub struct Mp4Encoder {
    format_context: *mut ffi::AVFormatContext,
    codec_context: *mut ffi::AVCodecContext,
    stream: *mut ffi::AVStream,
    frame: *mut ffi::AVFrame,
    packet: *mut ffi::AVPacket,
    width: usize,
    height: usize,
    /// Duration of one frame in the codec time base (1, as the time base is one frame).
    frame_duration: i64,
    encoder_name: String,
    /// NV12 or planar YUV 4:2:0, as [`pixel_format_for`] chose.
    pixel_format: ffi::AVPixelFormat,
    /// `None` when the project has no audio output: the file then has no audio stream.
    audio: Option<AudioTrack>,
}

/// The AAC stream: samples wait in `fifo` until a whole encoder frame has arrived.
struct AudioTrack {
    codec_context: *mut ffi::AVCodecContext,
    stream: *mut ffi::AVStream,
    frame: *mut ffi::AVFrame,
    /// Interleaved stereo samples not encoded yet.
    fifo: Vec<f32>,
    /// Timestamp of the next frame sent to the encoder, in samples.
    next_pts: i64,
    /// Samples per channel of an encoder frame; 1024 for AAC.
    frame_size: usize,
}

impl Drop for AudioTrack {
    fn drop(&mut self) {
        // SAFETY: each pointer is null or owned by self, and these calls accept null.
        unsafe {
            ffi::av_frame_free(&mut self.frame);
            ffi::avcodec_free_context(&mut self.codec_context);
        }
    }
}

/// Plain copy of a layout that owns no memory.
const fn stereo_layout() -> ffi::AVChannelLayout {
    ffi::AVChannelLayout {
        order: ffi::AV_CHANNEL_ORDER_NATIVE,
        nb_channels: 2,
        // AV_CH_LAYOUT_STEREO: front left and front right.
        u: ffi::AVChannelLayout__bindgen_ty_1 { mask: 3 },
        opaque: ptr::null_mut(),
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

impl Mp4Encoder {
    /// Creates `path` for frames of `output`. With [`EncoderChoice::Auto`] the encoders this FFmpeg
    /// has are opened in order until one opens, without a separate probe; that happens before the
    /// first frame, so a file is always written by one encoder.
    pub fn create(path: &Path, output: &Output, choice: &EncoderChoice) -> Result<Self, String> {
        Self::create_with_keyframes(path, output, choice, None)
    }

    /// [`Self::create`] with a keyframe every `keyframe_interval` frames instead of every two seconds.
    pub fn create_with_keyframes(path: &Path, output: &Output, choice: &EncoderChoice, keyframe_interval: Option<u32>) -> Result<Self, String> {
        if output.width % 2 != 0 || output.height % 2 != 0 {
            return Err(format!("output size {}x{} must be even for 4:2:0 video", output.width, output.height));
        }
        let (names, codec): (Vec<&str>, &str) = match choice {
            // Encoders this FFmpeg lacks are not candidates: that is no failure worth a line.
            EncoderChoice::Auto(codec) => (codec.auto_order().iter().copied().filter(|name| encoder_exists(name)).collect(), codec.id()),
            EncoderChoice::Named(name) => (vec![name.as_str()], "video"),
        };
        let mut problems = Vec::new();
        for name in names {
            if name == "libkvazaar" && (output.width % 8 != 0 || output.height % 8 != 0) {
                problems.push(format!("{name}: it needs a width and height that are multiples of 8, not {}x{}", output.width, output.height));
                continue;
            }
            match Self::open_encoder(path, output, name, keyframe_interval) {
                Ok(encoder) => return Ok(encoder),
                Err(reason) => problems.push(format!("{name}: {reason}")),
            }
        }
        if problems.is_empty() {
            return Err(format!("no {codec} encoder opens on this machine (tried {})", match choice {
                EncoderChoice::Auto(codec) => codec.auto_order().join(", "),
                EncoderChoice::Named(name) => name.clone(),
            }));
        }
        Err(format!("no {codec} encoder could be opened ({})", problems.join("; ")))
    }

    fn open_encoder(path: &Path, output: &Output, name: &str, keyframe_interval: Option<u32>) -> Result<Self, String> {
        let path_text = path.to_str().ok_or("the output path is not valid UTF-8")?;
        let c_path = CString::new(path_text).map_err(|_| "the output path contains NUL")?;
        let c_name = CString::new(name).map_err(|_| "the encoder name contains NUL")?;
        let mut encoder = Self {
            format_context: ptr::null_mut(),
            codec_context: ptr::null_mut(),
            stream: ptr::null_mut(),
            frame: ptr::null_mut(),
            packet: ptr::null_mut(),
            width: output.width as usize,
            height: output.height as usize,
            frame_duration: 0,
            encoder_name: name.to_owned(),
            pixel_format: ffi::AV_PIX_FMT_NV12,
            audio: None,
        };
        // SAFETY: on error, Drop releases what was created so far.
        unsafe { encoder.initialize(&c_path, &c_name, output, keyframe_interval)? };
        Ok(encoder)
    }

    unsafe fn initialize(&mut self, path: &CStr, name: &CStr, output: &Output, keyframe_interval: Option<u32>) -> Result<(), String> {
        let codec = ffi::avcodec_find_encoder_by_name(name.as_ptr());
        if codec.is_null() {
            return Err(missing_encoder_reason(&name.to_string_lossy(), cfg!(feature = "ffmpeg-gpl")));
        }
        self.pixel_format = pixel_format_for(codec)?;
        check(ffi::avformat_alloc_output_context2(&mut self.format_context, ptr::null(), ptr::null(), path.as_ptr()), "avformat_alloc_output_context2")?;
        self.codec_context = ffi::avcodec_alloc_context3(codec);
        if self.codec_context.is_null() {
            return Err("avcodec_alloc_context3 failed".to_owned());
        }
        let duration = output.frame_duration.ticks();
        let divisor = gcd(duration, TICKS_PER_SECOND);
        let time_base = ffi::AVRational { num: (duration / divisor) as i32, den: (TICKS_PER_SECOND / divisor) as i32 };
        let global_header = (*(*self.format_context).oformat).flags & ffi::AVFMT_GLOBALHEADER != 0;
        let gop = keyframe_interval.map_or_else(|| TICKS_PER_SECOND / duration.max(1) * 2, i64::from).clamp(1, 600) as i32;
        configure_video(&mut *self.codec_context, &self.encoder_name, output.width as i32, output.height as i32, self.pixel_format, time_base, Self::target_bit_rate(output), gop, global_header);
        check(ffi::avcodec_open2(self.codec_context, codec, ptr::null_mut()), "avcodec_open2")?;

        self.stream = ffi::avformat_new_stream(self.format_context, ptr::null());
        if self.stream.is_null() {
            return Err("avformat_new_stream failed".to_owned());
        }
        (*self.stream).time_base = time_base;
        (*self.stream).avg_frame_rate = (*self.codec_context).framerate;
        self.frame_duration = Self::duration_in_time_base(duration, time_base);
        check(ffi::avcodec_parameters_from_context((*self.stream).codecpar, self.codec_context), "avcodec_parameters_from_context")?;

        if let Some(settings) = output.audio {
            self.add_audio_stream(settings)?;
        }

        if (*(*self.format_context).oformat).flags & ffi::AVFMT_NOFILE == 0 {
            check(ffi::avio_open(&mut (*self.format_context).pb, path.as_ptr(), ffi::AVIO_FLAG_WRITE), "avio_open")?;
        }
        check(ffi::avformat_write_header(self.format_context, ptr::null_mut()), "avformat_write_header")?;

        self.frame = ffi::av_frame_alloc();
        self.packet = ffi::av_packet_alloc();
        if self.frame.is_null() || self.packet.is_null() {
            return Err("allocating the frame failed".to_owned());
        }
        (*self.frame).format = self.pixel_format;
        (*self.frame).width = output.width as i32;
        (*self.frame).height = output.height as i32;
        check(ffi::av_frame_get_buffer(self.frame, 0), "av_frame_get_buffer")?;
        Ok(())
    }

    /// Opens the AAC encoder and adds its stream. Runs before the header is written.
    unsafe fn add_audio_stream(&mut self, settings: AudioOutput) -> Result<(), String> {
        let codec = ffi::avcodec_find_encoder_by_name(c"aac".as_ptr());
        if codec.is_null() {
            return Err("aac: this FFmpeg does not have it".to_owned());
        }
        let mut track = AudioTrack { codec_context: ffi::avcodec_alloc_context3(codec), stream: ptr::null_mut(), frame: ptr::null_mut(), fifo: Vec::new(), next_pts: 0, frame_size: 0 };
        if track.codec_context.is_null() {
            return Err("aac: avcodec_alloc_context3 failed".to_owned());
        }
        let time_base = ffi::AVRational { num: 1, den: settings.sample_rate as i32 };
        let context = &mut *track.codec_context;
        context.sample_fmt = ffi::AV_SAMPLE_FMT_FLTP;
        context.sample_rate = settings.sample_rate as i32;
        context.ch_layout = stereo_layout();
        context.bit_rate = i64::from(settings.bitrate);
        context.time_base = time_base;
        if (*(*self.format_context).oformat).flags & ffi::AVFMT_GLOBALHEADER != 0 {
            context.flags |= ffi::AV_CODEC_FLAG_GLOBAL_HEADER;
        }
        check(ffi::avcodec_open2(track.codec_context, codec, ptr::null_mut()), "avcodec_open2(aac)")?;
        track.frame_size = (*track.codec_context).frame_size as usize;

        track.stream = ffi::avformat_new_stream(self.format_context, ptr::null());
        if track.stream.is_null() {
            return Err("avformat_new_stream(aac) failed".to_owned());
        }
        (*track.stream).time_base = time_base;
        check(ffi::avcodec_parameters_from_context((*track.stream).codecpar, track.codec_context), "avcodec_parameters_from_context(aac)")?;

        track.frame = ffi::av_frame_alloc();
        if track.frame.is_null() {
            return Err("allocating the audio frame failed".to_owned());
        }
        (*track.frame).format = ffi::AV_SAMPLE_FMT_FLTP;
        (*track.frame).sample_rate = settings.sample_rate as i32;
        (*track.frame).ch_layout = stereo_layout();
        (*track.frame).nb_samples = track.frame_size as i32;
        check(ffi::av_frame_get_buffer(track.frame, 0), "av_frame_get_buffer(aac)")?;
        self.audio = Some(track);
        Ok(())
    }

    /// `ticks` of project time as a count of `time_base` units, rounded to nearest.
    fn duration_in_time_base(ticks: i64, time_base: ffi::AVRational) -> i64 {
        let numerator = i128::from(ticks) * i128::from(time_base.den);
        let denominator = i128::from(TICKS_PER_SECOND) * i128::from(time_base.num);
        ((2 * numerator + denominator) / (2 * denominator)) as i64
    }

    /// About 0.1 bit per pixel per frame, a quality setting all encoders understand.
    fn target_bit_rate(output: &Output) -> i64 {
        let pixels = i64::from(output.width) * i64::from(output.height);
        (pixels * TICKS_PER_SECOND / output.frame_duration.ticks().max(1)) / 10
    }

    /// The name of the encoder that opened.
    pub fn encoder_name(&self) -> &str {
        &self.encoder_name
    }

    fn drain(&mut self) -> Result<(), String> {
        // SAFETY: the contexts and packet are valid after open.
        unsafe { self.drain_codec(self.codec_context, self.stream) }.map_err(|e| self.encoder_failed(&e))
    }

    /// `message` about the video encoder, naming it: a hardware encoder that fails mid-export
    /// stops the export with this, rather than another encoder taking over part of the file.
    fn encoder_failed(&self, message: &str) -> String {
        format!("the video encoder {} failed: {message}", self.encoder_name)
    }

    /// Writes the packets `codec` has ready to `stream`. The muxer interleaves the streams.
    unsafe fn drain_codec(&mut self, codec: *mut ffi::AVCodecContext, stream: *mut ffi::AVStream) -> Result<(), String> {
        loop {
            let status = ffi::avcodec_receive_packet(codec, self.packet);
            if status == ffi::AVERROR_EAGAIN || status == ffi::AVERROR_EOF {
                return Ok(());
            }
            check(status, "avcodec_receive_packet")?;
            let packet = &mut *self.packet;
            packet.stream_index = (*stream).index;
            ffi::av_packet_rescale_ts(packet, (*codec).time_base, (*stream).time_base);
            let written = ffi::av_interleaved_write_frame(self.format_context, packet);
            ffi::av_packet_unref(packet);
            check(written, "av_interleaved_write_frame")?;
        }
    }

    /// Encodes every whole frame in the audio FIFO, and with `finish` the short frame left over.
    fn encode_audio(&mut self, finish: bool) -> Result<(), String> {
        let Some(track) = self.audio.as_mut() else { return Ok(()) };
        let (codec, stream, frame_size) = (track.codec_context, track.stream, track.frame_size);
        loop {
            let track = self.audio.as_mut().expect("checked above");
            let waiting = track.fifo.len() / 2;
            if waiting == 0 || (waiting < frame_size && !finish) {
                return Ok(());
            }
            let count = waiting.min(frame_size);
            // SAFETY: the frame's buffers hold `frame_size` samples per plane and count is not
            // above that; av_frame_make_writable makes it safe to overwrite a frame the encoder
            // may still reference.
            unsafe {
                check(ffi::av_frame_make_writable(track.frame), "av_frame_make_writable(aac)")?;
                let frame = &mut *track.frame;
                let left = std::slice::from_raw_parts_mut(frame.data[0].cast::<f32>(), count);
                let right = std::slice::from_raw_parts_mut(frame.data[1].cast::<f32>(), count);
                for (i, sample) in track.fifo[..count * 2].chunks_exact(2).enumerate() {
                    left[i] = sample[0];
                    right[i] = sample[1];
                }
                frame.nb_samples = count as i32;
                frame.pts = track.next_pts;
                check(ffi::avcodec_send_frame(codec, track.frame), "avcodec_send_frame(aac)")?;
            }
            track.next_pts += count as i64;
            track.fifo.drain(..count * 2);
            // SAFETY: the contexts and packet are valid after open.
            unsafe { self.drain_codec(codec, stream)? };
        }
    }

    /// Flushes the encoder and writes the MP4 trailer.
    pub fn finish(&mut self) -> Result<(), String> {
        // SAFETY: the contexts are valid after open; a null frame starts draining.
        unsafe {
            check(ffi::avcodec_send_frame(self.codec_context, ptr::null()), "avcodec_send_frame(flush)").map_err(|e| self.encoder_failed(&e))?;
            self.drain()?;
            self.encode_audio(true)?;
            if let Some(track) = &self.audio {
                let (codec, stream) = (track.codec_context, track.stream);
                check(ffi::avcodec_send_frame(codec, ptr::null()), "avcodec_send_frame(flush aac)")?;
                self.drain_codec(codec, stream)?;
            }
            check(ffi::av_write_trailer(self.format_context), "av_write_trailer")?;
        }
        Ok(())
    }
}

/// Where [`rgba_to_yuv420`] writes the chroma of each 2x2 block.
enum ChromaPlanes<'a> {
    /// One plane of interleaved U and V, as NV12 has.
    Interleaved { plane: &'a mut [u8], stride: usize },
    /// Separate U and V planes, as planar YUV 4:2:0 has.
    Planar { u: &'a mut [u8], v: &'a mut [u8], stride: usize },
}

/// Fills 4:2:0 planes from tightly packed RGBA8, with BT.709 limited-range integer coefficients.
fn rgba_to_yuv420(rgba: &[u8], width: usize, height: usize, luma: &mut [u8], luma_stride: usize, mut chroma: ChromaPlanes<'_>) {
    let luma_of = |r: i32, g: i32, b: i32| ((47 * r + 157 * g + 16 * b + 128) >> 8) + 16;
    let row = width * 4;
    for y in 0..height {
        let rgba_row = &rgba[y * row..(y + 1) * row];
        let luma_row = &mut luma[y * luma_stride..y * luma_stride + width];
        for (x, out) in luma_row.iter_mut().enumerate() {
            let p = &rgba_row[x * 4..x * 4 + 4];
            *out = luma_of(i32::from(p[0]), i32::from(p[1]), i32::from(p[2])) as u8;
        }
    }
    for y in 0..height / 2 {
        let top = &rgba[y * 2 * row..(y * 2 + 1) * row];
        let bottom = &rgba[(y * 2 + 1) * row..(y * 2 + 2) * row];
        for x in (0..width).step_by(2) {
            let average = |channel: usize| (i32::from(top[x * 4 + channel]) + i32::from(top[x * 4 + 4 + channel]) + i32::from(bottom[x * 4 + channel]) + i32::from(bottom[x * 4 + 4 + channel]) + 2) / 4;
            let (r, g, b) = (average(0), average(1), average(2));
            let u = (((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128) as u8;
            let v = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128) as u8;
            match &mut chroma {
                ChromaPlanes::Interleaved { plane, stride } => {
                    plane[y * *stride + x] = u;
                    plane[y * *stride + x + 1] = v;
                }
                ChromaPlanes::Planar { u: u_plane, v: v_plane, stride } => {
                    u_plane[y * *stride + x / 2] = u;
                    v_plane[y * *stride + x / 2] = v;
                }
            }
        }
    }
}

impl FrameSink for Mp4Encoder {
    fn write_frame(&mut self, index: i64, rgba: &[u8]) -> Result<(), RenderError> {
        // SAFETY: the frame's planes hold `linesize * rows` bytes; `av_frame_make_writable` makes
        // it safe to overwrite a frame the encoder may still reference.
        unsafe {
            check(ffi::av_frame_make_writable(self.frame), "av_frame_make_writable")?;
            let frame = &mut *self.frame;
            let (luma_stride, chroma_stride) = (frame.linesize[0] as usize, frame.linesize[1] as usize);
            let luma = std::slice::from_raw_parts_mut(frame.data[0], luma_stride * self.height);
            let chroma_rows = self.height / 2;
            let chroma = if self.pixel_format == ffi::AV_PIX_FMT_YUV420P {
                // av_frame_get_buffer gives both chroma planes of planar 4:2:0 the same stride.
                let u = std::slice::from_raw_parts_mut(frame.data[1], chroma_stride * chroma_rows);
                let v = std::slice::from_raw_parts_mut(frame.data[2], frame.linesize[2] as usize * chroma_rows);
                ChromaPlanes::Planar { u, v, stride: chroma_stride }
            } else {
                ChromaPlanes::Interleaved { plane: std::slice::from_raw_parts_mut(frame.data[1], chroma_stride * chroma_rows), stride: chroma_stride }
            };
            rgba_to_yuv420(rgba, self.width, self.height, luma, luma_stride, chroma);
            frame.pts = index;
            frame.duration = self.frame_duration;
            let sent = ffi::avcodec_send_frame(self.codec_context, self.frame);
            if sent < 0 {
                return Err(RenderError(self.encoder_failed(&format!("avcodec_send_frame failed: {}", describe_error(sent)))));
            }
        }
        self.drain().map_err(RenderError)
    }

    fn write_audio(&mut self, samples: &[f32]) -> Result<(), RenderError> {
        let Some(track) = self.audio.as_mut() else { return Ok(()) };
        track.fifo.extend_from_slice(samples);
        self.encode_audio(false).map_err(RenderError)
    }
}

impl Drop for Mp4Encoder {
    fn drop(&mut self) {
        // SAFETY: each pointer is null or owned by self, and these calls accept null.
        unsafe {
            // Before the format context, which owns the stream it points at.
            self.audio = None;
            ffi::av_frame_free(&mut self.frame);
            ffi::av_packet_free(&mut self.packet);
            ffi::avcodec_free_context(&mut self.codec_context);
            if !self.format_context.is_null() {
                if (*(*self.format_context).oformat).flags & ffi::AVFMT_NOFILE == 0 {
                    ffi::avio_closep(&mut (*self.format_context).pb);
                }
                ffi::avformat_free_context(self.format_context);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_libx264_explains_the_lgpl_build() {
        assert!(missing_encoder_reason("libx264", false).contains("LGPL"));
        assert!(missing_encoder_reason("libx265", false).contains("LGPL"));
        assert!(!missing_encoder_reason("libx264", true).contains("LGPL"));
        assert!(!missing_encoder_reason("h264_nvenc", false).contains("LGPL"));
    }

    #[test]
    fn auto_prefers_hardware_then_gpl_software_then_lgpl_software() {
        assert_eq!(VideoCodec::H264.auto_order(), ["h264_nvenc", "h264_qsv", "h264_amf", "h264_mf", "libx264", "libopenh264"]);
        assert_eq!(VideoCodec::Hevc.auto_order(), ["hevc_nvenc", "hevc_qsv", "hevc_amf", "hevc_mf", "libx265", "libkvazaar"]);
    }

    #[test]
    fn auto_picks_the_first_available_encoder() {
        assert_eq!(auto_select(VideoCodec::H264, |_| true), Some("h264_nvenc"));
        assert_eq!(auto_select(VideoCodec::H264, |name| name != "h264_nvenc"), Some("h264_qsv"));
        assert_eq!(auto_select(VideoCodec::H264, |name| name.starts_with("lib")), Some("libx264"));
        assert_eq!(auto_select(VideoCodec::H264, |name| name == "libopenh264"), Some("libopenh264"), "the LGPL build without a GPU encoder");
        assert_eq!(auto_select(VideoCodec::Hevc, |name| name == "hevc_mf" || name == "libx265"), Some("hevc_mf"));
        assert_eq!(auto_select(VideoCodec::Hevc, |_| false), None);
    }

    #[test]
    fn the_codec_of_an_encoder_is_known_for_the_listed_ones() {
        assert_eq!(VideoCodec::of_encoder("hevc_qsv"), Some(VideoCodec::Hevc));
        assert_eq!(VideoCodec::of_encoder("libopenh264"), Some(VideoCodec::H264));
        assert_eq!(VideoCodec::of_encoder("h264_d3d12va"), None);
    }

    #[test]
    fn choices_parse_and_round_trip_through_the_cli_values() {
        assert_eq!(EncoderChoice::parse(None, None), Ok(EncoderChoice::Auto(VideoCodec::H264)));
        assert_eq!(EncoderChoice::parse(Some("hevc"), Some("auto")), Ok(EncoderChoice::Auto(VideoCodec::Hevc)));
        assert_eq!(EncoderChoice::parse(Some("H265"), None), Ok(EncoderChoice::Auto(VideoCodec::Hevc)));
        assert_eq!(EncoderChoice::parse(None, Some("hevc_nvenc")), Ok(EncoderChoice::Named("hevc_nvenc".into())));
        assert_eq!(EncoderChoice::parse(Some("hevc"), Some("hevc_nvenc")), Ok(EncoderChoice::Named("hevc_nvenc".into())));
        assert!(EncoderChoice::parse(Some("hevc"), Some("h264_nvenc")).is_err());
        assert!(EncoderChoice::parse(Some("vp9"), None).is_err());
        for choice in [EncoderChoice::Auto(VideoCodec::Hevc), EncoderChoice::Auto(VideoCodec::H264), EncoderChoice::Named("libx264".into())] {
            let (codec, encoder) = choice.to_args();
            assert_eq!(EncoderChoice::parse(codec, Some(&encoder)), Ok(choice));
        }
    }

    #[test]
    fn nv12_is_preferred_and_planar_yuv_is_the_fallback() {
        assert_eq!(choose_pixel_format(&[ffi::AV_PIX_FMT_YUV420P, ffi::AV_PIX_FMT_NV12]), Some(ffi::AV_PIX_FMT_NV12));
        assert_eq!(choose_pixel_format(&[ffi::AV_PIX_FMT_YUV420P]), Some(ffi::AV_PIX_FMT_YUV420P));
        assert_eq!(choose_pixel_format(&[]), None);
    }

    #[test]
    fn hardware_encoders_get_a_quality_preset_and_a_peak_rate() {
        assert!(quality_options("h264_nvenc").contains(&("preset", "p5".to_owned())));
        assert!(quality_options("hevc_mf").contains(&("hw_encoding", "1".to_owned())));
        assert!(has_peak_rate("hevc_qsv") && has_peak_rate("h264_amf"));
        assert!(!has_peak_rate("libx264") && !has_peak_rate("libopenh264"));
    }

    #[test]
    fn planar_chroma_matches_the_interleaved_chroma() {
        let rgba: Vec<u8> = (0..4 * 4 * 2).map(|i| (i * 37 % 256) as u8).collect();
        let (mut luma_a, mut luma_b) = ([0u8; 8], [0u8; 8]);
        let mut interleaved = [0u8; 4];
        let (mut u, mut v) = ([0u8; 2], [0u8; 2]);
        rgba_to_yuv420(&rgba, 4, 2, &mut luma_a, 4, ChromaPlanes::Interleaved { plane: &mut interleaved, stride: 4 });
        rgba_to_yuv420(&rgba, 4, 2, &mut luma_b, 4, ChromaPlanes::Planar { u: &mut u, v: &mut v, stride: 2 });
        assert_eq!(luma_a, luma_b);
        assert_eq!([u[0], v[0], u[1], v[1]], interleaved);
    }

    #[test]
    fn converts_primaries_to_bt709_limited() {
        let rgba = [255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255];
        let mut luma = [0u8; 4];
        let mut chroma = [0u8; 2];
        rgba_to_yuv420(&rgba, 2, 2, &mut luma, 2, ChromaPlanes::Interleaved { plane: &mut chroma, stride: 2 });
        assert_eq!(luma[0], 63);
        assert_eq!(chroma, [102, 240]);
    }

    #[test]
    fn white_and_black_are_limited_range() {
        let white = [255u8; 16];
        let mut luma = [0u8; 4];
        let mut chroma = [0u8; 2];
        rgba_to_yuv420(&white, 2, 2, &mut luma, 2, ChromaPlanes::Interleaved { plane: &mut chroma, stride: 2 });
        assert_eq!(luma, [235; 4]);
        assert_eq!(chroma, [128, 128]);
        rgba_to_yuv420(&[0, 0, 0, 255].repeat(4), 2, 2, &mut luma, 2, ChromaPlanes::Interleaved { plane: &mut chroma, stride: 2 });
        assert_eq!(luma, [16; 4]);
        assert_eq!(chroma, [128, 128]);
    }

    #[test]
    fn grey_has_neutral_chroma() {
        let mut luma = [0u8; 4];
        let mut chroma = [0u8; 2];
        rgba_to_yuv420(&[128, 128, 128, 255].repeat(4), 2, 2, &mut luma, 2, ChromaPlanes::Interleaved { plane: &mut chroma, stride: 2 });
        assert_eq!(chroma, [128, 128]);
    }

    #[test]
    fn the_frame_duration_in_the_time_base_is_one_frame() {
        for ticks in [3000, 3003, 3600, 1500] {
            let divisor = gcd(ticks, TICKS_PER_SECOND);
            let time_base = ffi::AVRational { num: (ticks / divisor) as i32, den: (TICKS_PER_SECOND / divisor) as i32 };
            assert_eq!(Mp4Encoder::duration_in_time_base(ticks, time_base), 1);
        }
    }
}
