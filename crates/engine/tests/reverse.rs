//! Reverse playback: a reversed clip plays the same source range from its last frame to its first,
//! and every edit keeps the frames it would keep on a clip playing forward.

mod common;

use common::*;
use engine::{
    layers_at_frame, set_animation, set_reverse, set_speed, split_clip, trim_end, trim_source_in, trim_start, validate, AcceptAll, AnimationRef, Beat, ClipAnimation, ClipKey, EditError, LayerContent, Project, Time,
};

const SECOND: i64 = 90_000;
const FRAME: i64 = 3_000;

fn key(clip: usize) -> ClipKey {
    ClipKey { track: 0, clip }
}

/// One reversed clip of two seconds from source 1 s, then room after it.
fn reversed() -> Project {
    let mut c = clip("a", 0, SECOND, 2 * SECOND);
    c.reverse = true;
    project(FRAME, vec![track(vec![c], vec![])])
}

/// The source time shown on every frame of track 0 from `from` to `to` (ticks, exclusive).
fn shown(project: &Project, from: i64, to: i64) -> Vec<i64> {
    (from / FRAME..to / FRAME)
        .map(|frame| match layers_at_frame(project, frame).first().map(|layer| layer.content) {
            Some(LayerContent::Clip(sample)) => sample.source_time.ticks(),
            other => panic!("no clip at frame {frame}: {other:?}"),
        })
        .collect()
}

#[test]
fn a_reversed_clip_plays_its_source_range_from_the_last_frame_to_the_first() {
    let p = reversed();
    let times = shown(&p, 0, 2 * SECOND);
    assert_eq!(times.first(), Some(&(3 * SECOND - 1)), "the last tick of the range first");
    assert_eq!(times.last(), Some(&(SECOND + FRAME - 1)), "the last frame shows the frame at source_in");
    assert!(times.windows(2).all(|pair| pair[1] < pair[0]), "time falls on every frame");
    let mut forward = p.clone();
    forward.tracks[0].clips[0].reverse = false;
    // Mirror images inside the range: forward + reversed = source_in + source_end - 1.
    for (f, r) in shown(&forward, 0, 2 * SECOND).iter().zip(&times) {
        assert_eq!(f + r, SECOND + 3 * SECOND - 1);
    }
}

#[test]
fn a_reversed_clip_at_another_speed_covers_the_same_range() {
    let p = set_speed(&reversed(), key(0), 2.0).unwrap();
    let c = &p.tracks[0].clips[0];
    assert_eq!((c.duration.ticks(), c.source_in.ticks(), c.source_end().ticks()), (SECOND, SECOND, 3 * SECOND));
    let times = shown(&p, 0, SECOND);
    assert_eq!(times.first(), Some(&(3 * SECOND - 1)));
    assert_eq!(times.last(), Some(&(SECOND + 2 * FRAME - 1)));
}

#[test]
fn the_holds_of_a_reversed_clip_show_its_first_and_last_moving_frames() {
    let hold = |duration: i64| Some(AnimationRef { preset: "fade".to_owned(), duration: t(duration) });
    let p = set_animation(&reversed(), key(0), ClipAnimation { enter: hold(-SECOND / 2), exit: hold(-SECOND / 2), ..ClipAnimation::default() }).unwrap();
    let c = &p.tracks[0].clips[0];
    assert_eq!(c.duration.ticks(), 3 * SECOND);
    let times = shown(&p, 0, 3 * SECOND);
    let half = (SECOND / 2 / FRAME) as usize;
    assert!(times[..half].iter().all(|at| *at == 3 * SECOND - 1), "the start hold is the last source frame: {:?}", &times[..half]);
    assert!(times[times.len() - half..].iter().all(|at| *at == SECOND), "the end hold is the frame at source_in");
}

#[test]
fn a_split_reversed_clip_shows_the_same_frames_in_both_halves() {
    let before = reversed();
    let whole = shown(&before, 0, 2 * SECOND);
    let cut = split_clip(&before, key(0), t(SECOND / 2)).unwrap();
    let (first, second) = (&cut.tracks[0].clips[0], &cut.tracks[0].clips[1]);
    assert!(first.reverse && second.reverse);
    assert_eq!((first.source_in.ticks(), first.source_end().ticks()), (SECOND + 3 * SECOND / 2, 3 * SECOND), "the first half keeps the top of the range");
    assert_eq!((second.source_in.ticks(), second.source_end().ticks()), (SECOND, SECOND + 3 * SECOND / 2), "the second half goes on below it");
    assert_eq!(shown(&cut, 0, 2 * SECOND), whole);
    assert!(validate(&cut, &AcceptAll).is_empty());
}

#[test]
fn a_split_reversed_clip_at_another_speed_keeps_its_frames() {
    let before = set_speed(&reversed(), key(0), 1.5).unwrap();
    let length = before.tracks[0].clips[0].duration.ticks();
    let whole = shown(&before, 0, length);
    let cut = split_clip(&before, key(0), t(SECOND / 3)).unwrap();
    assert_eq!(shown(&cut, 0, length), whole);
}

#[test]
fn trimming_the_start_of_a_reversed_clip_keeps_the_frames_under_its_end() {
    let before = reversed();
    let whole = shown(&before, 0, 2 * SECOND);
    let trimmed = trim_start(&before, key(0), t(SECOND / 2)).unwrap();
    let c = &trimmed.tracks[0].clips[0];
    assert_eq!((c.start.ticks(), c.source_in.ticks(), c.duration.ticks()), (SECOND / 2, SECOND, 3 * SECOND / 2));
    assert_eq!(shown(&trimmed, SECOND / 2, 2 * SECOND), whole[(SECOND / 2 / FRAME) as usize..]);
}

#[test]
fn trimming_the_end_of_a_reversed_clip_keeps_the_frames_under_its_start() {
    let before = reversed();
    let whole = shown(&before, 0, 2 * SECOND);
    let shorter = trim_end(&before, key(0), t(SECOND)).unwrap();
    let c = &shorter.tracks[0].clips[0];
    assert_eq!((c.source_in.ticks(), c.source_end().ticks()), (2 * SECOND, 3 * SECOND));
    assert_eq!(shown(&shorter, 0, SECOND), whole[..(SECOND / FRAME) as usize]);
    // Longer reads the source further down, until it begins.
    let longer = trim_end(&before, key(0), t(3 * SECOND)).unwrap();
    assert_eq!(longer.tracks[0].clips[0].source_in.ticks(), 0);
    assert_eq!(shown(&longer, 0, 2 * SECOND), whole);
    assert!(matches!(trim_end(&before, key(0), t(4 * SECOND)), Err(EditError::BeforeSourceStart { .. })));
}

#[test]
fn moving_the_source_in_of_a_reversed_clip_keeps_its_start() {
    let before = reversed();
    let whole = shown(&before, 0, 2 * SECOND);
    let later = trim_source_in(&before, key(0), t(2 * SECOND)).unwrap();
    let c = &later.tracks[0].clips[0];
    assert_eq!((c.start.ticks(), c.source_in.ticks(), c.duration.ticks()), (0, 2 * SECOND, SECOND));
    assert_eq!(shown(&later, 0, SECOND), whole[..(SECOND / FRAME) as usize]);
}

#[test]
fn the_beats_of_a_reversed_clip_come_in_timeline_order() {
    let p = reversed();
    let c = &p.tracks[0].clips[0];
    let beats: Vec<Beat> = (0..8).map(|i| Beat { at: t(i * SECOND / 2), downbeat: i % 4 == 0 }).collect();
    let found: Vec<(i64, i64)> = c.beat_times(&beats).map(|(at, beat)| (at.ticks(), beat.at.ticks())).collect();
    // Source beats 1, 1.5, 2, 2.5 s are played, the last one first.
    assert_eq!(found, vec![(SECOND / 2 - 1, 5 * SECOND / 2), (SECOND - 1, 2 * SECOND), (3 * SECOND / 2 - 1, 3 * SECOND / 2), (2 * SECOND - 1, SECOND)]);
    let window: Vec<i64> = c.beat_times_between(&beats, t(SECOND / 2), t(3 * SECOND / 2)).map(|(at, _)| at.ticks()).collect();
    assert_eq!(window, vec![SECOND - 1, 3 * SECOND / 2 - 1]);
    let all: Vec<i64> = c.beat_times_between(&beats, Time::ZERO, t(2 * SECOND)).map(|(at, _)| at.ticks()).collect();
    assert_eq!(all, found.iter().map(|(at, _)| *at).collect::<Vec<_>>());
}

#[test]
fn reverse_is_refused_on_a_still_and_left_out_of_the_file_when_off() {
    let still = project(FRAME, vec![track(vec![clip("logo.png", 0, 0, SECOND)], vec![])]);
    assert!(matches!(set_reverse(&still, key(0), true), Err(EditError::ReverseOfStill { .. })));
    let on = set_reverse(&reversed(), key(0), false).unwrap();
    assert!(!serde_json::to_string(&on).unwrap().contains("reverse"));
    let text = serde_json::to_string(&reversed()).unwrap();
    assert!(text.contains("\"reverse\":true"));
    let back: Project = serde_json::from_str(&text).unwrap();
    assert!(back.tracks[0].clips[0].reverse);
}
