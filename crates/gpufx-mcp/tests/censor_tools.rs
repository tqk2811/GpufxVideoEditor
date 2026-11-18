//! The censor and tracking tools on a [`FileSession`]: censor_add, track_set_sample,
//! clip_follow_set, clip_follow_clear, clip_lock_set, clip_lock_clear, track_run and censor_faces.

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
    let cleared = call_ok(&mut session, "clip_follow_clear", json!({"expected_revision": revision(&followed), "clip": {"track": 1, "clip": 0}}));
    assert!(project_of(&mut session).tracks[1].clips[0].follow.is_none());

    // Lock the video itself to the track, then bake and undo, then clear.
    let video = json!({"track": 0, "clip": 0});
    assert_eq!(call_err(&mut session, "clip_lock_set", json!({"expected_revision": revision(&cleared), "clip": video, "track_id": id, "zoom": 50.0})).code, "invalid_project");
    let locked = call_ok(&mut session, "clip_lock_set", json!({"expected_revision": revision(&cleared), "clip": video, "track_id": id, "at": 45_000, "lock_rotation": true, "zoom": 1.25}));
    let lock = project_of(&mut session).tracks[0].clips[0].lock.expect("locked");
    assert_eq!((lock.link.track_id, lock.link.anchor_source_time.ticks(), lock.link.follow_scale, lock.link.follow_rotation, lock.zoom), (id as u32, 45_000, false, true, 1.25));
    let baked = call_ok(&mut session, "track_bake_keyframes", json!({"expected_revision": revision(&locked), "clip": video}));
    let project = project_of(&mut session);
    assert!(project.tracks[0].clips[0].lock.is_none() && project.tracks[0].clips[0].motion.has_keyframes(engine::ClipProperty::Width), "the zoom bakes to width");
    let undone = call_ok(&mut session, "undo", json!({"expected_revision": revision(&baked)}));
    call_ok(&mut session, "clip_lock_clear", json!({"expected_revision": revision(&undone), "clip": video}));
    assert!(project_of(&mut session).tracks[0].clips[0].lock.is_none());
}


#[test]
fn track_create_takes_four_corners_and_clip_follow_set_pins_in_perspective() {
    let _lock = gpu_lock();
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let (clip_a, _) = clip_paths();
    call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    let now = revision(&call_ok(&mut session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": clip_a, "start": 0, "duration": 180_000})));
    let added = call_ok(&mut session, "censor_add", json!({"expected_revision": now, "start": 0, "duration": 180_000}));
    let video = json!({"track": 0, "clip": 0});
    assert_eq!(call_err(&mut session, "track_create", json!({"expected_revision": revision(&added), "clip": video, "cx": 0.5})).code, "invalid_params", "neither a whole box nor corners");
    let folded = [[0.2, 0.2], [0.6, 0.6], [0.6, 0.2], [0.2, 0.6]];
    assert_eq!(call_err(&mut session, "track_create", json!({"expected_revision": revision(&added), "clip": video, "quad": folded})).code, "invalid_project");
    let quad = [[0.2, 0.2], [0.6, 0.25], [0.6, 0.6], [0.2, 0.7]];
    let created = call_ok(&mut session, "track_create", json!({"expected_revision": revision(&added), "clip": video, "quad": quad}));
    let id = created["track_id"].as_u64().expect("an id");
    let project = project_of(&mut session);
    let sample = project.motion_tracks[0].samples[0];
    assert_eq!(sample.quad, Some(quad));
    assert!((sample.cx - 0.4).abs() < 1e-9 && (sample.cy - (0.2 + 0.25 + 0.6 + 0.7) / 4.0).abs() < 1e-9, "the box comes from the corners: {sample:?}");
    let followed = call_ok(&mut session, "clip_follow_set", json!({"expected_revision": revision(&created), "clip": {"track": 1, "clip": 0}, "track_id": id, "reference": video, "perspective": true}));
    assert!(project_of(&mut session).tracks[1].clips[0].follow.expect("follows").perspective);
    let more = call_ok(&mut session, "track_set_sample", json!({"expected_revision": revision(&followed), "track_id": id, "source_time": 90_000, "quad": [[0.3, 0.2], [0.7, 0.25], [0.7, 0.6], [0.3, 0.7]]}));
    assert!(revision(&more) > revision(&followed));
    assert_eq!(project_of(&mut session).motion_tracks[0].samples.len(), 2);
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

#[test]
fn track_create_runs_both_ways_then_smooth_and_bake_reach_the_follower() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let clip = moving_square_clip();
    call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    let added = call_ok(&mut session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": clip, "start": 0, "duration": 180_000}));
    // At 1 s (frame 30) the square's middle is at x = 40 + 60 + 32.
    let created = call_ok(&mut session, "track_create", json!({"expected_revision": revision(&added), "clip": {"track": 0, "clip": 0}, "at": 90_000, "cx": 132.0 / 320.0, "cy": 92.0 / 180.0, "w": 0.2, "h": 64.0 / 180.0}));
    let id = created["track_id"].as_u64().expect("an id");
    assert_eq!(created["source_time"], 90_000);
    assert_eq!(call_err(&mut session, "track_create", json!({"expected_revision": revision(&created), "clip": {"track": 0, "clip": 0}, "at": 900_000, "cx": 0.5, "cy": 0.5, "w": 0.1, "h": 0.1})).code, "invalid_params");
    let run = call_ok(&mut session, "track_run", json!({"expected_revision": revision(&created), "track_id": id, "direction": "both", "rotation": false}));
    assert_eq!(run["state"], "done", "{run}");
    assert_eq!(run["count"], 29, "frames 31 to 59");
    assert_eq!(run["backward"]["state"], "done", "{run}");
    assert_eq!(run["backward"]["count"], 30, "frames 29 down to 0");
    let samples = project_of(&mut session).motion_tracks[0].samples.clone();
    assert_eq!(samples.len(), 60);
    let first = samples[0];
    assert!(((first.cx - 72.0 / 320.0) * 320.0).abs() < 1.0, "{first:?}");

    let now = revision(&call_ok(&mut session, "project_get", json!({})));
    let censor = call_ok(&mut session, "censor_add", json!({"expected_revision": now, "start": 0, "duration": 180_000}));
    let followed = call_ok(&mut session, "clip_follow_set", json!({"expected_revision": revision(&censor), "clip": {"track": 1, "clip": 0}, "track_id": id, "reference": {"track": 0, "clip": 0}}));
    let smoothed = call_ok(&mut session, "track_smooth", json!({"expected_revision": revision(&followed), "track_id": id, "smooth": 0.25}));
    assert_eq!(project_of(&mut session).tracks[1].clips[0].follow.expect("follows").link.smooth, 0.25);
    assert_eq!(call_err(&mut session, "track_smooth", json!({"expected_revision": revision(&smoothed), "track_id": id, "smooth": 9.0})).code, "invalid_project");
    let baked = call_ok(&mut session, "track_bake_keyframes", json!({"expected_revision": revision(&smoothed), "clip": {"track": 1, "clip": 0}}));
    let project = project_of(&mut session);
    let overlay = &project.tracks[1].clips[0];
    assert!(overlay.follow.is_none());
    let xs = &overlay.motion.keyframes[&engine::ClipProperty::X];
    assert!(xs.len() >= 60 && xs.last().unwrap().value > xs[0].value + 0.2, "the box moves right: {} .. {}", xs[0].value, xs.last().unwrap().value);
    assert_eq!(call_err(&mut session, "track_bake_keyframes", json!({"expected_revision": revision(&baked), "clip": {"track": 1, "clip": 0}})).code, "not_following");
    call_ok(&mut session, "undo", json!({"expected_revision": revision(&baked)}));
    assert!(project_of(&mut session).tracks[1].clips[0].follow.is_some(), "the bake is one undo step");
}

#[test]
fn censor_faces_refuses_what_is_not_a_video_and_finds_the_faces_of_one() {
    let _lock = gpu_lock();
    // The face detector built in the FFmpegGpuFilter checkout, when there is one.
    let built = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../external/FFmpegGpuFilter/build/packages/gpufx_ai_tracking");
    if built.join("yunet-2023mar.zip").is_file() && std::env::var_os(gpufx_editor::tracking_models::TRACKING_MODELS_ENV).is_none() {
        std::env::set_var(gpufx_editor::tracking_models::TRACKING_MODELS_ENV, &built);
    }
    let mut session = FileSession::with_roots(vec![packages_root()]);
    assert_eq!(call_err(&mut session, "censor_faces_status", json!({})).code, "job_not_found");
    let (clip_a, _) = clip_paths();
    call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    let now = revision(&call_ok(&mut session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": clip_a, "start": 0, "duration": 180_000})));
    let added = call_ok(&mut session, "censor_add", json!({"expected_revision": now, "start": 0, "duration": 180_000}));
    let now = revision(&added);
    let region = json!({"track": 1, "clip": 0});
    assert_eq!(call_err(&mut session, "censor_faces", json!({"expected_revision": now, "clip": region})).code, "invalid_params", "a censor region is not a video");
    let video = json!({"track": 0, "clip": 0});
    assert_eq!(call_err(&mut session, "censor_faces", json!({"expected_revision": now, "clip": video, "mode": "smear"})).code, "invalid_params");
    assert_eq!(call_err(&mut session, "censor_faces", json!({"expected_revision": now, "clip": video, "feather": 3.0})).code, "invalid_params");
    if gpufx_editor::tracking_models::installed(gpufx_editor::tracking_models::TrackingModel::FaceDetector, &gpufx_editor::project_file::package_roots()).is_none() {
        assert_eq!(call_err(&mut session, "censor_faces", json!({"expected_revision": now, "clip": video})).code, "invalid_params", "no model");
        eprintln!("skipping the face detector: yunet-2023mar.zip is not built");
        return;
    }
    let answer = call(&mut session, "censor_faces", json!({"expected_revision": now, "clip": video, "shape": "rect", "mode": "pixelate"}));
    match answer {
        // a machine without a GPU
        Ok(failed) if failed["state"] == "failed" => assert!(std::env::var_os("GPUFX_REQUIRE_GPU").is_none() && failed["message"].as_str().unwrap_or("").contains("GPU"), "{failed}"),
        Ok(done) => {
            assert_eq!(done["state"], "done", "{done}");
            assert_eq!(done["faces"].as_u64(), Some(done["track_ids"].as_array().unwrap().len() as u64));
            assert_eq!(call_ok(&mut session, "censor_faces_status", json!({}))["state"], "done");
        }
        Err(error) => panic!("{error:?}"),
    }
}
