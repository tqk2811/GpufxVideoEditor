//! Reads real files through the audio decoder and checks which samples came back. No GPU involved.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use common::*;
use d3d12_backend::{AudioFileDecoder, AudioFiles, ProjectPaths};
use engine::{mix_audio, Project, Resolver};

const RATE: i64 = 48_000;

fn read(path: &Path, position: i64, frames: usize) -> Vec<f32> {
    let mut decoder = AudioFileDecoder::open(path).expect("open the clip");
    read_from(&mut decoder, position, frames)
}

fn read_from(decoder: &mut AudioFileDecoder, position: i64, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * 2];
    decoder.read(position, &mut out).expect("read");
    out
}

fn left(samples: &[f32]) -> Vec<f64> {
    samples.chunks_exact(2).map(|s| f64::from(s[0])).collect()
}

fn energy(signal: &[f64]) -> f64 {
    signal.iter().map(|s| s * s).sum::<f64>() / signal.len().max(1) as f64
}

/// The power of `signal` at `frequency`, by the Goertzel recurrence.
fn power_at(signal: &[f64], frequency: f64) -> f64 {
    let omega = 2.0 * std::f64::consts::PI * frequency / RATE as f64;
    let coefficient = 2.0 * omega.cos();
    let (mut previous, mut before) = (0.0, 0.0);
    for sample in signal {
        let current = sample + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    previous * previous + before * before - coefficient * previous * before
}

/// Which second of the stepped tone is sounding in `signal`.
fn dominant_second(signal: &[f64]) -> i64 {
    (0..6).max_by(|a, b| power_at(signal, stepped_tone_frequency(*a)).total_cmp(&power_at(signal, stepped_tone_frequency(*b)))).expect("not empty")
}

/// The lag in `-range..=range` samples at which `signal` best matches the stepped tone starting at
/// timeline sample `position`, with the normalized correlation there.
fn best_lag(signal: &[f64], position: i64, range: i64) -> (i64, f64) {
    let expected = |i: i64| stepped_tone(i as f64 / RATE as f64);
    let (mut best, mut best_score) = (0, f64::MIN);
    for lag in -range..=range {
        let (mut product, mut own, mut other) = (0.0, 0.0, 0.0);
        for (i, sample) in signal.iter().enumerate() {
            let reference = expected(position + i as i64 - lag);
            product += sample * reference;
            own += sample * sample;
            other += reference * reference;
        }
        let score = product / (own * other).sqrt().max(f64::MIN_POSITIVE);
        if score > best_score {
            (best, best_score) = (lag, score);
        }
    }
    (best, best_score)
}

fn sources() -> Vec<(&'static str, PathBuf)> {
    vec![
        ("wav 48k stereo", stepped_tone_clip("tone-48k-stereo.wav", 48_000, 2, "pcm_s16le")),
        ("wav 44.1k mono", stepped_tone_clip("tone-44k-mono.wav", 44_100, 1, "pcm_s16le")),
        ("aac 48k stereo", stepped_tone_clip("tone-48k-stereo.m4a", 48_000, 2, "aac")),
        ("aac 44.1k mono", stepped_tone_clip("tone-44k-mono.m4a", 44_100, 1, "aac")),
    ]
}

/// A window around a boundary: it holds the phase jump, so the correlation peak is unique.
fn boundary_position(second: i64) -> i64 {
    second * RATE - 1200
}

#[test]
fn every_seek_lands_on_the_right_second_and_sample() {
    for (name, path) in sources() {
        let mut decoder = AudioFileDecoder::open(&path).expect("open the clip");
        // Forward, a long jump forward, back, back to the start, and again.
        for second in [2, 5, 1, 3, 0, 4] {
            let centre = second * RATE + RATE / 2 - 2400;
            let signal = left(&read_from(&mut decoder, centre, 4800));
            assert_eq!(dominant_second(&signal), second, "{name}: wrong second after seeking to {second}");
            if second > 0 {
                let window = left(&read_from(&mut decoder, boundary_position(second), 2400));
                let (lag, score) = best_lag(&window, boundary_position(second), 40);
                assert!(lag.abs() <= 2, "{name}: {lag} samples off at second {second} (correlation {score:.3})");
                assert!(score > 0.9, "{name}: poor match at second {second}: {score:.3}");
            }
        }
    }
}

#[test]
fn a_seek_into_pcm_gives_the_exact_samples() {
    let path = stepped_tone_clip("tone-48k-stereo.wav", 48_000, 2, "pcm_s16le");
    let mut decoder = AudioFileDecoder::open(&path).expect("open the clip");
    // 3.3 s: not on a packet boundary of the WAV demuxer, so a position taken from the packet fails.
    for position in [158_417, 48_003, 271_111] {
        let signal = left(&read_from(&mut decoder, position, 512));
        for (i, value) in signal.iter().enumerate() {
            // Generated as mono and upmixed by ffmpeg, which scales it by 1/sqrt(2).
            let expected = stepped_tone((position + i as i64) as f64 / RATE as f64) * std::f64::consts::FRAC_1_SQRT_2;
            assert!((value - expected).abs() < 2e-3, "sample {i} after seeking to {position}: {value} vs {expected}");
        }
    }
}

#[test]
fn small_blocks_equal_one_large_read() {
    for (name, path) in sources() {
        let whole = read(&path, 70_000, 9_600);
        let mut decoder = AudioFileDecoder::open(&path).expect("open the clip");
        let mut pieces = Vec::new();
        let mut position = 70_000;
        for block in [1, 480, 1600, 1024, 3, 6492] {
            pieces.extend(read_from(&mut decoder, position, block));
            position += block as i64;
        }
        assert_eq!(pieces.len(), whole.len(), "{name}");
        assert_eq!(pieces, whole, "{name}: the block size changed the samples");
    }
}

#[test]
fn reading_the_same_place_twice_gives_the_same_samples() {
    for (name, path) in sources() {
        let mut decoder = AudioFileDecoder::open(&path).expect("open the clip");
        let first = read_from(&mut decoder, 120_000, 4800);
        let _elsewhere = read_from(&mut decoder, 200_000, 4800);
        let second = read_from(&mut decoder, 120_000, 4800);
        assert_eq!(first, second, "{name}");
        assert!(energy(&left(&first)) > 0.01, "{name}: silent");
    }
}

#[test]
fn audio_that_ends_early_leaves_silence() {
    for (name, path) in sources() {
        let end = 6 * RATE;
        let signal = left(&read(&path, end - 2400, 7200));
        assert!(energy(&signal[..1800]) > 0.01, "{name}: the audio before the end is missing");
        // AAC padding and the resampler tail may sound for a little while after the end.
        assert_eq!(energy(&signal[3600..]), 0.0, "{name}: audio after the end of the source");
        assert_eq!(energy(&left(&read(&path, 20 * RATE, 4800))), 0.0, "{name}: far past the end");
    }
}

#[test]
fn reads_past_the_end_stay_silent_and_can_come_back() {
    let path = stepped_tone_clip("tone-48k-stereo.m4a", 48_000, 2, "aac");
    let mut decoder = AudioFileDecoder::open(&path).expect("open the clip");
    assert_eq!(energy(&left(&read_from(&mut decoder, 10 * RATE, 1600))), 0.0);
    assert_eq!(energy(&left(&read_from(&mut decoder, 10 * RATE + 1600, 1600))), 0.0);
    let signal = left(&read_from(&mut decoder, RATE + RATE / 2 - 2400, 4800));
    assert_eq!(dominant_second(&signal), 1, "reading again after the end");
}

fn video_only_clip() -> PathBuf {
    audio_test_file("video-only.mp4", &["-f", "lavfi", "-i", "color=c=gray:s=64x64:r=30:d=2", "-c:v", "mpeg4"])
}

#[test]
fn a_file_without_audio_is_silent() {
    let path = video_only_clip();
    let mut out = vec![0.0f32; 4800];
    AudioFileDecoder::open(&path).expect("open the clip").read(1000, &mut out).expect("read");
    assert!(out.iter().all(|s| *s == 0.0));
}

#[test]
fn the_resolver_says_which_sources_have_audio() {
    let tone = stepped_tone_clip("tone-48k-stereo.wav", 48_000, 2, "pcm_s16le");
    let video = video_only_clip();
    let dir = clips_dir();
    let paths = ProjectPaths::new(&dir.join("project.json"), Vec::new());
    assert!(paths.source_has_audio(tone.file_name().and_then(|n| n.to_str()).expect("name")));
    assert!(!paths.source_has_audio(video.file_name().and_then(|n| n.to_str()).expect("name")));
    let broken = dir.join("not-media.m4a");
    std::fs::write(&broken, b"this is not media").expect("write");
    assert!(!paths.source_has_audio("not-media.m4a"), "a file that cannot be read has no usable audio");
}

fn start_time(path: &Path, stream: &str) -> f64 {
    let output = Command::new(ffprobe_tool()).args(["-v", "error", "-select_streams", stream, "-show_entries", "stream=start_time", "-of", "csv=p=0"]).arg(path).output().expect("run ffprobe");
    String::from_utf8_lossy(&output.stdout).trim().parse().expect("a start_time")
}

fn offset_clip(name: &str, video_offset: &str, audio_offset: &str) -> PathBuf {
    let tone = format!("aevalsrc='{STEPPED_TONE}':s=48000:d=4");
    audio_test_file(name, &["-itsoffset", video_offset, "-f", "lavfi", "-i", "color=c=gray:s=64x64:r=30:d=4", "-itsoffset", audio_offset, "-f", "lavfi", "-i", &tone, "-c:v", "mpeg4", "-c:a", "aac"])
}

#[test]
fn audio_that_starts_after_the_video_is_placed_against_it() {
    let path = offset_clip("audio-late.mp4", "0", "0.5");
    let (video, audio) = (start_time(&path, "v:0"), start_time(&path, "a:0"));
    assert!((audio - video - 0.5).abs() < 0.05, "the clip does not have the offset: video {video}, audio {audio}");
    // The muxer keeps the encoder's priming before the offset, so start_time is 1024 samples
    // early; the tone itself begins exactly at the requested offset.
    let offset = RATE / 2;

    let silent = left(&read(&path, 0, (offset - 1200) as usize));
    assert_eq!(energy(&silent), 0.0, "source time before the audio starts must be silent");
    let position = offset + boundary_position(2);
    let signal = left(&read(&path, position, 2400));
    let (lag, score) = best_lag(&signal, boundary_position(2), 40);
    assert!(lag.abs() <= 2 && score > 0.9, "lag {lag}, correlation {score:.3}");
}

#[test]
fn video_that_starts_after_the_audio_moves_the_origin() {
    let path = offset_clip("video-late.mp4", "0.5", "0");
    let (video, audio) = (start_time(&path, "v:0"), start_time(&path, "a:0"));
    assert!((video - audio - 0.5).abs() < 0.05, "the clip does not have the offset: video {video}, audio {audio}");
    let offset = ((video - audio) * RATE as f64).round() as i64;
    // Source position 0 is the first video frame, which is `offset` into the audio.
    let position = boundary_position(2) - offset;
    let signal = left(&read(&path, position, 2400));
    let (lag, score) = best_lag(&signal, boundary_position(2), 40);
    assert!(lag.abs() <= 2 && score > 0.9, "lag {lag}, correlation {score:.3}");
}

#[test]
fn a_mix_of_cuts_reads_the_right_seconds_of_the_source() {
    for (name, path) in sources() {
        let dir = path.parent().expect("clips directory").to_path_buf();
        let source = path.file_name().and_then(|n| n.to_str()).expect("name").to_owned();
        // Two cuts of one file, the second earlier in the source than the first ended.
        let json = format!(
            r#"{{"version":2,"output":{{"width":64,"height":64,"frame_duration":3000,"audio":{{"sample_rate":48000,"channels":2,"bitrate":128000}}}},"tracks":[
                {{"kind":"audio","clips":[
                  {{"source":"{source}","start":0,"source_in":315000,"duration":45000}},
                  {{"source":"{source}","start":45000,"source_in":157500,"duration":45000}}]}}]}}"#
        );
        let project = Project::from_json(&json).expect("project parses");
        let paths = Arc::new(ProjectPaths::new(&dir.join("project.json"), Vec::new()));
        let mut files = AudioFiles::new(paths);
        let mut mixed = vec![0.0f32; 48_000 * 2];
        // In the blocks the render loop uses.
        for (index, block) in mixed.chunks_mut(1600 * 2).enumerate() {
            mix_audio(&project, &mut files, index as i64 * 1600, block).expect("mix");
        }
        let signal = left(&mixed);
        // 315000 ticks is 3.5 s, 157500 ticks is 1.75 s.
        assert_eq!(dominant_second(&signal[9_600..14_400]), 3, "{name}: first cut, before the source second changes");
        assert_eq!(dominant_second(&signal[14_400..23_000]), 3, "{name}: first cut");
        assert_eq!(dominant_second(&signal[26_000..30_000]), 1, "{name}: second cut");
        assert_eq!(dominant_second(&signal[38_000..47_000]), 2, "{name}: the second cut crosses into source second 2");
    }
}
