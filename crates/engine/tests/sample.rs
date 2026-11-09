//! The sample project JSON parses, validates and schedules.

mod common;

use common::*;
use engine::{frame_count, layers_at_frame, validate, LayerContent, Project};

const SAMPLE: &str = include_str!("data/sample.json");

#[test]
fn sample_project_parses() {
    let p = Project::from_json(SAMPLE).expect("sample parses");
    assert_eq!(p.version, 2, "a version 1 file loads as the current version");
    assert_eq!(
        (p.output.width, p.output.height, p.output.frame_duration),
        (1920, 1080, t(3000))
    );
    assert_eq!(p.tracks.len(), 2);
    assert_eq!(p.tracks[0].clips.len(), 2);
    assert_eq!(p.tracks[0].clips[0].source_in, t(90_000));
    assert_eq!(p.tracks[0].clips[1].effects[0].package, "blur");
    assert_eq!(p.tracks[0].transitions[0].after_clip, 0);
    assert_eq!(
        p.tracks[1].transitions.len(),
        0,
        "transitions default to empty"
    );
}

#[test]
fn sample_project_is_valid_and_schedules() {
    let p = Project::from_json(SAMPLE).unwrap();
    let resolver = SetResolver {
        sources: vec!["a.mp4", "b.mp4", "overlay.png"],
        packages: vec!["mix", "blur"],
        without_audio: Vec::new(),
    };
    assert_eq!(validate(&p, &resolver), vec![]);
    assert_eq!(frame_count(&p), 240);
    // Frame 40 = 120000: clip a on track 0 and the overlay on track 1.
    let layers = layers_at_frame(&p, 40);
    assert_eq!(
        layers.iter().map(|l| l.track).collect::<Vec<_>>(),
        vec![0, 1]
    );
    // Cut at 450000, window [427500, 472500): frame 145 = 435000 is inside.
    let layers = layers_at_frame(&p, 145);
    assert!(matches!(layers[0].content, LayerContent::Transition { .. }));
}

#[test]
fn project_round_trips_through_json() {
    let p = Project::from_json(SAMPLE).unwrap();
    let text = serde_json::to_string(&p).unwrap();
    assert_eq!(Project::from_json(&text).unwrap(), p);
}
