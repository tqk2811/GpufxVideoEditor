//! Locking a video clip to a motion track of its own source: the object stays put on screen and
//! the rest of the frame moves against it.

mod common;

use common::*;
use engine::{
    bake_lock, layers_at_frame, remove_motion_track, set_follow, set_lock, split_clip, validate, AcceptAll, ClipKey, ClipPlacement, EditError, Follow, FrameMapping, LayerContent, MotionTrack, Project, TrackKind, TrackLink, TrackLock, TrackSample,
    ValidationError,
};

const SECOND: i64 = 90_000;
const FRAME: i64 = 3_000;
const ASPECT: f64 = 16.0 / 9.0;
const KEY: ClipKey = ClipKey { track: 0, clip: 0 };

fn sample(seconds: f64, cx: f64, cy: f64, size: f64, angle: f64) -> TrackSample {
    TrackSample { source_time: t((seconds * SECOND as f64) as i64), cx, cy, w: size, h: size, angle, confidence: 1.0, manual: false }
}

/// The object goes from the left quarter to the right quarter and down a little over two seconds
/// of `a.mp4`, doubling in size and turning a quarter turn.
fn moving_track() -> MotionTrack {
    MotionTrack { id: 7, media_path: "a.mp4".into(), aspect: ASPECT, samples: vec![sample(0.0, 0.25, 0.4, 0.1, 0.0), sample(2.0, 0.75, 0.6, 0.2, 90.0)] }
}

fn lock(anchor: i64, scale: bool, rotation: bool) -> TrackLock {
    TrackLock { link: TrackLink { track_id: 7, anchor_source_time: t(anchor), follow_scale: scale, follow_rotation: rotation, smooth: 0.0 }, zoom: 1.0 }
}

/// One video track with `video` locked by `with`.
fn rig(mut video: engine::Clip, with: TrackLock) -> Project {
    video.lock = Some(with);
    let mut p = project(FRAME, vec![track(vec![video], Vec::new())]);
    p.motion_tracks.push(moving_track());
    p
}

/// How the picture of track `lane` lies in the frame at output frame `frame`, read from the layer
/// as the renderer draws it.
fn drawn(p: &Project, lane: usize, frame: i64) -> FrameMapping {
    let layers = layers_at_frame(p, frame);
    let layer = layers.iter().find(|layer| layer.track == lane).expect("the clip shows");
    let LayerContent::Clip(sample) = layer.content else { panic!("no transition") };
    FrameMapping { rect: sample.transform.rect(ASPECT, ASPECT), rotation: sample.transform.rotation, output_aspect: ASPECT }
}

/// Where the object is on screen at output frame `frame`, its angle there and its width in parts
/// of the frame width.
fn object(p: &Project, frame: i64) -> ([f64; 2], f64, f64) {
    let video = &p.tracks[0];
    let at = t(frame * FRAME);
    let index = video.clips.iter().position(|c| at >= c.start && at < c.start.saturating_add(c.duration)).unwrap();
    let clip = &video.clips[index];
    let source = clip.source_time_at(at.saturating_sub(clip.start)).max(engine::Time::ZERO);
    let point = p.motion_tracks[0].point_at(source, 0.0).unwrap();
    let mapping = drawn(p, 0, frame);
    (mapping.to_frame([point.cx, point.cy]), point.angle + mapping.rotation, point.w * mapping.rect.width)
}

fn close(actual: [f64; 2], expected: [f64; 2]) {
    assert!((actual[0] - expected[0]).abs() < 1e-6 && (actual[1] - expected[1]).abs() < 1e-6, "{actual:?} != {expected:?}");
}

#[test]
fn the_object_stays_where_it_was_at_the_anchor() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), lock(0, false, false));
    assert!(validate(&p, &AcceptAll).is_empty(), "{:?}", validate(&p, &AcceptAll));
    // The video fills the frame, so at the anchor the object is where the track says.
    close(object(&p, 0).0, [0.25, 0.4]);
    for frame in [15, 30, 45, 59] {
        let (at, angle, _) = object(&p, frame);
        close(at, [0.25, 0.4]);
        assert!(angle > 0.0, "position only: the object still turns on screen");
    }
    // The picture moved left and up by how the object went right and down.
    let middle = drawn(&p, 0, 30).rect;
    assert!(middle.x < -0.2 && middle.y < -0.05, "{middle:?}");
}

#[test]
fn scale_and_rotation_are_kept_when_asked_and_the_frame_turns_the_other_way() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), lock(0, true, true));
    let (_, angle0, width0) = object(&p, 0);
    for frame in [15, 30, 59] {
        let (at, angle, width) = object(&p, frame);
        close(at, [0.25, 0.4]);
        assert!((angle - angle0).abs() < 1e-6, "the object keeps its angle: {angle}");
        assert!((width - width0).abs() < 1e-6, "the object keeps its size: {width}");
    }
    let mapping = drawn(&p, 0, 30);
    assert!(mapping.rotation < -40.0, "the frame turns against the object: {}", mapping.rotation);
    let layers = layers_at_frame(&p, 0);
    let LayerContent::Clip(sample) = layers[0].content else { panic!() };
    assert!(sample.transform.rotates, "a clip that may turn is drawn as one at every frame");
}

#[test]
fn the_extra_zoom_scales_around_the_object() {
    let mut with = lock(0, false, false);
    with.zoom = 2.0;
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), with);
    for frame in [0, 30] {
        close(object(&p, frame).0, [0.25, 0.4]);
        assert!((drawn(&p, 0, frame).rect.width - 2.0).abs() < 1e-9);
    }
}

#[test]
fn the_lock_reads_the_track_through_trim_speed_reverse_and_place() {
    // Source in at 1 s and twice as fast.
    let mut fast = clip("a.mp4", 0, SECOND / 2, SECOND / 2);
    fast.speed = 2.0;
    let p = rig(fast, lock(SECOND, true, true));
    let home = object(&p, 0).0;
    for frame in [3, 7, 14] {
        close(object(&p, frame).0, home);
    }

    // Backwards, drawn at half size and turned.
    let mut back = clip("a.mp4", 0, 0, 2 * SECOND);
    back.reverse = true;
    back.placement = Some(ClipPlacement { x: 0.3, y: 0.2, width: 0.5 });
    back.motion.rotation = 30.0;
    let p = rig(back, lock(SECOND / 2, true, false));
    let home = object(&p, 45).0;
    for frame in [0, 20, 59] {
        close(object(&p, frame).0, home);
    }
}

#[test]
fn a_split_keeps_the_lock_on_both_pieces() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), lock(SECOND / 2, true, true));
    let split = split_clip(&p, KEY, t(SECOND)).unwrap();
    assert_eq!(split.tracks[0].clips[0].lock, p.tracks[0].clips[0].lock);
    assert_eq!(split.tracks[0].clips[1].lock, p.tracks[0].clips[0].lock, "the second piece does not play the anchor but still locks to it");
    assert!(validate(&split, &AcceptAll).is_empty());
    for frame in [0, 15, 29, 30, 45, 59] {
        close(object(&split, frame).0, object(&p, frame).0);
    }
}

#[test]
fn an_overlay_following_the_locked_clip_stays_on_the_object() {
    let mut p = rig(clip("a.mp4", 0, 0, 2 * SECOND), lock(0, true, true));
    let mut sticker = clip("s.png", 0, 0, 2 * SECOND);
    sticker.placement = Some(ClipPlacement { x: 0.2, y: 0.3, width: 0.1 });
    sticker.follow = Some(Follow { link: TrackLink { track_id: 7, anchor_source_time: t(0), follow_scale: true, follow_rotation: true, smooth: 0.0 }, ref_track: 0, ref_clip: 0, offset: [0.0, 0.0] });
    let mut stickers = track(vec![sticker], Vec::new());
    stickers.kind = TrackKind::Sticker;
    p.tracks.push(stickers);
    assert!(validate(&p, &AcceptAll).is_empty(), "{:?}", validate(&p, &AcceptAll));
    // The object does not move on screen, so neither does what sits on it.
    let home = drawn(&p, 1, 0);
    for frame in [15, 30, 59] {
        let now = drawn(&p, 1, frame);
        close([now.rect.x, now.rect.y], [home.rect.x, home.rect.y]);
        assert!((now.rect.width - home.rect.width).abs() < 1e-6 && (now.rotation - home.rotation).abs() < 1e-6);
    }
    // And the box of the tracker drawn over the locked picture lands on the object too.
    let mapping = p.clip_frame_mapping(0, 0, t(30 * FRAME), ASPECT).unwrap();
    let point = p.motion_tracks[0].point_at(t(SECOND), 0.0).unwrap();
    close(mapping.to_frame([point.cx, point.cy]), [0.25, 0.4]);
}

#[test]
fn locks_that_do_not_hold_are_refused() {
    // Another file.
    let mut other = clip("b.mp4", 0, 0, 2 * SECOND);
    other.lock = Some(lock(0, false, false));
    let mut p = project(FRAME, vec![track(vec![other], Vec::new())]);
    p.motion_tracks.push(moving_track());
    assert!(validate(&p, &AcceptAll).iter().any(|e| matches!(e, ValidationError::InvalidLock { track: 0, clip: 0, .. })));

    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), lock(0, false, false));
    let mut bad = lock(0, false, false);
    bad.zoom = 0.0;
    assert!(matches!(set_lock(&p, KEY, Some(bad)), Err(EditError::Invalid(_))));
    let mut missing = lock(0, false, false);
    missing.link.track_id = 9;
    assert!(matches!(set_lock(&p, KEY, Some(missing)), Err(EditError::Invalid(_))));
    let cleared = set_lock(&p, KEY, None).unwrap();
    assert_eq!(cleared.tracks[0].clips[0].lock, None);
    assert_eq!(set_lock(&cleared, KEY, Some(lock(0, true, false))).unwrap().tracks[0].clips[0].lock, Some(lock(0, true, false)));

    // A clip cannot follow and lock at once, either way round.
    let mut both = project(FRAME, vec![track(vec![clip("a.mp4", 0, 0, 2 * SECOND)], Vec::new()), track(vec![clip("a.mp4", 0, 0, 2 * SECOND)], Vec::new())]);
    both.motion_tracks.push(moving_track());
    let follow = Follow { link: TrackLink { track_id: 7, anchor_source_time: t(0), follow_scale: false, follow_rotation: false, smooth: 0.0 }, ref_track: 0, ref_clip: 0, offset: [0.0, 0.0] };
    let following = set_follow(&both, ClipKey { track: 1, clip: 0 }, Some(follow)).unwrap();
    assert!(matches!(set_lock(&following, ClipKey { track: 1, clip: 0 }, Some(lock(0, false, false))), Err(EditError::Invalid(_))));
    let locked = set_lock(&both, ClipKey { track: 1, clip: 0 }, Some(lock(0, false, false))).unwrap();
    assert!(matches!(set_follow(&locked, ClipKey { track: 1, clip: 0 }, Some(follow)), Err(EditError::Invalid(_))));

    // Removing the track ends the lock.
    let gone = remove_motion_track(&p, 7).unwrap();
    assert_eq!(gone.tracks[0].clips[0].lock, None);
}

#[test]
fn the_lock_round_trips_and_leaves_its_defaults_out() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), lock(0, false, false));
    let text = serde_json::to_string(&p).unwrap();
    assert!(text.contains(r#""lock":{"track_id":7,"anchor_source_time":0}"#), "{text}");
    assert_eq!(Project::from_json(&text).unwrap(), p);
    let mut with = lock(SECOND, true, true);
    with.zoom = 1.5;
    with.link.smooth = 0.2;
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), with);
    let back = Project::from_json(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back.tracks[0].clips[0].lock, Some(with));
}

#[test]
fn baking_a_lock_writes_keyframes_where_the_picture_was_and_drops_the_lock() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), lock(0, true, true));
    let before: Vec<FrameMapping> = [0, 15, 30, 59].iter().map(|frame| drawn(&p, 0, *frame)).collect();
    let baked = bake_lock(&p, KEY, ASPECT).unwrap();
    let clip = &baked.tracks[0].clips[0];
    assert!(clip.lock.is_none());
    assert_eq!(clip.motion.keyframes.len(), 4, "x, y, width and rotation");
    for (frame, expected) in [0, 15, 30, 59].iter().zip(before) {
        let now = drawn(&baked, 0, *frame);
        close([now.rect.x, now.rect.y], [expected.rect.x, expected.rect.y]);
        assert!((now.rect.width - expected.rect.width).abs() < 1e-6 && (now.rotation - expected.rotation).abs() < 1e-6);
    }
    assert!(matches!(bake_lock(&baked, KEY, ASPECT), Err(EditError::NotLocked { .. })));
}

/// A zoom from nothing over the first second.
fn zoom_in() -> engine::ClipAnimation {
    engine::ClipAnimation { enter: Some(engine::AnimationRef { preset: "zoom".to_owned(), duration: t(SECOND) }), ..engine::ClipAnimation::default() }
}

#[test]
fn an_anchor_under_an_enter_animation_reads_the_clip_without_it() {
    let mut video = clip("a.mp4", 0, 0, 2 * SECOND);
    video.animation = zoom_in();
    let p = rig(video, lock(0, true, false));
    // Once the zoom is over, the object sits where the clip's own place put it at the anchor, at
    // the size it had there, not at the nothing the zoom started from.
    for frame in [30, 45, 59] {
        let (at, _, width) = object(&p, frame);
        close(at, [0.25, 0.4]);
        assert!((width - 0.1).abs() < 1e-6, "width {width} at frame {frame}");
    }
}

#[test]
fn a_split_piece_without_the_anchor_reads_its_place_inside_itself() {
    let p = rig(clip("a.mp4", 0, 0, 2 * SECOND), lock(SECOND / 2, true, true));
    let mut split = split_clip(&p, KEY, t(SECOND)).unwrap();
    // The second piece zooms in: the anchor, before it, must not read that zoom at its start.
    split.tracks[0].clips[1].animation = zoom_in();
    for frame in [59] {
        close(object(&split, frame).0, object(&p, frame).0);
        assert!(drawn(&split, 0, frame).rect.width.is_finite() && drawn(&split, 0, frame).rect.width > 0.1);
    }
}
