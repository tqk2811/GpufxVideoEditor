//! Locked and hidden tracks.

mod common;

use common::*;
use engine::{
    add_clip, add_effect, add_track, add_transition, audio_sample_count, audio_segments, clip_at, delete_clip, frame_count, layers_at_frame, move_clip, remove_effect, remove_transition, set_effect_params, set_fades, set_gain, set_muted, set_track_hidden, set_track_locked,
    set_transition_params, split_clip, trim_end, trim_start, validate, AcceptAll, ClipKey, EditError, EditHistory, Project, Time, TrackKind, ValidationError,
};

fn key(track: usize, clip: usize) -> ClipKey {
    ClipKey { track, clip }
}

/// Track 0: two touching clips with a transition and an effect. Track 1: a clip. Track 2: audio.
fn timeline() -> Project {
    let mut first = clip("a", 0, 0, 30_000);
    first.effects.push(effect("blur"));
    project(
        3000,
        vec![
            track(vec![first, clip("b", 30_000, 0, 30_000)], vec![transition(0, 6000)]),
            track(vec![clip("c", 0, 0, 90_000)], vec![]),
            audio_track(vec![clip("m", 0, 0, 30_000)]),
        ],
    )
}

fn locked(track: usize) -> Project {
    set_track_locked(&timeline(), track, true).unwrap()
}

#[test]
fn every_edit_of_a_locked_track_is_refused() {
    let p = locked(0);
    let refused = EditError::TrackLocked { track: 0 };
    let k = key(0, 0);
    let results = [
        add_clip(&p, 0, clip("x", 90_000, 0, 3000)),
        move_clip(&p, k, 0, t(1)),
        trim_start(&p, k, t(1500)),
        trim_end(&p, key(0, 1), t(45_000)),
        split_clip(&p, k, t(1500)),
        delete_clip(&p, k),
        add_effect(&p, k, effect("mix")),
        remove_effect(&p, k, 0),
        add_transition(&p, 0, transition(0, 6000)),
        remove_transition(&p, 0, 0),
        set_gain(&p, k, -3.0),
        set_muted(&p, k, true),
        set_fades(&p, key(0, 1), t(0), t(3000)),
        set_effect_params(&p, k, 0, Default::default()),
        set_transition_params(&p, 0, 0, Default::default(), t(6000)),
    ];
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result.as_ref().err(), Some(&refused), "command {index}");
    }
}

#[test]
fn a_move_is_refused_from_and_to_a_locked_track() {
    assert_eq!(move_clip(&locked(1), key(0, 0), 1, t(120_000)), Err(EditError::TrackLocked { track: 1 }));
    assert_eq!(move_clip(&locked(0), key(1, 0), 0, t(120_000)), Err(EditError::TrackLocked { track: 0 }));
}

#[test]
fn the_other_tracks_stay_editable() {
    let p = locked(0);
    let next = trim_end(&p, key(1, 0), t(60_000)).unwrap();
    assert_eq!(next.tracks[1].clips[0].duration, t(60_000));
    assert_eq!(next.tracks[0], p.tracks[0], "the locked track is untouched");
}

#[test]
fn a_missing_track_or_clip_is_still_named() {
    let p = timeline();
    assert_eq!(set_track_locked(&p, 9, true), Err(EditError::TrackNotFound { track: 9 }));
    assert_eq!(set_track_hidden(&p, 9, true), Err(EditError::TrackNotFound { track: 9 }));
    assert_eq!(delete_clip(&p, key(0, 9)), Err(EditError::ClipNotFound { key: key(0, 9) }));
}

#[test]
fn hiding_is_undone_like_any_edit() {
    let mut history = EditHistory::new(timeline(), 10);
    history.apply(|p| set_track_hidden(p, 0, true)).unwrap();
    assert!(history.undo());
    assert!(!history.current().tracks[0].hidden);
    assert!(history.redo());
    assert!(history.current().tracks[0].hidden);
}

#[test]
fn locking_adds_no_undo_step_and_keeps_the_redo_stack() {
    let mut history = EditHistory::new(timeline(), 10);
    history.set_track_locked(0, true).unwrap();
    assert!(history.current().tracks[0].locked);
    assert!(!history.can_undo(), "no step to undo");

    history.apply(|p| trim_end(p, key(1, 0), t(60_000))).unwrap();
    assert!(history.undo());
    history.set_track_locked(0, false).unwrap();
    assert!(history.can_redo(), "unlocking leaves the redo step");
}

#[test]
fn undo_and_redo_keep_the_lock_as_it_is_now() {
    let mut history = EditHistory::new(timeline(), 10);
    history.apply(|p| trim_end(p, key(1, 0), t(60_000))).unwrap();
    history.apply(|p| trim_end(p, key(1, 0), t(45_000))).unwrap();
    assert!(history.undo());
    history.set_track_locked(1, true).unwrap();

    assert!(history.undo());
    assert!(history.current().tracks[1].locked, "the older snapshot does not unlock");
    assert_eq!(history.current().tracks[1].clips[0].duration, t(90_000), "the content is restored on a locked track");
    assert!(history.redo());
    assert!(history.redo());
    assert!(history.current().tracks[1].locked);
    assert_eq!(history.current().tracks[1].clips[0].duration, t(45_000));

    history.set_track_locked(1, false).unwrap();
    assert!(history.undo());
    assert!(!history.current().tracks[1].locked, "unlocked stays unlocked too");
}

#[test]
fn the_lock_of_a_track_survives_undoing_and_redoing_its_creation() {
    let mut history = EditHistory::new(timeline(), 10);
    history.apply(|p| add_track(p, TrackKind::Video)).unwrap();
    history.set_track_locked(3, true).unwrap();
    assert!(history.undo());
    assert_eq!(history.current().tracks.len(), 3);
    assert!(history.redo());
    assert!(history.current().tracks[3].locked);
}

#[test]
fn locking_a_missing_track_is_refused() {
    let mut history = EditHistory::new(timeline(), 10);
    assert_eq!(history.set_track_locked(9, true), Err(EditError::TrackNotFound { track: 9 }));
}

#[test]
fn hiding_or_unlocking_a_locked_track_stays_possible() {
    let hidden = set_track_hidden(&locked(0), 0, true).unwrap();
    assert!(hidden.tracks[0].hidden && hidden.tracks[0].locked);
    let shown = set_track_hidden(&hidden, 0, false).unwrap();
    assert!(!shown.tracks[0].hidden);
    assert!(!set_track_locked(&shown, 0, false).unwrap().tracks[0].locked);
}

#[test]
fn adding_a_track_works_while_another_is_locked() {
    assert_eq!(add_track(&locked(0), TrackKind::Video).unwrap().tracks.len(), 4);
}

fn hidden(track: usize) -> Project {
    set_track_hidden(&timeline(), track, true).unwrap()
}

#[test]
fn a_hidden_track_gives_no_layer() {
    let tracks = |p: &Project, frame| layers_at_frame(p, frame).iter().map(|layer| layer.track).collect::<Vec<_>>();
    assert_eq!(tracks(&timeline(), 0), vec![0, 1]);
    assert_eq!(tracks(&hidden(1), 0), vec![0]);
    assert_eq!(tracks(&hidden(0), 0), vec![1]);
}

#[test]
fn a_hidden_track_gives_no_transition_layer() {
    let p = timeline();
    let inside_the_window = 9; // 27000 ticks, the window runs from 27000.
    assert!(matches!(layers_at_frame(&p, inside_the_window)[0].content, engine::LayerContent::Transition { .. }), "the fixture has a transition there");
    assert!(layers_at_frame(&hidden(0), inside_the_window).iter().all(|layer| layer.track != 0));
}

#[test]
fn a_hidden_track_is_silent() {
    assert!(audio_segments(&timeline(), 0, 1000).iter().any(|segment| segment.key.track == 2));
    assert!(audio_segments(&hidden(2), 0, 1000).iter().all(|segment| segment.key.track != 2));
    assert!(audio_segments(&hidden(0), 0, 1000).iter().all(|segment| segment.key.track != 0), "the sound of a video track goes with its picture");
}

#[test]
fn hiding_a_track_does_not_change_the_length_of_the_project() {
    let p = timeline();
    let hid = hidden(1);
    assert_eq!(frame_count(&hid), frame_count(&p), "track 1 is the longest");
    assert_eq!(audio_sample_count(&hid), audio_sample_count(&p));
    let everything_hidden = set_track_hidden(&hidden(0), 1, true).unwrap();
    assert_eq!(frame_count(&everything_hidden), frame_count(&p));
    assert!(layers_at_frame(&everything_hidden, 0).is_empty());
}

#[test]
fn a_missing_file_on_a_hidden_track_does_not_stop_the_project() {
    let mut p = project(3000, vec![track(vec![clip("gone.mp4", 0, 0, 3000)], vec![])]);
    p.tracks[0].clips[0].effects.push(effect("no-such-package"));
    assert_eq!(validate(&p, &SetResolver::default()).len(), 2, "a file and a package are missing");
    p.tracks[0].hidden = true;
    assert_eq!(validate(&p, &SetResolver::default()), vec![]);
}

#[test]
fn the_structure_of_a_hidden_track_is_still_checked() {
    let mut p = project(3000, vec![track(vec![clip("a.mp4", 0, 0, 0)], vec![])]);
    p.tracks[0].hidden = true;
    assert!(matches!(validate(&p, &AcceptAll).as_slice(), [ValidationError::NonPositiveClipDuration { .. }]));
}

#[test]
fn old_project_files_load_and_new_fields_round_trip() {
    let old = r#"{"version":2,"output":{"width":16,"height":16,"frame_duration":3000},"tracks":[{"clips":[]}]}"#;
    let loaded = Project::from_json(old).unwrap();
    assert!(!loaded.tracks[0].locked && !loaded.tracks[0].hidden);
    let written = serde_json::to_string(&loaded).unwrap();
    assert!(!written.contains("locked") && !written.contains("hidden"), "{written}");

    let mut flagged = timeline();
    flagged.tracks[0].locked = true;
    flagged.tracks[1].hidden = true;
    flagged.version = engine::SUPPORTED_VERSION;
    let read_back = Project::from_json(&serde_json::to_string(&flagged).unwrap()).unwrap();
    assert_eq!(read_back, flagged);
}

#[test]
fn a_file_of_another_version_is_refused() {
    let v1 = r#"{"version":1,"output":{"width":16,"height":16,"frame_duration":3000},"tracks":[{"clips":[],"locked":true}]}"#;
    assert!(matches!(Project::from_json(v1), Err(engine::LoadError::UnsupportedVersion(1))));
}

#[test]
fn clip_keys_stay_valid_after_toggles() {
    let p = locked(1);
    assert_eq!(clip_at(&p, 1, Time::ZERO), Some(key(1, 0)));
}
