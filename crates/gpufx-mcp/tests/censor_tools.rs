//! The censor and tracking tools on a [`FileSession`]: censor_add, track_set_sample,
//! clip_follow_set, clip_follow_clear and track_run.

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

/// A 2 s, 30 fps, 320x180 clip of a 64x64 textured square on gray whose left edge is at
/// x = 40 + 2n in frame n and top at y = 60.
fn moving_square_clip() -> String {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("tracking-moving-square.mp4");
    if !path.is_file() {
        let ffmpeg = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/ffmpeg/bin/ffmpeg.exe"));
        let graph = "color=c=0x808080:s=320x180:r=30:d=2[bg];color=s=64x64:r=30:d=2,format=gray,geq=lum='128+60*sin(X/3)*cos(Y/4)+40*sin((X+Y)/5)'[fg];[bg][fg]overlay=x='40+2*n':y=60:shortest=1,format=yuv420p";
        let status = std::process::Command::new(ffmpeg).args(["-hide_banner", "-y", "-v", "error", "-f", "lavfi", "-i", graph, "-c:v", "libopenh264", "-b:v", "4M", "-g", "30"]).arg(&path).status().expect("ffmpeg runs");
        assert!(status.success(), "making {} failed", path.display());
    }
    path.to_string_lossy().into_owned()
}

#[test]
fn track_run_follows_an_object_through_the_file_as_one_undo_step() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let clip = moving_square_clip();
    call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    let added = call_ok(&mut session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": clip, "start": 0, "duration": 180_000}));
    let started = call_ok(&mut session, "track_set_sample", json!({"expected_revision": revision(&added), "clip": {"track": 0, "clip": 0}, "source_time": 0, "cx": 72.0 / 320.0, "cy": 92.0 / 180.0, "w": 0.2, "h": 64.0 / 180.0}));
    let id = started["track_id"].as_u64().expect("an id");
    let before = revision(&started);
    assert_eq!(call_err(&mut session, "track_run", json!({"expected_revision": before, "track_id": id, "from": 12_345})).code, "invalid_params");
    assert_eq!(call_err(&mut session, "track_run", json!({"expected_revision": before, "track_id": id, "direction": "sideways"})).code, "invalid_params");
    let run = call_ok(&mut session, "track_run", json!({"expected_revision": before, "track_id": id, "rotation": false}));
    assert_eq!(run["state"], "done", "{run}");
    assert_eq!(run["count"], 59, "frames 1 to 59 after the sample at frame 0");
    let samples = project_of(&mut session).motion_tracks[0].samples.clone();
    assert_eq!(samples.len(), 60);
    for sample in &samples[1..] {
        let frame = sample.source_time.ticks() as f64 / 3000.0;
        let expected = (40.0 + 2.0 * frame.round() + 32.0) / 320.0;
        assert!(((sample.cx - expected) * 320.0).abs() < 1.0, "frame {frame}: cx {} vs {expected}", sample.cx);
        assert!(((sample.cy - 92.0 / 180.0) * 180.0).abs() < 1.0, "frame {frame}: cy {}", sample.cy);
        assert!(!sample.manual && sample.confidence > 0.7, "{sample:?}");
    }
    assert_eq!(call_ok(&mut session, "track_status", json!({}))["state"], "done");
    call_ok(&mut session, "undo", json!({"expected_revision": revision(&run)}));
    assert_eq!(project_of(&mut session).motion_tracks[0].samples.len(), 1, "one undo step takes the run back");
    assert_eq!(call_err(&mut session, "track_status", json!({"job": 99})).code, "job_not_found");
}
