//! Audio fields of the project model: loading and the validation errors of the audio specification, one test per error.

mod common;

use common::*;
use engine::{
    frame_count, layers_at_frame, validate, AcceptAll, AudioOutput, LayerContent, Project, TrackKind,
    ValidationError as E,
};

fn only(p: &Project) -> Vec<E> {
    validate(p, &SetResolver::default())
}

fn with_audio(sample_rate: u32, channels: u32) -> Project {
    let mut p = project(3000, vec![track(vec![clip("a.mp4", 0, 0, 30_000)], vec![])]);
    p.output.audio = Some(AudioOutput {
        sample_rate,
        channels,
        bitrate: 192_000,
    });
    p
}

fn one_clip() -> Project {
    project(3000, vec![track(vec![clip("a.mp4", 0, 0, 30_000)], vec![])])
}

fn audio_clip() -> Project {
    project(3000, vec![audio_track(vec![clip("a.mp4", 0, 0, 30_000)])])
}

const SPEC_EXAMPLE: &str = r#"{
  "version": 2,
  "output": {
    "width": 1920, "height": 1080, "frame_duration": 3000,
    "audio": { "sample_rate": 48000, "channels": 2, "bitrate": 192000 }
  },
  "tracks": [
    { "clips": [ { "source": "a.mp4", "start": 0, "source_in": 0, "duration": 450000, "gain_db": -3.0, "fade_out": 45000 } ] },
    { "kind": "audio", "clips": [ { "source": "music.m4a", "start": 0, "source_in": 0, "duration": 450000, "fade_in": 90000, "muted": true } ] }
  ]
}"#;

#[test]
fn version_2_fields_load() {
    let p = Project::from_json(SPEC_EXAMPLE).unwrap();
    assert_eq!(p.version, 2);
    assert_eq!(
        p.output.audio,
        Some(AudioOutput {
            sample_rate: 48_000,
            channels: 2,
            bitrate: 192_000
        })
    );
    assert_eq!(p.tracks[0].kind, TrackKind::Video, "kind defaults to video");
    assert_eq!(p.tracks[1].kind, TrackKind::Audio);
    assert_eq!(p.tracks[0].clips[0].gain_db, -3.0);
    assert_eq!(p.tracks[0].clips[0].fade_out, t(45_000));
    assert_eq!(p.tracks[0].clips[0].fade_in, t(0), "fade_in defaults to 0");
    assert!(!p.tracks[0].clips[0].muted, "muted defaults to false");
    assert!(p.tracks[1].clips[0].muted);
    assert_eq!(p.tracks[1].clips[0].fade_in, t(90_000));
    assert_eq!(validate(&p, &AcceptAll), vec![]);
}

#[test]
fn version_2_round_trips_through_json() {
    let p = Project::from_json(SPEC_EXAMPLE).unwrap();
    let text = serde_json::to_string(&p).unwrap();
    assert_eq!(Project::from_json(&text).unwrap(), p);
}

#[test]
fn malformed_version_2_field_is_a_json_error() {
    let text = r#"{"version":2,"output":{"width":1,"height":1,"frame_duration":3000},
        "tracks":[{"kind":"subtitle","clips":[]}]}"#;
    assert!(matches!(
        Project::from_json(text),
        Err(engine::LoadError::Json(_))
    ));
}

#[test]
fn video_scheduling_ignores_audio_tracks() {
    let p = project(
        3000,
        vec![
            audio_track(vec![clip("a.mp4", 0, 0, 300_000)]),
            track(vec![clip("b.mp4", 0, 0, 30_000)], vec![]),
        ],
    );
    assert_eq!(frame_count(&p), 10, "the longer audio track adds no frames");
    let layers = layers_at_frame(&p, 0);
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].track, 1, "layers keep their project track index");
    assert!(matches!(
        layers[0].content,
        LayerContent::Clip(s) if s.source == "b.mp4"
    ));
}

#[test]
fn only_audio_tracks_give_no_frames() {
    assert_eq!(frame_count(&audio_clip()), 0);
    assert!(layers_at_frame(&audio_clip(), 0).is_empty());
}

#[test]
fn accepts_the_supported_audio_format() {
    assert_eq!(only(&with_audio(48_000, 2)), vec![]);
}

#[test]
fn rejects_other_sample_rates() {
    let errors = only(&with_audio(44_100, 2));
    assert_eq!(errors, vec![E::UnsupportedSampleRate { sample_rate: 44_100 }]);
    assert_eq!(errors[0].to_string(), "output: audio sample_rate 44100 must be 48000");
}

#[test]
fn rejects_other_channel_counts() {
    assert_eq!(
        only(&with_audio(48_000, 1)),
        vec![E::UnsupportedChannelCount { channels: 1 }]
    );
}

#[test]
fn rejects_non_finite_gain() {
    for gain in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut p = one_clip();
        p.tracks[0].clips[0].gain_db = gain;
        assert_eq!(only(&p), vec![E::NonFiniteGain { track: 0, clip: 0 }]);
    }
}

#[test]
fn rejects_negative_fades() {
    let mut p = one_clip();
    p.tracks[0].clips[0].fade_in = t(-1);
    p.tracks[0].clips[0].fade_out = t(-2);
    assert_eq!(
        only(&p),
        vec![
            E::NegativeFade {
                track: 0,
                clip: 0,
                field: "fade_in",
                value: t(-1)
            },
            E::NegativeFade {
                track: 0,
                clip: 0,
                field: "fade_out",
                value: t(-2)
            },
        ]
    );
}

fn with_transition() -> Project {
    project(
        3000,
        vec![track(
            vec![clip("a.mp4", 0, 0, 30_000), clip("b.mp4", 30_000, 0, 30_000)],
            vec![transition(0, 6000)],
        )],
    )
}

#[test]
fn rejects_fade_out_on_the_earlier_clip_of_a_transition() {
    let mut p = with_transition();
    p.tracks[0].clips[0].fade_out = t(3000);
    assert_eq!(
        only(&p),
        vec![E::FadeBesideTransition { track: 0, clip: 0, field: "fade_out" }]
    );
}

#[test]
fn rejects_fade_in_on_the_later_clip_of_a_transition() {
    let mut p = with_transition();
    p.tracks[0].clips[1].fade_in = t(3000);
    assert_eq!(
        only(&p),
        vec![E::FadeBesideTransition { track: 0, clip: 1, field: "fade_in" }]
    );
}

#[test]
fn a_fade_on_the_side_without_a_transition_is_valid() {
    let mut p = with_transition();
    p.tracks[0].clips[0].fade_in = t(3000);
    p.tracks[0].clips[1].fade_out = t(3000);
    assert_eq!(only(&p), vec![]);
}

#[test]
fn gain_of_60_db_is_valid_and_above_it_is_rejected() {
    let mut p = one_clip();
    p.tracks[0].clips[0].gain_db = 60.0;
    assert_eq!(only(&p), vec![]);
    p.tracks[0].clips[0].gain_db = 60.5;
    assert_eq!(
        only(&p),
        vec![E::GainTooLarge { track: 0, clip: 0, gain_db: 60.5 }]
    );
}

#[test]
fn rejects_zero_audio_bitrate() {
    let mut p = with_audio(48_000, 2);
    p.output.audio.as_mut().unwrap().bitrate = 0;
    assert_eq!(only(&p), vec![E::InvalidAudioBitrate]);
}

#[test]
fn rejects_fades_longer_than_the_clip() {
    let mut p = one_clip();
    p.tracks[0].clips[0].fade_in = t(20_000);
    p.tracks[0].clips[0].fade_out = t(10_001);
    assert_eq!(
        only(&p),
        vec![E::FadesExceedClip {
            track: 0,
            clip: 0,
            fade_in: t(20_000),
            fade_out: t(10_001),
            duration: t(30_000)
        }]
    );
}

#[test]
fn fades_that_exactly_fill_the_clip_are_valid() {
    let mut p = one_clip();
    p.tracks[0].clips[0].fade_in = t(20_000);
    p.tracks[0].clips[0].fade_out = t(10_000);
    assert_eq!(only(&p), vec![]);
}

#[test]
fn fade_sum_overflow_is_too_long_rather_than_a_panic() {
    let mut p = one_clip();
    p.tracks[0].clips[0].fade_in = engine::Time::MAX;
    p.tracks[0].clips[0].fade_out = engine::Time::MAX;
    assert!(matches!(only(&p)[..], [E::FadesExceedClip { .. }]));
}

#[test]
fn rejects_transition_on_audio_track() {
    let mut p = project(
        3000,
        vec![audio_track(vec![
            clip("a.mp4", 0, 0, 30_000),
            clip("b.mp4", 30_000, 0, 30_000),
        ])],
    );
    p.tracks[0].transitions.push(transition(0, 6000));
    assert_eq!(
        only(&p),
        vec![E::AudioTrackTransition {
            track: 0,
            transition: 0
        }]
    );
}

#[test]
fn rejects_effect_on_audio_track() {
    let mut p = audio_clip();
    p.tracks[0].clips[0].effects = vec![effect("blur")];
    assert_eq!(
        only(&p),
        vec![E::AudioTrackEffect {
            track: 0,
            clip: 0,
            effect: 0
        }]
    );
}

#[test]
fn effect_on_video_track_is_still_valid() {
    let mut p = one_clip();
    p.tracks[0].clips[0].effects = vec![effect("blur")];
    assert_eq!(only(&p), vec![]);
}

#[test]
fn rejects_audio_track_source_without_audio() {
    let resolver = SetResolver {
        without_audio: vec!["a.mp4"],
        ..SetResolver::default()
    };
    let errors = validate(&audio_clip(), &resolver);
    assert_eq!(
        errors,
        vec![E::SourceHasNoAudio {
            track: 0,
            clip: 0,
            source: "a.mp4".into()
        }]
    );
    assert_eq!(
        errors[0].to_string(),
        "track 0, clip 0: source 'a.mp4' has no audio stream"
    );
}

#[test]
fn video_track_source_without_audio_is_silence_not_an_error() {
    let resolver = SetResolver {
        without_audio: vec!["a.mp4"],
        ..SetResolver::default()
    };
    assert_eq!(validate(&one_clip(), &resolver), vec![]);
}

#[test]
fn missing_audio_source_reports_only_not_found() {
    let resolver = SetResolver {
        sources: vec![],
        without_audio: vec!["a.mp4"],
        ..SetResolver::default()
    };
    assert_eq!(
        validate(&audio_clip(), &resolver),
        vec![E::SourceNotFound {
            track: 0,
            clip: 0,
            source: "a.mp4".into()
        }]
    );
}

#[test]
fn accept_all_accepts_audio_track_sources() {
    assert_eq!(validate(&audio_clip(), &AcceptAll), vec![]);
}
