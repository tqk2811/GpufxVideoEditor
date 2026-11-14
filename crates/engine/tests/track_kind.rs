mod common;

use common::*;
use engine::{add_clip, add_track, move_clip, text_clip, validate, AcceptAll, ClipKey, EditError, Project, TextContent, Time, TrackKind, ValidationError, TEXT_SOURCE};

const STICKER: &str = "asset:sticker/p/images/a.png";

fn empty() -> Project {
    let mut p = project(3003, vec![]);
    p.version = engine::SUPPORTED_VERSION;
    p
}

fn with_tracks(kinds: &[TrackKind]) -> Project {
    kinds.iter().fold(empty(), |p, kind| add_track(&p, *kind).unwrap())
}

#[test]
fn each_kind_accepts_its_own_sources() {
    let cases = [
        ("a.mp4", [true, true, false, false]),
        ("a.png", [true, false, true, false]),
        (STICKER, [false, false, true, false]),
        (TEXT_SOURCE, [false, false, false, true]),
        ("a.flac", [true, true, false, false]),
    ];
    for (source, expected) in cases {
        let got: Vec<bool> = TrackKind::ALL.iter().map(|kind| kind.accepts_source(source)).collect();
        assert_eq!(got, expected, "{source}");
    }
}

#[test]
fn a_text_clip_is_only_accepted_by_a_text_track() {
    let clip = text_clip(Time::ZERO, t(9000), TextContent::new("A", 0.05));
    let got: Vec<bool> = TrackKind::ALL.iter().map(|kind| kind.accepts_clip(&clip)).collect();
    assert_eq!(got, [false, false, false, true]);
}

#[test]
fn only_audio_has_no_picture() {
    let got: Vec<bool> = TrackKind::ALL.iter().map(|kind| kind.shows_picture()).collect();
    assert_eq!(got, [true, false, true, true]);
}

#[test]
fn a_refused_clip_is_invalid_and_cannot_be_added() {
    let p = with_tracks(&[TrackKind::Video, TrackKind::Sticker, TrackKind::Text]);
    let refused = add_clip(&p, 0, clip(STICKER, 0, 0, 9000));
    assert!(matches!(&refused, Err(EditError::Invalid(errors)) if errors == &vec![ValidationError::ClipNotAllowedOnTrack { track: 0, clip: 0, kind: TrackKind::Video, source: STICKER.to_owned() }]), "{refused:?}");
    assert!(add_clip(&p, 1, clip("a.mp4", 0, 0, 9000)).is_err(), "a video on a sticker track");
    assert!(add_clip(&p, 2, clip("a.png", 0, 0, 9000)).is_err(), "an image on a text track");
    assert!(add_clip(&p, 1, text_clip(Time::ZERO, t(9000), TextContent::new("A", 0.05))).is_err(), "text on a sticker track");
    assert!(add_clip(&p, 1, clip(STICKER, 0, 0, 9000)).is_ok());
    assert!(add_clip(&p, 1, clip("a.png", 0, 0, 9000)).is_ok());
    assert!(add_clip(&p, 0, clip("a.png", 0, 0, 9000)).is_ok());
    assert!(add_clip(&p, 2, text_clip(Time::ZERO, t(9000), TextContent::new("A", 0.05))).is_ok());
}

#[test]
fn a_picture_clip_moves_to_a_picture_track_that_takes_it() {
    let p = with_tracks(&[TrackKind::Video, TrackKind::Sticker, TrackKind::Text, TrackKind::Audio]);
    let p = add_clip(&p, 0, clip("a.png", 0, 0, 9000)).unwrap();
    let p = add_clip(&p, 0, clip("a.mp4", 9000, 0, 9000)).unwrap();
    let image = ClipKey { track: 0, clip: 0 };
    let moved = move_clip(&p, image, 1, Time::ZERO).unwrap();
    assert_eq!(moved.tracks[1].clips[0].source, "a.png");
    assert_eq!(move_clip(&p, image, 2, Time::ZERO), Err(EditError::TrackKindMismatch { from: TrackKind::Video, to: TrackKind::Text }));
    assert_eq!(move_clip(&p, ClipKey { track: 0, clip: 1 }, 1, Time::ZERO), Err(EditError::TrackKindMismatch { from: TrackKind::Video, to: TrackKind::Sticker }));
    assert_eq!(move_clip(&p, ClipKey { track: 0, clip: 1 }, 3, Time::ZERO), Err(EditError::TrackKindMismatch { from: TrackKind::Video, to: TrackKind::Audio }));
}

#[test]
fn new_kinds_round_trip_in_the_project_file() {
    let p = with_tracks(&[TrackKind::Sticker, TrackKind::Text]);
    let json = serde_json::to_string(&p).unwrap();
    assert!(json.contains("\"sticker\"") && json.contains("\"text\""), "{json}");
    assert_eq!(Project::from_json(&json).unwrap(), p);
    assert!(validate(&p, &AcceptAll).is_empty());
}
