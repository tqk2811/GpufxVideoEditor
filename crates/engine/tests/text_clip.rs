mod common;

use common::*;
use engine::{validate, AcceptAll, Clip, FontRef, Project, TextAlign, TextAnimationRef, TextContent, TextOutline, TextShadow, Track, TrackKind, ValidationError, TEXT_SOURCE};

pub fn text_clip(content: &str, start: i64, duration: i64) -> Clip {
    Clip { source: TEXT_SOURCE.to_owned(), text: Some(TextContent::new(content, 0.08)), ..clip(TEXT_SOURCE, start, 0, duration) }
}

fn project_with(clips: Vec<Clip>) -> Project {
    let mut p = project(3003, vec![]);
    p.version = engine::SUPPORTED_VERSION;
    p.tracks.push(Track { kind: TrackKind::Video, clips, transitions: vec![], locked: false, hidden: false });
    p
}

#[test]
fn text_source_is_not_an_image() {
    let c = text_clip("Hello", 0, 9000);
    assert!(c.is_text());
    assert!(!c.is_image());
    assert!(!clip("a.png", 0, 0, 9000).is_text());
}

#[test]
fn text_clip_round_trips_through_json() {
    let mut c = text_clip("Two\nlines", 0, 9000);
    let text = c.text.as_mut().unwrap();
    text.font = FontRef::System { family: "Arial".into() };
    text.align = TextAlign::Right;
    text.outline = Some(TextOutline { color: [0, 0, 0, 255], width: 0.1 });
    text.shadow = Some(TextShadow { color: [0, 0, 0, 128], offset: [0.05, 0.05], blur: 0.1 });
    text.animation.enter = Some(TextAnimationRef { preset: "fade".into(), duration: t(9000) });
    let p = project_with(vec![c]);
    let json = serde_json::to_string(&p).unwrap();
    assert!(json.contains("\"in\":"), "{json}");
    assert_eq!(Project::from_json(&json).unwrap(), p);
}

#[test]
fn project_without_text_loads_and_saves_unchanged() {
    let p = project_with(vec![clip("a.mp4", 0, 0, 9000)]);
    let json = serde_json::to_string(&p).unwrap();
    assert!(!json.contains("\"text\""));
    let loaded = Project::from_json(&json).unwrap();
    assert_eq!(loaded.tracks[0].clips[0].text, None);
    assert_eq!(serde_json::to_string(&loaded).unwrap(), json);
}

#[test]
fn minimal_text_json_gets_defaults() {
    let p = project_with(vec![clip("a.mp4", 0, 0, 9000)]);
    let json = serde_json::to_string(&p).unwrap().replace("\"source\":\"a.mp4\"", "\"source\":\"text:\",\"text\":{\"content\":\"Hi\",\"size\":0.1}");
    let loaded = Project::from_json(&json).unwrap();
    let text = loaded.tracks[0].clips[0].text.as_ref().unwrap();
    assert_eq!(text.font, FontRef::Default);
    assert_eq!(text.color, [255, 255, 255, 255]);
    assert_eq!(text.line_spacing, 1.0);
}

#[test]
fn valid_text_clip_does_not_ask_the_resolver() {
    struct NoFiles;
    impl engine::Resolver for NoFiles {
        fn source_exists(&self, _: &str) -> bool { false }
        fn package_exists(&self, _: &str) -> bool { true }
    }
    assert_eq!(validate(&project_with(vec![text_clip("Hi", 0, 9000)]), &NoFiles), vec![]);
}

#[test]
fn validate_rejects_empty_text_and_bad_size() {
    let mut c = text_clip("", 0, 9000);
    c.text.as_mut().unwrap().size = 0.0;
    let errors = validate(&project_with(vec![c]), &AcceptAll);
    assert_eq!(errors, vec![ValidationError::EmptyText { track: 0, clip: 0 }, ValidationError::InvalidTextSize { track: 0, clip: 0, size: 0.0 }]);
}

#[test]
fn validate_rejects_text_on_an_audio_track() {
    let mut p = project_with(vec![text_clip("Hi", 0, 9000)]);
    p.tracks[0].kind = TrackKind::Audio;
    assert_eq!(validate(&p, &AcceptAll), vec![ValidationError::TextOnAudioTrack { track: 0, clip: 0 }]);
}
