//! Helpers shared by the integration tests.
#![allow(dead_code)]

use engine::{Clip, Effect, Output, Project, Resolver, Time, Track, Transition};

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
    }
}

pub fn effect(package: &str) -> Effect {
    Effect {
        package: package.to_string(),
        params: Default::default(),
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

pub fn project(frame_duration: i64, tracks: Vec<Track>) -> Project {
    Project {
        version: 1,
        output: Output {
            width: 1920,
            height: 1080,
            frame_duration: t(frame_duration),
        },
        tracks,
    }
}

pub fn track(clips: Vec<Clip>, transitions: Vec<Transition>) -> Track {
    Track { clips, transitions }
}

/// Knows a fixed set of sources and packages.
pub struct SetResolver {
    pub sources: Vec<&'static str>,
    pub packages: Vec<&'static str>,
}

impl Default for SetResolver {
    fn default() -> Self {
        SetResolver {
            sources: vec!["a.mp4", "b.mp4", "c.mp4"],
            packages: vec!["mix", "blur"],
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
}
