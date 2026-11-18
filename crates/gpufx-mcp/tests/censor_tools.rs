//! The censor and tracking tools on a [`FileSession`]: censor_add, track_set_sample,
//! clip_follow_set and clip_follow_clear.

mod common;

use common::*;
use engine::Project;
use gpufx_mcp::session::FileSession;
use serde_json::json;

fn project_of(session: &mut FileSession) -> Project {
    serde_json::from_value(call_ok(session, "project_get", json!({}))["project"].clone()).expect("a project")
}

#[test]
fn censor_add_makes_a_sticker_track_and_refuses_an_unknown_mode() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let added = call_ok(&mut session, "censor_add", json!({"expected_revision": 0, "start": 0, "duration": 90_000, "shape": "ellipse", "mode": "solid", "color": [255, 0, 0, 255]}));
    assert_eq!(added["clip"], json!({"track": 0, "clip": 0}));
    let project = project_of(&mut session);
    assert_eq!(project.tracks[0].kind, engine::TrackKind::Sticker);
    let censor = project.tracks[0].clips[0].censor.expect("a censor region");
    assert_eq!((censor.shape, censor.mode), (engine::RegionShape::Ellipse, engine::CensorMode::Solid { color: [255, 0, 0, 255] }));
    let now = revision(&added);
    assert_eq!(call_err(&mut session, "censor_add", json!({"expected_revision": now, "start": 0, "duration": 90_000, "mode": "smear"})).code, "invalid_params");
    assert_eq!(call_err(&mut session, "censor_add", json!({"expected_revision": now, "start": 90_000, "duration": 90_000, "mode": "blur", "strength": 500})).code, "invalid_project");
}

#[test]
fn a_censor_region_follows_a_track_of_the_video_under_it() {
    let _lock = gpu_lock();
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let (clip_a, _) = clip_paths();
    call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    let now = revision(&call_ok(&mut session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": clip_a, "start": 0, "duration": 180_000})));
    let added = call_ok(&mut session, "censor_add", json!({"expected_revision": now, "start": 0, "duration": 180_000}));
    let started = call_ok(&mut session, "track_set_sample", json!({"expected_revision": revision(&added), "clip": {"track": 0, "clip": 0}, "source_time": 0, "cx": 0.25, "cy": 0.5, "w": 0.1, "h": 0.1}));
    let id = started["track_id"].as_u64().expect("an id");
    let more = call_ok(&mut session, "track_set_sample", json!({"expected_revision": revision(&started), "track_id": id, "source_time": 90_000, "cx": 0.75, "cy": 0.5, "w": 0.1, "h": 0.1}));
    let followed = call_ok(&mut session, "clip_follow_set", json!({"expected_revision": revision(&more), "clip": {"track": 1, "clip": 0}, "track_id": id, "reference": {"track": 0, "clip": 0}}));
    let project = project_of(&mut session);
    let follow = project.tracks[1].clips[0].follow.expect("follows");
    assert_eq!((follow.link.track_id, follow.ref_track, follow.ref_clip), (id as u32, 0, 0));
    assert!(project.motion_tracks[0].samples.iter().all(|sample| sample.manual), "entered by hand");
    call_ok(&mut session, "clip_follow_clear", json!({"expected_revision": revision(&followed), "clip": {"track": 1, "clip": 0}}));
    assert!(project_of(&mut session).tracks[1].clips[0].follow.is_none());
}
