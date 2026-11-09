//! Writes finished RGBA frames to an H.264 MP4 with libav.

use std::ffi::CString;
use std::path::Path;
use std::ptr;

use engine::render::{FrameSink, RenderError};
use engine::{Output, TICKS_PER_SECOND};

use crate::ffi::{self, check, describe_error};

/// Tried in this order for [`EncoderChoice::Auto`]: `libx264` first, then the encoders of an LGPL
/// FFmpeg that accept CPU NV12 frames. `h264_d3d12va` takes GPU frames only and is not used yet.
pub const AUTO_ENCODERS: [&str; 5] = ["libx264", "h264_nvenc", "h264_amf", "h264_qsv", "h264_mf"];

/// Which H.264 encoder to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncoderChoice {
    /// The first of [`AUTO_ENCODERS`] that opens.
    Auto,
    /// This encoder only.
    Named(String),
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
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

impl Mp4Encoder {
    /// Creates `path` for frames of `output`.
    pub fn create(path: &Path, output: &Output, choice: &EncoderChoice) -> Result<Self, String> {
        if output.width % 2 != 0 || output.height % 2 != 0 {
            return Err(format!("output size {}x{} must be even for H.264 4:2:0", output.width, output.height));
        }
        let names: Vec<&str> = match choice {
            EncoderChoice::Auto => AUTO_ENCODERS.to_vec(),
            EncoderChoice::Named(name) => vec![name.as_str()],
        };
        let mut problems = Vec::new();
        for name in names {
            match Self::open_encoder(path, output, name) {
                Ok(encoder) => return Ok(encoder),
                Err(reason) => problems.push(format!("{name}: {reason}")),
            }
        }
        Err(format!("no H.264 encoder could be opened ({})", problems.join("; ")))
    }

    fn open_encoder(path: &Path, output: &Output, name: &str) -> Result<Self, String> {
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
        };
        // SAFETY: on error, Drop releases what was created so far.
        unsafe { encoder.initialize(&c_path, &c_name, output)? };
        Ok(encoder)
    }

    unsafe fn initialize(&mut self, path: &std::ffi::CStr, name: &std::ffi::CStr, output: &Output) -> Result<(), String> {
        let codec = ffi::avcodec_find_encoder_by_name(name.as_ptr());
        if codec.is_null() {
            return Err("this FFmpeg does not have it".to_owned());
        }
        check(ffi::avformat_alloc_output_context2(&mut self.format_context, ptr::null(), ptr::null(), path.as_ptr()), "avformat_alloc_output_context2")?;
        self.codec_context = ffi::avcodec_alloc_context3(codec);
        if self.codec_context.is_null() {
            return Err("avcodec_alloc_context3 failed".to_owned());
        }
        let duration = output.frame_duration.ticks();
        let divisor = gcd(duration, TICKS_PER_SECOND);
        let time_base = ffi::AVRational { num: (duration / divisor) as i32, den: (TICKS_PER_SECOND / divisor) as i32 };
        let context_framerate = ffi::AVRational { num: time_base.den, den: time_base.num };
        let context = &mut *self.codec_context;
        context.width = output.width as i32;
        context.height = output.height as i32;
        context.pix_fmt = ffi::AV_PIX_FMT_NV12;
        context.time_base = time_base;
        context.framerate = context_framerate;
        context.colorspace = ffi::AVCOL_SPC_BT709;
        context.color_range = ffi::AVCOL_RANGE_MPEG;
        context.color_primaries = ffi::AVCOL_PRI_BT709;
        context.color_trc = ffi::AVCOL_TRC_BT709;
        context.bit_rate = Self::target_bit_rate(output);
        context.gop_size = (TICKS_PER_SECOND / duration.max(1) * 2).clamp(1, 600) as i32;
        context.max_b_frames = 0;
        if name.to_bytes() == b"libx264" {
            ffi::av_opt_set(context.priv_data, c"crf".as_ptr(), c"18".as_ptr(), 0);
        }
        if (*(*self.format_context).oformat).flags & ffi::AVFMT_GLOBALHEADER != 0 {
            context.flags |= ffi::AV_CODEC_FLAG_GLOBAL_HEADER;
        }
        check(ffi::avcodec_open2(self.codec_context, codec, ptr::null_mut()), "avcodec_open2")?;

        self.stream = ffi::avformat_new_stream(self.format_context, ptr::null());
        if self.stream.is_null() {
            return Err("avformat_new_stream failed".to_owned());
        }
        (*self.stream).time_base = time_base;
        (*self.stream).avg_frame_rate = context_framerate;
        self.frame_duration = Self::duration_in_time_base(duration, time_base);
        check(ffi::avcodec_parameters_from_context((*self.stream).codecpar, self.codec_context), "avcodec_parameters_from_context")?;

        if (*(*self.format_context).oformat).flags & ffi::AVFMT_NOFILE == 0 {
            check(ffi::avio_open(&mut (*self.format_context).pb, path.as_ptr(), ffi::AVIO_FLAG_WRITE), "avio_open")?;
        }
        check(ffi::avformat_write_header(self.format_context, ptr::null_mut()), "avformat_write_header")?;

        self.frame = ffi::av_frame_alloc();
        self.packet = ffi::av_packet_alloc();
        if self.frame.is_null() || self.packet.is_null() {
            return Err("allocating the frame failed".to_owned());
        }
        (*self.frame).format = ffi::AV_PIX_FMT_NV12;
        (*self.frame).width = output.width as i32;
        (*self.frame).height = output.height as i32;
        check(ffi::av_frame_get_buffer(self.frame, 0), "av_frame_get_buffer")?;
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
        // SAFETY: the contexts and packet are valid after open; packet is unreferenced after each use.
        unsafe {
            loop {
                let status = ffi::avcodec_receive_packet(self.codec_context, self.packet);
                if status == ffi::AVERROR_EAGAIN || status == ffi::AVERROR_EOF {
                    return Ok(());
                }
                check(status, "avcodec_receive_packet")?;
                let packet = &mut *self.packet;
                packet.stream_index = 0;
                ffi::av_packet_rescale_ts(packet, (*self.codec_context).time_base, (*self.stream).time_base);
                let written = ffi::av_interleaved_write_frame(self.format_context, packet);
                ffi::av_packet_unref(packet);
                check(written, "av_interleaved_write_frame")?;
            }
        }
    }

    /// Flushes the encoder and writes the MP4 trailer.
    pub fn finish(&mut self) -> Result<(), String> {
        // SAFETY: the contexts are valid after open; a null frame starts draining.
        unsafe {
            check(ffi::avcodec_send_frame(self.codec_context, ptr::null()), "avcodec_send_frame(flush)")?;
            self.drain()?;
            check(ffi::av_write_trailer(self.format_context), "av_write_trailer")?;
        }
        Ok(())
    }
}

/// Fills NV12 planes from tightly packed RGBA8, with BT.709 limited-range integer coefficients.
fn rgba_to_nv12(rgba: &[u8], width: usize, height: usize, luma: &mut [u8], luma_stride: usize, chroma: &mut [u8], chroma_stride: usize) {
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
        let chroma_row = &mut chroma[y * chroma_stride..y * chroma_stride + width];
        for x in (0..width).step_by(2) {
            let average = |channel: usize| (i32::from(top[x * 4 + channel]) + i32::from(top[x * 4 + 4 + channel]) + i32::from(bottom[x * 4 + channel]) + i32::from(bottom[x * 4 + 4 + channel]) + 2) / 4;
            let (r, g, b) = (average(0), average(1), average(2));
            chroma_row[x] = (((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128) as u8;
            chroma_row[x + 1] = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128) as u8;
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
            let chroma = std::slice::from_raw_parts_mut(frame.data[1], chroma_stride * (self.height / 2));
            rgba_to_nv12(rgba, self.width, self.height, luma, luma_stride, chroma, chroma_stride);
            frame.pts = index;
            frame.duration = self.frame_duration;
            let sent = ffi::avcodec_send_frame(self.codec_context, self.frame);
            if sent < 0 {
                return Err(RenderError(format!("avcodec_send_frame failed: {}", describe_error(sent))));
            }
        }
        self.drain().map_err(RenderError)
    }
}

impl Drop for Mp4Encoder {
    fn drop(&mut self) {
        // SAFETY: each pointer is null or owned by self, and these calls accept null.
        unsafe {
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
    fn converts_primaries_to_bt709_limited() {
        let rgba = [255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255];
        let mut luma = [0u8; 4];
        let mut chroma = [0u8; 2];
        rgba_to_nv12(&rgba, 2, 2, &mut luma, 2, &mut chroma, 2);
        assert_eq!(luma[0], 63);
        assert_eq!(chroma, [102, 240]);
    }

    #[test]
    fn white_and_black_are_limited_range() {
        let white = [255u8; 16];
        let mut luma = [0u8; 4];
        let mut chroma = [0u8; 2];
        rgba_to_nv12(&white, 2, 2, &mut luma, 2, &mut chroma, 2);
        assert_eq!(luma, [235; 4]);
        assert_eq!(chroma, [128, 128]);
        rgba_to_nv12(&[0, 0, 0, 255].repeat(4), 2, 2, &mut luma, 2, &mut chroma, 2);
        assert_eq!(luma, [16; 4]);
        assert_eq!(chroma, [128, 128]);
    }

    #[test]
    fn grey_has_neutral_chroma() {
        let mut luma = [0u8; 4];
        let mut chroma = [0u8; 2];
        rgba_to_nv12(&[128, 128, 128, 255].repeat(4), 2, 2, &mut luma, 2, &mut chroma, 2);
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
