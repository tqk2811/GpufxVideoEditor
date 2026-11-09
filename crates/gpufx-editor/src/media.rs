//! The media imported in this session and where a new clip lands. No window here.

use d3d12_backend::MediaProbe;
use engine::{nearest_free_start, Project, Time, TrackKind};

use crate::editor::clip_label;
use crate::project_file::same_source;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
}

impl MediaKind {
    pub fn track_kind(self) -> TrackKind {
        match self {
            Self::Video => TrackKind::Video,
            Self::Audio => TrackKind::Audio,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaItem {
    /// The path as stored in a clip.
    pub source: String,
    pub name: String,
    pub kind: MediaKind,
    pub duration: Option<Time>,
    /// The duration is only what the project's clips use of the file, not a probe.
    pub estimated: bool,
}

/// One item for `path` from what a probe found, or `None` when it has nothing playable.
pub fn item_from_probe(path: &str, probe: &MediaProbe) -> Option<MediaItem> {
    let (kind, duration) = if probe.has_video {
        (MediaKind::Video, probe.video_duration)
    } else if probe.has_audio {
        (MediaKind::Audio, probe.audio_duration)
    } else {
        return None;
    };
    Some(MediaItem { source: path.to_owned(), name: clip_label(path).to_owned(), kind, duration: duration.filter(|length| *length > Time::ZERO), estimated: false })
}

/// The media of this session. Stays when a project is replaced; not saved in the project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaLibrary {
    items: Vec<MediaItem>,
}

impl MediaLibrary {
    /// Adds `item`; the same source is replaced by a probed entry, never listed twice.
    pub fn add(&mut self, item: MediaItem) {
        match self.items.iter_mut().find(|known| same_source(&known.source, &item.source)) {
            Some(known) if known.estimated || !item.estimated => *known = item,
            Some(_) => {}
            None => self.items.push(item),
        }
    }

    /// Adds every source the clips of `project` use and the library does not have yet.
    pub fn adopt_project_media(&mut self, project: &Project) {
        for track in &project.tracks {
            let kind = if track.is_audio() { MediaKind::Audio } else { MediaKind::Video };
            for clip in &track.clips {
                let used_to = clip.source_in.saturating_add(clip.duration);
                let known = self.items.iter_mut().find(|item| same_source(&item.source, &clip.source));
                match known {
                    Some(item) if item.estimated => item.duration = item.duration.map_or(Some(used_to), |seen| Some(seen.max(used_to))),
                    Some(_) => {}
                    None => self.items.push(MediaItem { source: clip.source.clone(), name: clip_label(&clip.source).to_owned(), kind, duration: Some(used_to), estimated: true }),
                }
            }
        }
    }

    /// The items of `kind` whose name contains every word of `search`, ignoring case.
    pub fn visible(&self, kind: MediaKind, search: &str) -> Vec<&MediaItem> {
        let search = search.to_lowercase();
        let words: Vec<&str> = search.split_whitespace().collect();
        self.items.iter().filter(|item| item.kind == kind && words.iter().all(|word| item.name.to_lowercase().contains(word))).collect()
    }
}

/// The track that takes a clip of `duration` at the playhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackChoice {
    Existing(usize),
    /// No track of the kind has the place free: a new one, on top, gets the clip.
    New,
}

/// The first track of `kind` where the clip fits exactly at `playhead`, so the clip plays where the
/// user is looking. A clip is never moved to some other time just to reuse a track. A locked track
/// takes no new clip, and a hidden one would not show it, so both are passed over.
pub fn choose_track(project: &Project, kind: TrackKind, duration: Time, playhead: Time) -> TrackChoice {
    let found = project.tracks.iter().position(|track| track.kind == kind && !track.locked && !track.hidden && nearest_free_start(track, None, duration, playhead) == Some(playhead));
    found.map_or(TrackChoice::New, TrackChoice::Existing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::new_clip;
    use engine::{Output, Track, SUPPORTED_VERSION};

    fn seconds(value: i64) -> Time {
        Time::from_seconds(value).unwrap()
    }

    fn track(kind: TrackKind, spans: &[(i64, i64)]) -> Track {
        Track { kind, clips: spans.iter().map(|&(start, length)| new_clip("x.mp4", seconds(start), seconds(length))).collect(), transitions: Vec::new(), locked: false, hidden: false }
    }

    fn project(tracks: Vec<Track>) -> Project {
        Project { version: SUPPORTED_VERSION, output: Output { width: 1920, height: 1080, frame_duration: Time::from_ticks(3000), audio: None }, tracks }
    }

    fn probe(video: Option<i64>, audio: Option<i64>) -> MediaProbe {
        MediaProbe { has_video: video.is_some(), has_audio: audio.is_some(), video_duration: video.map(seconds), audio_duration: audio.map(seconds), video_problem: None, audio_problem: None }
    }

    #[test]
    fn a_free_track_of_the_kind_is_chosen() {
        let timeline = project(vec![track(TrackKind::Video, &[]), track(TrackKind::Audio, &[])]);
        assert_eq!(choose_track(&timeline, TrackKind::Video, seconds(5), seconds(1)), TrackChoice::Existing(0));
        assert_eq!(choose_track(&timeline, TrackKind::Audio, seconds(5), seconds(1)), TrackChoice::Existing(1));
    }

    #[test]
    fn a_track_taken_at_the_playhead_is_skipped_for_the_next_free_one() {
        let timeline = project(vec![track(TrackKind::Video, &[(0, 10)]), track(TrackKind::Audio, &[]), track(TrackKind::Video, &[])]);
        assert_eq!(choose_track(&timeline, TrackKind::Video, seconds(5), seconds(1)), TrackChoice::Existing(2));
    }

    #[test]
    fn a_clip_that_would_run_into_the_next_one_needs_another_track() {
        let timeline = project(vec![track(TrackKind::Video, &[(4, 10)])]);
        assert_eq!(choose_track(&timeline, TrackKind::Video, seconds(5), seconds(1)), TrackChoice::New);
        assert_eq!(choose_track(&timeline, TrackKind::Video, seconds(3), seconds(1)), TrackChoice::Existing(0), "it just fits before");
    }

    #[test]
    fn with_no_track_of_the_kind_a_new_one_is_needed() {
        assert_eq!(choose_track(&project(vec![track(TrackKind::Audio, &[])]), TrackKind::Video, seconds(1), Time::ZERO), TrackChoice::New);
        assert_eq!(choose_track(&project(Vec::new()), TrackKind::Audio, seconds(1), Time::ZERO), TrackChoice::New);
    }

    #[test]
    fn a_locked_or_hidden_track_is_passed_over() {
        let mut timeline = project(vec![track(TrackKind::Video, &[]), track(TrackKind::Video, &[])]);
        timeline.tracks[0].locked = true;
        assert_eq!(choose_track(&timeline, TrackKind::Video, seconds(1), Time::ZERO), TrackChoice::Existing(1));
        timeline.tracks[1].hidden = true;
        assert_eq!(choose_track(&timeline, TrackKind::Video, seconds(1), Time::ZERO), TrackChoice::New);
    }

    #[test]
    fn a_probe_decides_the_kind_and_the_length() {
        let video = item_from_probe("C:/a/clip.mp4", &probe(Some(4), Some(9))).unwrap();
        assert_eq!((video.kind, video.duration, video.name.as_str()), (MediaKind::Video, Some(seconds(4)), "clip.mp4"));
        let audio = item_from_probe("music.mp3", &probe(None, Some(9))).unwrap();
        assert_eq!((audio.kind, audio.duration), (MediaKind::Audio, Some(seconds(9))));
        assert_eq!(item_from_probe("x", &probe(None, None)), None);
    }

    fn item(source: &str, kind: MediaKind, duration: Option<i64>, estimated: bool) -> MediaItem {
        MediaItem { source: source.to_owned(), name: clip_label(source).to_owned(), kind, duration: duration.map(seconds), estimated }
    }

    #[test]
    fn the_same_source_is_listed_once_and_a_probe_replaces_an_estimate() {
        let mut library = MediaLibrary::default();
        library.add(item("a.mp4", MediaKind::Video, Some(3), true));
        library.add(item("a.mp4", MediaKind::Video, Some(9), false));
        assert_eq!(library.visible(MediaKind::Video, "")[0].duration, Some(seconds(9)));
        library.add(item("a.mp4", MediaKind::Video, Some(1), true));
        let listed = library.visible(MediaKind::Video, "");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].duration, Some(seconds(9)), "an estimate does not replace a probe");
    }

    #[cfg(windows)]
    #[test]
    fn a_source_differing_only_by_case_or_slash_is_the_same_item() {
        let mut library = MediaLibrary::default();
        library.add(item("C:/v/A.mp4", MediaKind::Video, Some(3), false));
        library.add(item("c:\\V\\a.mp4", MediaKind::Video, Some(9), false));
        assert_eq!(library.visible(MediaKind::Video, "").len(), 1);
        let mut timeline = project(vec![track(TrackKind::Video, &[(0, 2)])]);
        timeline.tracks[0].clips[0].source = "C:\\v\\a.MP4".to_owned();
        library.adopt_project_media(&timeline);
        assert_eq!(library.visible(MediaKind::Video, "").len(), 1, "the clip uses the item already there");
    }

    #[test]
    fn media_used_by_clips_is_adopted_with_the_length_they_use() {
        let mut timeline = project(vec![track(TrackKind::Video, &[(0, 2)]), track(TrackKind::Audio, &[(0, 7)])]);
        timeline.tracks[0].clips[0].source = "C:/v/a.mp4".to_owned();
        timeline.tracks[0].clips[0].source_in = seconds(3);
        timeline.tracks[1].clips[0].source = "C:/v/m.wav".to_owned();
        let mut library = MediaLibrary::default();
        library.adopt_project_media(&timeline);
        let video = &library.visible(MediaKind::Video, "")[0];
        assert_eq!((video.name.as_str(), video.duration, video.estimated), ("a.mp4", Some(seconds(5)), true));
        assert_eq!(library.visible(MediaKind::Audio, "")[0].name, "m.wav");
        library.adopt_project_media(&timeline);
        assert_eq!(library.visible(MediaKind::Video, "").len(), 1);
    }

    #[test]
    fn adopting_leaves_a_probed_length_alone() {
        let timeline = project(vec![track(TrackKind::Video, &[(0, 2)])]);
        let mut library = MediaLibrary::default();
        library.add(item("x.mp4", MediaKind::Video, Some(60), false));
        library.adopt_project_media(&timeline);
        assert_eq!(library.visible(MediaKind::Video, "")[0].duration, Some(seconds(60)));
    }

    #[test]
    fn the_search_matches_every_word_ignoring_case() {
        let mut library = MediaLibrary::default();
        library.add(item("Beach Day.mp4", MediaKind::Video, Some(1), false));
        library.add(item("night.mp4", MediaKind::Video, Some(1), false));
        library.add(item("beach.wav", MediaKind::Audio, Some(1), false));
        assert_eq!(library.visible(MediaKind::Video, "").len(), 2);
        assert_eq!(library.visible(MediaKind::Video, "day BEACH")[0].name, "Beach Day.mp4");
        assert!(library.visible(MediaKind::Video, "beach wav").is_empty(), "the kind is respected");
    }
}
