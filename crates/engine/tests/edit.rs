//! Timeline edit commands and the undo history.

mod common;

use common::*;
use engine::{
    add_clip, add_effect, add_track, add_transition, audio_sample_count, audio_segments, clip_at,
    delete_clip, frame_count, layers_at_frame, move_clip, nearest_free_start, remove_effect,
    remove_transition, set_fades, set_gain, set_muted, split_clip, trim_end, trim_start, validate,
    AcceptAll, ClipKey, EditError, EditHistory, Effect, LayerContent, Project, SourceSample, Time,
    TrackKind, ValidationError,
};

fn key(track: usize, clip: usize) -> ClipKey {
    ClipKey { track, clip }
}

fn assert_valid(p: &Project) {
    assert_eq!(validate(p, &AcceptAll), vec![]);
}

/// Three touching clips with a transition on each cut, then a clip after a gap.
fn chain() -> Project {
    project(
        3000,
        vec![
            track(
                vec![
                    clip("a", 0, 0, 30_000),
                    clip("b", 30_000, 0, 30_000),
                    clip("c", 60_000, 0, 30_000),
                    clip("d", 120_000, 0, 30_000),
                ],
                vec![transition(0, 6000), transition(1, 6000)],
            ),
            track(vec![clip("e", 0, 0, 30_000)], vec![]),
            audio_track(vec![clip("m", 0, 0, 30_000)]),
        ],
    )
}

fn after_clips(p: &Project, track: usize) -> Vec<usize> {
    p.tracks[track].transitions.iter().map(|tr| tr.after_clip).collect()
}

fn sources(p: &Project, track: usize) -> Vec<&str> {
    p.tracks[track].clips.iter().map(|c| c.source.as_str()).collect()
}

fn starts(p: &Project, track: usize) -> Vec<i64> {
    p.tracks[track].clips.iter().map(|c| c.start.ticks()).collect()
}

#[test]
fn add_track_appends_an_empty_track_of_the_kind() {
    let p = add_track(&chain(), TrackKind::Audio).unwrap();
    assert_eq!(p.tracks.len(), 4);
    assert_eq!(p.tracks[3].kind, TrackKind::Audio);
    assert!(p.tracks[3].clips.is_empty());
    assert_valid(&p);
}

#[test]
fn add_clip_before_a_transition_renumbers_it() {
    let base = project(
        3000,
        vec![track(
            vec![clip("a", 60_000, 0, 30_000), clip("b", 90_000, 0, 30_000)],
            vec![transition(0, 6000)],
        )],
    );
    let p = add_clip(&base, 0, clip("x", 0, 0, 30_000)).unwrap();
    assert_eq!(sources(&p, 0), ["x", "a", "b"]);
    assert_eq!(after_clips(&p, 0), [1]);
    assert_valid(&p);
}

#[test]
fn add_clip_refuses_an_overlap_and_a_missing_track() {
    let base = chain();
    assert_eq!(
        add_clip(&base, 0, clip("x", 10_000, 0, 1000)),
        Err(EditError::Overlap { track: 0, start: t(10_000), end: t(11_000) })
    );
    assert_eq!(
        add_clip(&base, 9, clip("x", 0, 0, 1000)),
        Err(EditError::TrackNotFound { track: 9 })
    );
}

#[test]
fn add_clip_refuses_a_clip_validation_rejects() {
    let result = add_clip(&chain(), 1, clip("x", 90_000, 0, 0));
    assert!(matches!(result, Err(EditError::Invalid(_))), "{result:?}");
}

#[test]
fn move_within_a_track_keeps_the_source_fields() {
    let p = move_clip(&chain(), key(0, 3), 0, t(200_000)).unwrap();
    let moved = &p.tracks[0].clips[3];
    assert_eq!(moved.start, t(200_000));
    assert_eq!(moved.source_in, t(0));
    assert_eq!(moved.duration, t(30_000));
    assert_eq!(after_clips(&p, 0), [0, 1], "other transitions are unchanged");
    assert_valid(&p);
}

#[test]
fn moving_a_clip_past_another_reorders_and_renumbers() {
    let base = project(
        3000,
        vec![track(
            vec![
                clip("a", 0, 0, 10_000),
                clip("b", 20_000, 0, 10_000),
                clip("c", 30_000, 0, 10_000),
            ],
            vec![transition(1, 2000)],
        )],
    );
    let p = move_clip(&base, key(0, 0), 0, t(50_000)).unwrap();
    assert_eq!(sources(&p, 0), ["b", "c", "a"]);
    assert_eq!(after_clips(&p, 0), [0]);
    assert_eq!(p.tracks[0].transitions[0].package, "mix");
    assert_valid(&p);
}

#[test]
fn move_between_tracks_of_the_same_kind() {
    let p = move_clip(&chain(), key(0, 3), 1, t(40_000)).unwrap();
    assert_eq!(sources(&p, 0), ["a", "b", "c"]);
    assert_eq!(sources(&p, 1), ["e", "d"]);
    assert_eq!(starts(&p, 1), [0, 40_000]);
    assert_valid(&p);
}

#[test]
fn move_between_kinds_is_refused() {
    assert_eq!(
        move_clip(&chain(), key(0, 3), 2, t(40_000)),
        Err(EditError::TrackKindMismatch { from: TrackKind::Video, to: TrackKind::Audio })
    );
}

#[test]
fn moving_onto_a_clip_lands_beside_it_and_pushes_nothing() {
    let base = project(
        3000,
        vec![track(
            vec![
                clip("a", 0, 0, 30_000),
                clip("b", 40_000, 0, 30_000),
                clip("c", 100_000, 0, 10_000),
            ],
            vec![],
        )],
    );
    // Inside b: the free spots at 30 000 and 70 000 are 22 000 and 18 000 away.
    let p = move_clip(&base, key(0, 2), 0, t(52_000)).unwrap();
    assert_eq!(starts(&p, 0), [0, 40_000, 70_000], "the nearest free spot");

    // Exactly between them: the earlier spot wins.
    let p = move_clip(&base, key(0, 2), 0, t(50_000)).unwrap();
    assert_eq!(starts(&p, 0), [0, 30_000, 40_000]);

    // a keeps its length: 10 000 before b is 31 000 away, 70 000 after b is 29 000 away.
    let p = move_clip(&base, key(0, 0), 0, t(41_000)).unwrap();
    assert_eq!(sources(&p, 0), ["b", "a", "c"]);
    assert_eq!(starts(&p, 0), [40_000, 70_000, 100_000], "a fits between b and c");
    assert_valid(&p);
}

#[test]
fn a_move_before_time_zero_lands_at_zero() {
    let base = project(3000, vec![track(vec![clip("a", 10_000, 0, 5000)], vec![])]);
    let p = move_clip(&base, key(0, 0), 0, t(-500)).unwrap();
    assert_eq!(starts(&p, 0), [0]);
}

#[test]
fn nearest_free_start_prefers_the_earlier_side_on_a_tie() {
    let track = track(
        vec![clip("a", 100, 0, 100), clip("b", 400, 0, 100)],
        vec![],
    );
    // The gap 200..400 holds a clip of 200 only at 200.
    assert_eq!(nearest_free_start(&track, None, t(200), t(300)), Some(t(200)));
    // The gap 0..100 is too small for 150, so the next gap is used.
    assert_eq!(nearest_free_start(&track, None, t(150), t(150)), Some(t(200)));
    // Asked for 125, inside a: 50 (from gap 0..100) and 200 are 75 away, the earlier wins.
    assert_eq!(nearest_free_start(&track, None, t(50), t(125)), Some(t(50)));
    assert_eq!(nearest_free_start(&track, None, Time::ZERO, t(0)), None);
}

#[test]
fn moving_a_clip_drops_only_the_transitions_touching_it() {
    let p = move_clip(&chain(), key(0, 1), 0, t(200_000)).unwrap();
    assert_eq!(sources(&p, 0), ["a", "c", "d", "b"]);
    assert_eq!(p.tracks[0].transitions.len(), 0, "both of its transitions are gone");
    assert_valid(&p);

    let base = project(
        3000,
        vec![track(
            vec![
                clip("a", 0, 0, 30_000),
                clip("b", 30_000, 0, 30_000),
                clip("c", 60_000, 0, 30_000),
                clip("d", 90_000, 0, 30_000),
            ],
            vec![transition(0, 6000), transition(2, 6000)],
        )],
    );
    let p = move_clip(&base, key(0, 1), 0, t(200_000)).unwrap();
    assert_eq!(sources(&p, 0), ["a", "c", "d", "b"]);
    assert_eq!(after_clips(&p, 0), [1], "c->d is renumbered");
    assert_valid(&p);
}

#[test]
fn a_move_that_ends_in_place_keeps_the_transitions() {
    let base = chain();
    // b cannot move: it sits between touching neighbours, so it returns to its own start.
    assert_eq!(move_clip(&base, key(0, 1), 0, t(30_100)).unwrap(), base);
    assert_eq!(move_clip(&base, key(0, 1), 0, t(30_000)).unwrap(), base);
}

#[test]
fn trim_start_moves_the_start_and_the_source_together() {
    let base = project(3000, vec![track(vec![clip("a", 10_000, 5000, 20_000)], vec![])]);
    let p = trim_start(&base, key(0, 0), t(13_000)).unwrap();
    let c = &p.tracks[0].clips[0];
    assert_eq!((c.start, c.source_in, c.duration), (t(13_000), t(8000), t(17_000)));
    let p = trim_start(&base, key(0, 0), t(7000)).unwrap();
    let c = &p.tracks[0].clips[0];
    assert_eq!((c.start, c.source_in, c.duration), (t(7000), t(2000), t(23_000)));
    assert_valid(&p);
}

#[test]
fn trim_start_refuses_what_it_cannot_do() {
    let base = project(
        3000,
        vec![track(
            vec![clip("a", 0, 0, 10_000), clip("b", 20_000, 1000, 10_000)],
            vec![],
        )],
    );
    assert_eq!(
        trim_start(&base, key(0, 1), t(18_000)),
        Err(EditError::BeforeSourceStart { key: key(0, 1), source_in: t(-1000) })
    );
    let wide = project(
        3000,
        vec![track(
            vec![clip("a", 0, 0, 10_000), clip("b", 20_000, 90_000, 10_000)],
            vec![],
        )],
    );
    assert_eq!(
        trim_start(&wide, key(0, 1), t(5000)),
        Err(EditError::Overlap { track: 0, start: t(5000), end: t(30_000) })
    );
    assert!(matches!(
        trim_start(&base, key(0, 1), t(30_000)),
        Err(EditError::Invalid(_))
    ));
}

#[test]
fn trim_end_changes_only_the_duration() {
    let base = project(3000, vec![track(vec![clip("a", 10_000, 5000, 20_000)], vec![])]);
    let p = trim_end(&base, key(0, 0), t(15_000)).unwrap();
    let c = &p.tracks[0].clips[0];
    assert_eq!((c.start, c.source_in, c.duration), (t(10_000), t(5000), t(5000)));
    assert_valid(&p);

    let two = project(
        3000,
        vec![track(vec![clip("a", 0, 0, 1000), clip("b", 2000, 0, 1000)], vec![])],
    );
    assert_eq!(
        trim_end(&two, key(0, 0), t(2500)),
        Err(EditError::Overlap { track: 0, start: t(0), end: t(2500) })
    );
}

#[test]
fn a_trim_with_a_transition_on_the_edge_is_refused() {
    let base = chain();
    assert_eq!(
        trim_start(&base, key(0, 1), t(31_000)),
        Err(EditError::TransitionOnEdge { key: key(0, 1), edge: "start" })
    );
    assert_eq!(
        trim_end(&base, key(0, 1), t(59_000)),
        Err(EditError::TransitionOnEdge { key: key(0, 1), edge: "end" })
    );
    // Asking for the edge where it already is changes nothing.
    assert_eq!(trim_start(&base, key(0, 1), t(30_000)).unwrap(), base);
    assert_eq!(trim_end(&base, key(0, 1), t(60_000)).unwrap(), base);
}

#[test]
fn a_trim_that_leaves_a_transition_too_little_is_refused() {
    // The transition needs 5000 ticks of b, which a trim to 4000 ticks does not leave.
    let base = project(
        3000,
        vec![track(
            vec![clip("a", 0, 0, 30_000), clip("b", 30_000, 0, 30_000)],
            vec![transition(0, 10_000)],
        )],
    );
    let result = trim_end(&base, key(0, 1), t(34_000));
    assert!(
        matches!(&result, Err(EditError::Invalid(e)) if matches!(e[0], ValidationError::TransitionTooLong { .. })),
        "{result:?}"
    );
}

#[test]
fn a_trim_below_the_fades_is_refused() {
    let mut base = project(3000, vec![track(vec![clip("a", 0, 0, 30_000)], vec![])]);
    base.tracks[0].clips[0].fade_in = t(20_000);
    let result = trim_end(&base, key(0, 0), t(10_000));
    assert!(
        matches!(&result, Err(EditError::Invalid(e)) if matches!(e[0], ValidationError::FadesExceedClip { .. })),
        "{result:?}"
    );
}

#[test]
fn split_makes_two_touching_halves() {
    let base = project(3000, vec![track(vec![clip("a", 15_000, 30_000, 60_000)], vec![])]);
    let p = split_clip(&base, key(0, 0), t(45_000)).unwrap();
    let (l, r) = (&p.tracks[0].clips[0], &p.tracks[0].clips[1]);
    assert_eq!((l.start, l.source_in, l.duration), (t(15_000), t(30_000), t(30_000)));
    assert_eq!((r.start, r.source_in, r.duration), (t(45_000), t(60_000), t(30_000)));
    assert_valid(&p);
}

#[test]
fn split_keeps_each_transition_on_the_half_that_touches_it() {
    let p = split_clip(&chain(), key(0, 1), t(45_000)).unwrap();
    assert_eq!(sources(&p, 0), ["a", "b", "b", "c", "d"]);
    assert_eq!(starts(&p, 0), [0, 30_000, 45_000, 60_000, 120_000]);
    assert_eq!(after_clips(&p, 0), [0, 2], "b's outgoing transition moved to its second half");
    assert_valid(&p);
}

#[test]
fn split_before_a_clip_with_a_transition_renumbers_it() {
    let base = project(
        3000,
        vec![track(
            vec![
                clip("a", 0, 0, 30_000),
                clip("b", 30_000, 0, 30_000),
                clip("c", 60_000, 0, 30_000),
            ],
            vec![transition(1, 6000)],
        )],
    );
    let p = split_clip(&base, key(0, 0), t(15_000)).unwrap();
    assert_eq!(after_clips(&p, 0), [2]);
    assert_valid(&p);
}

#[test]
fn split_inside_a_transition_window_is_refused() {
    // Window 27 000..33 000: the incoming half of b ends at 33 000.
    let base = chain();
    assert_eq!(
        split_clip(&base, key(0, 1), t(32_000)),
        Err(EditError::SplitInsideTransition { key: key(0, 1), at: t(32_000) })
    );
    // Clip b's outgoing window starts at 57 000.
    assert_eq!(
        split_clip(&base, key(0, 1), t(58_000)),
        Err(EditError::SplitInsideTransition { key: key(0, 1), at: t(58_000) })
    );
    // On the edge of the window is allowed.
    assert_valid(&split_clip(&base, key(0, 1), t(33_000)).unwrap());
    assert_valid(&split_clip(&base, key(0, 1), t(57_000)).unwrap());
}

#[test]
fn split_outside_the_clip_is_refused() {
    let base = project(3000, vec![track(vec![clip("a", 15_000, 0, 60_000)], vec![])]);
    for at in [15_000, 75_000, 0, 90_000] {
        assert_eq!(
            split_clip(&base, key(0, 0), t(at)),
            Err(EditError::SplitOutsideClip { key: key(0, 0), at: t(at) })
        );
    }
}

#[test]
fn split_divides_the_fades_and_refuses_a_cut_inside_one() {
    let mut base = project(3000, vec![track(vec![clip("a", 0, 0, 60_000)], vec![])]);
    base.tracks[0].clips[0].fade_in = t(15_000);
    base.tracks[0].clips[0].fade_out = t(15_000);
    for at in [14_985, 45_015] {
        assert_eq!(
            split_clip(&base, key(0, 0), t(at)),
            Err(EditError::SplitInsideFade { key: key(0, 0), at: t(at) })
        );
    }
    let p = split_clip(&base, key(0, 0), t(30_000)).unwrap();
    let (l, r) = (&p.tracks[0].clips[0], &p.tracks[0].clips[1]);
    assert_eq!((l.fade_in, l.fade_out), (t(15_000), t(0)));
    assert_eq!((r.fade_in, r.fade_out), (t(0), t(15_000)));
    assert_valid(&p);
}

#[test]
fn a_split_at_a_misaligned_tick_moves_the_cut_to_keep_the_samples() {
    // start and source_in differ modulo 15, so not every tick keeps the audio aligned.
    let base = project(3000, vec![track(vec![clip("a", 0, 7, 90_000)], vec![])]);
    let p = split_clip(&base, key(0, 0), t(1)).unwrap();
    let cut = p.tracks[0].clips[1].start;
    assert!(cut.ticks().abs_diff(1) <= 14, "the cut moved too far: {cut}");
    assert_same_output(&base, &p);
}

/// What a sample of a clip looks like, without the clip's index.
fn sample_identity<'a>(s: &SourceSample<'a>) -> (&'a str, Time, &'a [Effect]) {
    (s.source, s.source_time, s.effects)
}

/// Every frame and audio sample equals across the two projects, comparing what is shown, not the
/// clip indices.
fn assert_same_output(a: &Project, b: &Project) {
    assert_eq!(frame_count(a), frame_count(b));
    for frame in 0..frame_count(a) {
        let describe = |p: &'_ Project| -> Vec<String> {
            layers_at_frame(p, frame)
                .iter()
                .map(|layer| match &layer.content {
                    LayerContent::Clip(s) => format!("{} {:?}", layer.track, sample_identity(s)),
                    LayerContent::Transition { package, params, from, to, progress } => format!(
                        "{} {package} {params:?} {:?} {:?} {progress:?}",
                        layer.track,
                        sample_identity(from),
                        sample_identity(to)
                    ),
                })
                .collect()
        };
        assert_eq!(describe(a), describe(b), "frame {frame}");
    }
    assert_eq!(audio_sample_count(a), audio_sample_count(b));
    for sample in 0..audio_sample_count(a) {
        let describe = |p: &'_ Project| {
            let mut sounds: Vec<(usize, String, i64, u32)> = audio_segments(p, sample, 1)
                .iter()
                .map(|s| {
                    (
                        s.key.track,
                        s.source.to_string(),
                        s.source_start,
                        s.gain.gain_at(sample).to_bits(),
                    )
                })
                .collect();
            sounds.sort();
            sounds
        };
        assert_eq!(describe(a), describe(b), "sample {sample}");
    }
}

#[test]
fn split_keeps_every_frame_and_sample() {
    let mut base = project(
        3000,
        vec![
            track(
                vec![
                    clip("a", 0, 1234, 9000),
                    clip("b", 9000, 7, 9000),
                    clip("c", 18_000, 22, 9000),
                ],
                vec![transition(0, 1000), transition(1, 1000)],
            ),
            track(vec![clip("e", 300, 11, 8000)], vec![]),
        ],
    );
    base.tracks[1].clips[0].fade_in = t(900);
    base.tracks[1].clips[0].fade_out = t(900);
    base.tracks[1].clips[0].gain_db = -6.0;
    base.tracks[0].clips[1].effects.push(effect("blur"));

    let mut tried = 0;
    for at in (9000..18_000).step_by(89) {
        match split_clip(&base, key(0, 1), t(at)) {
            Ok(p) => {
                assert_valid(&p);
                assert_same_output(&base, &p);
                tried += 1;
            }
            Err(EditError::SplitInsideTransition { .. } | EditError::SplitOutsideClip { .. }) => {}
            Err(other) => panic!("unexpected {other:?}"),
        }
    }
    assert!(tried > 80, "only {tried} splits ran");

    // The other clip, with fades, at ticks of every rounding.
    for at in 1200..1215 {
        let p = split_clip(&base, key(1, 0), t(at)).unwrap();
        assert_same_output(&base, &p);
    }
}

#[test]
fn delete_removes_the_touching_transitions_and_renumbers_the_rest() {
    let p = delete_clip(&chain(), key(0, 1)).unwrap();
    assert_eq!(sources(&p, 0), ["a", "c", "d"]);
    assert_eq!(p.tracks[0].transitions.len(), 0);
    assert_valid(&p);

    let base = project(
        3000,
        vec![track(
            vec![
                clip("a", 0, 0, 30_000),
                clip("b", 40_000, 0, 30_000),
                clip("c", 70_000, 0, 30_000),
            ],
            vec![transition(1, 6000)],
        )],
    );
    let p = delete_clip(&base, key(0, 0)).unwrap();
    assert_eq!(after_clips(&p, 0), [0]);
    assert_valid(&p);

    assert_eq!(
        delete_clip(&base, key(0, 7)),
        Err(EditError::ClipNotFound { key: key(0, 7) })
    );
}

#[test]
fn effects_are_added_and_removed() {
    let p = add_effect(&chain(), key(1, 0), effect("blur")).unwrap();
    let p = add_effect(&p, key(1, 0), effect("mix")).unwrap();
    assert_eq!(p.tracks[1].clips[0].effects, vec![effect("blur"), effect("mix")]);
    let p = remove_effect(&p, key(1, 0), 0).unwrap();
    assert_eq!(p.tracks[1].clips[0].effects, vec![effect("mix")]);
    assert_eq!(
        remove_effect(&p, key(1, 0), 1),
        Err(EditError::EffectNotFound { key: key(1, 0), effect: 1 })
    );
}

#[test]
fn an_effect_on_an_audio_clip_is_refused() {
    let result = add_effect(&chain(), key(2, 0), effect("blur"));
    assert!(
        matches!(&result, Err(EditError::Invalid(e)) if matches!(e[0], ValidationError::AudioTrackEffect { .. })),
        "{result:?}"
    );
}

#[test]
fn transitions_are_added_and_removed() {
    let base = project(
        3000,
        vec![track(vec![clip("a", 0, 0, 30_000), clip("b", 30_000, 0, 30_000)], vec![])],
    );
    let p = add_transition(&base, 0, transition(0, 6000)).unwrap();
    assert_eq!(after_clips(&p, 0), [0]);
    let p = remove_transition(&p, 0, 0).unwrap();
    assert_eq!(p, base);
    assert_eq!(
        remove_transition(&p, 0, 0),
        Err(EditError::TransitionNotFound { track: 0, after_clip: 0 })
    );
}

#[test]
fn a_transition_that_breaks_a_rule_is_refused() {
    let separated = project(
        3000,
        vec![track(vec![clip("a", 0, 0, 30_000), clip("b", 40_000, 0, 30_000)], vec![])],
    );
    let result = add_transition(&separated, 0, transition(0, 6000));
    assert!(
        matches!(&result, Err(EditError::Invalid(e)) if matches!(e[0], ValidationError::TransitionNotAdjacent { .. })),
        "{result:?}"
    );

    let mut faded = project(
        3000,
        vec![track(vec![clip("a", 0, 0, 30_000), clip("b", 30_000, 0, 30_000)], vec![])],
    );
    faded.tracks[0].clips[0].fade_out = t(3000);
    let result = add_transition(&faded, 0, transition(0, 6000));
    assert!(
        matches!(&result, Err(EditError::Invalid(e)) if matches!(e[0], ValidationError::FadeBesideTransition { .. })),
        "{result:?}"
    );
}

#[test]
fn gain_mute_and_fades_are_set_within_the_rules() {
    let base = project(3000, vec![track(vec![clip("a", 0, 0, 30_000)], vec![])]);
    let p = set_gain(&base, key(0, 0), -3.0).unwrap();
    assert_eq!(p.tracks[0].clips[0].gain_db, -3.0);
    let p = set_muted(&p, key(0, 0), true).unwrap();
    assert!(p.tracks[0].clips[0].muted);
    let p = set_fades(&p, key(0, 0), t(3000), t(6000)).unwrap();
    assert_eq!(p.tracks[0].clips[0].fade_in, t(3000));
    assert_eq!(p.tracks[0].clips[0].fade_out, t(6000));
    assert_valid(&p);

    for result in [
        set_gain(&base, key(0, 0), f64::NAN),
        set_gain(&base, key(0, 0), 61.0),
        set_fades(&base, key(0, 0), t(20_000), t(20_000)),
        set_fades(&base, key(0, 0), t(-1), t(0)),
    ] {
        assert!(matches!(result, Err(EditError::Invalid(_))), "{result:?}");
    }
}

#[test]
fn a_fade_beside_a_transition_is_refused() {
    let result = set_fades(&chain(), key(0, 0), t(0), t(3000));
    assert!(
        matches!(&result, Err(EditError::Invalid(e)) if matches!(e[0], ValidationError::FadeBesideTransition { .. })),
        "{result:?}"
    );
}

#[test]
fn clip_at_finds_a_clip_again_after_an_edit() {
    let p = move_clip(&chain(), key(0, 3), 1, t(40_000)).unwrap();
    assert_eq!(clip_at(&p, 1, t(40_000)), Some(key(1, 1)));
    assert_eq!(clip_at(&p, 1, t(40_001)), None);
}

fn sequence() -> Vec<Box<dyn Fn(&Project) -> Result<Project, EditError>>> {
    vec![
        Box::new(|p| add_track(p, TrackKind::Video)),
        Box::new(|p| add_clip(p, 3, clip("x", 0, 0, 30_000))),
        Box::new(|p| split_clip(p, key(0, 1), t(45_000))),
        Box::new(|p| add_effect(p, key(3, 0), effect("blur"))),
        Box::new(|p| move_clip(p, key(0, 4), 3, t(90_000))),
        Box::new(|p| set_gain(p, key(3, 0), -2.0)),
        Box::new(|p| remove_transition(p, 0, 2)),
        Box::new(|p| delete_clip(p, key(0, 3))),
        Box::new(|p| trim_end(p, key(3, 0), t(20_000))),
        Box::new(|p| trim_start(p, key(3, 1), t(100_000))),
    ]
}

#[test]
fn undo_all_returns_to_the_start_and_redo_all_to_the_end() {
    let start = chain();
    let mut history = EditHistory::new(start.clone(), 100);
    let commands = sequence();
    for command in &commands {
        history.apply(command).unwrap();
        assert_valid(history.current());
    }
    let end = history.current().clone();
    assert_ne!(start, end);

    for _ in 0..commands.len() {
        assert!(history.undo());
    }
    assert!(!history.undo(), "more undo steps than commands");
    assert_eq!(history.current(), &start);

    for _ in 0..commands.len() {
        assert!(history.redo());
    }
    assert!(!history.redo(), "more redo steps than commands");
    assert_eq!(history.current(), &end);
}

#[test]
fn a_refused_command_leaves_the_history_alone() {
    let start = chain();
    let mut history = EditHistory::new(start.clone(), 10);
    history.apply(|p| add_track(p, TrackKind::Video)).unwrap();
    let after = history.current().clone();
    history.undo();
    let result = history.apply(|p| delete_clip(p, key(0, 99)));
    assert_eq!(result.err(), Some(EditError::ClipNotFound { key: key(0, 99) }));
    assert_eq!(history.current(), &start);
    assert!(history.can_redo(), "a refused command cleared the redo stack");
    assert!(history.redo());
    assert_eq!(history.current(), &after);
}

#[test]
fn an_edit_that_changes_nothing_adds_no_step() {
    let start = chain();
    let mut history = EditHistory::new(start, 10);
    history.apply(|p| move_clip(p, key(0, 1), 0, t(30_000))).unwrap();
    assert!(!history.can_undo());
}

#[test]
fn a_new_edit_after_undo_clears_redo() {
    let mut history = EditHistory::new(chain(), 10);
    history.apply(|p| add_track(p, TrackKind::Video)).unwrap();
    history.undo();
    history.apply(|p| add_track(p, TrackKind::Audio)).unwrap();
    assert!(!history.can_redo());
    assert_eq!(history.current().tracks[3].kind, TrackKind::Audio);
}

#[test]
fn the_history_keeps_only_the_newest_steps() {
    let start = chain();
    let mut history = EditHistory::new(start, 3);
    for _ in 0..5 {
        history.apply(|p| add_track(p, TrackKind::Video)).unwrap();
    }
    let mut undone = 0;
    while history.undo() {
        undone += 1;
    }
    assert_eq!(undone, 3);
    assert_eq!(history.current().tracks.len(), 3 + 2, "oldest two steps are gone");
}

#[test]
fn a_zero_depth_history_has_no_undo() {
    let mut history = EditHistory::new(chain(), 0);
    history.apply(|p| add_track(p, TrackKind::Video)).unwrap();
    assert!(!history.can_undo());
    assert!(!history.undo());
    assert_eq!(history.current().tracks.len(), 4);
}
