//! The tool table on a [`FileSession`]: each tool gives the project the engine command gives, a
//! stale revision or a locked track changes nothing, and errors carry stable codes.

mod common;

use common::*;
use engine::{ClipKey, Effect, Project, Time, TrackKind, Transition};
use gpufx_mcp::session::FileSession;
use serde_json::{json, Map, Value};

fn project_of(session: &mut FileSession) -> Project {
    let value = call_ok(session, "project_get", json!({}));
    serde_json::from_value(value["project"].clone()).expect("a project")
}

fn key(track: usize, clip: usize) -> ClipKey {
    ClipKey { track, clip }
}

const SECOND: i64 = 90_000;

/// A project with one video track holding two touching clips of two seconds.
fn two_clips(session: &mut FileSession) -> u64 {
    let (a, b) = clip_paths();
    call_ok(session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    call_ok(session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": a, "start": 0, "duration": 2 * SECOND}));
    let last = call_ok(session, "clip_add", json!({"expected_revision": 2, "track": 0, "source": b, "start": 2 * SECOND, "duration": 2 * SECOND}));
    revision(&last)
}

#[test]
fn clip_trim_start_pulls_the_left_edge_of_an_image_out_with_no_source_offset() {
    // A PNG header is all a clip of an image needs to be added.
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1F, 0x15, 0xC4, 0x89, 0, 0, 0, 0]);
    png.extend_from_slice(b"IDAT");
    png.extend_from_slice(&[0x35, 0xAF, 0x06, 0x1E]);
    let dir = std::env::temp_dir().join(format!("gpufx-mcp-image-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let image = dir.join("sticker.png");
    std::fs::write(&image, &png).unwrap();

    let mut session = FileSession::with_roots(vec![packages_root()]);
    call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    let added = call_ok(&mut session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": image.to_string_lossy(), "start": 2 * SECOND, "duration": 2 * SECOND}));
    call_ok(&mut session, "clip_trim_start", json!({"expected_revision": revision(&added), "clip": {"track": 0, "clip": 0}, "start": SECOND}));
    let clip = project_of(&mut session).tracks[0].clips[0].clone();
    assert_eq!((clip.start, clip.source_in, clip.duration), (Time::from_ticks(SECOND), Time::ZERO, Time::from_ticks(3 * SECOND)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn each_edit_tool_gives_what_the_engine_command_gives() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let mut revision_now = two_clips(&mut session);
    let mut expected = project_of(&mut session);
    let t = |seconds: f64| Time::from_ticks((seconds * SECOND as f64) as i64);
    let mut step = |session: &mut FileSession, tool: &str, mut arguments: Value, next: Project| {
        arguments["expected_revision"] = json!(revision_now);
        let result = call_ok(session, tool, arguments);
        revision_now = revision(&result);
        assert_eq!(project_of(session), next, "{tool}");
        next
    };
    let clip0 = json!({"track": 0, "clip": 0});
    expected = {
        let next = engine::set_gain(&expected, key(0, 0), -6.0).unwrap();
        step(&mut session, "clip_set_gain", json!({"clip": clip0, "gain_db": -6.0}), next)
    };
    expected = {
        let next = engine::set_muted(&expected, key(0, 0), true).unwrap();
        step(&mut session, "clip_set_muted", json!({"clip": clip0, "muted": true}), next)
    };
    expected = {
        let next = engine::set_fades(&expected, key(0, 1), t(0.1), Time::ZERO).unwrap();
        step(&mut session, "clip_set_fades", json!({"clip": {"track": 0, "clip": 1}, "fade_in": 9000, "fade_out": 0}), next)
    };
    let opacity = engine::KeyframeTarget::Property(engine::ClipProperty::Opacity);
    expected = {
        let next = engine::set_keyframe(&expected, key(0, 0), &opacity, Time::from_ticks(9000), 0.25, Some(engine::Easing::EaseOut)).unwrap();
        step(&mut session, "keyframe_set", json!({"clip": clip0, "property": "opacity", "at": 9000, "value": 0.25, "easing": "ease_out"}), next)
    };
    expected = {
        let next = engine::remove_keyframe(&expected, key(0, 0), &opacity, Time::from_ticks(9000)).unwrap();
        step(&mut session, "keyframe_remove", json!({"clip": clip0, "property": "opacity", "at": 9000}), next)
    };
    expected = {
        let next = engine::set_rotation(&engine::set_opacity(&expected, key(0, 0), 0.5).unwrap(), key(0, 0), 45.0).unwrap();
        step(&mut session, "clip_set_transform", json!({"clip": clip0, "opacity": 0.5, "rotation": 45.0}), next)
    };
    expected = {
        let next = engine::trim_end(&expected, key(0, 1), t(3.5)).unwrap();
        step(&mut session, "clip_trim_end", json!({"clip": {"track": 0, "clip": 1}, "end": 315000}), next)
    };
    expected = {
        let next = engine::split_clip(&expected, key(0, 0), t(1.0)).unwrap();
        step(&mut session, "clip_split", json!({"clip": clip0, "at": SECOND}), next)
    };
    expected = {
        let next = engine::add_effect(&expected, key(0, 0), Effect { package: "invert".into(), params: Map::new(), keyframes: Default::default() }).unwrap();
        step(&mut session, "effect_add", json!({"clip": clip0, "package": "invert"}), next)
    };
    expected = {
        let next = engine::remove_effect(&expected, key(0, 0), 0).unwrap();
        step(&mut session, "effect_remove", json!({"clip": clip0, "effect": 0}), next)
    };
    expected = {
        let transition = Transition { after_clip: 0, package: "xfade".into(), duration: Time::from_ticks(18000), params: Map::new() };
        let next = engine::add_transition(&expected, 0, transition).unwrap();
        step(&mut session, "transition_add", json!({"track": 0, "after_clip": 0, "package": "xfade", "duration": 18000}), next)
    };
    expected = {
        let next = engine::remove_transition(&expected, 0, 0).unwrap();
        step(&mut session, "transition_remove", json!({"track": 0, "after_clip": 0}), next)
    };
    expected = {
        let next = engine::add_track(&expected, TrackKind::Video).unwrap();
        step(&mut session, "track_add", json!({"kind": "video"}), next)
    };
    expected = {
        let next = engine::move_clip(&expected, key(0, 0), 1, t(5.0)).unwrap();
        step(&mut session, "clip_move", json!({"clip": clip0, "track": 1, "start": 5 * SECOND}), next)
    };
    expected = {
        let next = engine::delete_clip(&expected, key(1, 0)).unwrap();
        step(&mut session, "clip_delete", json!({"clip": {"track": 1, "clip": 0}}), next)
    };
    expected = {
        let start = expected.tracks[0].clips[0].end().unwrap();
        let next = engine::trim_start(&expected, key(0, 1), start.checked_add(t(0.25)).unwrap()).unwrap();
        step(&mut session, "clip_trim_start", json!({"clip": {"track": 0, "clip": 1}, "start": start.ticks() + SECOND / 4}), next)
    };
    expected = {
        let (a, _) = clip_paths();
        let next = engine::add_clip(&expected, 0, gpufx_editor::editor::new_clip(&a, t(20.0), t(1.0))).unwrap();
        step(&mut session, "clip_add", json!({"track": 0, "source": a, "start": 20 * SECOND, "duration": SECOND}), next)
    };
    expected = {
        let next = engine::set_track_hidden(&expected, 0, true).unwrap();
        step(&mut session, "track_set_hidden", json!({"track": 0, "hidden": true}), next)
    };
    let _ = expected;
}

#[test]
fn undo_and_redo_refuse_a_stale_revision() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let current = two_clips(&mut session);
    assert_eq!(call_err(&mut session, "undo", json!({"expected_revision": current - 1})).code, "stale_revision");
    let undone = revision(&call_ok(&mut session, "undo", json!({"expected_revision": current})));
    assert_eq!(call_err(&mut session, "redo", json!({"expected_revision": undone - 1})).code, "stale_revision");
}

#[test]
fn a_time_at_or_past_the_end_of_the_timeline_is_out_of_range() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    two_clips(&mut session);
    let end = 4 * SECOND;
    assert_eq!(call_err(&mut session, "frame_capture", json!({"time": end})).code, "out_of_range");
    assert_eq!(call_err(&mut session, "frame_capture", json!({"time": end + SECOND})).code, "out_of_range");
}

#[test]
fn opening_over_a_changed_project_needs_consent() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    two_clips(&mut session);
    assert_eq!(call_err(&mut session, "project_open", json!({"path": "D:/nothing/project.json"})).code, "unsaved_changes");
}

#[test]
fn an_export_output_that_is_a_source_is_refused_even_with_overwrite() {
    let dir = scratch("export-source");
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let source = project_with_copy(&mut session, &dir);
    let size = std::fs::metadata(&source).unwrap().len();
    assert_eq!(call_err(&mut session, "export_start", json!({"out": source, "overwrite": true})).code, "invalid_output");
    assert_eq!(std::fs::metadata(&source).unwrap().len(), size);
}

#[test]
fn a_stale_revision_is_refused_and_nothing_changes() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let current = two_clips(&mut session);
    let before = project_of(&mut session);
    let error = call_err(&mut session, "clip_delete", json!({"expected_revision": current - 1, "clip": {"track": 0, "clip": 0}}));
    assert_eq!(error.code, "stale_revision");
    assert_eq!(error.data["revision"], json!(current), "the error tells the revision to use");
    assert_eq!(project_of(&mut session), before);
}

#[test]
fn a_locked_track_is_refused_with_its_code_and_lock_is_not_an_undo_step() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let current = two_clips(&mut session);
    let locked = revision(&call_ok(&mut session, "track_set_locked", json!({"expected_revision": current, "track": 0, "locked": true})));
    let before = project_of(&mut session);
    let error = call_err(&mut session, "clip_delete", json!({"expected_revision": locked, "clip": {"track": 0, "clip": 0}}));
    assert_eq!(error.code, "track_locked");
    assert_eq!(project_of(&mut session), before);
    assert_eq!(project_of(&mut session).tracks[0].locked, true);
    let undone = call_ok(&mut session, "undo", json!({"expected_revision": locked}));
    assert!(project_of(&mut session).tracks[0].locked, "undo steps over the lock");
    assert_eq!(project_of(&mut session).tracks[0].clips.len(), 1, "undo took back the clip added last");
    let redone = revision(&call_ok(&mut session, "redo", json!({"expected_revision": revision(&undone)})));
    assert_eq!(project_of(&mut session).tracks[0].clips.len(), 2);
    let hidden = revision(&call_ok(&mut session, "track_set_hidden", json!({"expected_revision": redone, "track": 0, "hidden": true})));
    assert!(project_of(&mut session).tracks[0].hidden, "hiding stays possible");
    assert!(hidden > redone);
}

#[test]
fn new_clips_and_split_halves_return_the_key_to_use() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let current = two_clips(&mut session);
    let split = call_ok(&mut session, "clip_split", json!({"expected_revision": current, "clip": {"track": 0, "clip": 1}, "at": 3 * SECOND}));
    assert_eq!(split["clip"], json!({"track": 0, "clip": 2}));
    let (a, _) = clip_paths();
    let added = call_ok(&mut session, "clip_add", json!({"expected_revision": revision(&split), "track": 0, "source": a, "start": 4 * SECOND, "duration": SECOND}));
    assert_eq!(added["clip"], json!({"track": 0, "clip": 3}));
}

#[test]
fn clip_add_takes_the_length_of_the_stream_and_refuses_a_missing_file() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    let (a, _) = clip_paths();
    call_ok(&mut session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": a, "start": 0}));
    assert!(project_of(&mut session).tracks[0].clips[0].duration > Time::ZERO);
    let error = call_err(&mut session, "clip_add", json!({"expected_revision": 2, "track": 0, "source": "D:/nothing/here.mp4", "start": 9000000}));
    assert_eq!(error.code, "source_not_found");
}

#[test]
fn an_effect_outside_the_package_roots_is_refused() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let current = two_clips(&mut session);
    let error = call_err(&mut session, "effect_add", json!({"expected_revision": current, "clip": {"track": 0, "clip": 0}, "package": "../../secret"}));
    assert_eq!(error.code, "package_not_found");
}

#[test]
fn saving_opening_and_the_lock_between_two_sessions() {
    let dir = scratch("save-open");
    let file = dir.join("project.json");
    let mut first = FileSession::with_roots(vec![packages_root()]);
    two_clips(&mut first);
    let saved = call_ok(&mut first, "project_save", json!({"path": file}));
    assert_eq!(saved["path"], json!(file));
    assert_eq!(call_ok(&mut first, "session_status", json!({}))["dirty"], json!(false));
    let mut second = FileSession::with_roots(vec![packages_root()]);
    let refused = call_err(&mut second, "project_open", json!({"path": file}));
    assert_eq!(refused.code, "project_locked", "two sessions must not overwrite each other");
    call_ok(&mut first, "project_new", json!({}));
    let opened = project_open_get(&mut second, &file);
    assert_eq!(opened.tracks[0].clips.len(), 2);
    assert!(std::path::Path::new(&opened.tracks[0].clips[0].source).is_absolute());
}

fn project_open_get(session: &mut FileSession, file: &std::path::Path) -> Project {
    call_ok(session, "project_open", json!({"path": file}));
    project_of(session)
}

#[test]
fn replacing_a_changed_project_needs_consent() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    two_clips(&mut session);
    assert_eq!(call_err(&mut session, "project_new", json!({})).code, "unsaved_changes");
    call_ok(&mut session, "project_new", json!({"discard_changes": true}));
    assert!(project_of(&mut session).tracks.is_empty());
}

/// A project with a clip of a private copy of a test clip; returns the copy.
fn project_with_copy(session: &mut FileSession, dir: &std::path::Path) -> std::path::PathBuf {
    let (a, _) = clip_paths();
    let source = dir.join("source.mp4");
    std::fs::copy(a, &source).unwrap();
    call_ok(session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    call_ok(session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": source, "start": 0, "duration": 2 * SECOND}));
    source
}

#[test]
fn saving_refuses_a_source_and_asks_before_replacing_another_file() {
    let dir = scratch("save-guard");
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let source = project_with_copy(&mut session, &dir);
    let size = std::fs::metadata(&source).unwrap().len();
    assert_eq!(call_err(&mut session, "project_save", json!({"path": source, "overwrite": true})).code, "invalid_output");
    assert_eq!(call_err(&mut session, "project_save", json!({"path": dir.join("SOURCE.MP4")})).code, "invalid_output", "whatever its case");
    assert_eq!(std::fs::metadata(&source).unwrap().len(), size, "the source is intact");

    let other = dir.join("other.json");
    std::fs::write(&other, "x").unwrap();
    assert_eq!(call_err(&mut session, "project_save", json!({"path": other})).code, "file_exists");
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "x");
    call_ok(&mut session, "project_save", json!({"path": other, "overwrite": true}));
    assert!(std::fs::read_to_string(&other).unwrap().len() > 1);
    call_ok(&mut session, "project_save", json!({"path": other}));
    call_ok(&mut session, "project_save", json!({"path": dir.join("OTHER.JSON")}));
}

#[test]
fn an_export_of_a_broken_project_is_a_project_problem() {
    let dir = scratch("export-invalid");
    let mut session = FileSession::with_roots(vec![packages_root()]);
    let source = project_with_copy(&mut session, &dir);
    std::fs::remove_file(&source).unwrap();
    assert_eq!(call_err(&mut session, "export_start", json!({"out": dir.join("out.mp4")})).code, "invalid_project");
}

#[test]
fn clip_add_measures_what_remains_after_source_in() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    call_ok(&mut session, "track_add", json!({"expected_revision": 0, "kind": "video"}));
    let (a, _) = clip_paths();
    let added = call_ok(&mut session, "clip_add", json!({"expected_revision": 1, "track": 0, "source": a, "start": 0}));
    let length = project_of(&mut session).tracks[0].clips[0].duration.ticks();
    let trimmed = call_ok(&mut session, "clip_add", json!({"expected_revision": revision(&added), "track": 0, "source": a, "start": length, "source_in": 30_000}));
    let project = project_of(&mut session);
    assert_eq!(project.tracks[0].clips[1].duration.ticks(), length - 30_000);
    assert_eq!(project.tracks[0].clips[1].source_in.ticks(), 30_000);
    let current = revision(&trimmed);
    for source_in in [length, length + 1] {
        let error = call_err(&mut session, "clip_add", json!({"expected_revision": current, "track": 0, "source": a, "start": 20 * length, "source_in": source_in}));
        assert_eq!(error.code, "out_of_range", "{source_in}");
    }
}

#[test]
fn bad_arguments_are_a_parameter_error_and_packages_can_be_listed() {
    let mut session = FileSession::with_roots(vec![packages_root()]);
    assert_eq!(call_err(&mut session, "clip_split", json!({"expected_revision": 0})).code, "invalid_params");
    assert_eq!(call_err(&mut session, "project_new", json!({"discard": true})).code, "invalid_params", "a misnamed parameter");
    assert_eq!(call_err(&mut session, "clip_delete", json!({"expected_revision": 0, "clip": {"track": 0, "clip": 0, "extra": 1}})).code, "invalid_params", "a misnamed field of a clip");
    let listed = call_ok(&mut session, "packages_list", json!({"filter": "xfade"}));
    assert!(listed["transitions"].as_array().unwrap().iter().any(|entry| entry["package"].as_str().unwrap().contains("xfade")), "{listed}");
}

