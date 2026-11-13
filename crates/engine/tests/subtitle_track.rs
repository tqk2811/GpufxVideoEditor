mod common;

use common::*;
use engine::{add_subtitle_track, restyle_track, text_clip, validate, AcceptAll, ClipRestyle, EditError, Project, SubtitleTrack, TextContent, Time, TrackKind, ValidationError};

fn cue(text: &str, start: i64, duration: i64) -> engine::Clip {
    text_clip(Time::from_ticks(start), Time::from_ticks(duration), TextContent::new(text, 0.05))
}

fn empty() -> Project {
    let mut p = project(3003, vec![]);
    p.version = engine::SUPPORTED_VERSION;
    p
}

#[test]
fn a_subtitle_track_is_added_with_its_cues_in_order() {
    let next = add_subtitle_track(&empty(), SubtitleTrack { style: Some("s/low".into()) }, vec![cue("B", 9000, 9000), cue("A", 0, 9000)]).unwrap();
    let track = &next.tracks[0];
    assert!(track.is_subtitle() && track.kind == TrackKind::Video);
    assert_eq!(track.clips.iter().map(|clip| clip.text.as_ref().unwrap().content.as_str()).collect::<Vec<_>>(), vec!["A", "B"]);
}

#[test]
fn overlapping_cues_are_refused() {
    let refused = add_subtitle_track(&empty(), SubtitleTrack::default(), vec![cue("A", 0, 9000), cue("B", 4000, 9000)]);
    assert!(matches!(refused, Err(EditError::Overlap { track: 0, .. })), "{refused:?}");
}

#[test]
fn old_projects_have_no_subtitle_and_the_field_round_trips() {
    let next = add_subtitle_track(&empty(), SubtitleTrack { style: Some("s/low".into()) }, vec![cue("A", 0, 9000)]).unwrap();
    let json = serde_json::to_string(&next).unwrap();
    assert!(json.contains("\"subtitle\""));
    assert_eq!(Project::from_json(&json).unwrap(), next);
    let plain = empty();
    let mut with_track = plain.clone();
    with_track.tracks.push(track(vec![], vec![]));
    assert!(!serde_json::to_string(&with_track).unwrap().contains("subtitle"), "a plain track writes no subtitle field");
}

#[test]
fn a_bad_subtitle_track_is_invalid() {
    let p = add_subtitle_track(&empty(), SubtitleTrack { style: Some("  ".into()) }, vec![]).unwrap_err();
    assert!(matches!(&p, EditError::Invalid(errors) if errors == &vec![ValidationError::EmptySubtitleStyle { track: 0 }]), "{p:?}");
    let mut audio = empty();
    audio.tracks.push(audio_track(vec![]));
    audio.tracks[0].subtitle = Some(SubtitleTrack::default());
    assert_eq!(validate(&audio, &AcceptAll), vec![ValidationError::SubtitleOnAudioTrack { track: 0 }]);
}

#[test]
fn restyling_changes_the_look_and_keeps_the_time() {
    let p = add_subtitle_track(&empty(), SubtitleTrack::default(), vec![cue("A", 0, 9000), cue("B", 9000, 9000)]).unwrap();
    let mut bold = TextContent::new("B", 0.07);
    bold.weight = 800;
    let next = restyle_track(&p, 0, Some(SubtitleTrack { style: Some("s/low".into()) }), vec![ClipRestyle { clip: 1, text: bold.clone(), animation: Default::default(), placement: None }]).unwrap();
    assert_eq!(next.tracks[0].subtitle.as_ref().unwrap().style.as_deref(), Some("s/low"));
    assert_eq!(next.tracks[0].clips[1].text.as_ref(), Some(&bold));
    assert_eq!((next.tracks[0].clips[1].start, next.tracks[0].clips[1].duration), (Time::from_ticks(9000), Time::from_ticks(9000)));
    assert_eq!(next.tracks[0].clips[0], p.tracks[0].clips[0]);
    assert!(matches!(restyle_track(&p, 0, None, vec![ClipRestyle { clip: 5, text: bold, animation: Default::default(), placement: None }]), Err(EditError::ClipNotFound { .. })));
}
