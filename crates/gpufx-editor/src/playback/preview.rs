//! Hearing one sound of the library outside the timeline.
//!
//! There is no way to play a file on its own, so a preview plays a one-clip project through the
//! same [`Playback`] the timeline uses. It owns its output on its own thread: opening a device can
//! take a while, and the stream must stay on the thread that made it.

use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use d3d12_backend::ProjectPaths;
use engine::{AudioOutput, Clip, Output, Project, Time, Track, TrackKind, SUPPORTED_VERSION};

use super::device::Playback;

/// How often the thread looks whether the sound has ended.
const POLL: Duration = Duration::from_millis(100);
/// How long a preview lasts when the manifest gives no length.
const FALLBACK_LENGTH: Time = Time::from_ticks(engine::TICKS_PER_SECOND * 10);

/// How long to play: the shortest of what the manifest and a probe of the file say, since the
/// manifest is only what a pack's author wrote. With neither, a fixed length.
pub fn preview_length(manifest: Option<Time>, probed: Option<Time>) -> Time {
    [manifest, probed].into_iter().flatten().filter(|length| *length > Time::ZERO).min().unwrap_or(FALLBACK_LENGTH)
}

/// A project that plays `source` once from its start, for `duration`.
pub fn preview_project(source: &str, duration: Option<Time>) -> Project {
    let clip = Clip { source: source.to_owned(), start: Time::ZERO, source_in: Time::ZERO, duration: duration.filter(|length| *length > Time::ZERO).unwrap_or(FALLBACK_LENGTH), effects: Vec::new(), gain_db: 0.0, muted: false, fade_in: Time::ZERO, fade_out: Time::ZERO, speed: 1.0, placement: None };
    Project {
        version: SUPPORTED_VERSION,
        output: Output { width: 16, height: 16, frame_duration: Time::from_ticks(3000), audio: Some(AudioOutput { sample_rate: 48_000, channels: 2, bitrate: 128_000 }) },
        tracks: vec![Track { kind: TrackKind::Audio, clips: vec![clip], transitions: Vec::new(), locked: false, hidden: false }],
    }
}

enum Command {
    Play { source: String, duration: Option<Time>, paths: Arc<ProjectPaths> },
    Stop,
}

#[derive(Default)]
struct Report {
    playing: Option<String>,
    error: Option<String>,
}

/// Plays at most one sound at a time.
pub struct AudioPreview {
    commands: Sender<Command>,
    report: Arc<Mutex<Report>>,
}

impl Default for AudioPreview {
    fn default() -> Self {
        Self::spawn()
    }
}

impl AudioPreview {
    /// Starts the thread; it ends when this is dropped.
    pub fn spawn() -> Self {
        let (commands, receiver) = channel();
        let report = Arc::new(Mutex::new(Report::default()));
        let thread_report = report.clone();
        let spawned = std::thread::Builder::new().name("audio-preview".to_owned()).spawn(move || run(&receiver, &thread_report));
        if let Err(e) = spawned {
            report.lock().unwrap_or_else(std::sync::PoisonError::into_inner).error = Some(format!("starting the preview thread failed: {e}"));
        }
        Self { commands, report }
    }

    pub fn play(&self, source: String, duration: Option<Time>, paths: Arc<ProjectPaths>) {
        let _ = self.commands.send(Command::Play { source, duration, paths });
    }

    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }

    /// The source being heard.
    pub fn playing(&self) -> Option<String> {
        self.report.lock().unwrap_or_else(std::sync::PoisonError::into_inner).playing.clone()
    }

    /// What went wrong since the last call.
    pub fn take_error(&self) -> Option<String> {
        self.report.lock().unwrap_or_else(std::sync::PoisonError::into_inner).error.take()
    }
}

fn run(commands: &std::sync::mpsc::Receiver<Command>, report: &Mutex<Report>) {
    let set_playing = |source: Option<String>| report.lock().unwrap_or_else(std::sync::PoisonError::into_inner).playing = source;
    let set_error = |message: String| report.lock().unwrap_or_else(std::sync::PoisonError::into_inner).error = Some(message);
    let mut playback: Option<Playback> = None;
    loop {
        match commands.recv_timeout(POLL) {
            Ok(Command::Play { source, duration, paths }) => {
                playback = None;
                set_playing(None);
                // The file is probed here, off the UI thread, for how long it really lasts.
                let probed = d3d12_backend::probe_media(&paths.source_path(&source)).ok().and_then(|probe| probe.audio_duration);
                let length = preview_length(duration, probed);
                match Playback::open(&preview_project(&source, Some(length)), paths) {
                    Ok(opened) => {
                        if let Some(reason) = opened.no_sound() {
                            set_error(reason.to_owned());
                        }
                        opened.thread().play(Time::ZERO);
                        playback = Some(opened);
                        set_playing(Some(source));
                    }
                    Err(message) => set_error(message),
                }
            }
            Ok(Command::Stop) => {
                playback = None;
                set_playing(None);
            }
            Err(RecvTimeoutError::Timeout) => {
                let Some(current) = &playback else { continue };
                if let Some(message) = current.thread().take_error() {
                    set_error(message);
                    playback = None;
                    set_playing(None);
                } else if current.thread().reading().finished() {
                    playback = None;
                    set_playing(None);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{validate, AcceptAll};

    #[test]
    fn the_preview_ends_at_the_shorter_of_the_manifest_and_the_real_length() {
        let (short, long) = (Time::from_ticks(9000), Time::from_ticks(90_000));
        assert_eq!(preview_length(Some(long), Some(short)), short, "a manifest that says too much is not believed");
        assert_eq!(preview_length(Some(short), Some(long)), short);
        assert_eq!(preview_length(None, Some(short)), short);
        assert_eq!(preview_length(Some(long), None), long, "a file that cannot be probed keeps the manifest");
        assert_eq!(preview_length(Some(Time::ZERO), Some(Time::ZERO)), FALLBACK_LENGTH);
        assert_eq!(preview_length(None, None), FALLBACK_LENGTH);
    }

    #[test]
    fn a_preview_is_a_valid_project_of_one_audio_clip() {
        let project = preview_project("asset:sfx/p/a.flac", Some(Time::from_ticks(9000)));
        assert!(validate(&project, &AcceptAll).is_empty());
        assert_eq!(project.tracks[0].clips[0].duration, Time::from_ticks(9000));
        assert_eq!(preview_project("a.flac", None).tracks[0].clips[0].duration, FALLBACK_LENGTH);
        assert_eq!(preview_project("a.flac", Some(Time::ZERO)).tracks[0].clips[0].duration, FALLBACK_LENGTH);
    }
}
