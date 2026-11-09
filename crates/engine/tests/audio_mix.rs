//! Audio scheduling and mixing with fake sources: spec rows "Không lệch mẫu" and "Trộn đúng".

mod common;

use common::*;
use engine::{
    audio_sample_count, audio_segments, frame_count, frame_time, mix_audio, sample_position,
    AudioSource, ClipKey, MixReport, Project, RenderError, Time,
};

/// Each named source is a constant level; unknown sources have no audio at all.
struct Levels {
    levels: Vec<(&'static str, f32)>,
    reads: Vec<(ClipKey, i64, usize)>,
}

fn levels(levels: &[(&'static str, f32)]) -> Levels {
    Levels {
        levels: levels.to_vec(),
        reads: Vec::new(),
    }
}

impl AudioSource for Levels {
    fn read(
        &mut self,
        key: ClipKey,
        source: &str,
        position: i64,
        out: &mut [f32],
    ) -> Result<(), RenderError> {
        self.reads.push((key, position, out.len() / 2));
        if let Some(&(_, level)) = self.levels.iter().find(|(name, _)| *name == source) {
            out.fill(level);
        }
        Ok(())
    }
}

/// Left is `position * 1e-5`, right its negative, so every output sample names its source sample.
struct Positions;

impl AudioSource for Positions {
    fn read(
        &mut self,
        _key: ClipKey,
        _source: &str,
        position: i64,
        out: &mut [f32],
    ) -> Result<(), RenderError> {
        for (i, frame) in out.chunks_exact_mut(2).enumerate() {
            let value = (position + i as i64) as f32 * 1e-5;
            frame[0] = value;
            frame[1] = -value;
        }
        Ok(())
    }
}

fn mix(p: &Project, source: &mut impl AudioSource, start: i64, frames: usize) -> (Vec<f32>, MixReport) {
    let mut out = vec![9.0; frames * 2];
    let report = mix_audio(p, source, start, &mut out).unwrap();
    (out, report)
}

fn left(out: &[f32], frame: usize) -> f32 {
    out[frame * 2]
}

#[track_caller]
fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1e-4,
        "expected {expected}, got {actual}"
    );
}

fn single(c: engine::Clip) -> Project {
    project(3000, vec![track(vec![c], vec![])])
}

// 90000 ticks are 48000 samples, so this crossfade window of 24000 samples is [36000, 60000).
const CUT: i64 = 90_000;
const WINDOW: i64 = 45_000;
const WINDOW_START: i64 = 36_000;
const WINDOW_LEN: i64 = 24_000;

fn crossfade() -> Project {
    project(
        3000,
        vec![track(
            vec![clip("a", 0, 0, CUT), clip("b", CUT, 45_000, CUT)],
            vec![transition(0, WINDOW)],
        )],
    )
}

#[test]
fn sample_position_floors() {
    for (ticks, sample) in [
        (0, 0),
        (1, 0),
        (2, 1),
        (14, 7),
        (15, 8),
        (90_000, 48_000),
        (-1, -1),
        (-15, -8),
        (-16, -9),
    ] {
        assert_eq!(sample_position(t(ticks)), sample, "ticks {ticks}");
    }
}

#[test]
fn sample_position_is_exact_at_the_limits() {
    // i64::MAX = 15 * 614891469123651720 + 7, and 7 * 8 / 15 = 3 rounded down.
    assert_eq!(sample_position(Time::MAX), 8 * 614_891_469_123_651_720 + 3);
    assert_eq!(
        sample_position(t(i64::MIN)),
        (i128::from(i64::MIN) * 8).div_euclid(15) as i64
    );
}

#[test]
fn touching_clips_neither_gap_nor_overlap() {
    // Cuts at ticks that do not fall on a sample.
    let p = project(
        3000,
        vec![track(
            vec![
                clip("a", 1, 0, 1001),
                clip("b", 1002, 0, 2003),
                clip("c", 3005, 0, 7),
            ],
            vec![],
        )],
    );
    let first = sample_position(t(1));
    let last = sample_position(t(3012));
    let segments = audio_segments(&p, 0, last + 10);
    assert_eq!(segments.len(), 3);
    assert_eq!(segments[0].start, first);
    for pair in segments.windows(2) {
        assert_eq!(pair[0].start + pair[0].len, pair[1].start);
    }
    let end = segments[2].start + segments[2].len;
    assert_eq!(end, last);
    assert_eq!(
        segments.iter().map(|s| s.len).sum::<i64>(),
        last - first,
        "total equals the timeline span"
    );
}

#[test]
fn shifting_a_clip_keeps_its_source_samples() {
    let p = single(clip("a", 1000, 90_000, 30_000));
    let s = audio_segments(&p, 0, 100_000)[0];
    assert_eq!(s.start, sample_position(t(1000)));
    assert_eq!(s.source_start, 48_000);
    assert_eq!(s.len, sample_position(t(31_000)) - s.start);
}

#[test]
fn a_block_that_starts_inside_a_clip_keeps_the_source_position() {
    let p = single(clip("a", 0, 90_000, 90_000));
    let s = audio_segments(&p, 100, 50)[0];
    assert_eq!((s.start, s.len, s.source_start), (100, 50, 48_100));
}

#[test]
fn cut_reads_consecutive_source_samples_on_both_sides() {
    let p = project(
        3000,
        vec![track(
            vec![clip("a", 0, 15, 1500), clip("b", 1500, 30, 1500)],
            vec![],
        )],
    );
    let cut = sample_position(t(1500));
    let (out, _) = mix(&p, &mut Positions, 0, 1600);
    close(left(&out, 0), 8.0 * 1e-5);
    close(left(&out, cut as usize - 1), (8 + cut - 1) as f32 * 1e-5);
    close(left(&out, cut as usize), 16.0 * 1e-5);
    assert_eq!(out[cut as usize * 2 + 1], -out[cut as usize * 2], "right mirrors left");
}

#[test]
fn ntsc_cadence_over_ten_minutes_has_no_drift() {
    const FRAMES: i64 = 18_000; // 18000 frames of 3003 ticks is 10 minutes and 0.6 s.
    let p = project(
        3003,
        vec![track(vec![clip("a", 0, 0, 3003 * FRAMES)], vec![])],
    );
    assert_eq!(frame_count(&p), FRAMES);
    assert_eq!(audio_sample_count(&p), 28_828_800);

    let mut expected = 0;
    for frame in 0..FRAMES {
        let start = sample_position(frame_time(&p, frame).unwrap());
        let end = sample_position(frame_time(&p, frame + 1).unwrap());
        assert_eq!(start, expected, "frame {frame} starts where the last ended");
        assert!((1601..=1602).contains(&(end - start)), "frame {frame}");
        let segments = audio_segments(&p, start, end - start);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].len, end - start);
        assert_eq!(segments[0].source_start, start);
        expected = end;
    }
    assert_eq!(expected, audio_sample_count(&p));
}

#[test]
fn sample_count_covers_a_final_partial_frame() {
    let p = project(3000, vec![track(vec![clip("a", 0, 0, 3001)], vec![])]);
    assert_eq!(frame_count(&p), 2);
    assert_eq!(audio_sample_count(&p), sample_position(t(6000)));
}

#[test]
fn audio_tracks_add_no_length() {
    let p = project(
        3000,
        vec![
            track(vec![clip("a", 0, 0, 3000)], vec![]),
            audio_track(vec![clip("m", 0, 0, 90_000)]),
        ],
    );
    assert_eq!(audio_sample_count(&p), 1600);
}

#[test]
fn gain_db_scales_the_level() {
    let mut c = clip("a", 0, 0, 3000);
    c.gain_db = -20.0;
    let (out, _) = mix(&single(c.clone()), &mut levels(&[("a", 1.0)]), 0, 4);
    close(left(&out, 0), 0.1);
    c.gain_db = 20.0;
    let (out, _) = mix(&single(c), &mut levels(&[("a", 0.05)]), 0, 4);
    close(left(&out, 0), 0.5);
}

#[test]
fn fade_in_runs_from_silence_to_full_level() {
    let mut c = clip("a", 0, 0, 90_000);
    c.fade_in = t(15_000); // 8000 samples
    let p = single(c);
    let (out, _) = mix(&p, &mut levels(&[("a", 1.0)]), 0, 9000);
    assert_eq!(left(&out, 0), 0.0);
    close(left(&out, 2000), 0.25);
    close(left(&out, 4000), 0.5);
    close(left(&out, 7999), 7999.0 / 8000.0);
    assert_eq!(left(&out, 8000), 1.0);
    assert_eq!(left(&out, 8999), 1.0);
}

#[test]
fn fade_out_mirrors_fade_in_and_ends_silent() {
    let mut c = clip("a", 0, 0, 90_000);
    c.fade_out = t(15_000); // 8000 samples before sample 48000
    let p = single(c);
    let (out, _) = mix(&p, &mut levels(&[("a", 1.0)]), 39_000, 9000);
    assert_eq!(left(&out, 0), 1.0);
    let base = 39_000;
    assert_eq!(left(&out, 40_000 - 1 - base), 1.0);
    close(left(&out, 44_000 - 1 - base), 0.5);
    close(left(&out, 47_998 - base), 1.0 / 8000.0);
    assert_eq!(left(&out, 47_999 - base), 0.0);
}

#[test]
fn a_fade_shorter_than_a_sample_is_no_fade() {
    let mut c = clip("a", 0, 0, 3000);
    c.fade_in = t(1);
    let (out, _) = mix(&single(c), &mut levels(&[("a", 1.0)]), 0, 2);
    assert_eq!(left(&out, 0), 1.0);
}

#[test]
fn crossfade_window_is_centred_on_the_cut() {
    let p = crossfade();
    let segments = audio_segments(&p, 0, 200_000);
    let (a, b) = (segments[0], segments[1]);
    assert_eq!((a.start, a.len), (0, WINDOW_START + WINDOW_LEN), "a plays to the window end");
    assert_eq!((b.start, b.len), (WINDOW_START, 96_000 - WINDOW_START), "b starts at the window");
    assert_eq!(a.source_start, 0);
    assert_eq!(
        b.source_start,
        sample_position(t(45_000)) - (48_000 - WINDOW_START),
        "b runs before its in point"
    );
}

#[test]
fn odd_crossfade_gives_the_extra_tick_to_the_later_clip() {
    let mut p = crossfade();
    p.tracks[0].transitions[0].duration = t(45_001);
    let segments = audio_segments(&p, 0, 200_000);
    let (start, end) = (sample_position(t(67_500)), sample_position(t(112_501)));
    assert_eq!(segments[1].start, start);
    assert_eq!(segments[0].start + segments[0].len, end);
}

#[test]
fn crossfade_endpoints_and_midpoint_are_equal_power() {
    let p = crossfade();
    let segments = audio_segments(&p, 0, 200_000);
    let (a, b) = (segments[0].gain, segments[1].gain);
    let mid = WINDOW_START + WINDOW_LEN / 2;
    assert_eq!((a.gain_at(WINDOW_START), b.gain_at(WINDOW_START)), (1.0, 0.0));
    close(a.gain_at(mid), std::f32::consts::FRAC_1_SQRT_2);
    close(b.gain_at(mid), std::f32::consts::FRAC_1_SQRT_2);
    close(a.gain_at(WINDOW_START + WINDOW_LEN - 1), 0.0);
    close(b.gain_at(WINDOW_START + WINDOW_LEN - 1), 1.0);
    assert_eq!(a.gain_at(WINDOW_START + WINDOW_LEN), 0.0);
    assert_eq!(b.gain_at(WINDOW_START + WINDOW_LEN), 1.0);
    for s in (WINDOW_START..WINDOW_START + WINDOW_LEN).step_by(997) {
        let power = a.gain_at(s).powi(2) + b.gain_at(s).powi(2);
        close(power, 1.0);
    }
}

#[test]
fn crossfade_mixes_both_sources_with_their_gains() {
    let p = crossfade();
    let mut src = levels(&[("a", 0.5), ("b", 0.25)]);
    let (out, _) = mix(&p, &mut src, WINDOW_START - 1, WINDOW_LEN as usize + 2);
    close(left(&out, 0), 0.5);
    close(left(&out, 1), 0.5);
    close(
        left(&out, 1 + WINDOW_LEN as usize / 2),
        (0.5 + 0.25) * std::f32::consts::FRAC_1_SQRT_2,
    );
    close(left(&out, WINDOW_LEN as usize + 1), 0.25);
}

#[test]
fn audio_before_the_source_start_is_silence_and_is_not_read() {
    let mut p = crossfade();
    p.tracks[0].clips[1].source_in = t(0); // b would need 12000 samples before its beginning
    let mut src = levels(&[("b", 1.0)]);
    let (out, _) = mix(&p, &mut src, WINDOW_START, WINDOW_LEN as usize);
    assert_eq!(left(&out, 11_999), 0.0);
    assert!(left(&out, 12_000) > 0.0);
    assert!(src.reads.iter().all(|&(_, position, _)| position >= 0));
    let b = ClipKey { track: 0, clip: 1 };
    assert!(src.reads.contains(&(b, 0, 12_000)));
}

#[test]
fn block_size_does_not_change_the_mix() {
    let mut p = crossfade();
    p.tracks[0].clips[0].fade_in = t(9000);
    p.tracks[0].clips[0].gain_db = -3.0;
    let whole = mix(&p, &mut Positions, 0, 96_000).0;
    let mut pieces = Vec::new();
    for start in (0..96_000).step_by(1601) {
        let size = (96_000 - start).min(1601);
        pieces.extend(mix(&p, &mut Positions, start, size as usize).0);
    }
    assert_eq!(whole, pieces);
}

#[test]
fn clamps_and_counts_each_channel_sample() {
    let p = project(
        3000,
        vec![
            track(vec![clip("a", 0, 0, 3000)], vec![]),
            audio_track(vec![clip("b", 0, 0, 3000)]),
        ],
    );
    let (out, report) = mix(&p, &mut levels(&[("a", 0.8), ("b", 0.7)]), 0, 10);
    assert!(out.iter().all(|&v| v == 1.0));
    assert_eq!(report.clamped_samples, 20);

    let (out, report) = mix(&p, &mut levels(&[("a", -0.8), ("b", -0.7)]), 0, 10);
    assert!(out.iter().all(|&v| v == -1.0));
    assert_eq!(report.clamped_samples, 20);
}

#[test]
fn a_level_of_exactly_one_is_not_counted() {
    let p = single(clip("a", 0, 0, 3000));
    let (out, report) = mix(&p, &mut levels(&[("a", 1.0)]), 0, 10);
    assert!(out.iter().all(|&v| v == 1.0));
    assert_eq!(report.clamped_samples, 0);
}

#[test]
fn nan_from_a_source_becomes_counted_silence() {
    let p = single(clip("a", 0, 0, 3000));
    let (out, report) = mix(&p, &mut levels(&[("a", f32::NAN)]), 0, 3);
    assert!(out.iter().all(|&v| v == 0.0));
    assert_eq!(report.clamped_samples, 6);
}

#[test]
fn gaps_are_silent() {
    let p = project(
        3000,
        vec![track(
            vec![clip("a", 15, 0, 15), clip("a", 60, 0, 15)],
            vec![],
        )],
    );
    // Clips cover samples [8, 16) and [32, 40).
    let (out, report) = mix(&p, &mut levels(&[("a", 0.5)]), -4, 60);
    let at = |s: i64| left(&out, (s + 4) as usize);
    assert_eq!(at(-1), 0.0, "before the timeline");
    assert_eq!(at(7), 0.0, "before the first clip");
    assert_eq!(at(8), 0.5);
    assert_eq!(at(15), 0.5);
    assert_eq!(at(16), 0.0, "between clips");
    assert_eq!(at(32), 0.5);
    assert_eq!(at(40), 0.0, "after the last clip");
    assert_eq!(report, MixReport::default());
}

#[test]
fn a_range_past_the_timeline_reads_nothing() {
    let p = single(clip("a", 0, 0, 3000));
    let mut src = levels(&[("a", 1.0)]);
    let (out, _) = mix(&p, &mut src, 100_000, 8);
    assert!(out.iter().all(|&v| v == 0.0));
    assert!(src.reads.is_empty());
}

#[test]
fn muted_clips_are_silent_and_not_read() {
    let mut c = clip("a", 0, 0, 3000);
    c.muted = true;
    let mut src = levels(&[("a", 1.0)]);
    let (out, _) = mix(&single(c), &mut src, 0, 16);
    assert!(out.iter().all(|&v| v == 0.0));
    assert!(src.reads.is_empty());
}

#[test]
fn muting_one_side_of_a_crossfade_keeps_the_other() {
    let mut p = crossfade();
    p.tracks[0].clips[0].muted = true;
    let (out, _) = mix(&p, &mut levels(&[("b", 1.0)]), WINDOW_START, WINDOW_LEN as usize);
    close(left(&out, WINDOW_LEN as usize / 2), std::f32::consts::FRAC_1_SQRT_2);
}

#[test]
fn a_source_without_audio_is_silence() {
    let p = single(clip("no-audio", 0, 0, 3000));
    let (out, report) = mix(&p, &mut levels(&[]), 0, 16);
    assert!(out.iter().all(|&v| v == 0.0));
    assert_eq!(report.clamped_samples, 0);
}

#[test]
fn source_errors_are_returned() {
    struct Failing;
    impl AudioSource for Failing {
        fn read(&mut self, _: ClipKey, _: &str, _: i64, _: &mut [f32]) -> Result<(), RenderError> {
            Err("decode failed".into())
        }
    }
    let p = single(clip("a", 0, 0, 3000));
    let mut out = [0.0; 4];
    assert_eq!(
        mix_audio(&p, &mut Failing, 0, &mut out),
        Err(RenderError("decode failed".into()))
    );
}

#[test]
fn an_odd_output_slice_zeroes_its_last_element() {
    let p = single(clip("a", 0, 0, 3000));
    let mut out = [9.0; 5];
    mix_audio(&p, &mut levels(&[("a", 0.5)]), 0, &mut out).unwrap();
    assert_eq!(out, [0.5, 0.5, 0.5, 0.5, 0.0]);
}
