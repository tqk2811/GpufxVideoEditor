//! Scheduler tests: spec rows "Đúng thời gian" and the clip/transition frame boundaries.

mod common;

use common::*;
use engine::{frame_count, frame_time, layers_at_frame, LayerContent, Project, Time};

fn clip_layer<'a>(p: &'a Project, frame: i64, track: usize) -> engine::SourceSample<'a> {
    let layers = layers_at_frame(p, frame);
    let layer = layers
        .iter()
        .find(|l| l.track == track)
        .expect("track has a layer");
    match layer.content {
        LayerContent::Clip(s) => s,
        other => panic!("expected clip, got {other:?}"),
    }
}

#[test]
fn cut_maps_source_time() {
    // 30 fps (3000 ticks), clip shows source from 1 s, placed at timeline 0.
    let p = project(
        3000,
        vec![track(vec![clip("a.mp4", 0, 90_000, 30_000)], vec![])],
    );
    assert_eq!(frame_count(&p), 10);
    for n in 0..10 {
        let s = clip_layer(&p, n, 0);
        assert_eq!(s.source, "a.mp4");
        assert_eq!(s.source_time, t(90_000 + n * 3000));
    }
    assert!(layers_at_frame(&p, 10).is_empty());
}

#[test]
fn shifted_clip_starts_at_its_position() {
    let p = project(
        3000,
        vec![track(vec![clip("a.mp4", 30_000, 6000, 30_000)], vec![])],
    );
    assert_eq!(frame_count(&p), 20);
    assert!(layers_at_frame(&p, 9).is_empty(), "nothing before the clip");
    assert_eq!(
        clip_layer(&p, 10, 0).source_time,
        t(6000),
        "first frame of clip"
    );
    assert_eq!(
        clip_layer(&p, 19, 0).source_time,
        t(6000 + 27_000),
        "last frame of clip"
    );
}

#[test]
fn clip_first_and_last_frame_boundaries() {
    let p = project(
        3000,
        vec![track(
            vec![
                clip("a.mp4", 0, 0, 15_000),
                clip("b.mp4", 15_000, 300, 15_000),
            ],
            vec![],
        )],
    );
    assert_eq!(clip_layer(&p, 0, 0).source, "a.mp4");
    assert_eq!(
        clip_layer(&p, 4, 0).source,
        "a.mp4",
        "last frame of first clip"
    );
    assert_eq!(clip_layer(&p, 4, 0).source_time, t(12_000));
    let first_b = clip_layer(&p, 5, 0);
    assert_eq!((first_b.source, first_b.source_time), ("b.mp4", t(300)));
    assert_eq!(clip_layer(&p, 9, 0).source_time, t(300 + 12_000));
}

#[test]
fn partial_last_frame_is_kept() {
    // 10000 ticks at 3003 per frame: 3.33 frames -> 4 frames.
    let p = project(3003, vec![track(vec![clip("a.mp4", 0, 0, 10_000)], vec![])]);
    assert_eq!(frame_count(&p), 4);
    assert_eq!(clip_layer(&p, 3, 0).source_time, t(9009));
}

#[test]
fn ntsc_cadence_over_ten_minutes_has_no_drift() {
    // 30000/1001 fps is exactly 3003 ticks per frame.
    let ten_minutes = 600 * 90_000;
    let p = project(
        3003,
        vec![track(vec![clip("a.mp4", 0, 0, ten_minutes)], vec![])],
    );
    // 54_000_000 / 3003 = 17982.017... -> 17983 frames.
    assert_eq!(frame_count(&p), 17_983);
    // Every frame lands exactly on n * 3003; spot-check and run the full length.
    for n in [0, 1, 1000, 17_981, 17_982] {
        assert_eq!(frame_time(&p, n), Some(t(n * 3003)));
        assert_eq!(clip_layer(&p, n, 0).source_time, t(n * 3003));
    }
    // 17982 frames of 1001/30000 s is 599.999400 s, 600 s - 540 ticks: no accumulated error.
    assert_eq!(frame_time(&p, 17_982).unwrap().ticks(), 17_982 * 3003);
    assert_eq!(ten_minutes - 17_982 * 3003, 54);
    let mut previous = -1;
    for n in 0..frame_count(&p) {
        let s = clip_layer(&p, n, 0);
        assert_eq!(s.source_time.ticks(), n * 3003);
        assert!(s.source_time.ticks() > previous);
        previous = s.source_time.ticks();
    }
}

#[test]
fn frame_duration_3003_in_clip_offsets() {
    let p = project(
        3003,
        vec![track(vec![clip("a.mp4", 3003 * 7, 5, 3003 * 3)], vec![])],
    );
    assert_eq!(frame_count(&p), 10);
    assert!(layers_at_frame(&p, 6).is_empty());
    assert_eq!(clip_layer(&p, 7, 0).source_time, t(5));
    assert_eq!(clip_layer(&p, 9, 0).source_time, t(5 + 3003 * 2));
}

#[test]
fn empty_project_has_no_frames() {
    let p = project(3000, vec![]);
    assert_eq!(frame_count(&p), 0);
    assert!(layers_at_frame(&p, 0).is_empty());
    assert!(layers_at_frame(&p, -1).is_empty());
}

#[test]
fn layers_are_bottom_track_first() {
    let p = project(
        3000,
        vec![
            track(vec![clip("a.mp4", 0, 0, 30_000)], vec![]),
            track(vec![clip("b.mp4", 6000, 0, 9000)], vec![]),
            track(vec![clip("c.mp4", 0, 0, 30_000)], vec![]),
        ],
    );
    let at = |f| {
        layers_at_frame(&p, f)
            .iter()
            .map(|l| l.track)
            .collect::<Vec<_>>()
    };
    assert_eq!(at(0), vec![0, 2]);
    assert_eq!(at(2), vec![0, 1, 2]);
    assert_eq!(at(5), vec![0, 2], "middle track ended at 15000");
}

#[test]
fn transition_first_and_last_frame_boundaries() {
    // Cut at 15000. Transition 6000 long -> window [12000, 18000): frames 4 and 5.
    let p = project(
        3000,
        vec![track(
            vec![
                clip("a.mp4", 0, 1000, 15_000),
                clip("b.mp4", 15_000, 500, 15_000),
            ],
            vec![transition(0, 6000)],
        )],
    );
    assert_eq!(frame_count(&p), 10);
    assert_eq!(
        clip_layer(&p, 3, 0).source,
        "a.mp4",
        "frame before the window"
    );

    let layers = layers_at_frame(&p, 4);
    match layers[0].content {
        LayerContent::Transition {
            package,
            from,
            to,
            progress,
            ..
        } => {
            assert_eq!(package, "mix");
            assert_eq!((from.source, from.source_time), ("a.mp4", t(1000 + 12_000)));
            // b starts at 15000, so at 12000 it is 3000 before its in point 500: clamped to 0.
            assert_eq!((to.source, to.source_time), ("b.mp4", t(0)));
            assert_eq!(progress.parts_per_million(), 0);
        }
        other => panic!("expected transition, got {other:?}"),
    }

    let layers = layers_at_frame(&p, 5);
    match layers[0].content {
        LayerContent::Transition {
            from, to, progress, ..
        } => {
            assert_eq!(from.source_time, t(1000 + 15_000), "past a's out point");
            assert_eq!(to.source_time, t(500));
            assert_eq!(progress.parts_per_million(), 500_000);
        }
        other => panic!("expected transition, got {other:?}"),
    }

    let after = clip_layer(&p, 6, 0);
    assert_eq!((after.source, after.source_time), ("b.mp4", t(500 + 3000)));
}

#[test]
fn transition_odd_duration_and_last_progress() {
    // Window [15000 - 3, 15000 - 3 + 7) = [14997, 15004) with 3003 per frame: frames at 15015 etc.
    // Use frame_duration 1 so every tick is a frame.
    let p = project(
        1,
        vec![track(
            vec![
                clip("a.mp4", 0, 0, 15_000),
                clip("b.mp4", 15_000, 0, 15_000),
            ],
            vec![transition(0, 7)],
        )],
    );
    assert!(matches!(
        layers_at_frame(&p, 14_996)[0].content,
        LayerContent::Clip(_)
    ));
    let first = layers_at_frame(&p, 14_997);
    let last = layers_at_frame(&p, 15_003);
    let end = layers_at_frame(&p, 15_004);
    match first[0].content {
        LayerContent::Transition { progress, .. } => assert_eq!(progress.parts_per_million(), 0),
        _ => panic!("window must start at 14997"),
    }
    match last[0].content {
        LayerContent::Transition { progress, .. } => {
            assert_eq!(progress.elapsed, t(6));
            assert_eq!(progress.parts_per_million(), 857_142, "floor(6e6 / 7)");
        }
        _ => panic!("window must include 15003"),
    }
    assert!(
        matches!(end[0].content, LayerContent::Clip(_)),
        "window is half-open"
    );
}

#[test]
fn time_conversion_helpers() {
    assert_eq!(Time::from_seconds(600).unwrap().ticks(), 54_000_000);
}

/// Elapsed and duration of a clip's clock at frame `n`.
fn clock_at(p: &Project, frame: i64, track_index: usize) -> (i64, i64) {
    let s = clip_layer(p, frame, track_index);
    (s.clock.elapsed.ticks(), s.clock.duration.ticks())
}

#[test]
fn clip_clock_counts_from_the_clip_start_in_timeline_time() {
    // A clip at timeline 1 s, 10 frames long, and a 3000 tick frame.
    let p = project(3000, vec![track(vec![clip("a.mp4", 90_000, 6000, 30_000)], vec![])]);
    assert_eq!(clock_at(&p, 30, 0), (0, 30_000), "first frame of the clip");
    assert_eq!(clock_at(&p, 35, 0), (15_000, 30_000), "middle");
    assert_eq!(clock_at(&p, 39, 0), (27_000, 30_000), "last frame: one frame short of the end");
}

#[test]
fn clip_clock_is_not_scaled_by_speed() {
    let mut fast = clip("a.mp4", 0, 0, 30_000);
    fast.speed = 2.0;
    let p = project(3000, vec![track(vec![fast], vec![])]);
    assert_eq!(clock_at(&p, 5, 0), (15_000, 30_000), "seconds on screen, the source runs 2x");
    assert_eq!(clip_layer(&p, 5, 0).source_time, t(30_000), "the source itself does run faster");
}

#[test]
fn clip_clock_restarts_after_a_split() {
    let original = project(3000, vec![track(vec![clip("a.mp4", 0, 0, 30_000)], vec![])]);
    let p = engine::split_clip(&original, engine::ClipKey { track: 0, clip: 0 }, t(12_000)).expect("split");
    assert_eq!(clock_at(&p, 3, 0), (9_000, 12_000), "left piece");
    assert_eq!(clock_at(&p, 4, 0), (0, 18_000), "right piece starts its own clock");
    assert_eq!(clock_at(&p, 9, 0), (15_000, 18_000));
}

/// The clocks `(elapsed, duration)` of both clips of the transition at `frame`.
fn transition_clocks(p: &Project, frame: i64, track_index: usize) -> ((i64, i64), (i64, i64)) {
    let layers = layers_at_frame(p, frame);
    let layer = layers.iter().find(|l| l.track == track_index).expect("a layer");
    let LayerContent::Transition { from, to, .. } = layer.content else { panic!("expected a transition at frame {frame}") };
    ((from.clock.elapsed.ticks(), from.clock.duration.ticks()), (to.clock.elapsed.ticks(), to.clock.duration.ticks()))
}

#[test]
fn clip_clock_counts_from_the_moment_the_clip_becomes_visible() {
    // Two 30000 tick clips with a 12000 tick transition: the window is 24000..36000 around the cut
    // at 30000. Clip 0 is visible 0..36000, clip 1 is visible 24000..60000.
    let p = project(
        3000,
        vec![track(
            vec![clip("a.mp4", 0, 0, 30_000), clip("b.mp4", 30_000, 0, 30_000)],
            vec![transition(0, 12_000)],
        )],
    );
    // Transition out of clip 0: it counts on past its own end, up to the window end.
    assert_eq!(clock_at(&p, 7, 0), (21_000, 36_000), "before the window: the duration is the visible one");
    let (from, to) = transition_clocks(&p, 11, 0);
    assert_eq!(from, (33_000, 36_000), "past its own end the clip keeps counting");
    // Transition into clip 1: it counts from the window start, not from the cut.
    assert_eq!(to, (9_000, 36_000), "clip 1 has been visible for 9000 ticks, 3000 of them after the cut");
    let (_, first) = transition_clocks(&p, 8, 0);
    assert_eq!(first, (0, 36_000), "first visible frame of clip 1: 0, not clamped from a negative offset");
    let (_, before_cut) = transition_clocks(&p, 9, 0);
    assert_eq!(before_cut.0, 3_000, "before the cut it already counts");
    assert_eq!(clock_at(&p, 12, 0), (12_000, 36_000), "after the window clip 1 goes on");
    assert_eq!(clock_at(&p, 19, 0), (33_000, 36_000), "last frame of clip 1: one frame short of the visible end");
}

#[test]
fn clip_clock_of_a_clip_without_a_transition_is_unchanged_next_to_other_clips() {
    // Clip 2 has no transition on either side; only 0 and 1 are joined.
    let p = project(
        3000,
        vec![track(
            vec![clip("a.mp4", 0, 0, 30_000), clip("b.mp4", 30_000, 0, 30_000), clip("c.mp4", 60_000, 0, 30_000)],
            vec![transition(0, 12_000)],
        )],
    );
    assert_eq!(clock_at(&p, 20, 0), (0, 30_000));
    assert_eq!(clock_at(&p, 25, 0), (15_000, 30_000));
    assert_eq!(clock_at(&p, 29, 0), (27_000, 30_000));
}

#[test]
fn clip_clock_of_a_clip_between_two_transitions_spans_both_windows() {
    // Clips of 30000 ticks; transitions of 12000 (after clip 0) and 6000 (after clip 1).
    // Window 0 is 24000..36000, window 1 is 57000..63000 (cut at 60000).
    // Clip 1 is visible 24000..63000, so its clock lasts 39000.
    let p = project(
        3000,
        vec![track(
            vec![clip("a.mp4", 0, 0, 30_000), clip("b.mp4", 30_000, 0, 30_000), clip("c.mp4", 60_000, 0, 30_000)],
            vec![transition(0, 12_000), transition(1, 6_000)],
        )],
    );
    let (_, into) = transition_clocks(&p, 8, 0);
    assert_eq!(into, (0, 39_000), "starts counting at the first window");
    let (out_from, out_to) = transition_clocks(&p, 19, 0);
    assert_eq!(out_from, (33_000, 39_000), "57000 - 24000");
    assert_eq!(out_to, (0, 33_000), "clip 2 is visible from its window start 57000 to 90000");
    let (last_from, _) = transition_clocks(&p, 20, 0);
    assert_eq!(last_from, (36_000, 39_000), "the last frame of the second window");
}
