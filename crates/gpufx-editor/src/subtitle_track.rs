//! Subtitle tracks without a window: the cues of an SRT file as clip times, and a track back as
//! an SRT file. The editor turns each cue into one text clip (see
//! [`crate::editor::EditorState::import_subtitles`]); the start, end and text of the clip are the
//! cue, which is all a later word-by-word timing needs.

use std::fmt;

use engine::{parse_srt, write_srt, SrtWarning, SrtWarningKind, Time, Track};

use crate::subtitle_style_install::SubtitleLayout;

/// Where subtitles without a style go: centred near the bottom, at most 90% of the width.
pub const DEFAULT_SUBTITLE_LAYOUT: SubtitleLayout = SubtitleLayout { anchor_y: 0.88, max_width: 0.9, max_lines: 2 };

/// One cue as it becomes a clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueSpan {
    pub start: Time,
    pub end: Time,
    pub text: String,
}

/// Something about a cue of the file: why it was left out, or how it was changed to fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CueNote {
    /// What the SRT reader said.
    Srt(SrtWarning),
    /// The cue at `line` ran into the next one and was cut where that one starts.
    Shortened { line: usize },
    /// The cue at `line` starts with or inside an earlier one that could not be cut; it is left out.
    Overlapping { line: usize },
}

impl CueNote {
    /// Whether the cue it is about is not on the track.
    pub fn skipped(&self) -> bool {
        match self {
            Self::Srt(warning) => warning.kind != SrtWarningKind::BadNumber,
            Self::Shortened { .. } => false,
            Self::Overlapping { .. } => true,
        }
    }
}

impl fmt::Display for CueNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Srt(warning) => warning.fmt(f),
            Self::Shortened { line } => write!(f, "line {line}: the cue overlaps the next one and was cut where it starts"),
            Self::Overlapping { line } => write!(f, "line {line}: the cue starts together with an earlier one, skipped"),
        }
    }
}

/// The cues of `srt` as clip times that do not overlap, in time order, and the notes about it.
///
/// A track cannot hold two clips at once, so a cue that runs into the next is cut where the next
/// starts, and a cue starting at the same time as an earlier one is left out.
pub fn cue_spans(srt: &str) -> (Vec<CueSpan>, Vec<CueNote>) {
    let document = parse_srt(srt);
    let mut notes: Vec<CueNote> = document.warnings.into_iter().map(CueNote::Srt).collect();
    let mut cues = document.cues;
    cues.sort_by_key(|cue| cue.start);
    let mut spans: Vec<(CueSpan, usize)> = Vec::with_capacity(cues.len());
    for cue in cues {
        if let Some((previous, line)) = spans.last_mut() {
            if cue.start < previous.end {
                if cue.start > previous.start {
                    previous.end = cue.start;
                    notes.push(CueNote::Shortened { line: *line });
                } else {
                    notes.push(CueNote::Overlapping { line: cue.line });
                    continue;
                }
            }
        }
        spans.push((CueSpan { start: cue.start, end: cue.end, text: cue.text }, cue.line));
    }
    (spans.into_iter().map(|(span, _)| span).collect(), notes)
}

/// The text clips of `track` as an SRT file, in time order. Clips that are not texts are left out.
pub fn track_srt(track: &Track) -> String {
    let mut clips: Vec<_> = track.clips.iter().filter_map(|clip| Some((clip.start, clip.start.saturating_add(clip.duration), clip.text.as_ref()?.content.as_str()))).collect();
    clips.sort_by_key(|clip| clip.0);
    write_srt(clips)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{text_clip, TextContent, TrackKind};

    fn ms(millis: i64) -> Time {
        Time::from_ticks(millis * 90)
    }

    #[test]
    fn overlapping_cues_are_cut_or_left_out() {
        let srt = "1\n00:00:01,000 --> 00:00:03,000\nA\n\n2\n00:00:02,000 --> 00:00:04,000\nB\n\n3\n00:00:02,000 --> 00:00:05,000\nC\n\n4\n00:00:00,000 --> 00:00:00,500\nD\n";
        let (spans, notes) = cue_spans(srt);
        let got: Vec<(Time, Time, &str)> = spans.iter().map(|span| (span.start, span.end, span.text.as_str())).collect();
        assert_eq!(got, vec![(ms(0), ms(500), "D"), (ms(1000), ms(2000), "A"), (ms(2000), ms(4000), "B")]);
        assert_eq!(notes, vec![CueNote::Shortened { line: 2 }, CueNote::Overlapping { line: 10 }]);
        assert!(!notes[0].skipped() && notes[1].skipped());
    }

    #[test]
    fn a_track_writes_back_its_texts_in_order() {
        let mut track = Track { kind: TrackKind::Video, clips: Vec::new(), transitions: Vec::new(), locked: false, hidden: false, subtitle: None };
        track.clips.push(text_clip(ms(3000), ms(1000), TextContent::new("Two\nlines", 0.05)));
        track.clips.push(text_clip(ms(1000), ms(1500), TextContent::new("One", 0.05)));
        track.clips.push(engine::Clip { text: None, source: "a.mp4".to_owned(), ..text_clip(ms(5000), ms(1000), TextContent::new("x", 0.05)) });
        let srt = track_srt(&track);
        assert_eq!(srt, "1\r\n00:00:01,000 --> 00:00:02,500\r\nOne\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nTwo\r\nlines\r\n\r\n");
        let (spans, notes) = cue_spans(&srt);
        assert!(notes.is_empty());
        assert_eq!(spans.len(), 2);
    }
}
