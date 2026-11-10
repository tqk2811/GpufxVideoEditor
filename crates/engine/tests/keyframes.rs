//! Opacity, rotation and keyframes through the file format, validation and the edit commands.

mod common;

use common::*;
use engine::{
    add_effect, keyframe_neighbors, layers_at_frame, nearest_keyframe, nearest_keyframe_inside, remove_keyframe, PlacedRect, sample_keyframes, set_effect_params_at, set_keyframe, set_motion_value, set_opacity, set_placement, set_placement_at, set_rotation, set_speed, split_clip, trim_end, trim_source_in, trim_start, validate, AcceptAll, Clip, ClipKey, ClipPlacement, ClipProperty, Easing, EditError, EditHistory, Keyframe, KeyframeTarget, LayerContent, Project, Time,
    ValidationError, MAX_ROTATION,
};
use serde_json::{json, Map};

fn key(clip: usize) -> ClipKey {
    ClipKey { track: 0, clip }
}

const OPACITY: KeyframeTarget = KeyframeTarget::Property(ClipProperty::Opacity);
const ROTATION: KeyframeTarget = KeyframeTarget::Property(ClipProperty::Rotation);

fn one_clip(duration: i64) -> Project {
    project(3_000, vec![track(vec![clip("a.mp4", 0, 0, duration)], vec![])])
}

fn clip_of(project: &Project, index: usize) -> &Clip {
    &project.tracks[0].clips[index]
}

/// A clip with an eased curve on its opacity, across its whole length of 9000 ticks.
fn animated() -> Project {
    let base = one_clip(9_000);
    let base = set_keyframe(&base, key(0), &OPACITY, t(0), 0.0, Some(Easing::EaseInOut)).unwrap();
    let base = set_keyframe(&base, key(0), &OPACITY, t(3_000), 0.5, Some(Easing::EaseIn)).unwrap();
    set_keyframe(&base, key(0), &OPACITY, t(8_999), 1.0, None).unwrap()
}

fn sampled_opacity(clip: &Clip, timeline: i64) -> f64 {
    clip.transform_at(t(timeline - clip.start.ticks())).opacity
}

#[test]
fn a_project_without_motion_saves_as_it_did() {
    let saved = serde_json::to_value(one_clip(3_000)).unwrap();
    let saved_clip = &saved["tracks"][0]["clips"][0];
    assert!(saved_clip.get("motion").is_none(), "{saved_clip}");
    assert!(saved_clip.get("effects").is_some());
    let effect_saved = serde_json::to_value(effect("blur")).unwrap();
    assert!(effect_saved.get("keyframes").is_none(), "{effect_saved}");
}

#[test]
fn a_file_of_an_older_version_loads_with_no_motion() {
    let text = r#"{"version":2,"output":{"width":16,"height":16,"frame_duration":100},"tracks":[{"clips":[{"source":"a","start":0,"source_in":0,"duration":200,"placement":{"x":0.25,"y":0.5,"width":0.5}}]}]}"#;
    let loaded = Project::from_json(text).unwrap();
    assert_eq!(clip_of(&loaded, 0).motion, Default::default());
    let at_zero = clip_of(&loaded, 0).transform_at(t(0));
    assert_eq!((at_zero.opacity, at_zero.rotation), (1.0, 0.0));
    assert_eq!(at_zero.placement, clip_of(&loaded, 0).placement);
}

#[test]
fn a_version_one_file_ignores_motion_and_effect_keyframes_as_it_always_did() {
    let text = r#"{"version":1,"output":{"width":16,"height":16,"frame_duration":100},"tracks":[{"clips":[{"source":"a","start":0,"source_in":0,"duration":200,"motion":{"opacity":0.5},"effects":[{"package":"blur","params":{},"keyframes":{"radius":[{"at":0,"value":1}]}}]}]}]}"#;
    let loaded = Project::from_json(text).unwrap();
    assert_eq!(clip_of(&loaded, 0).motion, Default::default());
    assert!(clip_of(&loaded, 0).effects[0].keyframes.is_empty());
}

#[test]
fn motion_survives_a_save_and_a_load() {
    let mut original = animated();
    original.version = engine::SUPPORTED_VERSION;
    let text = serde_json::to_string(&original).unwrap();
    assert_eq!(Project::from_json(&text).unwrap(), original);
    assert!(text.contains(r#""easing":"ease_in_out""#), "{text}");
}

#[test]
fn opacity_and_rotation_outside_their_limits_are_refused() {
    let base = one_clip(3_000);
    assert_eq!(clip_of(&set_opacity(&base, key(0), 0.25).unwrap(), 0).motion.opacity, 0.25);
    assert_eq!(clip_of(&set_rotation(&base, key(0), -90.0).unwrap(), 0).motion.rotation, -90.0);
    for bad in [-0.1, 1.5, f64::NAN, f64::INFINITY] {
        assert!(matches!(set_opacity(&base, key(0), bad), Err(EditError::Invalid(_))), "{bad}");
    }
    for bad in [MAX_ROTATION + 1.0, -MAX_ROTATION - 1.0, f64::NAN] {
        assert!(matches!(set_rotation(&base, key(0), bad), Err(EditError::Invalid(_))), "{bad}");
    }
}

#[test]
fn validation_names_a_bad_keyframe_list_and_its_reason() {
    let mut bad = one_clip(3_000);
    bad.tracks[0].clips[0].motion.keyframes.insert(ClipProperty::Width, vec![Keyframe { at: t(0), value: 0.5, easing: Easing::Linear }, Keyframe { at: t(0), value: 0.6, easing: Easing::Linear }]);
    let errors = validate(&bad, &AcceptAll);
    assert!(matches!(&errors[..], [ValidationError::InvalidKeyframes { what, .. }] if what == "width"), "{errors:?}");

    let mut bad = one_clip(3_000);
    bad.tracks[0].clips[0].motion.keyframes.insert(ClipProperty::Width, vec![Keyframe { at: t(0), value: 99.0, easing: Easing::Linear }]);
    assert!(matches!(&validate(&bad, &AcceptAll)[..], [ValidationError::InvalidKeyframes { .. }]));

    let mut bad = one_clip(3_000);
    bad.tracks[0].clips[0].motion.keyframes.insert(ClipProperty::Opacity, vec![Keyframe { at: t(0), value: f64::NAN, easing: Easing::Linear }]);
    assert!(matches!(&validate(&bad, &AcceptAll)[..], [ValidationError::InvalidKeyframes { .. }]));
}

#[test]
fn an_audio_clip_cannot_have_motion() {
    let mut audio = project(3_000, vec![audio_track(vec![clip("a.wav", 0, 0, 3_000)])]);
    audio.tracks[0].clips[0].motion.opacity = 0.5;
    assert!(validate(&audio, &AcceptAll).iter().any(|error| matches!(error, ValidationError::AudioTrackMotion { .. })));
}

#[test]
fn keyframes_can_only_be_set_inside_the_clip_with_a_valid_value() {
    let base = one_clip(3_000);
    assert!(matches!(set_keyframe(&base, key(0), &OPACITY, t(-1), 0.5, None), Err(EditError::KeyframeOutsideClip { .. })));
    assert!(matches!(set_keyframe(&base, key(0), &OPACITY, t(3_000), 0.5, None), Err(EditError::KeyframeOutsideClip { .. })), "no frame is at the end");
    assert!(set_keyframe(&base, key(0), &OPACITY, t(2_999), 0.5, None).is_ok(), "the last tick");
    assert!(matches!(set_keyframe(&base, key(0), &OPACITY, t(0), 2.0, None), Err(EditError::Invalid(_))));
    assert!(matches!(set_keyframe(&base, key(0), &KeyframeTarget::EffectParam { effect: 3, param: "a".into() }, t(0), 2.0, None), Err(EditError::EffectNotFound { .. })));
}

#[test]
fn keyframes_of_a_locked_track_are_refused() {
    let mut base = one_clip(3_000);
    base.tracks[0].locked = true;
    assert!(matches!(set_keyframe(&base, key(0), &OPACITY, t(0), 0.5, None), Err(EditError::TrackLocked { .. })));
}

#[test]
fn a_removed_keyframe_leaves_the_static_value_again() {
    let base = set_opacity(&one_clip(3_000), key(0), 0.8).unwrap();
    let keyed = set_keyframe(&base, key(0), &OPACITY, t(0), 0.25, None).unwrap();
    assert_eq!(sampled_opacity(clip_of(&keyed, 0), 100), 0.25);
    let removed = remove_keyframe(&keyed, key(0), &OPACITY, t(0)).unwrap();
    assert_eq!(sampled_opacity(clip_of(&removed, 0), 100), 0.8);
    assert_eq!(removed, base, "no empty list stays behind");
    assert!(matches!(remove_keyframe(&keyed, key(0), &OPACITY, t(7)), Err(EditError::KeyframeNotFound { .. })));
}

#[test]
fn replacing_a_keyframe_keeps_its_easing_unless_told_otherwise() {
    let base = set_keyframe(&one_clip(3_000), key(0), &OPACITY, t(0), 0.25, Some(Easing::EaseOut)).unwrap();
    let replaced = set_keyframe(&base, key(0), &OPACITY, t(0), 0.5, None).unwrap();
    assert_eq!(clip_of(&replaced, 0).motion.keyframes[&ClipProperty::Opacity], vec![Keyframe { at: t(0), value: 0.5, easing: Easing::EaseOut }]);
}

#[test]
fn a_start_trim_keeps_the_curve_where_it_was_on_the_timeline() {
    let before = animated();
    let after = trim_start(&before, key(0), t(1_500)).unwrap();
    let trimmed = clip_of(&after, 0);
    for timeline in (1_500..9_000).step_by(250) {
        assert_eq!(sampled_opacity(trimmed, timeline).to_bits(), sampled_opacity(clip_of(&before, 0), timeline).to_bits(), "{timeline}");
    }
    assert_eq!(trimmed.motion.keyframes[&ClipProperty::Opacity][0].at, t(-1_500), "a keyframe cut away stays, before the clip");
}

#[test]
fn an_end_trim_and_a_trim_back_change_nothing_about_the_curve() {
    let before = animated();
    let cut = trim_end(&before, key(0), t(4_000)).unwrap();
    assert_eq!(clip_of(&cut, 0).motion, clip_of(&before, 0).motion, "the keyframe past the end stays");
    let grown = trim_end(&cut, key(0), t(9_000)).unwrap();
    assert_eq!(sampled_opacity(clip_of(&grown, 0), 9_000), 1.0, "and comes back when the clip grows");
}

#[test]
fn a_source_trim_keeps_each_keyframe_on_its_source_frame() {
    let before = animated();
    let after = trim_source_in(&before, key(0), t(1_500)).unwrap();
    let trimmed = clip_of(&after, 0);
    assert_eq!(trimmed.motion.keyframes[&ClipProperty::Opacity][1].at, t(1_500), "from 3000 by what was cut");
    assert_eq!(trimmed.transform_at(t(1_500)).opacity, 0.5, "the same value on the same source frame");
}

#[test]
fn both_halves_of_a_split_play_exactly_the_same_curve() {
    let before = animated();
    let after = split_clip(&before, key(0), t(4_000)).unwrap();
    let (first, second) = (clip_of(&after, 0), clip_of(&after, 1));
    let cut = second.start.ticks();
    for timeline in (0..cut).step_by(250) {
        assert_eq!(sampled_opacity(first, timeline).to_bits(), sampled_opacity(clip_of(&before, 0), timeline).to_bits(), "first half at {timeline}");
    }
    for timeline in (cut..9_000).step_by(250) {
        assert_eq!(sampled_opacity(second, timeline).to_bits(), sampled_opacity(clip_of(&before, 0), timeline).to_bits(), "second half at {timeline}");
    }
}

#[test]
fn a_speed_change_keeps_a_keyframe_on_its_source_frame() {
    let before = animated();
    let doubled = set_speed(&before, key(0), 2.0).unwrap();
    let keys = &clip_of(&doubled, 0).motion.keyframes[&ClipProperty::Opacity];
    assert_eq!(keys.iter().map(|keyframe| keyframe.at).collect::<Vec<_>>(), vec![t(0), t(1_500), t(4_500)]);
    assert_eq!(clip_of(&doubled, 0).duration, t(4_500));
}

#[test]
fn keyframes_that_a_speed_change_lands_on_one_tick_become_one() {
    let mut base = one_clip(3_000);
    base.tracks[0].clips[0].speed = 1.0;
    base = set_keyframe(&base, key(0), &OPACITY, t(1_000), 0.2, None).unwrap();
    base = set_keyframe(&base, key(0), &OPACITY, t(1_001), 0.9, None).unwrap();
    let fast = set_speed(&base, key(0), 10.0).unwrap();
    let keys = &clip_of(&fast, 0).motion.keyframes[&ClipProperty::Opacity];
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert_eq!(keys[0].value, 0.9, "the later one wins");
}

#[test]
fn undoing_a_keyframe_takes_one_step() {
    let mut history = EditHistory::new(one_clip(3_000), 10);
    history.apply(|project| set_keyframe(project, key(0), &ROTATION, t(0), 45.0, None)).unwrap();
    assert!(clip_of(history.current(), 0).motion.has_keyframes(ClipProperty::Rotation));
    assert!(history.undo());
    assert!(!clip_of(history.current(), 0).motion.has_keyframes(ClipProperty::Rotation));
    assert!(!history.can_undo());
}

#[test]
fn a_motion_value_is_static_without_keyframes_and_a_keyframe_with_them() {
    let base = one_clip(3_000);
    let static_value = set_motion_value(&base, key(0), ClipProperty::Rotation, t(100), 30.0, 1.0, 1.0).unwrap();
    assert_eq!(clip_of(&static_value, 0).motion.rotation, 30.0);
    assert!(clip_of(&static_value, 0).motion.keyframes.is_empty(), "no keyframe was asked for");
    let keyed = set_keyframe(&base, key(0), &ROTATION, t(0), 10.0, None).unwrap();
    let edited = set_motion_value(&keyed, key(0), ClipProperty::Rotation, t(2_000), 30.0, 1.0, 1.0).unwrap();
    let keys = &clip_of(&edited, 0).motion.keyframes[&ClipProperty::Rotation];
    assert_eq!(keys.len(), 2);
    assert_eq!((keys[1].at, keys[1].value), (t(2_000), 30.0));
    assert_eq!(clip_of(&edited, 0).motion.rotation, 0.0, "the static value is left alone");
}

#[test]
fn a_number_of_the_place_needs_a_place_unless_it_has_keyframes() {
    let base = one_clip(3_000);
    assert!(matches!(set_motion_value(&base, key(0), ClipProperty::X, t(0), 0.5, 1.0, 1.0), Err(EditError::NoPlacement { .. })));
    let keyed = set_keyframe(&base, key(0), &KeyframeTarget::Property(ClipProperty::X), t(0), 0.25, None).unwrap();
    let edited = set_motion_value(&keyed, key(0), ClipProperty::X, t(0), 0.5, 1.0, 1.0).unwrap();
    assert_eq!(clip_of(&edited, 0).motion.keyframes[&ClipProperty::X][0].value, 0.5);
}

#[test]
fn a_drag_on_the_player_sets_keyframes_only_on_keyed_numbers() {
    let placed = set_placement(&one_clip(3_000), key(0), Some(ClipPlacement { x: 0.1, y: 0.2, width: 0.5 })).unwrap();
    let keyed = set_keyframe(&placed, key(0), &KeyframeTarget::Property(ClipProperty::X), t(0), 0.1, None).unwrap();
    let dragged = set_placement_at(&keyed, key(0), t(2_000), ClipPlacement { x: 0.4, y: 0.6, width: 0.7 }, 1.0, 1.0).unwrap();
    let clip = clip_of(&dragged, 0);
    assert_eq!(clip.placement, Some(ClipPlacement { x: 0.1, y: 0.6, width: 0.7 }), "x only moved through its keyframes");
    assert_eq!(clip.motion.keyframes[&ClipProperty::X].iter().map(|keyframe| (keyframe.at, keyframe.value)).collect::<Vec<_>>(), vec![(t(0), 0.1), (t(2_000), 0.4)]);
}

#[test]
fn going_back_to_the_fit_drops_the_keyframes_of_the_place_only() {
    let placed = set_placement(&one_clip(3_000), key(0), Some(ClipPlacement { x: 0.1, y: 0.2, width: 0.5 })).unwrap();
    let keyed = set_keyframe(&placed, key(0), &KeyframeTarget::Property(ClipProperty::X), t(0), 0.1, None).unwrap();
    let keyed = set_keyframe(&keyed, key(0), &ROTATION, t(0), 5.0, None).unwrap();
    let reset = set_placement(&keyed, key(0), None).unwrap();
    let motion = &clip_of(&reset, 0).motion;
    assert!(!motion.has_keyframes(ClipProperty::X));
    assert!(motion.has_keyframes(ClipProperty::Rotation));
}

#[test]
fn effect_parameters_shown_at_the_playhead_go_to_keyframes_only_when_keyed() {
    let mut base = add_effect(&one_clip(3_000), key(0), effect("blur")).unwrap();
    base.tracks[0].clips[0].effects[0].params.insert("kept".into(), json!(1.0));
    base.tracks[0].clips[0].effects[0].params.insert("radius".into(), json!(2.0));
    let param = KeyframeTarget::EffectParam { effect: 0, param: "radius".into() };
    let keyed = set_keyframe(&base, key(0), &param, t(0), 4.0, None).unwrap();

    let mut shown = Map::new();
    shown.insert("kept".into(), json!(9.0));
    shown.insert("radius".into(), json!(8.0));
    let edited = set_effect_params_at(&keyed, key(0), 0, shown, t(600)).unwrap();
    let keys = &clip_of(&edited, 0).effects[0].keyframes["radius"];
    assert_eq!(sample_keyframes(keys, t(600)), Some(8.0));
    assert_eq!(clip_of(&edited, 0).effects[0].params["radius"], json!(2.0), "the static value is left alone");
    assert_eq!(clip_of(&edited, 0).effects[0].params["kept"], json!(9.0), "a number without keyframes is just set");
}

#[test]
fn a_schedule_gives_each_layer_its_value_of_that_frame() {
    let mut scheduled = one_clip(9_000);
    scheduled.tracks[0].clips[0].motion.keyframes.insert(ClipProperty::Opacity, vec![Keyframe { at: t(0), value: 0.0, easing: Easing::Linear }, Keyframe { at: t(9_000), value: 1.0, easing: Easing::Linear }]);
    let layer = layers_at_frame(&scheduled, 1);
    let LayerContent::Clip(sample) = layer[0].content else { panic!("a clip") };
    assert_eq!(sample.offset, t(3_000));
    assert_eq!(sample.transform.opacity, 3_000.0 / 9_000.0);
    assert_eq!(sample.transform, clip_of(&scheduled, 0).transform_at(Time::from_ticks(3_000)));
}

#[test]
fn inside_a_transition_a_clip_holds_its_first_and_last_values() {
    let first = clip("a.mp4", 0, 0, 6_000);
    let mut second = clip("b.mp4", 6_000, 0, 6_000);
    second.motion.keyframes.insert(ClipProperty::Opacity, vec![Keyframe { at: t(0), value: 0.25, easing: Easing::Linear }]);
    let scheduled = project(3_000, vec![track(vec![first, second], vec![transition(0, 3_000)])]);
    // The window is [4500, 7500): frame 1 at 3000 is before it, frame 2 at 6000 is inside it.
    let layer = layers_at_frame(&scheduled, 2);
    let LayerContent::Transition { to, .. } = layer[0].content else { panic!("a transition") };
    assert_eq!(to.offset, t(0));
    assert_eq!(to.transform.opacity, 0.25);
    let before = scheduled.tracks[0].clips[1].transform_at(t(-1_500));
    assert_eq!(before.opacity, 0.25, "ahead of its first keyframe the clip holds it");
}

const WIDTH: KeyframeTarget = KeyframeTarget::Property(ClipProperty::Width);

fn centre(rect: PlacedRect) -> (f64, f64) {
    (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

fn assert_close(left: f64, right: f64, what: &str) {
    assert!((left - right).abs() < 1e-9, "{what}: {left} against {right}");
}

#[test]
fn a_keyframe_outside_the_clip_can_be_removed_and_the_last_one_inside_takes_the_rest() {
    let mut base = one_clip(9_000);
    base.tracks[0].clips[0].motion.keyframes.insert(ClipProperty::Opacity, vec![Keyframe { at: t(0), value: 0.2, easing: Easing::Linear }, Keyframe { at: t(9_000), value: 0.9, easing: Easing::Linear }]);
    let split = split_clip(&base, key(0), t(4_000)).unwrap();
    let first = clip_of(&split, 0);
    assert_eq!(first.motion.keyframes[&ClipProperty::Opacity].len(), 2, "the one past the end stays after a split");
    let removed = remove_keyframe(&split, key(0), &OPACITY, t(0)).unwrap();
    assert!(clip_of(&removed, 0).motion.keyframes.is_empty(), "nothing inside is left, so the static value applies again");
    assert_eq!(clip_of(&removed, 0).transform_at(t(100)).opacity, 1.0);
    let outside_only = remove_keyframe(&split, key(0), &OPACITY, t(9_000)).unwrap();
    assert_eq!(clip_of(&outside_only, 0).motion.keyframes[&ClipProperty::Opacity].len(), 1, "one inside is left");
    assert!(matches!(remove_keyframe(&split, key(0), &OPACITY, t(8_000)), Err(EditError::KeyframeNotFound { .. })));
}

#[test]
fn a_keyframe_exactly_at_the_end_is_outside_the_clip() {
    let base = one_clip(3_000);
    assert!(matches!(set_keyframe(&base, key(0), &OPACITY, t(3_000), 0.5, None), Err(EditError::KeyframeOutsideClip { .. })));
}

#[test]
fn a_value_set_near_a_keyframe_changes_that_keyframe_and_makes_no_second() {
    let base = set_keyframe(&one_clip(9_000), key(0), &OPACITY, t(3_003), 0.5, None).unwrap();
    // The frame is 3000 ticks, so the playhead is on the keyframe within 1500.
    let edited = set_motion_value(&base, key(0), ClipProperty::Opacity, t(3_004), 0.25, 1.0, 1.0).unwrap();
    let keys = &clip_of(&edited, 0).motion.keyframes[&ClipProperty::Opacity];
    assert_eq!(keys.iter().map(|keyframe| (keyframe.at, keyframe.value)).collect::<Vec<_>>(), vec![(t(3_003), 0.25)]);
    let far = set_motion_value(&base, key(0), ClipProperty::Opacity, t(6_000), 0.25, 1.0, 1.0).unwrap();
    assert_eq!(clip_of(&far, 0).motion.keyframes[&ClipProperty::Opacity].len(), 2, "beyond half a frame it is a new one");

    let mut with_effect = add_effect(&one_clip(9_000), key(0), effect("blur")).unwrap();
    let param = KeyframeTarget::EffectParam { effect: 0, param: "radius".into() };
    with_effect = set_keyframe(&with_effect, key(0), &param, t(3_003), 4.0, None).unwrap();
    let mut shown = Map::new();
    shown.insert("radius".into(), json!(8.0));
    let edited = set_effect_params_at(&with_effect, key(0), 0, shown, t(3_004)).unwrap();
    let keys = &clip_of(&edited, 0).effects[0].keyframes["radius"];
    assert_eq!(keys.iter().map(|keyframe| (keyframe.at, keyframe.value)).collect::<Vec<_>>(), vec![(t(3_003), 8.0)]);

    let placed = set_placement(&one_clip(9_000), key(0), Some(ClipPlacement { x: 0.1, y: 0.2, width: 0.5 })).unwrap();
    let keyed = set_keyframe(&placed, key(0), &KeyframeTarget::Property(ClipProperty::X), t(3_003), 0.1, None).unwrap();
    let dragged = set_placement_at(&keyed, key(0), t(3_001), ClipPlacement { x: 0.4, y: 0.2, width: 0.5 }, 1.0, 1.0).unwrap();
    assert_eq!(clip_of(&dragged, 0).motion.keyframes[&ClipProperty::X].iter().map(|keyframe| (keyframe.at, keyframe.value)).collect::<Vec<_>>(), vec![(t(3_003), 0.4)]);
}

#[test]
fn the_nearest_keyframe_within_the_tolerance_is_found_at_its_own_tick() {
    let keys = [Keyframe { at: t(0), value: 0.0, easing: Easing::Linear }, Keyframe { at: t(3_003), value: 1.0, easing: Easing::Linear }];
    assert_eq!(nearest_keyframe(&keys, t(3_004), t(1_500)), Some(t(3_003)));
    assert_eq!(nearest_keyframe(&keys, t(1_500), t(1_500)), Some(t(0)), "on the edge of the tolerance it still counts");
    assert_eq!(nearest_keyframe(&keys, t(1_600), t(1_500)), Some(t(3_003)));
    assert_eq!(nearest_keyframe(&keys, t(-2_000), t(1_500)), None);
    assert_eq!(keyframe_neighbors(&keys, t(3_004), t(1_500), t(9_000)), (Some(t(0)), None), "on a keyframe, it is neither neighbour");
    assert_eq!(keyframe_neighbors(&keys, t(2_000), t(10), t(9_000)), (Some(t(0)), Some(t(3_003))));
    assert_eq!(keyframe_neighbors(&keys, t(-5_000), t(10), t(3_000)), (None, Some(t(0))), "ahead of the clip only the next one is there, and the one past the end never counts");
}

#[test]
fn a_width_with_keyframes_alone_grows_the_picture_around_its_middle() {
    let placed = set_placement(&one_clip(9_000), key(0), Some(ClipPlacement { x: 0.25, y: 0.25, width: 0.5 })).unwrap();
    let keyed = set_keyframe(&placed, key(0), &WIDTH, t(0), 0.5, None).unwrap();
    let keyed = set_keyframe(&keyed, key(0), &WIDTH, t(6_000), 1.0, None).unwrap();
    for at in [0, 1_500, 3_000, 6_000, 8_000] {
        let rect = clip_of(&keyed, 0).transform_at(t(at)).rect(1.0, 1.0);
        let (x, y) = centre(rect);
        assert_close(x, 0.5, "x of the middle");
        assert_close(y, 0.5, "y of the middle");
    }
    assert_close(clip_of(&keyed, 0).transform_at(t(6_000)).rect(1.0, 1.0).x, 0.0, "the left edge of the full width");
}

#[test]
fn a_frame_dragged_amid_a_changing_width_stays_where_it_was_dragged() {
    let placed = set_placement(&one_clip(9_000), key(0), Some(ClipPlacement { x: 0.25, y: 0.25, width: 0.5 })).unwrap();
    let keyed = set_keyframe(&placed, key(0), &WIDTH, t(0), 0.5, None).unwrap();
    let keyed = set_keyframe(&keyed, key(0), &WIDTH, t(6_000), 1.0, None).unwrap();
    // Output 2:1 over a source 1:1: the picture is twice as wide as high on screen.
    let (output, source) = (2.0, 1.0);
    let dropped = ClipPlacement { x: 0.1, y: 0.2, width: 0.7 };
    let dragged = set_placement_at(&keyed, key(0), t(3_000), dropped, output, source).unwrap();
    let shown = clip_of(&dragged, 0).transform_at(t(3_000)).rect(output, source);
    assert_close(shown.x, 0.1, "x");
    assert_close(shown.y, 0.2, "y");
    assert_close(shown.width, 0.7, "width");
    // Setting one number reads back the same, here the vertical one.
    let moved = set_motion_value(&dragged, key(0), ClipProperty::Y, t(4_000), 0.6, output, source).unwrap();
    assert_close(clip_of(&moved, 0).transform_at(t(4_000)).rect(output, source).y, 0.6, "y after set_motion_value");
    let moved = set_motion_value(&dragged, key(0), ClipProperty::X, t(4_000), 0.3, output, source).unwrap();
    assert_close(clip_of(&moved, 0).transform_at(t(4_000)).rect(output, source).x, 0.3, "x after set_motion_value");
    // Without a place the fit is the static one, and the drag still reads back.
    let fit = set_keyframe(&one_clip(9_000), key(0), &WIDTH, t(0), 0.5, None).unwrap();
    let fit = set_keyframe(&fit, key(0), &WIDTH, t(6_000), 1.0, None).unwrap();
    let dragged = set_placement_at(&fit, key(0), t(3_000), dropped, output, source).unwrap();
    let shown = clip_of(&dragged, 0).transform_at(t(3_000)).rect(output, source);
    assert_close(shown.x, 0.1, "x without a place");
    assert_close(shown.y, 0.2, "y without a place");
}

#[test]
fn interpolating_between_values_at_the_limits_of_a_float_stays_finite() {
    let keys = [Keyframe { at: t(0), value: 1.7e308, easing: Easing::Linear }, Keyframe { at: t(1_000), value: -1.7e308, easing: Easing::EaseInOut }];
    for at in [1, 250, 500, 999] {
        assert!(sample_keyframes(&keys, t(at)).unwrap().is_finite(), "at {at}");
    }
    let keys = [Keyframe { at: t(0), value: -1.7e308, easing: Easing::Linear }, Keyframe { at: t(1_000), value: 1.7e308, easing: Easing::Linear }];
    for at in [1, 250, 500, 999] {
        assert!(sample_keyframes(&keys, t(at)).unwrap().is_finite(), "at {at}");
    }
}

#[test]
fn a_clip_that_has_rotation_keyframes_or_a_turn_rotates_at_every_frame() {
    let upright = one_clip(3_000);
    assert!(!clip_of(&upright, 0).transform_at(t(0)).rotates);
    let turned = set_rotation(&upright, key(0), 30.0).unwrap();
    assert!(clip_of(&turned, 0).transform_at(t(0)).rotates);
    let keyed = set_keyframe(&upright, key(0), &ROTATION, t(0), 0.0, None).unwrap();
    let keyed = set_keyframe(&keyed, key(0), &ROTATION, t(2_000), 90.0, None).unwrap();
    let transform = clip_of(&keyed, 0).transform_at(t(0));
    assert_eq!(transform.rotation, 0.0);
    assert!(transform.rotates, "upright at this frame, and still drawn as a turning clip");
}

#[test]
fn a_keyframe_outside_the_clip_is_not_the_nearest_one() {
    let key_at = |at: i64| Keyframe { at: t(at), value: 0.0, easing: Easing::Linear };
    assert_eq!(nearest_keyframe_inside(&[key_at(-10)], t(100), t(1_500), t(3_003)), None, "before the clip");
    assert_eq!(nearest_keyframe_inside(&[key_at(3_003)], t(3_000), t(1_500), t(3_003)), None, "at the end of the clip");
    assert_eq!(nearest_keyframe_inside(&[key_at(2_900), key_at(3_000)], t(2_990), t(1_500), t(3_000)), Some(t(2_900)), "the one outside does not hide the one inside");
    assert_eq!(nearest_keyframe_inside(&[key_at(-10), key_at(700)], t(100), t(1_500), t(3_003)), Some(t(700)));
}

#[test]
fn changing_one_effect_number_leaves_the_keyframes_of_another_where_they_are() {
    let base = add_effect(&one_clip(9_000), key(0), effect("blur")).unwrap();
    let mut keyed = base;
    for (param, first, second) in [("a", 4.0, 8.0), ("b", 10.0, 20.0)] {
        let target = KeyframeTarget::EffectParam { effect: 0, param: param.into() };
        keyed = set_keyframe(&keyed, key(0), &target, t(3_003), first, None).unwrap();
        keyed = set_keyframe(&keyed, key(0), &target, t(6_003), second, None).unwrap();
    }
    let keys_of = |project: &Project, param: &str| clip_of(project, 0).effects[0].keyframes[param].clone();
    // The playhead is a tick off the keyframes, so what is shown is the value between them.
    let mut shown = Map::new();
    shown.insert("a".into(), json!(5.0));
    shown.insert("b".into(), json!(sample_keyframes(&keys_of(&keyed, "b"), t(3_004)).unwrap()));
    let edited = set_effect_params_at(&keyed, key(0), 0, shown, t(3_004)).unwrap();
    assert_eq!(keys_of(&edited, "b"), keys_of(&keyed, "b"), "b was not changed");
    assert_eq!(keys_of(&edited, "a").iter().map(|keyframe| (keyframe.at, keyframe.value)).collect::<Vec<_>>(), vec![(t(3_003), 5.0), (t(6_003), 8.0)]);
    // The same when the number shows the value of the keyframe the playhead is on.
    let mut on_key = Map::new();
    on_key.insert("a".into(), json!(5.0));
    on_key.insert("b".into(), json!(10.0));
    let edited = set_effect_params_at(&keyed, key(0), 0, on_key, t(3_004)).unwrap();
    assert_eq!(keys_of(&edited, "b"), keys_of(&keyed, "b"));
}

#[test]
fn dragging_the_frame_a_few_ticks_off_a_keyframe_leaves_a_keyed_width_alone() {
    let placed = set_placement(&one_clip(9_000), key(0), Some(ClipPlacement { x: 0.1, y: 0.2, width: 0.5 })).unwrap();
    let width = KeyframeTarget::Property(ClipProperty::Width);
    let keyed = set_keyframe(&placed, key(0), &width, t(3_003), 0.5, None).unwrap();
    let keyed = set_keyframe(&keyed, key(0), &width, t(6_003), 0.9, None).unwrap();
    let keys = clip_of(&keyed, 0).motion.keyframes[&ClipProperty::Width].clone();
    let shown_width = sample_keyframes(&keys, t(3_004)).unwrap();
    let dragged = set_placement_at(&keyed, key(0), t(3_004), ClipPlacement { x: 0.4, y: 0.2, width: shown_width }, 1.0, 1.0).unwrap();
    assert_eq!(clip_of(&dragged, 0).motion.keyframes[&ClipProperty::Width], keys, "only the position moved");
    let resized = set_placement_at(&keyed, key(0), t(3_004), ClipPlacement { x: 0.1, y: 0.2, width: 0.7 }, 1.0, 1.0).unwrap();
    assert_eq!(clip_of(&resized, 0).motion.keyframes[&ClipProperty::Width][0].value, 0.7, "a real change still moves the keyframe");
}
