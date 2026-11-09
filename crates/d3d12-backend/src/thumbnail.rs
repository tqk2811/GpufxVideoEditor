//! Small pictures of a source file, decoded in software through libavcodec and libswscale. Nothing
//! here touches the GPU or the submit gate, so it can run beside the preview and playback.
//!
//! One decoder is opened per sampler and reads forward between nearby requests: a seek lands on a
//! keyframe and decodes up to the wanted frame, which is the expensive part when frames are asked
//! for one by one.

use std::ffi::{CStr, CString};
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};

use engine::{Time, TICKS_PER_SECOND};

use crate::ffi::{self, check, describe_error};

/// A request at most this far past the last decoded frame is reached by decoding, not seeking.
const FORWARD_LIMIT_TICKS: i64 = TICKS_PER_SECOND * 2;
/// `SWS_BILINEAR`.
const SWS_BILINEAR: i32 = 2;
/// Stop looking for the exact frame after this many frames: a very long GOP shows the nearest one.
const MAX_DECODED_PER_REQUEST: u32 = 900;
/// The same limit in wall-clock time, for sources that decode slowly.
const MAX_DECODE_TIME: Duration = Duration::from_secs(3);

/// An RGBA picture, `width * height * 4` bytes, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Why no picture came. A timeout says only that the machine was too busy this time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbnailError {
    /// The time budget ended before any frame was decoded; a later attempt may succeed.
    TimedOut,
    /// The file cannot give this picture.
    Failed(String),
}

impl std::fmt::Display for ThumbnailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => write!(f, "no frame within the time budget"),
            Self::Failed(reason) => write!(f, "{reason}"),
        }
    }
}

impl From<String> for ThumbnailError {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

/// How a search for a frame ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Search {
    Found,
    /// The stream ended, or the frame quota did, with nothing to show.
    NothingDecoded,
    OutOfTime,
}

/// The display size of a thumbnail `height` tall for a stored `width`x`height` picture.
fn scaled_width(width: i32, height: i32, sample_aspect: ffi::AVRational, target_height: u32) -> u32 {
    let aspect = if sample_aspect.num > 0 && sample_aspect.den > 0 { f64::from(sample_aspect.num) / f64::from(sample_aspect.den) } else { 1.0 };
    let shown = f64::from(width.max(1)) * aspect / f64::from(height.max(1));
    ((shown * f64::from(target_height)).round() as u32).clamp(1, target_height.saturating_mul(4).max(1))
}

/// `at` as a timestamp in `time_base`, from the first video timestamp `start`, rounded down.
fn to_stream_pts(at: Time, start: i64, time_base: ffi::AVRational) -> i64 {
    let offset = i128::from(at.ticks()) * i128::from(time_base.den);
    let denominator = i128::from(TICKS_PER_SECOND) * i128::from(time_base.num.max(1));
    i64::try_from(offset.div_euclid(denominator)).unwrap_or(i64::MAX).saturating_add(start)
}

/// Picks frames of one video file at arbitrary positions.
pub struct FrameSampler {
    format_context: *mut ffi::AVFormatContext,
    codec_context: *mut ffi::AVCodecContext,
    packet: *mut ffi::AVPacket,
    frame: *mut ffi::AVFrame,
    /// The newest decoded frame, the answer when the stream ends first.
    kept: *mut ffi::AVFrame,
    scaler: *mut ffi::SwsContext,
    scaler_source: (i32, i32, i32),
    stream_index: i32,
    time_base: ffi::AVRational,
    start_pts: i64,
    /// Timestamp of `kept`, when there is one since the last seek.
    last_pts: Option<i64>,
    draining: bool,
    finished: bool,
    target_height: u32,
    shown_width: u32,
    path: String,
    /// How long one search for a frame may take.
    decode_budget: Duration,
}

impl FrameSampler {
    /// Opens the video stream of `path` for pictures `target_height` pixels tall.
    pub fn open(path: &Path, target_height: u32) -> Result<Self, String> {
        let text = path.to_str().ok_or_else(|| format!("{}: the path is not valid UTF-8", path.display()))?;
        let c_path = CString::new(text).map_err(|_| format!("{text}: the path contains NUL"))?;
        let mut sampler = Self {
            format_context: ptr::null_mut(),
            codec_context: ptr::null_mut(),
            packet: ptr::null_mut(),
            frame: ptr::null_mut(),
            kept: ptr::null_mut(),
            scaler: ptr::null_mut(),
            scaler_source: (0, 0, -1),
            stream_index: -1,
            time_base: ffi::AVRational { num: 1, den: 1 },
            start_pts: 0,
            last_pts: None,
            draining: false,
            finished: false,
            target_height: target_height.max(1),
            shown_width: 1,
            path: text.to_owned(),
            decode_budget: MAX_DECODE_TIME,
        };
        // SAFETY: on error, Drop releases whatever was created.
        unsafe { sampler.initialize(&c_path) }.map_err(|e| format!("{text}: {e}"))?;
        Ok(sampler)
    }

    unsafe fn initialize(&mut self, path: &CStr) -> Result<(), String> {
        check(ffi::avformat_open_input(&mut self.format_context, path.as_ptr(), ptr::null(), ptr::null_mut()), "avformat_open_input")?;
        check(ffi::avformat_find_stream_info(self.format_context, ptr::null_mut()), "avformat_find_stream_info")?;
        let mut codec: *const ffi::AVCodec = ptr::null();
        let index = ffi::av_find_best_stream(self.format_context, ffi::AVMEDIA_TYPE_VIDEO, -1, -1, &mut codec, 0);
        check(index, "av_find_best_stream")?;
        let stream = *(*self.format_context).streams.add(index as usize);
        if (*stream).disposition & ffi::AV_DISPOSITION_ATTACHED_PIC != 0 {
            return Err("the only picture is cover art".to_owned());
        }
        for other in (0..(*self.format_context).nb_streams as usize).filter(|other| *other != index as usize) {
            (**(*self.format_context).streams.add(other)).discard = ffi::AVDISCARD_ALL;
        }
        self.stream_index = index;
        self.time_base = (*stream).time_base;
        self.start_pts = if (*stream).start_time == ffi::NO_PTS { 0 } else { (*stream).start_time };
        self.codec_context = ffi::avcodec_alloc_context3(codec);
        if self.codec_context.is_null() {
            return Err("avcodec_alloc_context3 failed".to_owned());
        }
        check(ffi::avcodec_parameters_to_context(self.codec_context, (*stream).codecpar), "avcodec_parameters_to_context")?;
        (*self.codec_context).pkt_timebase = self.time_base;
        // One core per sampler, and cheap decoding. Skipping non-reference frames can show a frame
        // a little later than the one asked for, which a thumbnail can afford.
        (*self.codec_context).thread_count = 1;
        (*self.codec_context).skip_loop_filter = ffi::AVDISCARD_ALL;
        (*self.codec_context).skip_frame = ffi::AVDISCARD_NONREF;
        check(ffi::avcodec_open2(self.codec_context, codec, ptr::null_mut()), "avcodec_open2")?;
        self.packet = ffi::av_packet_alloc();
        self.frame = ffi::av_frame_alloc();
        self.kept = ffi::av_frame_alloc();
        if self.packet.is_null() || self.frame.is_null() || self.kept.is_null() {
            return Err("allocating the packet or frame failed".to_owned());
        }
        let parameters = &*(*stream).codecpar;
        if parameters.width <= 0 || parameters.height <= 0 {
            return Err("the video stream states no picture size".to_owned());
        }
        self.shown_width = scaled_width(parameters.width, parameters.height, parameters.sample_aspect_ratio, self.target_height);
        Ok(())
    }

    /// Width and height of the pictures this sampler makes.
    pub fn picture_size(&self) -> (u32, u32) {
        (self.shown_width, self.target_height)
    }

    /// The picture at source position `at` (the origin is the first video timestamp), or the
    /// nearest one the stream has. `cancelled` is polled between packets.
    pub fn picture_at(&mut self, at: Time, cancelled: &dyn Fn() -> bool) -> Result<ThumbnailImage, ThumbnailError> {
        let target = to_stream_pts(at, self.start_pts, self.time_base);
        // SAFETY: the contexts are valid after open.
        unsafe {
            let mut outcome = self.search(target, cancelled)?;
            if outcome == Search::NothingDecoded && (*self.codec_context).skip_frame != ffi::AVDISCARD_DEFAULT {
                // A stream of almost only non-reference frames shows nothing while they are
                // skipped. It stays off for this sampler, so later requests do not wait again.
                // Running out of time proves nothing about the stream, so it does not count.
                (*self.codec_context).skip_frame = ffi::AVDISCARD_DEFAULT;
                outcome = self.search(target, cancelled)?;
            }
            match outcome {
                Search::Found => Ok(self.convert_kept()?),
                Search::OutOfTime => Err(ThumbnailError::TimedOut),
                Search::NothingDecoded => Err(ThumbnailError::Failed(format!("{}: no frame could be decoded", self.path))),
            }
        }
    }

    /// Decodes towards `target` within the budget.
    unsafe fn search(&mut self, target: i64, cancelled: &dyn Fn() -> bool) -> Result<Search, String> {
        self.position(target)?;
        let started = Instant::now();
        let mut decoded = 0;
        let mut out_of_time = false;
        loop {
            if cancelled() {
                return Err("cancelled".to_owned());
            }
            if self.finished || decoded >= MAX_DECODED_PER_REQUEST {
                break;
            }
            if started.elapsed() > self.decode_budget {
                out_of_time = true;
                break;
            }
            if self.decode_one()? {
                decoded += 1;
                if self.last_pts.is_some_and(|pts| pts >= target) {
                    break;
                }
            }
        }
        Ok(if self.last_pts.is_some() {
            Search::Found
        } else if out_of_time {
            Search::OutOfTime
        } else {
            Search::NothingDecoded
        })
    }

    /// Seeks unless `target` can be reached by decoding on.
    unsafe fn position(&mut self, target: i64) -> Result<(), String> {
        let limit = to_stream_pts(Time::from_ticks(FORWARD_LIMIT_TICKS), 0, self.time_base);
        let near = self.last_pts.is_some_and(|last| target >= last && target - last <= limit) && !self.finished;
        if near {
            return Ok(());
        }
        check(ffi::av_seek_frame(self.format_context, self.stream_index, target, ffi::AVSEEK_FLAG_BACKWARD as i32), "av_seek_frame")?;
        ffi::avcodec_flush_buffers(self.codec_context);
        self.draining = false;
        self.finished = false;
        self.last_pts = None;
        Ok(())
    }

    /// Decodes one more frame into `kept`; false when more input was needed or the stream ended.
    unsafe fn decode_one(&mut self) -> Result<bool, String> {
        let status = ffi::avcodec_receive_frame(self.codec_context, self.frame);
        if status == 0 {
            let frame = &*self.frame;
            let pts = if frame.best_effort_timestamp != ffi::NO_PTS { frame.best_effort_timestamp } else { frame.pts };
            let keep = pts != ffi::NO_PTS || self.last_pts.is_none();
            if keep {
                ffi::av_frame_unref(self.kept);
                ffi::av_frame_move_ref(self.kept, self.frame);
                self.last_pts = Some(if pts != ffi::NO_PTS { pts } else { self.start_pts });
            } else {
                ffi::av_frame_unref(self.frame);
            }
            return Ok(keep);
        }
        if status == ffi::AVERROR_EOF {
            self.finished = true;
            return Ok(false);
        }
        if status != ffi::AVERROR_EAGAIN {
            return Err(format!("avcodec_receive_frame failed: {}", describe_error(status)));
        }
        self.feed()?;
        Ok(false)
    }

    unsafe fn feed(&mut self) -> Result<(), String> {
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
            let is_video = (*self.packet).stream_index == self.stream_index;
            // A damaged packet is not the end of the file: the next one may decode.
            if is_video {
                let _ = ffi::avcodec_send_packet(self.codec_context, self.packet);
            }
            ffi::av_packet_unref(self.packet);
            if is_video {
                return Ok(());
            }
        }
    }

    unsafe fn convert_kept(&mut self) -> Result<ThumbnailImage, String> {
        let kept = &*self.kept;
        if kept.width <= 0 || kept.height <= 0 || kept.format < 0 {
            return Err(format!("{}: the frame has no picture", self.path));
        }
        let source = (kept.width, kept.height, kept.format);
        let width = self.shown_width as i32;
        let height = self.target_height as i32;
        if self.scaler.is_null() || self.scaler_source != source {
            ffi::sws_freeContext(self.scaler);
            self.scaler = ffi::sws_getContext(kept.width, kept.height, kept.format, width, height, ffi::AV_PIX_FMT_RGBA, SWS_BILINEAR, ptr::null_mut(), ptr::null_mut(), ptr::null());
            if self.scaler.is_null() {
                return Err(format!("{}: sws_getContext failed", self.path));
            }
            self.scaler_source = source;
        }
        // The scaler may read and write whole aligned blocks at the end of a line.
        let stride = (self.shown_width as usize * 4 + 63) & !63;
        let mut buffer = vec![0_u8; stride * (self.target_height as usize + 1)];
        let destination = [buffer.as_mut_ptr(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut()];
        let strides = [stride as i32, 0, 0, 0];
        let lines = ffi::sws_scale(self.scaler, kept.data.as_ptr().cast(), kept.linesize.as_ptr(), 0, kept.height, destination.as_ptr(), strides.as_ptr());
        check(lines, "sws_scale")?;
        let row = self.shown_width as usize * 4;
        let mut rgba = Vec::with_capacity(row * self.target_height as usize);
        for line in buffer.chunks_exact(stride).take(self.target_height as usize) {
            rgba.extend_from_slice(&line[..row]);
        }
        Ok(ThumbnailImage { width: self.shown_width, height: self.target_height, rgba })
    }
}

impl Drop for FrameSampler {
    fn drop(&mut self) {
        // SAFETY: each pointer is null or owned by self, and these calls accept null.
        unsafe {
            ffi::sws_freeContext(self.scaler);
            ffi::av_frame_free(&mut self.kept);
            ffi::av_frame_free(&mut self.frame);
            ffi::av_packet_free(&mut self.packet);
            ffi::avcodec_free_context(&mut self.codec_context);
            ffi::avformat_close_input(&mut self.format_context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(name: &str, arguments: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-thumbnail-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        let path = dir.join(name);
        let tool = concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg/bin/ffmpeg.exe");
        let status = std::process::Command::new(tool).args(["-hide_banner", "-y", "-v", "error"]).args(arguments).arg(&path).status().expect("ffmpeg runs");
        assert!(status.success(), "generating {name} failed");
        path
    }

    fn never() -> bool {
        false
    }

    #[test]
    fn widths_follow_the_shape_of_the_picture() {
        let square = ffi::AVRational { num: 1, den: 1 };
        assert_eq!(scaled_width(1920, 1080, square, 72), 128);
        assert_eq!(scaled_width(1080, 1920, square, 72), 41);
        assert_eq!(scaled_width(720, 576, ffi::AVRational { num: 16, den: 15 }, 72), 96);
        assert_eq!(scaled_width(100, 1, square, 72), 288, "a sliver is bounded");
    }

    #[test]
    fn a_position_maps_to_the_stream_from_the_first_timestamp() {
        let base = ffi::AVRational { num: 1, den: 90_000 };
        assert_eq!(to_stream_pts(Time::from_ticks(TICKS_PER_SECOND), 500, base), 90_500);
    }

    #[test]
    fn pictures_at_different_positions_differ() {
        // A bar of white moves across black: the mean brightness of the top-left region changes.
        let path = clip("moving.mp4", &["-f", "lavfi", "-i", "testsrc=s=320x180:r=30:d=4", "-c:v", "mpeg4", "-g", "60"]);
        let mut sampler = FrameSampler::open(&path, 36).expect("open");
        assert_eq!(sampler.picture_size(), (64, 36));
        let first = sampler.picture_at(Time::from_ticks(0), &never).expect("first");
        let later = sampler.picture_at(Time::from_ticks(TICKS_PER_SECOND * 2), &never).expect("later");
        assert_eq!(first.rgba.len(), 64 * 36 * 4);
        assert_ne!(first.rgba, later.rgba);
        let again = sampler.picture_at(Time::from_ticks(0), &never).expect("seek back");
        assert_eq!(first.rgba, again.rgba, "seeking back shows the same frame");
    }

    #[test]
    fn a_position_past_the_end_shows_the_last_frame() {
        let path = clip("short.mp4", &["-f", "lavfi", "-i", "testsrc=s=64x64:r=30:d=1", "-c:v", "mpeg4"]);
        let mut sampler = FrameSampler::open(&path, 16).expect("open");
        assert!(sampler.picture_at(Time::from_ticks(TICKS_PER_SECOND * 50), &never).is_ok());
    }

    #[test]
    fn a_stream_with_nothing_left_after_skipping_is_read_again_without_skipping() {
        let path = clip("all-skipped.mp4", &["-f", "lavfi", "-i", "testsrc=s=64x64:r=30:d=1", "-c:v", "mpeg4"]);
        let mut sampler = FrameSampler::open(&path, 16).expect("open");
        // What a stream without reference frames looks like to the decoder.
        unsafe { (*sampler.codec_context).skip_frame = ffi::AVDISCARD_ALL };
        assert!(sampler.picture_at(Time::from_ticks(TICKS_PER_SECOND / 2), &never).is_ok());
        assert_eq!(unsafe { (*sampler.codec_context).skip_frame }, ffi::AVDISCARD_DEFAULT, "later requests do not search twice");
    }

    #[test]
    fn the_time_budget_holds_before_the_first_frame() {
        let path = clip("slow.mp4", &["-f", "lavfi", "-i", "testsrc=s=64x64:r=30:d=2", "-c:v", "mpeg4"]);
        let mut sampler = FrameSampler::open(&path, 16).expect("open");
        unsafe { (*sampler.codec_context).skip_frame = ffi::AVDISCARD_ALL };
        sampler.decode_budget = Duration::ZERO;
        assert_eq!(sampler.picture_at(Time::from_ticks(TICKS_PER_SECOND), &never), Err(ThumbnailError::TimedOut), "out of time is told apart from a broken file");
        assert!(!sampler.finished, "it read the file to the end instead of giving up");
        assert_eq!(unsafe { (*sampler.codec_context).skip_frame }, ffi::AVDISCARD_ALL, "running out of time does not turn the skipping off");
    }

    /// Encodes with the GPL build, which has x264 and x265; `None` when it is not installed.
    fn gpl_clip(name: &str, codec: &[&str]) -> Option<std::path::PathBuf> {
        let tool = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg-gpl/bin/ffmpeg.exe"));
        if !tool.exists() {
            assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none(), "third_party/ffmpeg-gpl is missing");
            println!("skipping {name}: third_party/ffmpeg-gpl is missing");
            return None;
        }
        let dir = std::env::temp_dir().join(format!("gpufx-thumbnail-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        let path = dir.join(name);
        let status = std::process::Command::new(tool).args(["-hide_banner", "-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=s=320x180:r=30:d=4", "-pix_fmt", "yuv420p"]).args(codec).arg(&path).status().expect("ffmpeg runs");
        assert!(status.success(), "generating {name} failed");
        Some(path)
    }

    fn check_real_stream(name: &str, codec: &[&str]) {
        let Some(path) = gpl_clip(name, codec) else { return };
        let mut sampler = FrameSampler::open(&path, 36).expect("open");
        let started = Instant::now();
        let first = sampler.picture_at(Time::from_ticks(TICKS_PER_SECOND), &never).expect("one second in");
        let shown = sampler.last_pts.expect("a frame");
        let wanted = to_stream_pts(Time::from_ticks(TICKS_PER_SECOND), sampler.start_pts, sampler.time_base);
        let late = (shown - wanted) as f64 * f64::from(sampler.time_base.num) / f64::from(sampler.time_base.den);
        assert!((0.0..1.0).contains(&late), "{name}: shown {late} s after the position");
        let later = sampler.picture_at(Time::from_ticks(TICKS_PER_SECOND * 3), &never).expect("three seconds in");
        assert_ne!(first.rgba, later.rgba, "{name}");
        let again = sampler.picture_at(Time::from_ticks(TICKS_PER_SECOND), &never).expect("seek back");
        assert_eq!(first.rgba, again.rgba, "{name}: seeking back shows the same frame");
        println!("{name}: {:?}, shown {late:.3} s late", started.elapsed());
    }

    #[test]
    fn h264_with_b_frames_gives_pictures() {
        check_real_stream("x264-b.mp4", &["-c:v", "libx264", "-preset", "veryfast", "-bf", "3", "-g", "60"]);
    }

    #[test]
    fn h264_with_intra_refresh_gives_pictures() {
        check_real_stream("x264-intra-refresh.mp4", &["-c:v", "libx264", "-preset", "veryfast", "-bf", "0", "-x264-params", "intra-refresh=1:keyint=30"]);
    }

    #[test]
    fn h264_of_mostly_non_reference_frames_gives_pictures() {
        check_real_stream("x264-non-ref.mp4", &["-c:v", "libx264", "-preset", "veryfast", "-x264-params", "bframes=16:b-adapt=0:b-pyramid=none:keyint=250"]);
    }

    #[test]
    fn hevc_gives_pictures() {
        check_real_stream("x265.mp4", &["-c:v", "libx265", "-preset", "veryfast", "-x265-params", "log-level=error", "-tag:v", "hvc1"]);
    }

    #[test]
    fn a_corrupt_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("gpufx-thumbnail-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create");
        let path = dir.join("corrupt.mp4");
        std::fs::write(&path, [0xA5_u8; 4096]).expect("write");
        assert!(FrameSampler::open(&path, 16).is_err());
    }

    #[test]
    fn cancelling_stops_the_search() {
        let path = clip("cancel.mp4", &["-f", "lavfi", "-i", "testsrc=s=64x64:r=30:d=2", "-c:v", "mpeg4"]);
        let mut sampler = FrameSampler::open(&path, 16).expect("open");
        assert_eq!(sampler.picture_at(Time::from_ticks(TICKS_PER_SECOND), &|| true), Err(ThumbnailError::Failed("cancelled".to_owned())));
    }
}
