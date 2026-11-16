//! Project markers and the beats of sources: the file format, the edits, and how beats follow
//! the clips of their source.

mod common;

use common::{clip, project, t, track};
use engine::{
    add_marker, add_marker_report, move_clip, move_marker, move_marker_report, remove_marker, set_beats, set_source_beats, set_marker, set_speed, split_clip, trim_end, trim_start, validate,
    AcceptAll, Beat, ClipBeats, ClipKey, EditError, EditHistory, Marker, Project, Time, ValidationError, MAX_MARKER_LABEL,
};

const SECOND: i64 = 90_000;

fn s(seconds: i64) -> Time {
    t(seconds * SECOND)
}

fn beats_every_second(count: i64) -> ClipBeats {
    ClipBeats { tempo: 60.0, beats: (0..count).map(|i| Beat { at: s(i), downbeat: i % 4 == 0 }).collect() }
}

/// One clip of 10 s at 5 s on the timeline, playing its source from 2 s, with a beat at every
/// whole second of its source from 0 to 19.
fn with_beats() -> Project {
    let music = clip("song.mp3", 5 * SECOND, 2 * SECOND, 10 * SECOND);
    let mut project = project(3000, vec![track(vec![music], Vec::new())]);
    project.beats.insert("song.mp3".into(), std::sync::Arc::new(beats_every_second(20)));
    project
}

fn key() -> ClipKey {
    ClipKey { track: 0, clip: 0 }
}

fn times(project: &Project, clip: usize) -> Vec<Time> {
    project.clip_beat_times(&project.tracks[0].clips[clip]).map(|(at, _)| at).collect()
}

#[test]
fn markers_and_beats_are_left_out_of_the_file_when_there_are_none() {
    let plain = project(3000, vec![track(vec![clip("a.mp4", 0, 0, SECOND)], Vec::new())]);
    let json = serde_json::to_string(&plain).unwrap();
    assert!(!json.contains("markers") && !json.contains("beats"), "{json}");
    assert_eq!(Project::from_json(&json).unwrap(), plain);
}

#[test]
fn markers_and_beats_survive_a_round_trip() {
    let mut project = with_beats();
    project = add_marker(&project, Marker { at: s(3), label: "Drop".into(), color: Some([255, 0, 0, 255]) }).unwrap();
    project = add_marker(&project, Marker::at(s(1))).unwrap();
    let json = serde_json::to_string(&project).unwrap();
    assert!(json.contains("\"markers\"") && json.contains("\"beats\"") && json.contains("\"downbeat\":true"), "{json}");
    // A plain marker and a plain beat keep only their time.
    assert!(json.contains("{\"at\":90000}"), "{json}");
    let back = Project::from_json(&json).unwrap();
    assert_eq!(back, project);
    assert_eq!(back.markers[0].at, s(1), "sorted by time");
}

#[test]
fn a_beat_maps_to_the_timeline_through_the_clip() {
    let project = with_beats();
    // Source 2 s plays at 5 s; the clip plays source 2 to 12 s, so beats 2..=11 show.
    let shown = times(&project, 0);
    assert_eq!(shown, (5..15).map(s).collect::<Vec<_>>());
    let downbeats: Vec<bool> = project.clip_beat_times(&project.tracks[0].clips[0]).map(|(_, beat)| beat.downbeat).collect();
    assert_eq!(&downbeats[..3], &[false, false, true], "source 4 s is a downbeat");
}

#[test]
fn beats_follow_a_move() {
    let moved = move_clip(&with_beats(), key(), 0, s(20)).unwrap();
    assert_eq!(times(&moved, 0), (20..30).map(s).collect::<Vec<_>>());
}

#[test]
fn beats_stay_on_their_sound_across_trims() {
    let project = with_beats();
    // Trimming the start to 7 s cuts source 2 to 4 s: the beat at source 4 s stays at 7 s.
    let trimmed = trim_start(&project, key(), s(7)).unwrap();
    assert_eq!(times(&trimmed, 0), (7..15).map(s).collect::<Vec<_>>());
    let shorter = trim_end(&project, key(), s(10)).unwrap();
    assert_eq!(times(&shorter, 0), (5..10).map(s).collect::<Vec<_>>());
}

#[test]
fn each_half_of_a_split_shows_its_own_beats() {
    let split = split_clip(&with_beats(), key(), s(9)).unwrap();
    assert_eq!(times(&split, 0), (5..9).map(s).collect::<Vec<_>>());
    assert_eq!(times(&split, 1), (9..15).map(s).collect::<Vec<_>>());
    assert_eq!(split.beats.len(), 1, "both halves share the one list of their source");
}

#[test]
fn every_clip_of_a_source_shares_its_beats() {
    let mut project = with_beats();
    project.tracks[0].clips.push(clip("song.mp3", 30 * SECOND, 0, 3 * SECOND));
    assert_eq!(times(&project, 1), (30..33).map(s).collect::<Vec<_>>(), "the file placed again plays the same list");
    let cleared = set_beats(&project, ClipKey { track: 0, clip: 1 }, ClipBeats::default()).unwrap();
    assert!(cleared.beats.is_empty() && times(&cleared, 0).is_empty(), "clearing through one clip clears the source");
}

#[test]
fn setting_the_same_beats_again_is_no_undo_step() {
    let mut history = EditHistory::new(with_beats(), 10);
    history.apply(|project| set_source_beats(project, "song.mp3", beats_every_second(20))).unwrap();
    history.apply(|project| set_beats(project, key(), beats_every_second(20))).unwrap();
    assert!(!history.can_undo());
}

#[test]
fn a_speed_change_moves_the_beats_with_the_sound() {
    let doubled = set_speed(&with_beats(), key(), 2.0).unwrap();
    // At 2x the clip lasts 5 s and source 2 + 2k s plays at 5 + k s.
    let shown = times(&doubled, 0);
    assert_eq!(shown.len(), 10);
    assert_eq!(shown[0], s(5));
    assert_eq!(shown[2], s(6), "source 4 s");
    assert_eq!(shown[9], t(5 * SECOND + 9 * SECOND / 2), "source 11 s");
}

#[test]
fn a_hold_at_the_start_delays_the_beats() {
    let mut project = with_beats();
    let clip = &mut project.tracks[0].clips[0];
    clip.animation.enter = Some(engine::AnimationRef { preset: "fade".into(), duration: t(-SECOND) });
    clip.duration = s(11);
    assert_eq!(times(&project, 0)[0], s(6));
}

#[test]
fn marker_edits_keep_the_markers_sorted_and_report_the_index() {
    let project = with_beats();
    let (project, first) = add_marker_report(&project, Marker::at(s(4))).unwrap();
    let (project, second) = add_marker_report(&project, Marker::at(s(2))).unwrap();
    let (project, third) = add_marker_report(&project, Marker::at(s(4))).unwrap();
    assert_eq!((first, second, third), (0, 0, 2), "a marker at the time of another comes after it");
    let (moved, index) = move_marker_report(&project, 0, s(9)).unwrap();
    assert_eq!(index, 2);
    assert_eq!(moved.markers.iter().map(|marker| marker.at).collect::<Vec<_>>(), vec![s(4), s(4), s(9)]);
    let removed = remove_marker(&moved, 1).unwrap();
    assert_eq!(removed.markers.len(), 2);
    let labelled = set_marker(&removed, 1, "Chorus".into(), Some([0, 128, 255, 255])).unwrap();
    assert_eq!(labelled.markers[1], Marker { at: s(9), label: "Chorus".into(), color: Some([0, 128, 255, 255]) });
}

#[test]
fn a_missing_marker_is_refused() {
    let project = with_beats();
    assert_eq!(remove_marker(&project, 0), Err(EditError::MarkerNotFound { marker: 0 }));
    assert_eq!(move_marker(&project, 3, s(1)), Err(EditError::MarkerNotFound { marker: 3 }));
    assert_eq!(set_marker(&project, 0, String::new(), None), Err(EditError::MarkerNotFound { marker: 0 }));
}

#[test]
fn markers_do_not_move_with_clips() {
    let project = add_marker(&with_beats(), Marker::at(s(6))).unwrap();
    let moved = move_clip(&project, key(), 0, s(30)).unwrap();
    assert_eq!(moved.markers, project.markers);
}

#[test]
fn marker_and_beat_edits_are_undo_steps() {
    let mut history = EditHistory::new(with_beats(), 10);
    history.apply(|project| add_marker(project, Marker::at(s(3)))).unwrap();
    history.apply(|project| move_marker(project, 0, s(8))).unwrap();
    history.apply(|project| set_beats(project, key(), ClipBeats::default())).unwrap();
    assert!(history.current().beats.is_empty());
    assert!(history.undo());
    assert_eq!(*history.current().beats["song.mp3"], beats_every_second(20));
    assert!(history.undo());
    assert_eq!(history.current().markers[0].at, s(3));
    assert!(history.undo());
    assert!(history.current().markers.is_empty());
}

#[test]
fn bad_markers_are_invalid_and_refused() {
    assert!(matches!(add_marker(&with_beats(), Marker::at(t(-1))), Err(EditError::Invalid(errors)) if errors == vec![ValidationError::NegativeMarkerTime { marker: 0, at: t(-1) }]));
    let long = "x".repeat(MAX_MARKER_LABEL + 1);
    assert!(matches!(add_marker(&with_beats(), Marker { at: s(1), label: long, color: None }), Err(EditError::Invalid(_))));
    let mut unsorted = with_beats();
    unsorted.markers = vec![Marker::at(s(2)), Marker::at(s(1))];
    assert_eq!(validate(&unsorted, &AcceptAll), vec![ValidationError::MarkersOutOfOrder { marker: 1 }]);
}

#[test]
fn bad_beats_are_invalid() {
    let invalid = |beats: ClipBeats| matches!(set_beats(&with_beats(), key(), beats), Err(EditError::Invalid(errors)) if matches!(errors[..], [ValidationError::InvalidBeats { .. }]));
    assert!(invalid(ClipBeats { tempo: 60.0, beats: vec![Beat { at: s(2), downbeat: false }, Beat { at: s(1), downbeat: false }] }), "out of order");
    assert!(invalid(ClipBeats { tempo: 60.0, beats: vec![Beat { at: s(1), downbeat: false }, Beat { at: s(1), downbeat: false }] }), "repeated");
    assert!(invalid(ClipBeats { tempo: 60.0, beats: vec![Beat { at: t(-1), downbeat: false }] }), "negative");
    assert!(invalid(ClipBeats { tempo: f64::NAN, beats: vec![Beat { at: s(1), downbeat: false }] }), "tempo");
    // A still has no sound to have beats.
    let mut still = project(3000, vec![track(vec![clip("a.png", 0, 0, SECOND)], Vec::new())]);
    still.beats.insert("a.png".into(), std::sync::Arc::new(beats_every_second(2)));
    assert!(matches!(&validate(&still, &AcceptAll)[..], [ValidationError::InvalidBeats { source, .. }] if source == "a.png"));
    assert!(set_source_beats(&with_beats(), engine::TEXT_SOURCE, beats_every_second(2)).is_err(), "text has no sound");
}

#[test]
fn beats_on_a_locked_track_are_refused() {
    let mut project = with_beats();
    project.tracks[0].locked = true;
    assert_eq!(set_beats(&project, key(), ClipBeats::default()), Err(EditError::TrackLocked { track: 0 }));
}
