mod common;

use common::*;
use engine::{add_text_clip, audio_segments, layers_at_frame, set_speed, set_text, trim_start, ClipKey, EditError, EditHistory, Time, validate, AcceptAll, Clip, FontRef, Project, TextAlign, TextAnimationRef, TextContent, TextOutline, TextShadow, Track, TrackKind, ValidationError, TEXT_SOURCE};

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

const KEY: ClipKey = ClipKey { track: 0, clip: 0 };

#[test]
fn add_text_clip_is_one_undo_step() {
    let base = project_with(vec![]);
    let mut history = EditHistory::new(base.clone(), 10);
    history.apply(|p| add_text_clip(p, 0, t(9000), t(18000), TextContent::new("Hi", 0.1))).unwrap();
    let added = history.current().tracks[0].clips[0].clone();
    assert!(added.is_text());
    assert_eq!(added.source, TEXT_SOURCE);
    assert_eq!((added.start, added.duration), (t(9000), t(18000)));
    assert!(history.undo());
    assert_eq!(history.current(), &base);
    assert!(history.redo());
    assert_eq!(history.current().tracks[0].clips[0], added);
}

#[test]
fn add_text_clip_refuses_an_overlap_and_empty_text() {
    let base = project_with(vec![text_clip("A", 0, 9000)]);
    assert!(matches!(add_text_clip(&base, 0, t(3000), t(9000), TextContent::new("B", 0.1)), Err(EditError::Overlap { .. })));
    assert!(matches!(add_text_clip(&base, 0, t(9000), t(9000), TextContent::new("", 0.1)), Err(EditError::Invalid(_))));
}

#[test]
fn set_text_changes_style_but_not_time_and_undoes() {
    let base = project_with(vec![text_clip("A", 9000, 18000)]);
    let mut history = EditHistory::new(base.clone(), 10);
    let mut style = TextContent::new("Bigger", 0.2);
    style.align = TextAlign::Left;
    history.apply(|p| set_text(p, KEY, style.clone())).unwrap();
    let changed = &history.current().tracks[0].clips[0];
    assert_eq!(changed.text.as_ref(), Some(&style));
    assert_eq!((changed.start, changed.duration, changed.source_in), (t(9000), t(18000), Time::ZERO));
    assert!(history.undo());
    assert_eq!(history.current(), &base);
}

#[test]
fn set_text_refuses_a_media_clip_and_bad_text() {
    let media = project_with(vec![clip("a.mp4", 0, 0, 9000)]);
    assert!(matches!(set_text(&media, KEY, TextContent::new("A", 0.1)), Err(EditError::NotText { .. })));
    let text = project_with(vec![text_clip("A", 0, 9000)]);
    assert!(matches!(set_text(&text, KEY, TextContent::new("A", -1.0)), Err(EditError::Invalid(_))));
}

#[test]
fn text_clip_has_no_speed() {
    let base = project_with(vec![text_clip("A", 0, 9000)]);
    assert!(matches!(set_speed(&base, KEY, 2.0), Err(EditError::SpeedOfImage { .. })));
}

#[test]
fn trimming_the_start_of_text_keeps_no_source_offset() {
    let base = project_with(vec![text_clip("A", 9000, 9000)]);
    let earlier = trim_start(&base, KEY, t(0)).unwrap();
    let clip = &earlier.tracks[0].clips[0];
    assert_eq!((clip.start, clip.duration, clip.source_in), (t(0), t(18000), Time::ZERO));
}

#[test]
fn text_clip_gives_a_layer_but_no_sound() {
    let mut p = project_with(vec![text_clip("A", 0, 30030)]);
    p.output.audio = Some(engine::AudioOutput { sample_rate: 48_000, channels: 2, bitrate: 128_000 });
    assert!(audio_segments(&p, 0, 4800).is_empty());
    let layers = layers_at_frame(&p, 0);
    assert_eq!(layers.len(), 1);
    let engine::LayerContent::Clip(sample) = &layers[0].content else { panic!("one clip: {layers:?}") };
    assert_eq!(sample.model.text.as_ref().map(|text| text.content.as_str()), Some("A"));
}

#[test]
fn text_clip_takes_part_in_a_transition() {
    let mut p = project_with(vec![text_clip("A", 0, 30030), text_clip("B", 30030, 30030)]);
    p.tracks[0].transitions.push(engine::Transition { after_clip: 0, package: "mix".into(), params: Default::default(), duration: Time::from_ticks(9009) });
    let at = engine::frame_count(&p) / 2;
    let layers = layers_at_frame(&p, at);
    assert_eq!(layers.len(), 1);
    let engine::LayerContent::Transition { from, to, .. } = &layers[0].content else { panic!("a transition: {layers:?}") };
    assert_eq!((from.model.text.as_ref().unwrap().content.as_str(), to.model.text.as_ref().unwrap().content.as_str()), ("A", "B"));
}

#[test]
fn widest_takes_the_largest_width_keyframe() {
    let mut clip = text_clip("A", 0, 9000);
    assert_eq!(clip.widest(16.0 / 9.0, 4.0), 1.0, "unplaced text as wide as the frame");
    clip.placement = Some(engine::ClipPlacement { x: 0.0, y: 0.0, width: 0.3 });
    assert_eq!(clip.widest(16.0 / 9.0, 4.0), 0.3);
    clip.motion.keyframes.insert(engine::ClipProperty::Width, vec![engine::Keyframe { at: Time::ZERO, value: 0.2, easing: Default::default() }, engine::Keyframe { at: t(3000), value: 1.5, easing: Default::default() }]);
    assert_eq!(clip.widest(16.0 / 9.0, 4.0), 1.5);
}

#[test]
fn raster_scale_covers_the_widest_and_stays_within_limits() {
    // 1920 px wide frame, text 480 px wide at scale 1.
    assert_eq!(engine::text_raster_scale(0.25, 1920, 480), 1.0);
    assert_eq!(engine::text_raster_scale(0.5, 1920, 480), 2.0);
    assert_eq!(engine::text_raster_scale(8.0, 1920, 480), engine::MAX_TEXT_SCALE);
    assert_eq!(engine::text_raster_scale(0.01, 1920, 480), engine::MIN_TEXT_SCALE);
    assert_eq!(engine::text_raster_scale(1.0, 1920, 0), 1.0, "nothing drawn");
}

#[test]
fn text_steps_round_up_to_quarter_octaves() {
    use engine::text_step_up;
    assert_eq!(text_step_up(1.0), 1.0);
    assert_eq!(text_step_up(2.0), 2.0);
    assert_eq!(text_step_up(0.5), 0.5);
    let step = 2f64.powf(0.25);
    // A drag through a step gives one value, never below what was asked.
    let dragged: Vec<f64> = (0..20).map(|i| text_step_up(1.01 + i as f64 * 0.008)).collect();
    assert!(dragged.iter().all(|&s| (s - step).abs() < 1e-9), "{dragged:?}");
    for i in 1..400 {
        let wanted = 0.125 + i as f64 * 0.01;
        let got = text_step_up(wanted);
        assert!(got >= wanted && got <= wanted * step * 1.000001, "{wanted} gave {got}");
    }
    // The scale only changes when the widest crosses a step.
    assert_eq!(engine::text_raster_scale(0.45, 1920, 480), engine::text_raster_scale(0.5, 1920, 480));
}

#[test]
fn a_text_source_without_its_text_is_empty_text_not_a_missing_file() {
    struct NoFiles;
    impl engine::Resolver for NoFiles {
        fn source_exists(&self, _: &str) -> bool { false }
        fn package_exists(&self, _: &str) -> bool { true }
    }
    let mut c = text_clip("Hi", 0, 9000);
    c.text = None;
    assert_eq!(validate(&project_with(vec![c]), &NoFiles), vec![ValidationError::EmptyText { track: 0, clip: 0 }]);
}

#[test]
fn trimming_the_source_in_of_a_still_keeps_it_at_zero() {
    for still in [text_clip("Hi", 0, 9000), clip("a.png", 0, 0, 9000)] {
        let base = project_with(vec![still]);
        let after = engine::trim_source_in(&base, KEY, t(3000)).unwrap();
        assert_eq!(after, base, "a still has no source position to trim");
    }
}

#[test]
fn trimming_the_source_in_of_a_still_resets_a_stale_one_to_zero() {
    let base = project_with(vec![clip("a.png", 9000, 6000, 18000)]);
    let mut history = EditHistory::new(base.clone(), 10);
    history.apply(|p| engine::trim_source_in(p, KEY, t(3000))).unwrap();
    let trimmed = &history.current().tracks[0].clips[0];
    assert_eq!((trimmed.source_in, trimmed.start, trimmed.duration), (Time::ZERO, t(9000), t(18000)));
    assert!(history.undo());
    assert_eq!(history.current(), &base);
    assert_eq!(history.current().tracks[0].clips[0].source_in, t(6000));
}

#[test]
fn the_second_half_of_a_split_still_starts_at_source_zero() {
    for still in [text_clip("Hi", 0, 9000), clip("a.png", 0, 0, 9000)] {
        let base = project_with(vec![still]);
        let after = engine::split_clip(&base, KEY, t(3003)).unwrap();
        let clips = &after.tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[1].source_in, Time::ZERO, "{}", clips[1].source);
        assert_eq!(clips[1].start, t(3003));
    }
}
