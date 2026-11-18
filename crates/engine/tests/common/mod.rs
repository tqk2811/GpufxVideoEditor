//! Helpers shared by the integration tests.
#![allow(dead_code)]

use engine::{Clip, Effect, Output, Project, Resolver, Time, Track, TrackKind, Transition};

pub fn t(ticks: i64) -> Time {
    Time::from_ticks(ticks)
}

pub fn clip(source: &str, start: i64, source_in: i64, duration: i64) -> Clip {
    Clip {
        source: source.to_string(),
        start: t(start),
        source_in: t(source_in),
        duration: t(duration),
        effects: Vec::new(),
        gain_db: 0.0,
        muted: false,
        fade_in: Time::ZERO,
        fade_out: Time::ZERO,
        speed: 1.0,
        placement: None,
        motion: Default::default(),
        text: None,
        animation: Default::default(),
        denoise: Default::default(),
        voice: Default::default(),
        reverse: false,
        matte: Default::default(),
        follow: None,
        lock: None,
        censor: None,
    }
}

pub fn effect(package: &str) -> Effect {
    Effect {
        package: package.to_string(),
        params: Default::default(),
        keyframes: Default::default(),
        region: None,
    }
}

pub fn transition(after_clip: usize, duration: i64) -> Transition {
    Transition {
        after_clip,
        package: "mix".to_string(),
        duration: t(duration),
        params: Default::default(),
    }
}

/// The rate whose frames last `frame_duration` ticks; a duration that is not positive gives an
/// invalid rate with a zero numerator.
pub fn rate(frame_duration: i64) -> engine::FrameRate {
    engine::FrameRate::from_frame_duration(t(frame_duration)).unwrap_or(engine::FrameRate { num: 0, den: 1 })
}

/// A 1920x1080 project whose frames last `frame_duration` ticks.
pub fn project(frame_duration: i64, tracks: Vec<Track>) -> Project {
    project_at(rate(frame_duration), tracks)
}

/// A 1920x1080 project at `frame_rate`.
pub fn project_at(frame_rate: engine::FrameRate, tracks: Vec<Track>) -> Project {
    Project {
        version: engine::SUPPORTED_VERSION,
        output: Output {
            width: 1920,
            height: 1080,
            frame_rate,
            audio: None,
        },
        tracks,
        markers: Vec::new(), beats: Default::default(), motion_tracks: Vec::new(),
    }
}

pub fn track(clips: Vec<Clip>, transitions: Vec<Transition>) -> Track {
    Track {
        kind: TrackKind::Video,
        clips,
        transitions,
        locked: false,
        hidden: false,
        subtitle: None,
    }
}

pub fn audio_track(clips: Vec<Clip>) -> Track {
    Track {
        kind: TrackKind::Audio,
        clips,
        transitions: Vec::new(),
        locked: false,
        hidden: false,
        subtitle: None,
    }
}

/// Knows a fixed set of sources and packages.
pub struct SetResolver {
    pub sources: Vec<&'static str>,
    pub packages: Vec<&'static str>,
    pub without_audio: Vec<&'static str>,
}

impl Default for SetResolver {
    fn default() -> Self {
        SetResolver {
            sources: vec!["a.mp4", "b.mp4", "c.mp4"],
            packages: vec!["mix", "blur"],
            without_audio: Vec::new(),
        }
    }
}

impl Resolver for SetResolver {
    fn source_exists(&self, source: &str) -> bool {
        self.sources.contains(&source)
    }
    fn package_exists(&self, package: &str) -> bool {
        self.packages.contains(&package)
    }
    fn source_has_audio(&self, source: &str) -> bool {
        !self.without_audio.contains(&source)
    }
}
