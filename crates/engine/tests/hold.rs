//! Holds: an enter or exit animation with a negative duration adds a still of the first or last
//! source frame to the clip, which ripples the track and excludes a transition on the same cut.

mod common;

use common::*;
use engine::{
    add_transition, add_transition_report, audio_segments, frame_count, layers_at_frame, sample_position, set_animation, set_animation_report, set_speed, split_clip, validate, AcceptAll, AnimationGroup,
    AnimationRef, ClipAnimation, ClipKey, EditError, LayerContent, Project, ReleasedHold, SourceSample, Time, ValidationError,
};

const SECOND: i64 = 90_000;
const HALF: i64 = SECOND / 2;
const FRAME: i64 = 3_000;

fn key(clip: usize) -> ClipKey {
    ClipKey { track: 0, clip }
}

fn fade(duration: i64) -> Option<AnimationRef> {
    Some(AnimationRef { preset: "fade".to_owned(), duration: t(duration) })
}

fn enter(duration: i64) -> ClipAnimation {
    ClipAnimation { enter: fade(duration), ..ClipAnimation::default() }
}

fn exit(duration: i64) -> ClipAnimation {
    ClipAnimation { exit: fade(duration), ..ClipAnimation::default() }
}

/// Two touching clips of one second, `a` from source 1 s and `b` after it, and a third one later.
fn start() -> Project {
    project(FRAME, vec![track(vec![clip("a", 0, SECOND, SECOND), clip("b", SECOND, 0, SECOND), clip("c", 3 * SECOND, 0, SECOND)], vec![])])
}

fn sample(project: &Project, at: i64) -> SourceSample<'_> {
    let layers = layers_at_frame(project, at / FRAME);
    match layers.first().map(|layer| layer.content) {
        Some(LayerContent::Clip(sample)) => sample,
        other => panic!("no single clip at {at}: {other:?}"),
    }
}

fn starts(project: &Project) -> Vec<i64> {
    project.tracks[0].clips.iter().map(|clip| clip.start.ticks()).collect()
}

#[test]
fn a_negative_enter_shows_the_first_frame_before_the_video_and_ripples_the_track() {
    let before = start();
    let held = set_animation(&before, key(0), enter(-HALF)).unwrap();
    let clip = &held.tracks[0].clips[0];
    assert_eq!((clip.duration.ticks(), clip.hold_start().ticks(), clip.media_duration().ticks()), (SECOND + HALF, HALF, SECOND));
    assert_eq!(starts(&held), vec![0, SECOND + HALF, 3 * SECOND + HALF], "every later clip moves by the hold");
    assert_eq!(frame_count(&held), frame_count(&before) + HALF / FRAME);
    // The still: the first source frame for the whole hold, then the video runs from it.
    for at in [0, FRAME, HALF - FRAME] {
        assert_eq!(sample(&held, at).source_time, t(SECOND), "{at}");
    }
    assert_eq!(sample(&held, HALF).source_time, t(SECOND));
    assert_eq!(sample(&held, HALF + FRAME).source_time, t(SECOND + FRAME));
    assert_eq!(sample(&held, SECOND + HALF - FRAME).source_time, t(2 * SECOND - FRAME), "the last frame plays as before");
    // The enter animation plays over the hold: it starts at the first frame and is done at its end.
    let first = sample(&held, 0).transform.opacity;
    let settled = sample(&held, HALF + FRAME).transform.opacity;
    assert!(first < 0.1 && (settled - 1.0).abs() < 1e-9, "{first} {settled}");
}

#[test]
fn a_negative_exit_holds_the_last_frame_after_the_video() {
    let held = set_animation(&start(), key(0), exit(-HALF)).unwrap();
    let clip = &held.tracks[0].clips[0];
    assert_eq!((clip.duration.ticks(), clip.hold_end().ticks()), (SECOND + HALF, HALF));
    assert_eq!(clip.source_end(), t(2 * SECOND), "the source range does not change");
    assert_eq!(starts(&held), vec![0, SECOND + HALF, 3 * SECOND + HALF]);
    assert_eq!(sample(&held, 0).source_time, t(SECOND));
    for at in [SECOND, SECOND + FRAME, SECOND + HALF - FRAME] {
        assert_eq!(sample(&held, at).source_time, t(2 * SECOND - 1), "the last frame, {at}");
    }
    assert_eq!(sample(&held, SECOND + HALF).source, "b");
}

#[test]
fn the_holds_are_silent() {
    let held = set_animation(&start(), key(0), ClipAnimation { enter: fade(-HALF), exit: fade(-HALF), ..ClipAnimation::default() }).unwrap();
    let segments = audio_segments(&held, 0, sample_position(t(2 * SECOND)));
    let a = segments.iter().find(|segment| segment.source == "a").unwrap();
    assert_eq!((a.start, a.len), (sample_position(t(HALF)), sample_position(t(SECOND))));
    assert_eq!(a.source_start, sample_position(t(SECOND)), "the sound starts with the video");
    assert!(audio_segments(&held, 0, sample_position(t(HALF))).is_empty());
}

#[test]
fn changing_and_removing_a_hold_ripples_back() {
    let before = start();
    let held = set_animation(&before, key(0), enter(-HALF)).unwrap();
    let longer = set_animation(&held, key(0), enter(-SECOND)).unwrap();
    assert_eq!(starts(&longer), vec![0, 2 * SECOND, 4 * SECOND]);
    let plain = set_animation(&longer, key(0), enter(HALF)).unwrap();
    assert_eq!(starts(&plain), starts(&before));
    assert_eq!(plain.tracks[0].clips[0].duration, t(SECOND));
    assert_eq!(set_animation(&plain, key(0), ClipAnimation::default()).unwrap(), before);
}

#[test]
fn a_hold_keeps_the_keyframes_on_the_video() {
    let mut before = start();
    before.tracks[0].clips[0].motion.keyframes.insert(engine::ClipProperty::Opacity, vec![engine::Keyframe { at: t(FRAME), value: 0.5, easing: Default::default() }]);
    let held = set_animation(&before, key(0), enter(-HALF)).unwrap();
    assert_eq!(held.tracks[0].clips[0].motion.keyframes[&engine::ClipProperty::Opacity][0].at, t(HALF + FRAME));
}

#[test]
fn a_hold_on_a_cut_with_a_transition_removes_the_transition() {
    let mut before = start();
    before.tracks[0].transitions.push(transition(0, 2 * FRAME));
    let (held, removed) = set_animation_report(&before, key(1), enter(-HALF)).unwrap();
    assert_eq!(removed, vec![transition(0, 2 * FRAME)]);
    assert!(held.tracks[0].transitions.is_empty());
    assert_eq!(held.tracks[0].clips[1].start, t(SECOND), "b keeps its place, its hold is its start");
    // A hold on the other edge keeps it.
    let (kept, removed) = set_animation_report(&before, key(0), enter(-HALF)).unwrap();
    assert!(removed.is_empty());
    assert_eq!(kept.tracks[0].transitions.len(), 1);
}

#[test]
fn a_transition_on_a_held_cut_turns_the_holds_into_plain_animations() {
    let before = start();
    let held = set_animation(&before, key(0), exit(-HALF)).unwrap();
    let held = set_animation(&held, key(1), enter(-HALF)).unwrap();
    assert_eq!(starts(&held), vec![0, SECOND + HALF, 4 * SECOND]);
    let (joined, released) = add_transition_report(&held, 0, transition(0, 2 * FRAME)).unwrap();
    assert_eq!(released, vec![ReleasedHold { key: key(0), group: AnimationGroup::Exit }, ReleasedHold { key: key(1), group: AnimationGroup::Enter }]);
    assert_eq!(starts(&joined), starts(&before));
    let clips = &joined.tracks[0].clips;
    assert_eq!((clips[0].animation.exit.as_ref().unwrap().duration, clips[1].animation.enter.as_ref().unwrap().duration), (t(HALF), t(HALF)));
    assert_eq!(joined.tracks[0].transitions.len(), 1);
    assert_eq!(add_transition(&held, 0, transition(0, 2 * FRAME)).unwrap(), joined);
}

#[test]
fn validation_refuses_a_hold_beside_a_transition_and_holds_that_fill_the_clip() {
    let mut both = set_animation(&start(), key(1), enter(-HALF)).unwrap();
    both.tracks[0].transitions.push(transition(0, 2 * FRAME));
    assert!(validate(&both, &AcceptAll).iter().any(|e| matches!(e, ValidationError::HoldBesideTransition { clip: 1, edge: "start", .. })));

    let mut full = start();
    full.tracks[0].clips[0].animation = ClipAnimation { enter: fade(-HALF), exit: fade(-HALF), ..ClipAnimation::default() };
    assert!(validate(&full, &AcceptAll).iter().any(|e| matches!(e, ValidationError::HoldsFillClip { clip: 0, .. })));

    let mut repeat = start();
    repeat.tracks[0].clips[0].animation = ClipAnimation { repeat: Some(AnimationRef { preset: "pulse".to_owned(), duration: t(-HALF) }), ..ClipAnimation::default() };
    assert!(validate(&repeat, &AcceptAll).iter().any(|e| matches!(e, ValidationError::InvalidAnimationDuration { group: "loop", .. })));

    let mut zero = start();
    zero.tracks[0].clips[0].animation = enter(0);
    assert!(validate(&zero, &AcceptAll).iter().any(|e| matches!(e, ValidationError::InvalidAnimationDuration { .. })));
}

#[test]
fn a_split_keeps_each_hold_with_its_half_and_refuses_a_cut_inside_one() {
    let held = set_animation(&start(), key(0), ClipAnimation { enter: fade(-HALF), exit: fade(-HALF), ..ClipAnimation::default() }).unwrap();
    assert!(matches!(split_clip(&held, key(0), t(HALF / 2)), Err(EditError::SplitInsideHold { .. })));
    assert!(matches!(split_clip(&held, key(0), t(SECOND + HALF + HALF / 2)), Err(EditError::SplitInsideHold { .. })));
    let cut = split_clip(&held, key(0), t(SECOND)).unwrap();
    let (first, second) = (&cut.tracks[0].clips[0], &cut.tracks[0].clips[1]);
    assert_eq!((first.hold_start(), first.hold_end(), first.duration), (t(HALF), Time::ZERO, t(SECOND)));
    assert_eq!((second.hold_start(), second.hold_end(), second.duration), (Time::ZERO, t(HALF), t(SECOND)));
    assert_eq!(second.source_in, t(SECOND + HALF), "the second half goes on from where the first one is");
    for at in (0..2 * SECOND).step_by(FRAME as usize) {
        assert_eq!(sample(&cut, at).source_time, sample(&held, at).source_time, "{at}");
    }
}

#[test]
fn a_speed_change_rescales_only_the_video() {
    let held = set_animation(&start(), key(0), enter(-HALF)).unwrap();
    let mut alone = held.clone();
    alone.tracks[0].clips.truncate(1);
    let fast = set_speed(&alone, key(0), 2.0).unwrap();
    let clip = &fast.tracks[0].clips[0];
    assert_eq!((clip.duration, clip.hold_start(), clip.source_end()), (t(HALF + HALF), t(HALF), t(2 * SECOND)));
}

#[test]
fn a_speed_change_keeps_the_keyframes_on_their_frame_beside_the_holds() {
    let mut alone = start();
    alone.tracks[0].clips.truncate(1);
    let held = set_animation(&alone, key(0), ClipAnimation { enter: fade(-HALF), exit: fade(-HALF), ..ClipAnimation::default() }).unwrap();
    // One keyframe a quarter second into the video, one a frame into the end hold.
    let opacity = |at: i64| engine::Keyframe { at: t(at), value: 0.5, easing: Default::default() };
    let mut keyed = held.clone();
    keyed.tracks[0].clips[0].motion.keyframes.insert(engine::ClipProperty::Opacity, vec![opacity(HALF + SECOND / 4), opacity(HALF + SECOND + FRAME)]);
    let fast = set_speed(&keyed, key(0), 2.0).unwrap();
    let times: Vec<i64> = fast.tracks[0].clips[0].motion.keyframes[&engine::ClipProperty::Opacity].iter().map(|key| key.at.ticks()).collect();
    assert_eq!(times, vec![HALF + SECOND / 8, HALF + HALF + FRAME], "the video keyframe scales after the start hold, the hold keyframe moves with the end");
}

#[test]
fn an_image_hold_just_lasts_longer() {
    let mut before = start();
    before.tracks[0].clips[0] = clip("a.png", 0, 0, SECOND);
    let held = set_animation(&before, key(0), enter(-HALF)).unwrap();
    assert_eq!(held.tracks[0].clips[0].duration, t(SECOND + HALF));
    assert_eq!(sample(&held, 0).source_time, Time::ZERO);
    assert_eq!(sample(&held, HALF - FRAME).source_time, Time::ZERO);
    assert_eq!(sample(&held, SECOND + HALF - FRAME).source, "a.png");
}

#[test]
fn a_negative_duration_round_trips_in_the_file() {
    let held = set_animation(&start(), key(0), enter(-HALF)).unwrap();
    let text = serde_json::to_string(&held).unwrap();
    assert!(text.contains(&format!("\"duration\":{}", -HALF)), "{text}");
    assert_eq!(Project::from_json(&text).unwrap(), held);
}
