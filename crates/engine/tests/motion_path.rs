//! Motion paths through the transform, the file format, validation and the edit commands.

mod common;

use common::*;
use engine::{layers_at_frame, set_motion_path, set_placement, set_rotation, split_clip, trim_end, trim_start, ClipKey, ClipPlacement, ClipTransform, EditError, EditHistory, LayerContent, MotionPath, PathPreset, Project, ValidationError};

const KEY: ClipKey = ClipKey { track: 0, clip: 0 };

/// One clip of 2 seconds.
fn one_clip() -> Project {
    project(3_000, vec![track(vec![clip("a.mp4", 0, 0, 180_000)], vec![])])
}

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() < 1e-6, "{what}: {a} != {b}");
}

const ASPECT: f64 = 16.0 / 9.0;

/// The transform the renderer draws the clip of the only track with, at frame `frame` (frames of
/// 3 000 ticks).
fn drawn(project: &Project, frame: i64) -> ClipTransform {
    match &layers_at_frame(project, frame)[..] {
        [layer] => match &layer.content {
            LayerContent::Clip(sample) => sample.transform,
            _ => panic!("a transition"),
        },
        other => panic!("{} layers", other.len()),
    }
}

/// The middle of the drawn picture at frame `frame`.
fn drawn_middle(project: &Project, frame: i64) -> [f64; 2] {
    let rect = drawn(project, frame).rect(ASPECT, ASPECT);
    [rect.x + rect.width / 2.0, rect.y + rect.height / 2.0]
}

#[test]
fn the_path_shifts_the_drawn_picture_after_its_place() {
    let base = one_clip();
    let still = drawn(&base, 15).rect(ASPECT, ASPECT);
    let moved = set_motion_path(&base, KEY, Some(MotionPath::new("0.1*t", "0-p", None))).unwrap();
    let transform = drawn(&moved, 15);
    let rect = transform.rect(ASPECT, ASPECT);
    close(rect.x - still.x, 0.05, "x after 0.5 s");
    close(rect.y - still.y, -0.25, "y at a quarter of the clip");
    assert!(!transform.rotates);
}

#[test]
fn the_resting_place_leaves_the_path_out() {
    let moved = set_motion_path(&one_clip(), KEY, Some(MotionPath::new("0.3", "0.2", Some("45".into())))).unwrap();
    let clip = &moved.tracks[0].clips[0];
    let resting = clip.transform_at(t(45_000));
    assert_eq!(resting, one_clip().tracks[0].clips[0].transform_at(t(45_000)), "the place a drag or a bake edits");
    assert_eq!(clip.path_pose_at(t(45_000), ASPECT), Some(([0.3, 0.2], 45.0)));
    // A drag writes the place the picture rests at; the path still moves it from there.
    let placed = set_placement(&moved, KEY, Some(ClipPlacement { x: 0.1, y: 0.1, width: 0.5 })).unwrap();
    let rect = drawn(&placed, 15).rect(ASPECT, ASPECT);
    close(rect.x, 0.4, "placement x plus path");
    close(rect.y, 0.3, "placement y plus path");
    close(placed.tracks[0].clips[0].transform_at(t(45_000)).placement_now(ASPECT, ASPECT).x, 0.1, "the resting x is the placement");
}

#[test]
fn the_rotation_adds_to_the_rotation_and_dir_turns_with_the_heading() {
    let base = set_rotation(&one_clip(), KEY, 10.0).unwrap();
    let moved = set_motion_path(&base, KEY, Some(MotionPath::new("", "t", Some("dir".into())))).unwrap();
    let transform = drawn(&moved, 30);
    close(transform.rotation, 100.0, "10 plus heading down");
    assert!(transform.rotates);
}

#[test]
fn dir_is_the_heading_on_screen() {
    // As many frame widths as heights per second: on a 16:9 frame that runs flatter than 45 degrees.
    let moved = set_motion_path(&one_clip(), KEY, Some(MotionPath::new("0.1*t", "0.1*t", Some("dir".into())))).unwrap();
    close(drawn(&moved, 30).rotation, (9.0f64 / 16.0).atan().to_degrees(), "heading on screen");
}

#[test]
fn a_split_keeps_the_motion_going() {
    let moved = set_motion_path(&one_clip(), KEY, Some(MotionPath::new("0.2*p", "0.1*t", None))).unwrap();
    let before: Vec<[f64; 2]> = (0..60).map(|frame| drawn_middle(&moved, frame)).collect();
    let split = split_clip(&moved, KEY, t(90_000)).unwrap();
    assert_eq!(split.tracks[0].clips.len(), 2);
    for frame in 0..60 {
        let after = drawn_middle(&split, frame);
        close(after[0], before[frame as usize][0], &format!("x at frame {frame}"));
        close(after[1], before[frame as usize][1], &format!("y at frame {frame}"));
    }
}

#[test]
fn trimming_keeps_the_motion_where_it_was() {
    let moved = set_motion_path(&one_clip(), KEY, Some(MotionPath::new("0.2*p", "0.1*t", None))).unwrap();
    let at_40 = drawn_middle(&moved, 40);
    let trimmed = trim_start(&moved, KEY, t(60_000)).unwrap();
    let after = drawn_middle(&trimmed, 40);
    close(after[0], at_40[0], "x after a start trim");
    close(after[1], at_40[1], "y after a start trim");
    let at_10 = drawn_middle(&moved, 10);
    let shorter = trim_end(&moved, KEY, t(120_000)).unwrap();
    let after = drawn_middle(&shorter, 10);
    close(after[0], at_10[0], "p keeps the old length after an end trim");
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
