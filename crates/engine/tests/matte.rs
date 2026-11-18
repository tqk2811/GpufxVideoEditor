//! Per-clip AI background removal settings: how they are stored, edited and checked.

mod common;

use common::*;
use engine::{set_matte, validate, AcceptAll, ClipMatte, EditHistory, MatteModel, Output, Project, Track, TrackKind, ValidationError, SUPPORTED_VERSION};
use engine::render::ClipKey;

fn project_of(clips: Vec<engine::Clip>) -> Project {
    Project {
        version: SUPPORTED_VERSION,
        output: Output { width: 16, height: 16, frame_rate: engine::FrameRate::from_frame_duration(t(3000)).unwrap(), audio: None },
        tracks: vec![Track { kind: TrackKind::Video, clips, transitions: Vec::new(), locked: false, hidden: false, subtitle: None }],
        markers: Vec::new(),
        beats: Default::default(),
        motion_tracks: Vec::new(),
    }
}

fn key() -> ClipKey {
    ClipKey { track: 0, clip: 0 }
}

fn matte(model: MatteModel) -> ClipMatte {
    ClipMatte { model, threshold: 0.5, feather: 2.5, edge: -1.0, invert: true, temporal: 2 }
}

#[test]
fn an_off_matte_is_left_out_of_the_file() {
    let project = project_of(vec![clip("a.mp4", 0, 0, 90_000)]);
    let json = serde_json::to_string(&project).unwrap();
    assert!(!json.contains("matte"), "{json}");
}

#[test]
fn a_matte_survives_a_round_trip() {
    let set = set_matte(&project_of(vec![clip("a.mp4", 0, 0, 90_000)]), key(), matte(MatteModel::Quality)).unwrap();
    let json = serde_json::to_string(&set).unwrap();
    assert!(json.contains(r#""matte":{"model":"quality","threshold":0.5,"feather":2.5,"edge":-1.0,"invert":true,"temporal":2}"#), "{json}");
    assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), set);
    // Defaults inside are left out.
    let plain = ClipMatte { model: MatteModel::Fast, ..ClipMatte::default() };
    let set = set_matte(&set, key(), plain).unwrap();
    let json = serde_json::to_string(&set).unwrap();
    assert!(json.contains(r#""matte":{"model":"fast"}"#), "{json}");
    assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), set);
}

#[test]
fn out_of_range_matte_values_are_refused() {
    let project = project_of(vec![clip("a.mp4", 0, 0, 90_000)]);
    assert!(set_matte(&project, key(), matte(MatteModel::Fast)).is_ok());
    let cases = [
        ClipMatte { threshold: 1.5, ..matte(MatteModel::Fast) },
        ClipMatte { threshold: f64::NAN, ..matte(MatteModel::Fast) },
        ClipMatte { feather: -1.0, ..matte(MatteModel::Fast) },
        ClipMatte { feather: 21.0, ..matte(MatteModel::Fast) },
        ClipMatte { edge: -21.0, ..matte(MatteModel::Fast) },
        ClipMatte { temporal: 5, ..matte(MatteModel::Fast) },
    ];
    for case in cases {
        assert!(set_matte(&project, key(), case).is_err(), "{case:?}");
    }
    let mut bad = project.clone();
    bad.tracks[0].clips[0].matte.feather = 100.0;
    let errors = validate(&bad, &AcceptAll);
    assert!(errors.iter().any(|e| matches!(e, ValidationError::InvalidMatte { field: "feather", .. })), "{errors:?}");
}

#[test]
fn a_matte_edit_undoes_and_redoes() {
    let project = project_of(vec![clip("a.mp4", 0, 0, 90_000)]);
    let mut history = EditHistory::new(project.clone(), 16);
    history.apply(|p| set_matte(p, key(), matte(MatteModel::Quality))).unwrap();
    assert_eq!(history.current().tracks[0].clips[0].matte, matte(MatteModel::Quality));
    assert!(history.undo());
    assert!(history.current().tracks[0].clips[0].matte.is_off());
    assert!(history.redo());
    assert_eq!(history.current().tracks[0].clips[0].matte.model, MatteModel::Quality);
}

#[test]
fn each_model_names_its_package() {
    assert_eq!(MatteModel::Off.package_id(), None);
    assert_eq!(MatteModel::Quality.package_id(), Some("modnet-webcam"));
    assert_eq!(MatteModel::Fast.package_id(), Some("pp-humanseg-v2-lite"));
}
