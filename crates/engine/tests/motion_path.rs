//! Motion paths through the transform, the file format, validation and the edit commands.

mod common;

use common::*;
use engine::{set_motion_path, set_rotation, ClipKey, EditError, EditHistory, MotionPath, PathPreset, Project, ValidationError};

const KEY: ClipKey = ClipKey { track: 0, clip: 0 };

/// One clip of 2 seconds.
fn one_clip() -> Project {
    project(3_000, vec![track(vec![clip("a.mp4", 0, 0, 180_000)], vec![])])
}

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() < 1e-6, "{what}: {a} != {b}");
}

#[test]
fn the_path_shifts_the_picture_after_its_place() {
    let base = one_clip();
    let still = base.tracks[0].clips[0].transform_at(t(45_000)).rect(16.0 / 9.0, 16.0 / 9.0);
    let moved = set_motion_path(&base, KEY, Some(MotionPath::new("0.1*t", "0-p", None))).unwrap();
    let clip = &moved.tracks[0].clips[0];
    let transform = clip.transform_at(t(45_000));
    let rect = transform.rect(16.0 / 9.0, 16.0 / 9.0);
    close(rect.x - still.x, 0.05, "x after 0.5 s");
    close(rect.y - still.y, -0.25, "y at a quarter of the clip");
    assert!(!transform.rotates);
}

#[test]
fn the_rotation_adds_to_the_rotation_and_dir_turns_with_the_heading() {
    let base = set_rotation(&one_clip(), KEY, 10.0).unwrap();
    let moved = set_motion_path(&base, KEY, Some(MotionPath::new("", "t", Some("dir".into())))).unwrap();
    let transform = moved.tracks[0].clips[0].transform_at(t(90_000));
    close(transform.rotation, 100.0, "10 plus heading down");
    assert!(transform.rotates);
}

#[test]
fn a_path_that_does_not_parse_is_refused() {
    let error = set_motion_path(&one_clip(), KEY, Some(MotionPath::new("t", "", Some("dirr".into())))).unwrap_err();
    match error {
        EditError::Invalid(errors) => assert!(matches!(&errors[..], [ValidationError::InvalidMotionPath { field: "rotation", .. }]), "{errors:?}"),
        other => panic!("{other}"),
    }
}

#[test]
fn the_path_saves_loads_and_undoes() {
    let mut history = EditHistory::new(one_clip(), 10);
    history.apply(|p| set_motion_path(p, KEY, Some(PathPreset::Spiral.path()))).unwrap();
    let text = serde_json::to_string(history.current()).unwrap();
    let loaded = Project::from_json(&text).unwrap();
    assert_eq!(loaded.tracks[0].clips[0].motion.path, Some(PathPreset::Spiral.path()));
    let offset = loaded.tracks[0].clips[0].motion.path.as_ref().unwrap().offset_at(1.0, 2.0);
    close(offset[0], 0.15, "spiral x halfway");
    history.apply(|p| set_motion_path(p, KEY, None)).unwrap();
    assert_eq!(history.current().tracks[0].clips[0].motion.path, None);
    assert!(history.undo());
    assert_eq!(history.current().tracks[0].clips[0].motion.path, Some(PathPreset::Spiral.path()));
    assert!(history.undo());
    assert!(history.current().tracks[0].clips[0].motion.is_default());
}
