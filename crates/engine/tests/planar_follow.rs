//! Planar (four corner) tracks: their data, and overlays that follow them in perspective (corner
//! pin).

mod common;

use common::*;
use engine::{layers_at_frame, set_follow, ClipKey, ClipPlacement, Follow, LayerContent, MotionTrack, Project, TrackKind, TrackLink, TrackSample};

const SECOND: i64 = 90_000;
const FRAME: i64 = 3_000;
const ASPECT: f64 = 16.0 / 9.0;

/// The square sticker's place at the anchor: x 0.25 to 0.5, y 0.25 to 0.25 + 0.25 * ASPECT.
const BOTTOM: f64 = 0.25 + 0.25 * ASPECT;
const AT_ANCHOR: [[f64; 2]; 4] = [[0.25, 0.25], [0.5, 0.25], [0.5, BOTTOM], [0.25, BOTTOM]];
/// The object two seconds later: a trapezoid, its right side shorter.
const LATER: [[f64; 2]; 4] = [[0.35, 0.2], [0.7, 0.3], [0.7, 0.6], [0.3, 0.75]];

fn planar(seconds: f64, quad: [[f64; 2]; 4]) -> TrackSample {
    TrackSample::from_quad(t((seconds * SECOND as f64) as i64), quad, ASPECT, 1.0, false)
}

fn planar_track() -> MotionTrack {
    MotionTrack { id: 7, media_path: "a.mp4".into(), aspect: ASPECT, samples: vec![planar(0.0, AT_ANCHOR), planar(2.0, LATER)] }
}

fn follow(perspective: bool) -> Follow {
    Follow { link: TrackLink { track_id: 7, anchor_source_time: t(0), follow_scale: false, follow_rotation: false, smooth: 0.0 }, ref_track: 0, ref_clip: 0, offset: [0.0, 0.0], perspective }
}

/// A full-frame video clip of `a.mp4` on track 0 and a square sticker following it on track 1,
/// standing exactly on the object at the anchor.
fn rig(perspective: bool) -> Project {
    let mut sticker = clip("s.png", 0, 0, 2 * SECOND);
    sticker.placement = Some(ClipPlacement { x: 0.25, y: 0.25, width: 0.25 });
    sticker.follow = Some(follow(perspective));
    let mut stickers = track(vec![sticker], Vec::new());
    stickers.kind = TrackKind::Sticker;
    let mut p = project(FRAME, vec![track(vec![clip("a.mp4", 0, 0, 2 * SECOND)], Vec::new()), stickers]);
    p.motion_tracks.push(planar_track());
    p
}

fn sticker_transform(p: &Project, frame: i64) -> engine::ClipTransform {
    let layers = layers_at_frame(p, frame);
    let layer = layers.iter().find(|layer| layer.track == 1).expect("the sticker shows");
    let LayerContent::Clip(sample) = layer.content else { panic!("no transition") };
    sample.transform
}

fn assert_quad(actual: [[f64; 2]; 4], expected: [[f64; 2]; 4]) {
    for (a, e) in actual.iter().zip(&expected) {
        assert!((a[0] - e[0]).abs() < 1e-6 && (a[1] - e[1]).abs() < 1e-6, "{actual:?} != {expected:?}");
    }
}

#[test]
fn a_perspective_follower_has_its_corners_on_the_object_corners() {
    let p = rig(true);
    for (frame, k) in [(0, 0.0), (30, 0.5), (45, 0.75)] {
        let transform = sticker_transform(&p, frame);
        let corners = transform.pinned_corners(transform.unfollowed_rect(ASPECT, 1.0), ASPECT).expect("pinned");
        let expected: [[f64; 2]; 4] = std::array::from_fn(|i| [AT_ANCHOR[i][0] + (LATER[i][0] - AT_ANCHOR[i][0]) * k, AT_ANCHOR[i][1] + (LATER[i][1] - AT_ANCHOR[i][1]) * k]);
        assert_quad(corners, expected);
    }
}

#[test]
fn the_offset_shifts_the_pinned_corners_and_a_plain_follow_has_no_pin() {
    let mut p = rig(true);
    p.tracks[1].clips[0].follow.as_mut().unwrap().offset = [0.1, -0.05];
    let transform = sticker_transform(&p, 30);
    let corners = transform.pinned_corners(transform.unfollowed_rect(ASPECT, 1.0), ASPECT).unwrap();
    let middle: [[f64; 2]; 4] = std::array::from_fn(|i| [(AT_ANCHOR[i][0] + LATER[i][0]) / 2.0 + 0.1, (AT_ANCHOR[i][1] + LATER[i][1]) / 2.0 - 0.05]);
    assert_quad(corners, middle);
    assert!(sticker_transform(&rig(false), 30).pin.is_none());
}

#[test]
fn the_reference_clip_place_carries_the_pin() {
    let mut p = rig(true);
    // The reference clip shrunk to the right half of the frame, top aligned.
    p.tracks[0].clips[0].placement = Some(ClipPlacement { x: 0.5, y: 0.0, width: 0.5 });
    let reference = |q: [[f64; 2]; 4]| q.map(|[x, y]| [0.5 + x * 0.5, y * 0.5]);
    let pin = p.follow_pin(&follow(true), t(2 * SECOND - FRAME)).unwrap();
    let anchor = reference(AT_ANCHOR);
    let near_end: [[f64; 2]; 4] = std::array::from_fn(|i| {
        let k = (2 * SECOND - FRAME) as f64 / (2 * SECOND) as f64;
        [AT_ANCHOR[i][0] + (LATER[i][0] - AT_ANCHOR[i][0]) * k, AT_ANCHOR[i][1] + (LATER[i][1] - AT_ANCHOR[i][1]) * k]
    });
    assert_quad(anchor.map(|c| pin.apply(c).unwrap()), reference(near_end));
}

#[test]
fn a_track_of_boxes_gives_the_corners_of_its_turned_boxes() {
    let mut track = planar_track();
    track.samples = vec![TrackSample { source_time: t(0), cx: 0.5, cy: 0.5, w: 0.2, h: 0.2 * ASPECT, angle: 90.0, confidence: 1.0, manual: false, quad: None }];
    assert!(!track.is_planar());
    let quad = track.quad_at(t(0), 0.0).unwrap();
    // A square turned a quarter clockwise: its top-left corner is now at the top right.
    assert_quad(quad, [[0.6, 0.5 - 0.1 * ASPECT], [0.6, 0.5 + 0.1 * ASPECT], [0.4, 0.5 + 0.1 * ASPECT], [0.4, 0.5 - 0.1 * ASPECT]]);
}

#[test]
fn quads_round_trip_validate_and_are_left_out_when_absent() {
    let p = rig(true);
    let text = serde_json::to_string(&p).unwrap();
    assert!(text.contains(r#""quad":[[0.25,0.25]"#) && text.contains(r#""perspective":true"#), "{text}");
    let back = Project::from_json(&text).unwrap();
    assert!(back.tracks[1].clips[0].follow.unwrap().perspective);
    assert_quad(back.motion_tracks[0].samples[1].quad.unwrap(), LATER);
    let plain = rig(false);
    let mut plain_track = plain.clone();
    plain_track.motion_tracks[0].samples.iter_mut().for_each(|s| s.quad = None);
    let text = serde_json::to_string(&plain_track).unwrap();
    assert!(!text.contains("quad") && !text.contains("perspective"), "{text}");

    let mut bad = planar_track();
    bad.samples[1].quad = Some([LATER[0], LATER[2], LATER[1], LATER[3]]);
    assert!(bad.problem().unwrap().contains("convex"), "{:?}", bad.problem());
    bad.samples[1].quad = Some([[f64::NAN, 0.0], LATER[1], LATER[2], LATER[3]]);
    assert!(bad.problem().is_some());
    assert!(planar_track().problem().is_none());
}

#[test]
fn set_follow_accepts_a_perspective_follow() {
    let mut p = rig(true);
    p.tracks[1].clips[0].follow = None;
    let key = ClipKey { track: 1, clip: 0 };
    let p = set_follow(&p, key, Some(follow(true))).unwrap();
    assert!(p.tracks[1].clips[0].follow.unwrap().perspective);
}

#[test]
fn a_quad_with_its_corners_the_wrong_way_round_does_not_validate() {
    let mut p = rig(false);
    let q = LATER;
    p.motion_tracks[0].samples[1] = planar(2.0, [q[0], q[3], q[2], q[1]]);
    assert!(p.motion_tracks[0].problem().is_some_and(|problem| problem.contains("convex")));
}

#[test]
fn a_follow_in_perspective_is_not_baked() {
    let p = rig(true);
    let key = ClipKey { track: 1, clip: 0 };
    assert!(matches!(engine::bake_follow(&p, key, 1.0), Err(engine::EditError::BakePerspective { .. })));
    assert!(engine::bake_follow(&rig(false), key, 1.0).is_ok());
}

#[test]
fn a_reference_clip_cannot_itself_follow_in_perspective() {
    // The frame mapping of a follower reads the reference clip's place without any corner pin, so
    // a pinned reference clip would put its followers in the wrong place.
    let mut p = rig(true);
    let mut other = clip("t.png", 0, 0, 2 * SECOND);
    other.placement = Some(ClipPlacement { x: 0.1, y: 0.1, width: 0.2 });
    let mut lane = track(vec![other], Vec::new());
    lane.kind = TrackKind::Sticker;
    p.tracks.push(lane);
    let mut leaning = follow(true);
    leaning.ref_track = 1;
    assert!(set_follow(&p, ClipKey { track: 2, clip: 0 }, Some(leaning)).is_err());
}

#[test]
fn a_clip_cannot_both_lock_and_follow_in_perspective() {
    // So a corner pin and a lock never stack on one clip, and their order does not matter.
    let mut p = rig(true);
    let lock = engine::TrackLock { link: follow(true).link, zoom: 1.0 };
    p.tracks[1].clips[0].lock = Some(lock);
    assert!(engine::validate(&p, &engine::AcceptAll).iter().any(|error| matches!(error, engine::ValidationError::InvalidFollow { .. } | engine::ValidationError::InvalidLock { .. })));
}

#[test]
fn between_two_samples_whose_mean_folds_the_corners_of_the_nearer_sample_hold() {
    // The second sample is the first turned half way round: half way between, every corner meets
    // in the middle, which is no quad at all.
    let mut p = rig(true);
    let a = AT_ANCHOR;
    let turned = [a[2], a[3], a[0], a[1]];
    p.motion_tracks[0].samples[1] = planar(2.0, turned);
    let track = &p.motion_tracks[0];
    assert!(track.problem().is_none());
    for at in [SECOND - FRAME, SECOND, SECOND + FRAME] {
        let quad = track.quad_at(t(at), 0.0).unwrap();
        let square = quad.map(|[x, y]| [x * ASPECT, y]);
        assert!(engine::tracker::homography::is_convex(&square), "{at}: {quad:?}");
    }
    assert_quad(track.quad_at(t(SECOND), 0.0).unwrap(), a);
    // Smoothing over both samples folds the same way.
    let smoothed = track.quad_at(t(SECOND), 1.0).unwrap();
    assert!(engine::tracker::homography::is_convex(&smoothed.map(|[x, y]| [x * ASPECT, y])), "{smoothed:?}");
}

#[test]
fn a_censor_clip_following_in_perspective_hides_the_object_quadrilateral() {
    let mut p = rig(true);
    let censor = engine::CensorContent { aspect: 1.0, ..engine::CensorContent::default() };
    p.tracks[1].clips[0].censor = Some(censor);
    let layers = layers_at_frame(&p, 30);
    let layer = layers.iter().find(|layer| layer.track == 1).expect("the censor clip shows");
    let LayerContent::Clip(sample) = layer.content else { panic!("no transition") };
    let region = engine::CensorRegion::of(&sample, &censor, ASPECT);
    let middle: [[f64; 2]; 4] = std::array::from_fn(|i| [(AT_ANCHOR[i][0] + LATER[i][0]) / 2.0, (AT_ANCHOR[i][1] + LATER[i][1]) / 2.0]);
    assert_quad(region.quad.expect("in perspective"), middle);
    // Without perspective the region stays a turned box.
    let mut plain = rig(false);
    plain.tracks[1].clips[0].censor = Some(censor);
    let layers = layers_at_frame(&plain, 30);
    let layer = layers.iter().find(|layer| layer.track == 1).unwrap();
    let LayerContent::Clip(sample) = layer.content else { panic!("no transition") };
    assert!(engine::CensorRegion::of(&sample, &censor, ASPECT).quad.is_none());
}

#[test]
fn an_effect_region_linked_in_perspective_lies_on_the_object_corners() {
    let p = rig(true);
    let clip = &p.tracks[0].clips[0];
    let link = follow(true).link;
    // The box of the object at the anchor, in parts of the picture.
    let region = engine::RegionBox { cx: 0.375, cy: (0.25 + BOTTOM) / 2.0, w: 0.25, h: BOTTOM - 0.25, angle: 0.0 };
    assert_quad(engine::follow_region_quad(&p, clip, &link, region, t(0)).unwrap(), AT_ANCHOR);
    assert_quad(engine::follow_region_quad(&p, clip, &link, region, t(2 * SECOND - FRAME)).unwrap(), {
        let k = (2 * SECOND - FRAME) as f64 / (2 * SECOND) as f64;
        std::array::from_fn(|i| [AT_ANCHOR[i][0] + (LATER[i][0] - AT_ANCHOR[i][0]) * k, AT_ANCHOR[i][1] + (LATER[i][1] - AT_ANCHOR[i][1]) * k])
    });
    // A track of boxes alone gives no quadrilateral: the region moves as a box.
    let mut boxes = rig(true);
    for sample in &mut boxes.motion_tracks[0].samples {
        sample.quad = None;
    }
    assert!(engine::follow_region_quad(&boxes, &boxes.tracks[0].clips[0], &link, region, t(SECOND)).is_none());
}
