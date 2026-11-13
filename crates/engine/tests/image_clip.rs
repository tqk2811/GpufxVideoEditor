//! Clips of still images in the project model, the validator and the audio schedule.

use engine::{audio_segments, is_image_source, set_speed, trim_end, trim_start, validate, AcceptAll, Clip, ClipKey, EditError, Output, Project, Time, Track, TrackKind, ValidationError, SUPPORTED_VERSION, TICKS_PER_SECOND};

fn seconds(value: i64) -> Time {
    Time::from_ticks(value * TICKS_PER_SECOND)
}

fn clip(source: &str, start: i64, duration: i64) -> Clip {
    Clip { source: source.to_owned(), start: seconds(start), source_in: Time::ZERO, duration: seconds(duration), effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None, motion: Default::default(), text: None, animation: Default::default() }
}

fn project(kind: TrackKind, clips: Vec<Clip>) -> Project {
    Project { version: SUPPORTED_VERSION, output: Output { width: 320, height: 180, frame_duration: Time::from_ticks(3000), audio: None }, tracks: vec![Track { kind, clips, transitions: Vec::new(), locked: false, hidden: false }] }
}

const KEY: ClipKey = ClipKey { track: 0, clip: 0 };

#[test]
fn an_image_source_is_told_by_its_extension_in_any_case_and_form() {
    for source in ["a.png", "C:\\media\\A.JPG", "b.jpeg", "x/y.WebP", "asset:sticker/pack/images/a.png"] {
        assert!(is_image_source(source), "{source}");
    }
    for source in ["a.mp4", "clip.mp3", "png", "a.png.mp4", "dir.png/clip.mp4", "asset:sfx/p/a.flac"] {
        assert!(!is_image_source(source), "{source}");
    }
}

#[test]
fn an_image_clip_can_be_longer_than_five_seconds_and_keeps_its_source_in_and_duration() {
    let mut timeline = project(TrackKind::Video, vec![clip("sticker.png", 0, 5)]);
    timeline = trim_end(&timeline, KEY, seconds(60)).expect("no source length limits an image");
    assert_eq!(timeline.tracks[0].clips[0].duration, seconds(60));
    assert!(validate(&timeline, &AcceptAll).is_empty(), "{:?}", validate(&timeline, &AcceptAll));
}

#[test]
fn the_left_edge_of_an_image_can_be_pulled_out_and_in_keeping_no_source_offset() {
    let timeline = project(TrackKind::Video, vec![clip("sticker.png", 10, 5)]);
    let earlier = trim_start(&timeline, KEY, seconds(4)).expect("no first frame limits an image");
    let earlier_clip = &earlier.tracks[0].clips[0];
    assert_eq!((earlier_clip.start, earlier_clip.source_in, earlier_clip.duration), (seconds(4), Time::ZERO, seconds(11)));
    assert!(validate(&earlier, &AcceptAll).is_empty(), "{:?}", validate(&earlier, &AcceptAll));
    let later = trim_start(&earlier, KEY, seconds(7)).expect("the edge can come back");
    let later_clip = &later.tracks[0].clips[0];
    assert_eq!((later_clip.start, later_clip.source_in, later_clip.duration), (seconds(7), Time::ZERO, seconds(8)));
}

#[test]
fn the_left_edge_of_a_video_still_stops_at_its_first_frame() {
    let timeline = project(TrackKind::Video, vec![clip("video.mp4", 10, 5)]);
    assert!(matches!(trim_start(&timeline, KEY, seconds(4)), Err(EditError::BeforeSourceStart { .. })));
}

#[test]
fn an_image_on_an_audio_track_is_refused_by_validation() {
    let timeline = project(TrackKind::Audio, vec![clip("sticker.png", 0, 5)]);
    let errors = validate(&timeline, &AcceptAll);
    assert_eq!(errors, vec![ValidationError::ImageOnAudioTrack { track: 0, clip: 0, source: "sticker.png".to_owned() }]);
    assert!(errors[0].to_string().contains("sticker.png"));
}

#[test]
fn an_image_makes_no_sound_even_on_a_project_with_audio() {
    let mut timeline = project(TrackKind::Video, vec![clip("sticker.png", 0, 2), clip("video.mp4", 2, 2)]);
    timeline.output.audio = Some(engine::AudioOutput { sample_rate: 48_000, channels: 2, bitrate: 128_000 });
    let segments = audio_segments(&timeline, 0, 48_000 * 4);
    assert_eq!(segments.len(), 1, "only the video clip is opened for its sound");
    assert_eq!(segments[0].source, "video.mp4");
}

#[test]
fn the_speed_of_an_image_cannot_be_changed() {
    let timeline = project(TrackKind::Video, vec![clip("sticker.png", 0, 5)]);
    assert!(matches!(set_speed(&timeline, KEY, 2.0), Err(EditError::SpeedOfImage { .. })));
}

#[test]
fn a_file_of_the_previous_format_with_an_image_loads_and_saves_without_new_fields() {
    let json = r#"{"version":2,"output":{"width":320,"height":180,"frame_duration":3000},"tracks":[{"clips":[{"source":"a.mp4","start":0,"source_in":0,"duration":90000},{"source":"logo.png","start":90000,"source_in":0,"duration":90000}]}]}"#;
    let loaded = Project::from_json(json).expect("a version 2 file loads");
    assert!(validate(&loaded, &AcceptAll).is_empty());
    assert!(!loaded.tracks[0].clips[0].is_image());
    assert!(loaded.tracks[0].clips[1].is_image());
    let saved = serde_json::to_string(&loaded).unwrap();
    assert_eq!(Project::from_json(&saved).unwrap(), loaded, "round trip");
    assert!(!saved.contains("image"), "no field is added: {saved}");
    assert!(saved.contains(r#""version":2"#));
}

#[test]
fn a_version_one_project_with_no_images_is_read_as_before() {
    let json = r#"{"version":1,"output":{"width":320,"height":180,"frame_duration":3000},"tracks":[{"clips":[{"source":"a.mp4","start":0,"source_in":0,"duration":90000}]}]}"#;
    let loaded = Project::from_json(json).expect("a version 1 file loads");
    assert_eq!(loaded.tracks[0].clips[0].source, "a.mp4");
    assert!(validate(&loaded, &AcceptAll).is_empty());
}
