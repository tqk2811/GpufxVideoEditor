//! What a media file holds, read through libavformat alone: no decoder is opened and no GPU is used.

use std::ffi::{CStr, CString};
use std::path::Path;
use std::ptr;

use engine::{Time, TICKS_PER_SECOND};

use crate::ffi::{self, check};

/// `AV_TIME_BASE`, the unit of the container duration.
const CONTAINER_TICKS_PER_SECOND: i128 = 1_000_000;

/// The streams of a file an editor can use, and how long each one runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaProbe {
    pub has_video: bool,
    pub has_audio: bool,
    /// `None` when there is no video stream or neither it nor the container states a length.
    pub video_duration: Option<Time>,
    /// `None` when there is no audio stream or neither it nor the container states a length.
    pub audio_duration: Option<Time>,
}

struct Input(*mut ffi::AVFormatContext);

impl Drop for Input {
    fn drop(&mut self) {
        // SAFETY: the pointer is null or owned by self, and this call accepts null.
        unsafe { ffi::avformat_close_input(&mut self.0) };
    }
}

fn rational_to_ticks(duration: i64, time_base: ffi::AVRational) -> Time {
    let numerator = i128::from(duration) * i128::from(TICKS_PER_SECOND) * i128::from(time_base.num);
    let denominator = i128::from(time_base.den);
    Time::from_ticks((2 * numerator + denominator).div_euclid(2 * denominator) as i64)
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
    let stated = ((*stream).duration != ffi::NO_PTS && (*stream).duration > 0).then(|| rational_to_ticks((*stream).duration, (*stream).time_base));
    Ok(Some(Found { duration: stated.or(container) }))
}

/// Reads which streams `path` has and how long they run. A file with no usable video or audio is
/// an error that says why.
pub fn probe_media(path: &Path) -> Result<MediaProbe, String> {
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
        let container = (stated != ffi::NO_PTS && stated > 0).then(|| Time::from_ticks((i128::from(stated) * i128::from(TICKS_PER_SECOND) / CONTAINER_TICKS_PER_SECOND) as i64));
        let video = find_stream(&input, ffi::AVMEDIA_TYPE_VIDEO, "video", container);
        let audio = find_stream(&input, ffi::AVMEDIA_TYPE_AUDIO, "audio", container);
        let (video, audio) = match (video, audio) {
            (Err(video), Err(audio)) => return Err(format!("{shown}: {video}, and {audio}")),
            (Err(reason), Ok(None)) | (Ok(None), Err(reason)) => return Err(format!("{shown}: {reason}")),
            (video, audio) => (video.ok().flatten(), audio.ok().flatten()),
        };
        if video.is_none() && audio.is_none() {
            return Err(format!("{shown}: it has no video or audio stream"));
        }
        Ok(MediaProbe {
            has_video: video.is_some(),
            has_audio: audio.is_some(),
            video_duration: video.and_then(|found| found.duration),
            audio_duration: audio.and_then(|found| found.duration),
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
