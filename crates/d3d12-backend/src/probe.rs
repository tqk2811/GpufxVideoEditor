//! What a media file holds, read through libavformat alone: no GPU is used. `avformat_find_stream_info`
//! may open a decoder briefly to read the stream parameters.

use std::ffi::{CStr, CString};
use std::path::Path;
use std::ptr;

use engine::{Time, TICKS_PER_SECOND};

use crate::ffi::{self, check};

/// `AV_TIME_BASE`, the unit of the container duration.
const CONTAINER_TICKS_PER_SECOND: i32 = 1_000_000;

/// The streams of a file an editor can use, and how long each one runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaProbe {
    pub has_video: bool,
    pub has_audio: bool,
    /// `None` when there is no video stream or neither it nor the container states a length.
    pub video_duration: Option<Time>,
    /// `None` when there is no audio stream or neither it nor the container states a length.
    pub audio_duration: Option<Time>,
    /// Why a video stream the file has cannot be used, when it cannot (`has_video` is false then).
    pub video_problem: Option<String>,
    /// Why an audio stream the file has cannot be used, when it cannot (`has_audio` is false then).
    pub audio_problem: Option<String>,
}

struct Input(*mut ffi::AVFormatContext);

impl Drop for Input {
    fn drop(&mut self) {
        // SAFETY: the pointer is null or owned by self, and this call accepts null.
        unsafe { ffi::avformat_close_input(&mut self.0) };
    }
}

/// `duration` in `time_base` units as ticks, rounded to the nearest. `None` when the time base is not
/// positive or the result is not in `1..=i64::MAX`: such a length is unknown, not a wrapped number.
fn rational_to_ticks(duration: i64, time_base: ffi::AVRational) -> Option<Time> {
    if time_base.den <= 0 {
        return None;
    }
    let numerator = i128::from(duration) * i128::from(TICKS_PER_SECOND) * i128::from(time_base.num);
    let denominator = i128::from(time_base.den);
    let ticks = (2 * numerator + denominator).div_euclid(2 * denominator);
    i64::try_from(ticks).ok().filter(|ticks| *ticks >= 1).map(Time::from_ticks)
}

/// One stream kind of an opened input.
struct Found {
    duration: Option<Time>,
}

/// The best stream of `kind`, or why there is none that can be used; `None` when the file has none.
unsafe fn find_stream(input: &Input, kind: ffi::AVMediaType, label: &str, container: Option<Time>) -> Result<Option<Found>, String> {
    let mut codec: *const ffi::AVCodec = ptr::null();
    let index = ffi::av_find_best_stream(input.0, kind, -1, -1, &mut codec, 0);
    if index == ffi::AVERROR_STREAM_NOT_FOUND {
        return Ok(None);
    }
    if index == ffi::AVERROR_DECODER_NOT_FOUND {
        let stream = ffi::av_find_best_stream(input.0, kind, -1, -1, ptr::null_mut(), 0);
        if stream < 0 {
            return Err(format!("no decoder for its {label} stream"));
        }
        let parameters = (**(*input.0).streams.add(stream as usize)).codecpar;
        let name = CStr::from_ptr(ffi::avcodec_get_name((*parameters).codec_id)).to_string_lossy();
        return Err(format!("no decoder for its {label} codec {name}"));
    }
    check(index, "av_find_best_stream")?;
    let stream = *(*input.0).streams.add(index as usize);
    if (*stream).disposition & ffi::AV_DISPOSITION_ATTACHED_PIC != 0 {
        return Ok(None);
    }
    let stated = if (*stream).duration != ffi::NO_PTS && (*stream).duration > 0 { rational_to_ticks((*stream).duration, (*stream).time_base) } else { None };
    Ok(Some(Found { duration: stated.or(container) }))
}

/// Reads which streams `path` has and how long they run. A file with no usable video or audio is
/// an error that says why.
pub fn probe_media(path: &Path) -> Result<MediaProbe, String> {
    if engine::is_image_source(&path.to_string_lossy()) {
        crate::still::still_dimensions(path)?;
        return Ok(MediaProbe { has_video: true, has_audio: false, video_duration: None, audio_duration: None, video_problem: None, audio_problem: None });
    }
    let shown = path.display();
    let text = path.to_str().ok_or_else(|| format!("{shown}: the path is not valid UTF-8"))?;
    let c_path = CString::new(text).map_err(|_| format!("{shown}: the path contains NUL"))?;
    let c_path: &CStr = &c_path;
    let mut input = Input(ptr::null_mut());
    // SAFETY: Drop releases the context either way; it is valid once open_input succeeds.
    unsafe {
        check(ffi::avformat_open_input(&mut input.0, c_path.as_ptr(), ptr::null(), ptr::null_mut()), "avformat_open_input").map_err(|e| format!("{shown}: {e}"))?;
        check(ffi::avformat_find_stream_info(input.0, ptr::null_mut()), "avformat_find_stream_info").map_err(|e| format!("{shown}: {e}"))?;
        let stated = (*input.0).duration;
        let container = if stated != ffi::NO_PTS && stated > 0 { rational_to_ticks(stated, ffi::AVRational { num: 1, den: CONTAINER_TICKS_PER_SECOND }) } else { None };
        let video = find_stream(&input, ffi::AVMEDIA_TYPE_VIDEO, "video", container);
        let audio = find_stream(&input, ffi::AVMEDIA_TYPE_AUDIO, "audio", container);
        let (video, audio) = match (video, audio) {
            (Err(video), Err(audio)) => return Err(format!("{shown}: {video}, and {audio}")),
            (Err(reason), Ok(None)) | (Ok(None), Err(reason)) => return Err(format!("{shown}: {reason}")),
            (video, audio) => (video, audio),
        };
        let (video_problem, audio_problem) = (video.as_ref().err().cloned(), audio.as_ref().err().cloned());
        let (video, audio) = (video.ok().flatten(), audio.ok().flatten());
        if video.is_none() && audio.is_none() {
            return Err(format!("{shown}: it has no video or audio stream"));
        }
        Ok(MediaProbe {
            has_video: video.is_some(),
            has_audio: audio.is_some(),
            video_duration: video.and_then(|found| found.duration),
            audio_duration: audio.and_then(|found| found.duration),
            video_problem,
            audio_problem,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of the 30 fps clips below.
    const FRAME: i64 = TICKS_PER_SECOND / 30;

    fn clip(name: &str, arguments: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-probe-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        let path = dir.join(name);
        let tool = concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg/bin/ffmpeg.exe");
        let status = std::process::Command::new(tool).args(["-hide_banner", "-y", "-v", "error"]).args(arguments).arg(&path).status().expect("ffmpeg runs");
        assert!(status.success(), "generating {name} failed");
        path
    }

    fn scratch_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-probe-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write the file");
        path
    }

    fn assert_within_a_frame(actual: Option<Time>, seconds: i64) {
        let actual = actual.expect("a stated length").ticks();
        assert!((actual - seconds * TICKS_PER_SECOND).abs() <= FRAME, "{actual} ticks is not {seconds} s");
    }

    fn base(num: i32, den: i32) -> ffi::AVRational {
        ffi::AVRational { num, den }
    }

    #[test]
    fn a_zero_or_negative_time_base_is_unknown_and_does_not_panic() {
        assert_eq!(rational_to_ticks(5, base(1, 0)), None);
        assert_eq!(rational_to_ticks(5, base(1, -3)), None);
    }

    #[test]
    fn a_length_that_does_not_fit_is_unknown_instead_of_wrapping() {
        assert_eq!(rational_to_ticks(i64::MAX, base(1, 1)), None);
        assert_eq!(rational_to_ticks(1, base(-1, 1)), None, "a negative length is unknown");
        assert_eq!(rational_to_ticks(1, base(0, 1)), None, "a zero length is unknown");
    }

    #[test]
    fn a_length_in_range_converts_and_rounds_to_the_nearest_tick() {
        assert_eq!(rational_to_ticks(3, base(1, 1)), Some(Time::from_ticks(3 * TICKS_PER_SECOND)));
        assert_eq!(rational_to_ticks(1_500_000, base(1, 1_000_000)), Some(Time::from_ticks(TICKS_PER_SECOND * 3 / 2)));
    }

    #[test]
    fn video_and_audio_each_report_their_own_length() {
        let path = clip("both.mp4", &["-f", "lavfi", "-i", "color=c=gray:s=64x64:r=30:d=1", "-f", "lavfi", "-i", "sine=f=440:d=2", "-c:v", "mpeg4", "-c:a", "aac"]);
        let probe = probe_media(&path).expect("probe");
        assert!(probe.has_video && probe.has_audio);
        assert_within_a_frame(probe.video_duration, 1);
        assert_within_a_frame(probe.audio_duration, 2);
    }

    #[test]
    fn a_video_only_file_has_no_audio() {
        let path = clip("video-only.mp4", &["-f", "lavfi", "-i", "color=c=gray:s=64x64:r=30:d=2", "-an", "-c:v", "mpeg4"]);
        let probe = probe_media(&path).expect("probe");
        assert!(probe.has_video && !probe.has_audio);
        assert_eq!(probe.audio_duration, None);
        assert_within_a_frame(probe.video_duration, 2);
    }

    #[test]
    fn an_audio_only_file_has_no_video() {
        let path = clip("audio-only.wav", &["-f", "lavfi", "-i", "sine=f=440:d=2", "-c:a", "pcm_s16le"]);
        let probe = probe_media(&path).expect("probe");
        assert!(probe.has_audio && !probe.has_video);
        assert_eq!(probe.video_duration, None);
        assert_within_a_frame(probe.audio_duration, 2);
    }

    #[test]
    fn a_stream_with_no_stated_length_uses_the_container() {
        let path = clip("video-only.mkv", &["-f", "lavfi", "-i", "color=c=gray:s=64x64:r=30:d=2", "-an", "-c:v", "mpeg4"]);
        let probe = probe_media(&path).expect("probe");
        assert_within_a_frame(probe.video_duration, 2);
    }

    #[test]
    fn cover_art_is_not_video() {
        let path = clip("cover.m4a", &["-f", "lavfi", "-i", "sine=f=440:d=1", "-f", "lavfi", "-i", "color=c=gray:s=64x64:d=1", "-frames:v", "1", "-c:a", "aac", "-c:v", "mjpeg", "-disposition:v", "attached_pic"]);
        let probe = probe_media(&path).expect("probe");
        assert!(probe.has_audio);
        assert!(!probe.has_video, "the attached picture must not count");
    }

    #[test]
    fn a_corrupt_file_is_refused_and_says_why() {
        let path = scratch_file("corrupt.mp4", &[0xA5; 8192]);
        let error = probe_media(&path).expect_err("garbage");
        assert!(error.contains("corrupt.mp4") && error.contains("avformat_open_input"), "{error}");
    }

    #[test]
    fn a_file_without_audio_or_video_streams_is_refused() {
        let path = scratch_file("subtitles.srt", b"1\r\n00:00:00,000 --> 00:00:01,000\r\nhello\r\n");
        let error = probe_media(&path).expect_err("no streams to edit");
        assert!(error.contains("no video or audio stream"), "{error}");
    }

    #[test]
    fn a_missing_file_is_refused() {
        let error = probe_media(Path::new("does-not-exist.mp4")).expect_err("missing");
        assert!(error.contains("does-not-exist.mp4"), "{error}");
    }
}
