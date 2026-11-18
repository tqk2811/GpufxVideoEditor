//! Object tracking: motion tracks, clips that follow them, censor regions and effect regions.

mod common;

use common::*;
use engine::{
    add_clip, delete_clip, layers_at_frame, move_clip, remove_track, set_follow, set_track_sample, apply_track_run, split_clip, TrackRun, validate, AcceptAll, CensorContent, CensorMode, ClipKey, ClipPlacement, ClipProperty, EditError, EffectRegion, Follow, Keyframe, LayerContent, MotionTrack, Project, RegionShape, Time, TrackKind, TrackLink,
    TrackSample, ValidationError,
};

const SECOND: i64 = 90_000;
const FRAME: i64 = 3_000;
const ASPECT: f64 = 16.0 / 9.0;

fn sample(seconds: f64, cx: f64, cy: f64) -> TrackSample {
    TrackSample { source_time: t((seconds * SECOND as f64) as i64), cx, cy, w: 0.1, h: 0.1, angle: 0.0, confidence: 1.0, manual: false }
}

/// The object goes from the left quarter to the right quarter over the first two seconds of
/// `a.mp4`, at mid height.
fn moving_track() -> MotionTrack {
    MotionTrack { id: 7, media_path: "a.mp4".into(), aspect: ASPECT, samples: vec![sample(0.0, 0.25, 0.5), sample(2.0, 0.75, 0.5)] }
}

fn link(anchor: i64) -> TrackLink {
    TrackLink { track_id: 7, anchor_source_time: t(anchor), follow_scale: false, follow_rotation: false, smooth: 0.0 }
}

fn sticker(start: i64, duration: i64) -> engine::Clip {
    let mut c = clip("s.png", start, 0, duration);
    c.placement = Some(ClipPlacement { x: 0.2, y: 0.4, width: 0.1 });
    c
}

/// A video clip of `a.mp4` on track 0 and a sticker following it on track 1, from 0 for 2 s.
fn rig(reference: engine::Clip, anchor: i64) -> Project {
    let mut overlay = sticker(0, 2 * SECOND);
    overlay.follow = Some(Follow { link: link(anchor), ref_track: 0, ref_clip: 0, offset: [0.0, 0.0] });
    let mut stickers = track(vec![overlay], Vec::new());
    stickers.kind = TrackKind::Sticker;
    let mut p = project(FRAME, vec![track(vec![reference], Vec::new()), stickers]);
    p.motion_tracks.push(moving_track());
    p
}

/// The middle of the sticker (a square picture) at output frame `frame`.
fn middle(p: &Project, frame: i64) -> (f64, f64) {
    let layers = layers_at_frame(p, frame);
    let layer = layers.iter().find(|layer| layer.track == 1).expect("the sticker shows");
    let LayerContent::Clip(sample) = layer.content else { panic!("no transition") };
    let rect = sample.transform.rect(ASPECT, 1.0);
    (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

fn close(actual: (f64, f64), expected: (f64, f64)) {
    assert!((actual.0 - expected.0).abs() < 1e-3 && (actual.1 - expected.1).abs() < 1e-3, "{actual:?} != {expected:?}");
}

/// The sticker's own middle: x 0.2 + 0.05, y 0.4 + half its height (0.1 * 16/9 / 2).
const HOME: (f64, f64) = (0.25, 0.4 + 0.1 * ASPECT / 2.0);

#[test]
fn a_follower_stands_on_its_place_at_the_anchor_and_moves_with_the_object() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    assert!(validate(&p, &AcceptAll).is_empty(), "{:?}", validate(&p, &AcceptAll));
    close(middle(&p, 0), HOME);
    // At 1 s the object went a quarter of the frame to the right.
    close(middle(&p, 30), (HOME.0 + 0.25, HOME.1));
}

#[test]
fn the_track_is_read_through_source_in_speed_and_reverse() {
    // Source in at 1 s: the clip starts on the middle of the path, which is the anchor.
    let p = rig(clip("a.mp4", 0, SECOND, SECOND), SECOND);
    close(middle(&p, 0), HOME);
    close(middle(&p, 15), (HOME.0 + 0.125, HOME.1));

    // Twice as fast: half a second of timeline is one second of source.
    let mut fast = clip("a.mp4", 0, 0, SECOND);
    fast.speed = 2.0;
    let p = rig(fast, 0);
    close(middle(&p, 15), (HOME.0 + 0.25, HOME.1));

    // Backwards over 0..2 s: the clip starts on the last frame, the anchor at source 0 is its end.
    let mut back = clip("a.mp4", 0, 0, 2 * SECOND);
    back.reverse = true;
    let p = rig(back, 0);
    close(middle(&p, 0), (HOME.0 + 0.5, HOME.1));
    close(middle(&p, 30), (HOME.0 + 0.25, HOME.1));
}

#[test]
fn the_place_and_turn_of_the_reference_clip_carry_the_motion() {
    // The video drawn at half size: the object moves half as far on the output.
    let mut small = clip("a.mp4", 0, 0, 2 * SECOND);
    small.placement = Some(ClipPlacement { x: 0.5, y: 0.5, width: 0.5 });
    let p = rig(small, 0);
    close(middle(&p, 30), (HOME.0 + 0.125, HOME.1));

    // Turned half a turn: the object moves the other way.
    let mut turned = clip("a.mp4", 0, 0, 2 * SECOND);
    turned.motion.rotation = 180.0;
    let p = rig(turned, 0);
    close(middle(&p, 30), (HOME.0 - 0.25, HOME.1));
}

#[test]
fn the_keyframes_of_the_follower_add_to_the_motion() {
    let mut p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    p.tracks[1].clips[0].motion.keyframes.insert(ClipProperty::X, vec![Keyframe { at: t(0), value: 0.2, easing: Default::default() }, Keyframe { at: t(SECOND), value: 0.3, easing: Default::default() }]);
    close(middle(&p, 30), (HOME.0 + 0.1 + 0.25, HOME.1));
}

#[test]
fn scale_rotation_and_smoothing_are_read_when_asked() {
    let mut p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    // The object doubles in size and turns a quarter turn over the two seconds, and the sticker sits
    // on it at the anchor.
    p.motion_tracks[0].samples[1].w = 0.2;
    p.motion_tracks[0].samples[1].h = 0.2;
    p.motion_tracks[0].samples[1].angle = 90.0;
    p.motion_tracks[0].samples[0].cx = HOME.0;
    p.motion_tracks[0].samples[0].cy = HOME.1;
    p.motion_tracks[0].samples[1].cx = HOME.0;
    p.motion_tracks[0].samples[1].cy = HOME.1;
    let follow = p.tracks[1].clips[0].follow.as_mut().unwrap();
    follow.link.follow_scale = true;
    follow.link.follow_rotation = true;
    let layers = layers_at_frame(&p, 59);
    let LayerContent::Clip(drawn) = layers[1].content else { panic!() };
    let rect = drawn.transform.rect(ASPECT, 1.0);
    assert!((rect.width - 0.2).abs() < 0.01, "twice as wide near the end: {}", rect.width);
    assert!((drawn.transform.rotation - 90.0).abs() < 2.0 && drawn.transform.rotates, "turned with the object: {}", drawn.transform.rotation);

    // Smoothing evens out a jump of one sample and leaves a still object where it is.
    let mut jumpy = moving_track();
    jumpy.samples = (0..=10).map(|i| sample(i as f64 * 0.1, if i == 5 { 0.9 } else { 0.5 }, 0.5)).collect();
    let raw = jumpy.point_at(t(SECOND / 2), 0.0).unwrap().cx;
    let smooth = jumpy.point_at(t(SECOND / 2), 0.1).unwrap().cx;
    assert_eq!(raw, 0.9);
    assert!(smooth < 0.7 && smooth > 0.5, "{smooth}");
    assert!((jumpy.point_at(t(SECOND / 10), 0.02).unwrap().cx - 0.5).abs() < 1e-6);
}

#[test]
fn a_split_keeps_the_follow_on_the_piece_that_shows_its_anchor_and_a_delete_drops_it() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), SECOND + SECOND / 2);
    let split = split_clip(&p, ClipKey { track: 0, clip: 0 }, t(SECOND)).unwrap();
    let follow = split.tracks[1].clips[0].follow.expect("kept");
    assert_eq!(follow.ref_clip, 1, "the second piece shows 1.5 s");
    // The motion is the same as before the split, on both pieces.
    close(middle(&split, 15), middle(&p, 15));
    close(middle(&split, 50), middle(&p, 50));

    let gone = delete_clip(&split, ClipKey { track: 0, clip: 1 }).unwrap();
    assert_eq!(gone.tracks[1].clips[0].follow, None, "no clip shows the anchor any more");
    close(middle(&gone, 15), HOME);
}

#[test]
fn removing_a_track_renumbers_or_drops_the_references() {
    let mut p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    p.tracks.insert(0, track(Vec::new(), Vec::new()));
    p.tracks[2].clips[0].follow.as_mut().unwrap().ref_track = 1;
    assert!(validate(&p, &AcceptAll).is_empty());
    let below = remove_track(&p, 0).unwrap();
    assert_eq!(below.tracks[1].clips[0].follow.unwrap().ref_track, 0);
    let reference = remove_track(&p, 1).unwrap();
    assert_eq!(reference.tracks[1].clips[0].follow, None);
}

#[test]
fn set_follow_refuses_what_does_not_hold() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    let key = ClipKey { track: 1, clip: 0 };
    let bad = |follow: Follow| matches!(set_follow(&p, key, Some(follow)), Err(EditError::Invalid(errors)) if matches!(errors[0], ValidationError::InvalidFollow { .. }));
    let good = Follow { link: link(0), ref_track: 0, ref_clip: 0, offset: [0.0, 0.0] };
    assert!(bad(Follow { link: TrackLink { track_id: 9, ..link(0) }, ..good }), "no such track");
    assert!(bad(Follow { link: link(3 * SECOND), ..good }), "anchor not shown");
    assert!(bad(Follow { ref_track: 1, ..good }), "itself");
    assert!(bad(Follow { link: TrackLink { smooth: -1.0, ..link(0) }, ..good }));
    assert!(set_follow(&p, key, None).unwrap().tracks[1].clips[0].follow.is_none());
    assert!(set_follow(&p, key, Some(good)).is_ok());
}

#[test]
fn samples_are_kept_in_order_and_replaced_at_the_same_time() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    let p = set_track_sample(&p, 7, sample(1.0, 0.1, 0.1)).unwrap();
    let p = set_track_sample(&p, 7, TrackSample { manual: true, ..sample(1.0, 0.2, 0.2) }).unwrap();
    let times: Vec<i64> = p.motion_tracks[0].samples.iter().map(|s| s.source_time.ticks()).collect();
    assert_eq!(times, [0, SECOND, 2 * SECOND]);
    assert!(p.motion_tracks[0].samples[1].manual && p.motion_tracks[0].samples[1].cx == 0.2);
    assert!(matches!(set_track_sample(&p, 1, sample(0.0, 0.0, 0.0)), Err(EditError::MotionTrackNotFound { id: 1 })));
}

#[test]
fn a_tracker_run_replaces_the_tracker_samples_of_its_span_but_not_the_users() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    let p = set_track_sample(&p, 7, sample(1.0, 0.1, 0.1)).unwrap();
    let run = forward_run(&p, 0.0, vec![sample(0.5, 0.3, 0.5), sample(1.0, 0.9, 0.9), sample(1.5, 0.4, 0.5)]);
    let next = apply_track_run(&p, &run).unwrap();
    let samples = &next.motion_tracks[0].samples;
    assert_eq!(times(&next), [0, SECOND / 2, SECOND, SECOND * 3 / 2, 2 * SECOND]);
    assert!(samples[2].cx == 0.9 && samples.iter().all(|s| !s.manual), "the tracker's old sample is replaced");
    // A run over the whole track replaces the tracker's samples at its ends.
    let next = apply_track_run(&p, &forward_run(&p, 0.0, vec![sample(0.0, 0.6, 0.6), sample(2.0, 0.7, 0.7)])).unwrap();
    let cx: Vec<f64> = next.motion_tracks[0].samples.iter().map(|s| s.cx).collect();
    assert_eq!(cx, [0.6, 0.7]);
    assert_eq!(apply_track_run(&p, &forward_run(&p, 0.0, Vec::new())).unwrap(), p);
    assert!(matches!(apply_track_run(&p, &TrackRun { track_id: 1, ..run }), Err(EditError::MotionTrackNotFound { id: 1 })));
}

fn times(p: &Project) -> Vec<i64> {
    p.motion_tracks[0].samples.iter().map(|s| s.source_time.ticks()).collect()
}

/// A run of track 7 forwards from its sample at `from` seconds.
fn forward_run(p: &Project, from: f64, samples: Vec<TrackSample>) -> TrackRun {
    let start = *p.motion_tracks[0].samples.iter().find(|s| s.source_time == t((from * SECOND as f64) as i64)).unwrap();
    TrackRun { track_id: 7, media_path: "a.mp4".into(), start, forward: true, samples, lost: false }
}

#[test]
fn a_run_that_no_longer_fits_its_track_is_not_written() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    let run = forward_run(&p, 0.0, vec![sample(0.5, 0.3, 0.5)]);
    // The user moved the start box while the run went.
    let moved = set_track_sample(&p, 7, TrackSample { manual: true, ..sample(0.0, 0.4, 0.4) }).unwrap();
    assert!(matches!(apply_track_run(&moved, &run), Err(EditError::TrackRunStale { id: 7 })));
    let mut other_file = p.clone();
    other_file.motion_tracks[0].media_path = "b.mp4".into();
    assert!(matches!(apply_track_run(&other_file, &run), Err(EditError::TrackRunStale { id: 7 })));
    assert!(apply_track_run(&p, &run).is_ok());
}

#[test]
fn a_box_placed_by_hand_during_the_run_cuts_it_there() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    let run = forward_run(&p, 0.0, vec![sample(0.5, 0.3, 0.5), sample(1.0, 0.4, 0.5), sample(1.5, 0.5, 0.5)]);
    let placed = set_track_sample(&p, 7, TrackSample { manual: true, ..sample(1.0, 0.9, 0.9) }).unwrap();
    let next = apply_track_run(&placed, &run).unwrap();
    assert_eq!(times(&next), [0, SECOND / 2, SECOND, 2 * SECOND], "nothing of the run at or past the user's box");
    assert!(next.motion_tracks[0].samples[2].manual && next.motion_tracks[0].samples[2].cx == 0.9);
    // Backwards the same, the other way.
    let mut back = forward_run(&p, 2.0, vec![sample(1.5, 0.5, 0.5), sample(1.0, 0.4, 0.5), sample(0.5, 0.3, 0.5)]);
    back.forward = false;
    let next = apply_track_run(&placed, &back).unwrap();
    assert_eq!(times(&next), [0, SECOND, SECOND * 3 / 2, 2 * SECOND]);
}

#[test]
fn a_lost_run_takes_away_the_old_tracker_samples_past_the_loss() {
    let mut p = rig(clip("a.mp4", 0, 0, 4 * SECOND), 0);
    p.motion_tracks[0].samples = vec![sample(0.0, 0.2, 0.5), sample(1.0, 0.3, 0.5), sample(2.0, 0.4, 0.5), TrackSample { manual: true, ..sample(3.0, 0.5, 0.5) }, sample(3.5, 0.6, 0.5)];
    let mut run = forward_run(&p, 0.0, vec![sample(0.5, 0.25, 0.5), sample(1.5, 0.3, 0.5)]);
    run.lost = true;
    let next = apply_track_run(&p, &run).unwrap();
    assert_eq!(times(&next), [0, SECOND / 2, SECOND * 3 / 2, 3 * SECOND, SECOND * 7 / 2], "the old samples at 1 s and 2 s go, the user's box and what lies past it stay");
    run.lost = false;
    assert_eq!(times(&apply_track_run(&p, &run).unwrap()), [0, SECOND / 2, SECOND * 3 / 2, 2 * SECOND, 3 * SECOND, SECOND * 7 / 2], "without a loss the old sample past the run stays");
}

#[test]
fn validation_names_bad_tracks_follows_censors_and_regions() {
    let mut p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    p.motion_tracks.push(MotionTrack { samples: vec![sample(1.0, 0.0, 0.0), sample(0.5, 0.0, 0.0)], ..moving_track() });
    let errors = validate(&p, &AcceptAll);
    assert!(errors.iter().any(|e| matches!(e, ValidationError::InvalidMotionTrack { track: 1, problem } if problem.contains("used twice") || problem.contains("not after"))), "{errors:?}");

    let mut p = rig(clip("b.mp4", 0, 0, 2 * SECOND), 0);
    p.tracks[1].clips[0].follow.as_mut().unwrap().ref_clip = 0;
    assert!(validate(&p, &AcceptAll).iter().any(|e| matches!(e, ValidationError::InvalidFollow { track: 1, clip: 0, .. })), "the reference shows another file");

    let mut p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    p.tracks[1].clips.push(engine::censor_clip(t(2 * SECOND), t(SECOND), CensorContent { feather: 2.0, ..Default::default() }, None));
    p.tracks[1].clips.push(engine::censor_clip(t(3 * SECOND), t(SECOND), CensorContent { mode: CensorMode::Pixelate { block: 0.0 }, ..Default::default() }, None));
    let errors = validate(&p, &AcceptAll);
    assert_eq!(errors.iter().filter(|e| matches!(e, ValidationError::InvalidCensor { track: 1, .. })).count(), 2, "{errors:?}");

    // A censor region goes on a sticker track only.
    let mut p = project(FRAME, vec![track(vec![engine::censor_clip(t(0), t(SECOND), CensorContent::default(), None)], Vec::new())]);
    assert!(validate(&p, &AcceptAll).iter().any(|e| matches!(e, ValidationError::ClipNotAllowedOnTrack { .. })));
    p.tracks[0].kind = TrackKind::Sticker;
    assert!(validate(&p, &AcceptAll).is_empty());

    let mut p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    let region = EffectRegion { shape: RegionShape::Ellipse, cx: 0.5, cy: 0.5, w: 0.2, h: 0.2, angle: 0.0, feather: 0.5, link: Some(link(0)) };
    p.tracks[0].clips[0].effects.push(engine::Effect { region: Some(region), ..effect("gaussian-blur") });
    assert!(validate(&p, &AcceptAll).is_empty());
    p.tracks[0].clips[0].effects.push(engine::Effect { region: Some(region), ..effect("mix") });
    p.tracks[0].clips[0].effects.push(engine::Effect { region: Some(EffectRegion { link: Some(TrackLink { track_id: 3, ..link(0) }), ..region }), ..effect("pixelate") });
    let errors = validate(&p, &AcceptAll);
    assert!(errors.iter().any(|e| matches!(e, ValidationError::InvalidEffectRegion { effect: 1, .. })), "{errors:?}");
    assert!(errors.iter().any(|e| matches!(e, ValidationError::InvalidEffectRegion { effect: 2, .. })), "{errors:?}");
}

#[test]
fn the_new_fields_round_trip_and_are_left_out_when_absent() {
    let plain = project(FRAME, vec![track(vec![clip("a.mp4", 0, 0, SECOND)], Vec::new())]);
    let text = serde_json::to_string(&plain).unwrap();
    assert!(!text.contains("motion_tracks") && !text.contains("follow") && !text.contains("censor") && !text.contains("region"), "{text}");

    let mut p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    p.tracks[1].clips.push(engine::censor_clip(t(2 * SECOND), t(SECOND), CensorContent { shape: RegionShape::Ellipse, mode: CensorMode::Solid { color: [1, 2, 3, 255] }, ..Default::default() }, Some(ClipPlacement { x: 0.1, y: 0.1, width: 0.3 })));
    p.tracks[0].clips[0].effects.push(engine::Effect { region: Some(EffectRegion { shape: RegionShape::Rect, cx: 0.5, cy: 0.5, w: 0.2, h: 0.1, angle: 10.0, feather: 0.0, link: None }), ..effect("pixelate") });
    let text = serde_json::to_string(&p).unwrap();
    let back = Project::from_json(&text).unwrap();
    assert_eq!(back, p);
    assert!(text.contains(r#""track_id":7"#) && text.contains(r#""kind":"solid""#), "{text}");
    assert_eq!(Time::ZERO, back.tracks[1].clips[0].follow.unwrap().link.anchor_source_time);
}

#[test]
fn moving_the_reference_clip_to_another_track_keeps_the_follow() {
    let mut p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    p.tracks.push(track(Vec::new(), Vec::new()));
    let moved = move_clip(&p, ClipKey { track: 0, clip: 0 }, 2, t(0)).unwrap();
    let follow = moved.tracks[1].clips[0].follow.expect("kept");
    assert_eq!((follow.ref_track, follow.ref_clip), (2, 0));
    close(middle(&moved, 30), middle(&p, 30));
}

#[test]
fn a_copy_put_before_the_reference_does_not_take_the_follow() {
    let mut p = rig(clip("a.mp4", 3 * SECOND, 0, 2 * SECOND), 0);
    p.tracks[1].clips[0].start = t(3 * SECOND);
    assert!(validate(&p, &AcceptAll).is_empty(), "{:?}", validate(&p, &AcceptAll));
    let copied = add_clip(&p, 0, clip("a.mp4", 0, 0, 2 * SECOND)).unwrap();
    assert_eq!(copied.tracks[1].clips[0].follow.expect("kept").ref_clip, 1, "still the clip at 3 s");
}

#[test]
fn a_reference_clip_cannot_follow_and_says_so() {
    let mut p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 0);
    p.tracks.push(track(vec![clip("a.mp4", 0, 0, 2 * SECOND)], Vec::new()));
    let follow = Follow { link: link(0), ref_track: 2, ref_clip: 0, offset: [0.0, 0.0] };
    let Err(EditError::Invalid(errors)) = set_follow(&p, ClipKey { track: 0, clip: 0 }, Some(follow)) else { panic!("refused") };
    let ValidationError::InvalidFollow { problem, .. } = &errors[0] else { panic!("{errors:?}") };
    assert!(problem.contains("reference of clip 0 on track 1"), "{problem}");
}

#[test]
fn validation_refuses_an_anchor_the_reference_clip_does_not_show() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), 3 * SECOND);
    assert!(validate(&p, &AcceptAll).iter().any(|error| matches!(error, ValidationError::InvalidFollow { .. })));
}
