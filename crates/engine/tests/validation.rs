//! Validation tests: spec row "Từ chối project sai", one test per error.

mod common;

use common::*;
use engine::{validate, AcceptAll, LoadError, Project, ValidationError as E};

fn only(p: &Project) -> Vec<E> {
    validate(p, &SetResolver::default())
}

fn valid_pair() -> Project {
    project(
        3000,
        vec![track(
            vec![
                clip("a.mp4", 0, 0, 30_000),
                clip("b.mp4", 30_000, 0, 30_000),
            ],
            vec![transition(0, 6000)],
        )],
    )
}

#[test]
fn valid_project_has_no_errors() {
    assert_eq!(only(&valid_pair()), vec![]);
}

#[test]
fn rejects_unknown_version() {
    let text = r#"{"version":3,"output":{"width":1,"height":1,"frame_duration":3000},"tracks":[]}"#;
    match Project::from_json(text) {
        Err(LoadError::UnsupportedVersion(3)) => {}
        other => panic!("expected UnsupportedVersion(3), got {other:?}"),
    }
}

#[test]
fn rejects_malformed_json() {
    assert!(matches!(Project::from_json("{"), Err(LoadError::Json(_))));
}

#[test]
fn overlapping_clips_name_track_and_clips() {
    let p = project(
        3000,
        vec![
            track(vec![clip("a.mp4", 0, 0, 30_000)], vec![]),
            track(
                vec![
                    clip("a.mp4", 0, 0, 30_000),
                    clip("b.mp4", 29_999, 0, 30_000),
                ],
                vec![],
            ),
        ],
    );
    let errors = only(&p);
    assert_eq!(
        errors,
        vec![E::ClipOverlap {
            track: 1,
            clip: 0,
            other_clip: 1
        }]
    );
    assert_eq!(errors[0].to_string(), "track 1: clip 0 overlaps clip 1");
}

#[test]
fn touching_clips_do_not_overlap() {
    let p = project(
        3000,
        vec![track(
            vec![clip("a.mp4", 0, 0, 100), clip("b.mp4", 100, 0, 100)],
            vec![],
        )],
    );
    assert_eq!(only(&p), vec![]);
}

#[test]
fn transition_longer_than_usable_clip() {
    // First clip only 2000 long, centred window needs 3000 from it.
    let p = project(
        3000,
        vec![track(
            vec![clip("a.mp4", 0, 0, 2000), clip("b.mp4", 2000, 0, 30_000)],
            vec![transition(0, 6000)],
        )],
    );
    let errors = only(&p);
    assert_eq!(
        errors,
        vec![E::TransitionTooLong {
            track: 0,
            transition: 0,
            clip: 0,
            needed: t(3000),
            usable: t(2000)
        }]
    );
}

#[test]
fn transition_longer_than_second_clip() {
    let p = project(
        3000,
        vec![track(
            vec![clip("a.mp4", 0, 0, 30_000), clip("b.mp4", 30_000, 0, 1000)],
            vec![transition(0, 6000)],
        )],
    );
    assert_eq!(
        only(&p),
        vec![E::TransitionTooLong {
            track: 0,
            transition: 0,
            clip: 1,
            needed: t(3000),
            usable: t(1000)
        }]
    );
}

#[test]
fn two_transitions_share_one_clip() {
    // Middle clip 5000 long; it gives 3000 to the first window and 3000 to the second.
    let p = project(
        3000,
        vec![track(
            vec![
                clip("a.mp4", 0, 0, 30_000),
                clip("b.mp4", 30_000, 0, 5000),
                clip("c.mp4", 35_000, 0, 30_000),
            ],
            vec![transition(0, 6000), transition(1, 6000)],
        )],
    );
    let errors = only(&p);
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors
        .iter()
        .all(|e| matches!(e, E::TransitionTooLong { clip: 1, .. })));
}

#[test]
fn transition_between_non_adjacent_clips() {
    let p = project(
        3000,
        vec![track(
            vec![
                clip("a.mp4", 0, 0, 30_000),
                clip("b.mp4", 33_000, 0, 30_000),
            ],
            vec![transition(0, 6000)],
        )],
    );
    assert_eq!(
        only(&p),
        vec![E::TransitionNotAdjacent {
            track: 0,
            transition: 0,
            after_clip: 0
        }]
    );
}

#[test]
fn transition_without_following_clip() {
    let p = project(
        3000,
        vec![track(
            vec![clip("a.mp4", 0, 0, 30_000)],
            vec![transition(0, 6000)],
        )],
    );
    assert_eq!(
        only(&p),
        vec![E::TransitionClipMissing {
            track: 0,
            transition: 0,
            after_clip: 0
        }]
    );
}

#[test]
fn duplicate_transition_after_same_clip() {
    let mut p = valid_pair();
    p.tracks[0].transitions.push(transition(0, 3000));
    assert_eq!(
        only(&p),
        vec![E::DuplicateTransition {
            track: 0,
            transition: 1,
            after_clip: 0
        }]
    );
}

#[test]
fn non_positive_clip_duration() {
    let p = project(3000, vec![track(vec![clip("a.mp4", 0, 0, 0)], vec![])]);
    assert_eq!(
        only(&p),
        vec![E::NonPositiveClipDuration {
            track: 0,
            clip: 0,
            duration: t(0)
        }]
    );
    let p = project(3000, vec![track(vec![clip("a.mp4", 0, 0, -5)], vec![])]);
    assert_eq!(
        only(&p),
        vec![E::NonPositiveClipDuration {
            track: 0,
            clip: 0,
            duration: t(-5)
        }]
    );
}

#[test]
fn non_positive_transition_duration() {
    let mut p = valid_pair();
    p.tracks[0].transitions[0].duration = t(0);
    assert_eq!(
        only(&p),
        vec![E::NonPositiveTransitionDuration {
            track: 0,
            transition: 0,
            duration: t(0)
        }]
    );
}

#[test]
fn non_positive_frame_duration() {
    for fd in [0, -3003] {
        let p = project(fd, vec![track(vec![clip("a.mp4", 0, 0, 3000)], vec![])]);
        assert_eq!(
            only(&p),
            vec![E::NonPositiveFrameDuration {
                frame_duration: t(fd)
            }]
        );
    }
}

#[test]
fn zero_output_size() {
    let mut p = valid_pair();
    p.output.width = 0;
    assert_eq!(
        only(&p),
        vec![E::InvalidOutputSize {
            width: 0,
            height: 1080
        }]
    );
}

#[test]
fn negative_start_and_source_in() {
    let p = project(3000, vec![track(vec![clip("a.mp4", -1, -2, 3000)], vec![])]);
    assert_eq!(
        only(&p),
        vec![
            E::NegativeClipTime {
                track: 0,
                clip: 0,
                field: "start",
                value: t(-1)
            },
            E::NegativeClipTime {
                track: 0,
                clip: 0,
                field: "source_in",
                value: t(-2)
            },
        ]
    );
}

#[test]
fn clip_end_overflow() {
    let p = project(
        3000,
        vec![track(vec![clip("a.mp4", i64::MAX, 0, 10)], vec![])],
    );
    assert_eq!(only(&p), vec![E::ClipTimeOverflow { track: 0, clip: 0 }]);
}

#[test]
fn missing_source_file() {
    let p = project(
        3000,
        vec![track(
            vec![clip("a.mp4", 0, 0, 3000), clip("gone.mp4", 3000, 0, 3000)],
            vec![],
        )],
    );
    let errors = only(&p);
    assert_eq!(
        errors,
        vec![E::SourceNotFound {
            track: 0,
            clip: 1,
            source: "gone.mp4".into()
        }]
    );
    assert_eq!(
        errors[0].to_string(),
        "track 0, clip 1: source 'gone.mp4' not found"
    );
}

#[test]
fn missing_effect_package() {
    let mut p = project(3000, vec![track(vec![clip("a.mp4", 0, 0, 3000)], vec![])]);
    p.tracks[0].clips[0].effects = vec![effect("blur"), effect("nope")];
    assert_eq!(
        only(&p),
        vec![E::EffectPackageNotFound {
            track: 0,
            clip: 0,
            effect: 1,
            package: "nope".into()
        }]
    );
}

#[test]
fn missing_transition_package() {
    let mut p = valid_pair();
    p.tracks[0].transitions[0].package = "nope".into();
    assert_eq!(
        only(&p),
        vec![E::TransitionPackageNotFound {
            track: 0,
            transition: 0,
            package: "nope".into()
        }]
    );
}

#[test]
fn accept_all_resolver_skips_lookups() {
    let p = project(
        3000,
        vec![track(vec![clip("anything.mp4", 0, 0, 3000)], vec![])],
    );
    assert_eq!(validate(&p, &AcceptAll), vec![]);
}
