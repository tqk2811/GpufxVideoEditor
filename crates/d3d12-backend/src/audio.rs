//! Decodes the audio of source files to the mix format: 48 kHz stereo `f32`, through libavformat,
//! libavcodec and libswresample.
//!
//! # Positions
//!
//! A source position counts mix samples from the same origin the video decoder uses: the first
//! video timestamp of the file. Internally every sample is addressed on the media timeline of the
//! file, in mix samples, so the audio stream's own `start_time` is what places it against the
//! video. A file without a video stream has its origin at the start of its audio.
//!
//! # Seeking
//!
//! Containers land on a packet near the target, and the first frame after a seek is incomplete
//! because the codec overlaps frames. A seek therefore starts ahead of the target by a preroll and
//! decodes forward; the samples before the target are discarded.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::Arc;

use engine::render::{ClipKey, RenderError};
use engine::{AudioSource, MIX_CHANNELS, MIX_SAMPLE_RATE};

use crate::ffi::{self, check, describe_error};
use crate::paths::ProjectPaths;

const MIX_RATE: i128 = MIX_SAMPLE_RATE as i128;
const CHANNELS: usize = MIX_CHANNELS;
/// How far before the target a seek starts: 200 ms covers the codec overlap and the resampler.
const SEEK_PREROLL: i64 = 9_600;
/// Gives up widening the preroll past this and starts from the beginning of the stream.
const MAX_SEEK_PREROLL: i64 = 2_400_000;
/// A request at most this far past the decoded audio is reached by decoding instead of seeking.
const FORWARD_LIMIT: i64 = 48_000;
/// A hole in the timestamps larger than this (10 ms) is filled with silence.
const GAP_TOLERANCE: i64 = 480;
/// The largest hole in the timestamps that is filled with silence: 60 seconds of samples.
const MAX_TIMESTAMP_GAP: i64 = 60 * MIX_SAMPLE_RATE as i64;
/// How far before the first audio sample a seek to the start of the stream asks for. The packets
/// that prime the codec are timestamped before that sample, and a frame decoded without its
/// predecessor comes out aliased: AAC needs one packet, 200 ms is ample for any codec.
const START_SEEK_LEAD: i64 = 9_600;
/// Output samples per channel fetched at once when the resampler is drained.
const DRAIN_BLOCK: usize = 4096;
/// Files kept open at once; the least recently used is closed first. Each clip sounding in a mix
/// holds one, and closing one that the same mix needs again forces a reopen and a seek on every
/// block, so this is far above any realistic number of simultaneous clips. One decoder costs a
/// file handle and its small buffers, which is cheap at this size.
const MAX_OPEN_FILES: usize = 256;

/// The mix sample of `pts`, rounded to nearest.
fn to_mix_samples(pts: i64, time_base: ffi::AVRational) -> i64 {
    let numerator = i128::from(pts) * MIX_RATE * i128::from(time_base.num);
    let denominator = i128::from(time_base.den);
    (2 * numerator + denominator).div_euclid(2 * denominator) as i64
}

/// The timestamp of mix sample `sample`, rounded down so a seek lands before it.
fn from_mix_samples(sample: i64, time_base: ffi::AVRational) -> i64 {
    (i128::from(sample) * i128::from(time_base.den)).div_euclid(MIX_RATE * i128::from(time_base.num)) as i64
}

const fn stereo_layout() -> ffi::AVChannelLayout {
    ffi::AVChannelLayout {
        order: ffi::AV_CHANNEL_ORDER_NATIVE,
        nb_channels: 2,
        // AV_CH_LAYOUT_STEREO: front left and front right.
        u: ffi::AVChannelLayout__bindgen_ty_1 { mask: 3 },
        opaque: ptr::null_mut(),
    }
}

/// Where the audio decoded after a seek starts on the media timeline, when `frame_start` is the
/// timestamp of its first frame. A first frame without one can only be placed by assuming it is the
/// first frame of the stream, which is wrong after a seek into the middle of the file.
fn first_frame_position(frame_start: Option<i64>, seeked_to_start: bool, first_sample: i64) -> Result<i64, String> {
    match frame_start {
        Some(start) => Ok(start),
        None if seeked_to_start => Ok(first_sample),
        None => Err("the first frame after a seek has no timestamp, so its position is unknown".to_owned()),
    }
}

/// The frames of silence for a hole of `hole` frames in the timestamps; refuses a hole too large to fill.
fn silence_frames(hole: i64) -> Result<usize, String> {
    if hole > MAX_TIMESTAMP_GAP {
        return Err(format!("a timestamp gap of {hole} samples exceeds the limit of {MAX_TIMESTAMP_GAP}"));
    }
    Ok(hole.max(0) as usize)
}

/// The audio of one file, read at arbitrary positions.
pub struct AudioFileDecoder {
    format_context: *mut ffi::AVFormatContext,
    codec_context: *mut ffi::AVCodecContext,
    packet: *mut ffi::AVPacket,
    frame: *mut ffi::AVFrame,
    resampler: *mut ffi::SwrContext,
    /// What the resampler was opened for.
    input_format: i32,
    input_rate: i32,
    input_layout: ffi::AVChannelLayout,
    /// `None` when the file has no audio stream.
    stream_index: Option<i32>,
    time_base: ffi::AVRational,
    /// Media-timeline sample of source position 0.
    origin: i64,
    /// Media-timeline sample of the first audio sample.
    first_sample: i64,
    /// Decoded interleaved samples not consumed yet.
    pending: Vec<f32>,
    /// Media-timeline sample of `pending[0]`.
    pending_start: i64,
    /// Whether `pending_start` has been set since the last seek.
    positioned: bool,
    /// Whether the last seek aimed at the start of the stream rather than into the file.
    seeked_to_start: bool,
    /// Set when a seek found nothing earlier than `pending_start`, so reads before it are silence.
    floor: Option<i64>,
    draining: bool,
    end_of_stream: bool,
    converted: Vec<f32>,
    path: String,
}

impl AudioFileDecoder {
    /// Opens `path`. A file without an audio stream opens fine and reads as silence.
    pub fn open(path: &Path) -> Result<Self, String> {
        let (mut decoder, c_path) = Self::unopened(path)?;
        // SAFETY: on error, Drop releases whatever has been created so far.
        unsafe { decoder.initialize(&c_path) }.map_err(|e| format!("{}: {e}", decoder.path))?;
        Ok(decoder)
    }

    /// A decoder with nothing opened yet, and `path` as FFmpeg takes it.
    fn unopened(path: &Path) -> Result<(Self, CString), String> {
        let path_text = path.to_str().ok_or_else(|| format!("{}: the path is not valid UTF-8", path.display()))?;
        let c_path = CString::new(path_text).map_err(|_| format!("{path_text}: the path contains NUL"))?;
        let decoder = Self {
            format_context: ptr::null_mut(),
            codec_context: ptr::null_mut(),
            packet: ptr::null_mut(),
            frame: ptr::null_mut(),
            resampler: ptr::null_mut(),
            input_format: ffi::AV_SAMPLE_FMT_NONE,
            input_rate: 0,
            input_layout: stereo_layout(),
            stream_index: None,
            time_base: ffi::AVRational { num: 1, den: 1 },
            origin: 0,
            first_sample: 0,
            pending: Vec::new(),
            pending_start: 0,
            positioned: false,
            seeked_to_start: false,
            floor: None,
            draining: false,
            end_of_stream: false,
            converted: Vec::new(),
            path: path_text.to_owned(),
        };
        Ok((decoder, c_path))
    }

    unsafe fn open_input(&mut self, path: &CStr) -> Result<(), String> {
        check(ffi::avformat_open_input(&mut self.format_context, path.as_ptr(), ptr::null(), ptr::null_mut()), "avformat_open_input")?;
        check(ffi::avformat_find_stream_info(self.format_context, ptr::null_mut()), "avformat_find_stream_info")?;
        Ok(())
    }

    /// The best audio stream of the opened input, whether or not it can be decoded.
    unsafe fn best_audio_stream(&self) -> i32 {
        ffi::av_find_best_stream(self.format_context, ffi::AVMEDIA_TYPE_AUDIO, -1, -1, ptr::null_mut(), 0)
    }

    unsafe fn initialize(&mut self, path: &CStr) -> Result<(), String> {
        self.open_input(path)?;

        let mut codec: *const ffi::AVCodec = ptr::null();
        let index = ffi::av_find_best_stream(self.format_context, ffi::AVMEDIA_TYPE_AUDIO, -1, -1, &mut codec, 0);
        if index == ffi::AVERROR_STREAM_NOT_FOUND {
            return Ok(());
        }
        if index == ffi::AVERROR_DECODER_NOT_FOUND {
            return Err(self.missing_decoder_message());
        }
        check(index, "av_find_best_stream")?;
        let stream = *(*self.format_context).streams.add(index as usize);
        self.stream_index = Some(index);
        self.time_base = (*stream).time_base;
        self.first_sample = if (*stream).start_time == ffi::NO_PTS { 0 } else { to_mix_samples((*stream).start_time, self.time_base) };
        self.origin = self.video_origin().unwrap_or(self.first_sample);
        // Packets of the other streams would only be read to be thrown away.
        for other in (0..(*self.format_context).nb_streams as usize).filter(|other| *other != index as usize) {
            (**(*self.format_context).streams.add(other)).discard = ffi::AVDISCARD_ALL;
        }

        self.codec_context = ffi::avcodec_alloc_context3(codec);
        if self.codec_context.is_null() {
            return Err("avcodec_alloc_context3 failed".to_owned());
        }
        check(ffi::avcodec_parameters_to_context(self.codec_context, (*stream).codecpar), "avcodec_parameters_to_context")?;
        // Needed by libavcodec to move the timestamp of a frame whose start it trims.
        (*self.codec_context).pkt_timebase = self.time_base;
        check(ffi::avcodec_open2(self.codec_context, codec, ptr::null_mut()), "avcodec_open2")?;

        self.packet = ffi::av_packet_alloc();
        self.frame = ffi::av_frame_alloc();
        if self.packet.is_null() || self.frame.is_null() {
            return Err("allocating the packet or frame failed".to_owned());
        }
        Ok(())
    }

    /// Explains that the audio stream exists but this FFmpeg cannot decode it.
    unsafe fn missing_decoder_message(&self) -> String {
        let index = self.best_audio_stream();
        if index < 0 {
            return "no decoder for its audio stream".to_owned();
        }
        let stream = *(*self.format_context).streams.add(index as usize);
        let name = CStr::from_ptr(ffi::avcodec_get_name((*(*stream).codecpar).codec_id)).to_string_lossy();
        format!("no decoder for its audio codec {name}")
    }

    /// The media-timeline sample where the video starts, the origin of the video decoder.
    /// `None` when there is no real video stream; cover art does not count.
    unsafe fn video_origin(&self) -> Option<i64> {
        let index = ffi::av_find_best_stream(self.format_context, ffi::AVMEDIA_TYPE_VIDEO, -1, -1, ptr::null_mut(), 0);
        if index < 0 {
            return None;
        }
        let stream = *(*self.format_context).streams.add(index as usize);
        if (*stream).disposition & ffi::AV_DISPOSITION_ATTACHED_PIC != 0 {
            return None;
        }
        Some(if (*stream).start_time == ffi::NO_PTS { 0 } else { to_mix_samples((*stream).start_time, (*stream).time_base) })
    }

    /// The media-timeline sample just after the decoded audio.
    fn produced_end(&self) -> i64 {
        self.pending_start + (self.pending.len() / CHANNELS) as i64
    }

    /// Fills `out` with stereo frames starting at source sample `position`. Whatever the file does
    /// not cover is left as it was, so a zeroed `out` gets silence.
    pub fn read(&mut self, position: i64, out: &mut [f32]) -> Result<(), String> {
        if self.stream_index.is_none() {
            return Ok(());
        }
        let frames = (out.len() / CHANNELS) as i64;
        let target = position.saturating_add(self.origin);
        // SAFETY: the contexts are valid after open, which set stream_index.
        unsafe {
            if !self.positioned || self.needs_seek(target) {
                self.seek_to(target)?;
            }
            while self.produced_end() < target.saturating_add(frames) && !self.end_of_stream {
                self.decode_more()?;
            }
        }
        self.deliver(target, frames, out);
        Ok(())
    }

    /// Whether a read from here on would only be silence: the stream ended and everything decoded
    /// was consumed. A file without an audio stream is finished from the start.
    pub fn finished(&self) -> bool {
        self.stream_index.is_none() || (self.end_of_stream && self.pending.len() < CHANNELS)
    }

    fn needs_seek(&self, target: i64) -> bool {
        if target < self.pending_start {
            return self.floor != Some(self.pending_start);
        }
        // Past the end of a finished stream there is nothing to find: this stays silent.
        !self.end_of_stream && target > self.produced_end().saturating_add(FORWARD_LIMIT)
    }

    /// Copies what is decoded of `[target, target + frames)` to `out` and forgets everything before
    /// the end of that range.
    fn deliver(&mut self, target: i64, frames: i64, out: &mut [f32]) {
        let available = (self.pending.len() / CHANNELS) as i64;
        let skip = (target - self.pending_start).clamp(0, available);
        let lead = (self.pending_start - target).clamp(0, frames);
        let count = (frames - lead).min(available - skip).max(0);
        let (from, to) = ((skip as usize) * CHANNELS, (lead as usize) * CHANNELS);
        let length = (count as usize) * CHANNELS;
        out[to..to + length].copy_from_slice(&self.pending[from..from + length]);
        let consumed = skip + count;
        self.pending.drain(..(consumed as usize) * CHANNELS);
        self.pending_start += consumed;
    }

    /// Restarts decoding so that the decoded audio covers `target`, or starts at the first audio
    /// of the stream when `target` is earlier than that.
    unsafe fn seek_to(&mut self, target: i64) -> Result<(), String> {
        let mut preroll = SEEK_PREROLL;
        loop {
            let wanted = target.saturating_sub(preroll).max(self.first_sample);
            self.seeked_to_start = wanted == self.first_sample;
            self.seek_demuxer(wanted)?;
            ffi::avcodec_flush_buffers(self.codec_context);
            ffi::swr_free(&mut self.resampler);
            self.pending.clear();
            self.positioned = false;
            self.floor = None;
            self.draining = false;
            self.end_of_stream = false;
            while !self.positioned && !self.end_of_stream {
                self.decode_more()?;
            }
            if !self.positioned {
                // Nothing to decode there: every read from here on is silent.
                self.pending_start = target;
                self.positioned = true;
                return Ok(());
            }
            if self.pending_start <= target {
                return Ok(());
            }
            // The stream landed after the target. Unless it is already at its beginning, the
            // container's index was too coarse for this preroll.
            if self.seeked_to_start || preroll >= MAX_SEEK_PREROLL {
                self.floor = Some(self.pending_start);
                return Ok(());
            }
            preroll = preroll.saturating_mul(4);
        }
    }

    /// Seeks the demuxer to `sample`. At the start of the stream it first asks for a position before
    /// the first sample, to include the packets that prime the codec; a demuxer that refuses a
    /// position before its first index entry gets the plain one.
    unsafe fn seek_demuxer(&mut self, sample: i64) -> Result<(), String> {
        let stream_index = self.stream_index.expect("checked by the caller");
        let flags = ffi::AVSEEK_FLAG_BACKWARD as i32;
        if self.seeked_to_start {
            let early = from_mix_samples(sample.saturating_sub(START_SEEK_LEAD), self.time_base);
            if ffi::av_seek_frame(self.format_context, stream_index, early, flags) >= 0 {
                return Ok(());
            }
        }
        check(ffi::av_seek_frame(self.format_context, stream_index, from_mix_samples(sample, self.time_base), flags), "av_seek_frame")?;
        Ok(())
    }

    /// Decodes one more frame into `pending`, or marks the end of the stream.
    unsafe fn decode_more(&mut self) -> Result<(), String> {
        loop {
            let status = ffi::avcodec_receive_frame(self.codec_context, self.frame);
            if status == 0 {
                let accepted = self.accept_frame();
                ffi::av_frame_unref(self.frame);
                return accepted;
            }
            if status == ffi::AVERROR_EOF {
                self.drain_resampler()?;
                self.end_of_stream = true;
                return Ok(());
            }
            if status != ffi::AVERROR_EAGAIN {
                return Err(format!("avcodec_receive_frame failed: {}", describe_error(status)));
            }
            self.feed_decoder()?;
        }
    }

    unsafe fn feed_decoder(&mut self) -> Result<(), String> {
        let stream_index = self.stream_index.expect("set when decoding");
        loop {
            let status = ffi::av_read_frame(self.format_context, self.packet);
            if status == ffi::AVERROR_EOF {
                if !self.draining {
                    self.draining = true;
                    check(ffi::avcodec_send_packet(self.codec_context, ptr::null()), "avcodec_send_packet(drain)")?;
                }
                return Ok(());
            }
            check(status, "av_read_frame")?;
            let is_audio = (*self.packet).stream_index == stream_index;
            let sent = if is_audio { ffi::avcodec_send_packet(self.codec_context, self.packet) } else { 0 };
            ffi::av_packet_unref(self.packet);
            check(sent, "avcodec_send_packet")?;
            if is_audio {
                return Ok(());
            }
        }
    }

    /// Resamples the current frame into `pending`.
    unsafe fn accept_frame(&mut self) -> Result<(), String> {
        let frame = &*self.frame;
        let timestamp = if frame.best_effort_timestamp != ffi::NO_PTS { frame.best_effort_timestamp } else { frame.pts };
        let frame_start = (timestamp != ffi::NO_PTS).then(|| to_mix_samples(timestamp, self.time_base));

        let same_input = !self.resampler.is_null() && frame.format == self.input_format && frame.sample_rate == self.input_rate && ffi::av_channel_layout_compare(&frame.ch_layout, &self.input_layout) == 0;
        if !same_input {
            self.drain_resampler()?;
            self.open_resampler()?;
        }

        if !self.positioned {
            self.pending_start = first_frame_position(frame_start, self.seeked_to_start, self.first_sample)?;
            self.positioned = true;
        } else if let Some(frame_start) = frame_start {
            let expected = self.produced_end().saturating_add(ffi::swr_get_delay(self.resampler, i64::from(MIX_SAMPLE_RATE)));
            if frame_start.saturating_sub(expected) > GAP_TOLERANCE {
                self.drain_resampler()?;
                let hole = silence_frames(frame_start.saturating_sub(self.produced_end()))?;
                self.pending.resize(self.pending.len() + hole * CHANNELS, 0.0);
                self.open_resampler()?;
            }
        }

        let frame = &*self.frame;
        let capacity = ffi::swr_get_out_samples(self.resampler, frame.nb_samples);
        check(capacity, "swr_get_out_samples")?;
        self.convert(capacity as usize, frame.extended_data as *const *const u8, frame.nb_samples)?;
        Ok(())
    }

    /// Opens the resampler for the format of the current frame.
    unsafe fn open_resampler(&mut self) -> Result<(), String> {
        ffi::swr_free(&mut self.resampler);
        let frame = &*self.frame;
        let mut layout: ffi::AVChannelLayout = std::mem::zeroed();
        if frame.ch_layout.order == ffi::AV_CHANNEL_ORDER_UNSPEC {
            ffi::av_channel_layout_default(&mut layout, frame.ch_layout.nb_channels);
        } else {
            check(ffi::av_channel_layout_copy(&mut layout, &frame.ch_layout), "av_channel_layout_copy")?;
        }
        let output = stereo_layout();
        let status = ffi::swr_alloc_set_opts2(&mut self.resampler, &output, ffi::AV_SAMPLE_FMT_FLT, MIX_SAMPLE_RATE as i32, &layout, frame.format, frame.sample_rate, 0, ptr::null_mut());
        ffi::av_channel_layout_uninit(&mut layout);
        check(status, "swr_alloc_set_opts2")?;
        check(ffi::swr_init(self.resampler), "swr_init")?;
        self.input_format = frame.format;
        self.input_rate = frame.sample_rate;
        ffi::av_channel_layout_uninit(&mut self.input_layout);
        check(ffi::av_channel_layout_copy(&mut self.input_layout, &frame.ch_layout), "av_channel_layout_copy")?;
        Ok(())
    }

    /// Runs the resampler into `converted` and appends the result to `pending`; returns the frames written.
    unsafe fn convert(&mut self, capacity: usize, input: *const *const u8, in_count: i32) -> Result<usize, String> {
        self.converted.resize(capacity * CHANNELS, 0.0);
        let out = self.converted.as_mut_ptr().cast::<u8>();
        let written = check(ffi::swr_convert(self.resampler, &out, capacity as i32, input, in_count), "swr_convert")? as usize;
        self.pending.extend_from_slice(&self.converted[..written * CHANNELS]);
        Ok(written)
    }

    /// Takes the samples the resampler still holds. Does nothing before it is opened.
    unsafe fn drain_resampler(&mut self) -> Result<(), String> {
        if self.resampler.is_null() {
            return Ok(());
        }
        while self.convert(DRAIN_BLOCK, ptr::null(), 0)? > 0 {}
        Ok(())
    }
}

impl Drop for AudioFileDecoder {
    fn drop(&mut self) {
        // SAFETY: each pointer is null or owned by self, and these calls accept null.
        unsafe {
            ffi::swr_free(&mut self.resampler);
            ffi::av_channel_layout_uninit(&mut self.input_layout);
            ffi::av_frame_free(&mut self.frame);
            ffi::av_packet_free(&mut self.packet);
            ffi::avcodec_free_context(&mut self.codec_context);
            ffi::avformat_close_input(&mut self.format_context);
        }
    }
}

/// Whether FFmpeg can open `path` and finds an audio stream in it. The stream need not be
/// decodable: that is reported when it is read, by a message naming the codec.
pub(crate) fn has_audio_stream(path: &Path) -> bool {
    let Ok((mut decoder, c_path)) = AudioFileDecoder::unopened(path) else {
        return false;
    };
    // SAFETY: the format context is valid once open_input succeeds; Drop releases it either way.
    unsafe { decoder.open_input(&c_path).is_ok() && decoder.best_audio_stream() >= 0 }
}

/// The [`AudioSource`] of a project: one decoder per clip, because a clip reads forward and two
/// clips of one file would otherwise make each other seek.
pub struct AudioFiles {
    paths: Arc<ProjectPaths>,
    open: HashMap<ClipKey, (AudioFileDecoder, u64, String)>,
    uses: u64,
}

impl AudioFiles {
    /// Sources are found through `paths`.
    pub fn new(paths: Arc<ProjectPaths>) -> Self {
        Self { paths, open: HashMap::new(), uses: 0 }
    }
}

impl AudioSource for AudioFiles {
    fn read(&mut self, key: ClipKey, source: &str, position: i64, out: &mut [f32]) -> Result<(), RenderError> {
        self.uses += 1;
        // Edits renumber clips, so a key can come back naming another file.
        if self.open.get(&key).is_some_and(|(_, _, opened)| opened != source) {
            self.open.remove(&key);
        }
        if !self.open.contains_key(&key) {
            if self.open.len() >= MAX_OPEN_FILES {
                if let Some(oldest) = self.open.iter().min_by_key(|(_, (_, used, _))| *used).map(|(k, _)| *k) {
                    self.open.remove(&oldest);
                }
            }
            let decoder = AudioFileDecoder::open(&self.paths.source_path(source)).map_err(RenderError)?;
            self.open.insert(key, (decoder, 0, source.to_owned()));
        }
        let (decoder, used, _) = self.open.get_mut(&key).expect("inserted above");
        *used = self.uses;
        decoder.read(position, out).map_err(|e| RenderError(format!("{}: {e}", decoder.path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUDIO_BASE: ffi::AVRational = ffi::AVRational { num: 1, den: 44_100 };

    #[test]
    fn timestamps_convert_to_mix_samples_at_the_nearest_sample() {
        assert_eq!(to_mix_samples(44_100, AUDIO_BASE), 48_000);
        assert_eq!(to_mix_samples(1, AUDIO_BASE), 1);
        assert_eq!(to_mix_samples(-44_100, AUDIO_BASE), -48_000);
    }

    #[test]
    fn a_seek_timestamp_never_lands_after_its_sample() {
        for sample in [0, 1, 47_999, 48_000, 1_000_001] {
            let timestamp = from_mix_samples(sample, AUDIO_BASE);
            assert!(to_mix_samples(timestamp, AUDIO_BASE) <= sample + 1, "{sample}");
            assert!(i128::from(timestamp) * MIX_RATE <= i128::from(sample) * 44_100, "{sample}");
        }
    }

    #[test]
    fn a_hole_up_to_the_limit_is_filled_and_a_larger_one_is_an_error() {
        assert_eq!(silence_frames(-5), Ok(0), "audio that overlaps needs no silence");
        assert_eq!(silence_frames(GAP_TOLERANCE + 1), Ok(GAP_TOLERANCE as usize + 1));
        assert_eq!(silence_frames(MAX_TIMESTAMP_GAP), Ok(MAX_TIMESTAMP_GAP as usize));
        let error = silence_frames(MAX_TIMESTAMP_GAP + 1).expect_err("one sample over the limit");
        assert!(error.contains("timestamp gap"), "{error}");
        assert!(silence_frames(i64::MAX).is_err(), "a corrupt timestamp must not allocate");
    }

    #[test]
    fn a_first_frame_without_a_timestamp_is_placed_only_at_the_start_of_the_stream() {
        assert_eq!(first_frame_position(None, true, 123), Ok(123));
        assert_eq!(first_frame_position(Some(500), true, 123), Ok(500));
        assert_eq!(first_frame_position(Some(500), false, 123), Ok(500));
        let error = first_frame_position(None, false, 123).expect_err("position unknown after a mid-file seek");
        assert!(error.contains("no timestamp"), "{error}");
    }

    #[test]
    fn a_clip_key_that_now_names_another_file_reads_that_file() {
        let quiet = generated_clip("level-quarter.wav", &["-f", "lavfi", "-i", "aevalsrc=0.25|0.25:s=48000:d=1", "-c:a", "pcm_s16le"]);
        generated_clip("level-half.wav", &["-f", "lavfi", "-i", "aevalsrc=0.5|0.5:s=48000:d=1", "-c:a", "pcm_s16le"]);
        let paths = Arc::new(ProjectPaths::new(&quiet.with_file_name("project.json"), Vec::new()));
        let mut files = AudioFiles::new(paths);
        let key = ClipKey { track: 0, clip: 0 };
        let mut out = [0.0_f32; 64];
        files.read(key, "level-quarter.wav", 0, &mut out).expect("read the first file");
        assert!((out[0] - 0.25).abs() < 1e-3, "{}", out[0]);
        // Deleting an earlier clip renumbers this one onto the old key.
        files.read(key, "level-half.wav", 0, &mut out).expect("read the second file");
        assert!((out[0] - 0.5).abs() < 1e-3, "{}", out[0]);
    }

    /// A scratch file generated once by the ffmpeg of third_party.
    fn generated_clip(name: &str, arguments: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-audio-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        let path = dir.join(name);
        let tool = concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg/bin/ffmpeg.exe");
        let status = std::process::Command::new(tool).args(["-hide_banner", "-y", "-v", "error"]).args(arguments).arg(&path).status().expect("ffmpeg runs");
        assert!(status.success(), "generating {name} failed");
        path
    }

    #[test]
    fn the_decoder_discards_every_stream_but_its_audio() {
        let path = generated_clip("with-video.mp4", &["-f", "lavfi", "-i", "color=c=gray:s=64x64:r=30:d=1", "-f", "lavfi", "-i", "sine=f=440:d=1", "-c:v", "mpeg4", "-c:a", "aac"]);
        let decoder = AudioFileDecoder::open(&path).expect("open the clip");
        let audio = decoder.stream_index.expect("the clip has audio") as usize;
        // SAFETY: the format context stays valid while the decoder lives.
        unsafe {
            let context = &*decoder.format_context;
            assert_eq!(context.nb_streams, 2);
            for index in 0..context.nb_streams as usize {
                let discard = (**context.streams.add(index)).discard;
                assert_eq!(discard == ffi::AVDISCARD_ALL, index != audio, "stream {index}");
            }
        }
    }

    #[test]
    fn clips_sounding_together_stay_open() {
        let path = generated_clip("short.wav", &["-f", "lavfi", "-i", "sine=f=440:d=0.1", "-c:a", "pcm_s16le"]);
        let dir = path.parent().expect("scratch directory");
        let source = path.file_name().and_then(|n| n.to_str()).expect("name");
        let mut files = AudioFiles::new(Arc::new(ProjectPaths::new(&dir.join("project.json"), Vec::new())));
        let clips = 64;
        let mut out = [0.0f32; 64];
        for clip in 0..clips {
            files.read(ClipKey { track: 0, clip }, source, 0, &mut out).expect("read");
        }
        assert_eq!(files.open.len(), clips, "a clip of the same mix was closed");
    }
}
