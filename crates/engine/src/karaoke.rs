//! Karaoke on a text clip: the words of the text, when each is spoken, and the look each word has
//! at a moment (spoken, active or upcoming, and emphasis).
//!
//! Words are found in the content as it is shaped, after its case change
//! ([`TextContent::shaped_content`]), since a case change may change the length of the text
//! ("ß" becomes "SS"). Every byte range here is a range of that shaped text.
//!
//! The look changes in steps: once at each start or end of a word. A clip is drawn once per step,
//! so a cue of a few words has only a few pictures.

use crate::project::{Clip, Rgba, TextContent};
use crate::time::Time;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// The most a karaoke look may scale a word.
pub const MAX_KARAOKE_SCALE: f32 = 4.0;
/// The padding of the box behind the active word in `box` mode, in parts of the font size.
pub const KARAOKE_BOX_PADDING: f32 = 0.12;
/// The corner radius of that box, in parts of the font size.
pub const KARAOKE_BOX_RADIUS: f32 = 0.18;
/// The box colour in `box` mode when the active look names no background. Never the word colour,
/// which would hide the word.
pub const DEFAULT_KARAOKE_BOX: Rgba = [255, 200, 0, 255];

/// How the spoken word is marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KaraokeMode {
    /// No marking: the text is drawn as it is.
    #[default]
    None,
    /// The words take the colour and opacity of their state.
    Color,
    /// As `color`, with a box behind the active word.
    Box,
    /// Meant to scale the active word. Each word would need a picture of its own, which is not
    /// made yet, so it draws as `color`.
    Scale,
}

/// The look of the words in one state. A field left out keeps the text's own.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KaraokeLook {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Rgba>,
    /// The box behind the word in `box` mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Rgba>,
    /// Above 0 and at most [`MAX_KARAOKE_SCALE`]. Kept; nothing scales a word yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
}

/// When one word is spoken, in source time of the clip (the time `source_in` counts in), so a
/// split clip keeps its words where they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KaraokeWord {
    pub start: Time,
    pub end: Time,
    /// Drawn with the `emphasis` look on top of its state's.
    #[serde(default, skip_serializing_if = "is_false")]
    pub emphasis: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The karaoke of a text clip.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextKaraoke {
    pub mode: KaraokeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<KaraokeLook>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken: Option<KaraokeLook>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upcoming: Option<KaraokeLook>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emphasis: Option<KaraokeLook>,
    /// The time of each word, one per word of the shaped text. Empty: the words share the clip's
    /// cue by their number of characters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<KaraokeWord>,
}

impl TextKaraoke {
    /// Whether it marks anything.
    pub fn is_on(&self) -> bool {
        self.mode != KaraokeMode::None
    }

    /// Whether it draws a box behind the active word, so the picture keeps room for one.
    pub fn draws_box(&self) -> bool {
        self.mode == KaraokeMode::Box
    }
}

/// The state of a word at a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordState {
    Spoken,
    Active,
    Upcoming,
}

/// How one word is drawn at a moment: its fill colour (opacity applied), and the box behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WordStyle {
    pub color: Rgba,
    pub background: Option<Rgba>,
}

/// The byte ranges of the words of `shaped`, the text as shaped: runs of characters that are not
/// white space.
pub fn word_ranges(shaped: &str) -> Vec<Range<usize>> {
    let mut words = Vec::new();
    let mut start = None;
    for (index, ch) in shaped.char_indices() {
        match (ch.is_whitespace(), start) {
            (true, Some(s)) => {
                words.push(s..index);
                start = None;
            }
            (false, None) => start = Some(index),
            _ => {}
        }
    }
    if let Some(s) = start {
        words.push(s..shaped.len());
    }
    words
}

/// The words of `text` share `start..end` by their number of characters, in order and touching.
pub fn split_by_characters(text: &TextContent, start: Time, end: Time) -> Vec<KaraokeWord> {
    let shaped = text.shaped_content();
    let counts: Vec<i64> = word_ranges(&shaped).into_iter().map(|range| shaped[range].chars().count() as i64).collect();
    let total: i64 = counts.iter().sum();
    let span = end.ticks().saturating_sub(start.ticks()).max(0);
    let mut words = Vec::with_capacity(counts.len());
    let mut before = 0i64;
    for count in counts {
        // Each edge is placed from the total so far, so rounding never adds up.
        let at = |chars: i64| Time::from_ticks(start.ticks() + (span as i128 * chars as i128 / total.max(1) as i128) as i64);
        words.push(KaraokeWord { start: at(before), end: at(before + count), emphasis: false });
        before += count;
    }
    words
}

/// The words of a clip with karaoke on, with their times: the karaoke's own, or its cue shared by
/// characters. The cue runs from source time 0 to the source end of the clip. `None` when the
/// clip has no text or its karaoke is off.
pub fn clip_words(clip: &Clip) -> Option<Vec<KaraokeWord>> {
    let text = clip.text.as_ref()?;
    let karaoke = text.karaoke.as_ref().filter(|karaoke| karaoke.is_on())?;
    if !karaoke.words.is_empty() {
        return Some(karaoke.words.clone());
    }
    Some(split_by_characters(text, Time::ZERO, cue_end(clip)))
}

/// Where a clip's source ends: `source_in` plus the source it consumes.
pub fn cue_end(clip: &Clip) -> Time {
    clip.source_end()
}

/// The times at which the look of `words` changes, sorted, each once.
pub fn step_times(words: &[KaraokeWord]) -> Vec<Time> {
    let mut times: Vec<Time> = words.iter().flat_map(|word| [word.start, word.end]).collect();
    times.sort_unstable();
    times.dedup();
    times
}

/// The step `time` falls in: how many of `steps` (from [`step_times`]) are at or before it.
pub fn step_at(steps: &[Time], time: Time) -> usize {
    steps.partition_point(|step| *step <= time)
}

/// The state of each word at `time`.
pub fn word_states(words: &[KaraokeWord], time: Time) -> Vec<WordState> {
    words
        .iter()
        .map(|word| {
            if word.end <= time {
                WordState::Spoken
            } else if word.start <= time {
                WordState::Active
            } else {
                WordState::Upcoming
            }
        })
        .collect()
}

/// The state of each word during step `step` of `steps`: before the first step every word is to
/// come, and in a later step the words are as at the time it starts.
pub fn step_states(words: &[KaraokeWord], steps: &[Time], step: usize) -> Vec<WordState> {
    match step.checked_sub(1).and_then(|index| steps.get(index)) {
        Some(time) => word_states(words, *time),
        None => vec![WordState::Upcoming; words.len()],
    }
}

/// How each word of `text` is drawn with these states. `text` must have karaoke on.
pub fn word_styles(text: &TextContent, words: &[KaraokeWord], states: &[WordState]) -> Vec<WordStyle> {
    let karaoke = text.karaoke.clone().unwrap_or_default();
    states
        .iter()
        .zip(words.iter().map(|word| word.emphasis).chain(std::iter::repeat(false)))
        .map(|(state, emphasis)| {
            let look = match state {
                WordState::Spoken => karaoke.spoken,
                WordState::Active => karaoke.active,
                WordState::Upcoming => karaoke.upcoming,
            }
            .unwrap_or_default();
            let extra = if emphasis { karaoke.emphasis.unwrap_or_default() } else { KaraokeLook::default() };
            let mut color = extra.color.or(look.color).unwrap_or(text.color);
            let opacity = extra.opacity.or(look.opacity).unwrap_or(1.0);
            if opacity.is_finite() {
                color[3] = (f32::from(color[3]) * opacity.clamp(0.0, 1.0)).round() as u8;
            }
            let background = (karaoke.draws_box() && *state == WordState::Active).then(|| extra.background.or(look.background).unwrap_or(DEFAULT_KARAOKE_BOX));
            WordStyle { color, background }
        })
        .collect()
}

/// What is wrong with the karaoke of `text`, if anything: a look out of range, or word times that
/// are not one per word, in order and not overlapping.
pub fn karaoke_problem(text: &TextContent) -> Option<String> {
    let karaoke = text.karaoke.as_ref()?;
    for (state, look) in [("active", karaoke.active), ("spoken", karaoke.spoken), ("upcoming", karaoke.upcoming), ("emphasis", karaoke.emphasis)] {
        let Some(look) = look else { continue };
        if let Some(scale) = look.scale {
            if !(scale.is_finite() && scale > 0.0 && scale <= MAX_KARAOKE_SCALE) {
                return Some(format!("karaoke.{state}.scale must be above 0 and at most {MAX_KARAOKE_SCALE}, not {scale}"));
            }
        }
        if let Some(opacity) = look.opacity {
            if !(opacity.is_finite() && (0.0..=1.0).contains(&opacity)) {
                return Some(format!("karaoke.{state}.opacity must be from 0 to 1, not {opacity}"));
            }
        }
    }
    if karaoke.words.is_empty() {
        return None;
    }
    let count = word_ranges(&text.shaped_content()).len();
    if karaoke.words.len() != count {
        return Some(format!("karaoke.words has {} times but the text has {count} words", karaoke.words.len()));
    }
    let mut previous_end = Time::ZERO;
    for (index, word) in karaoke.words.iter().enumerate() {
        if word.start < previous_end || word.end <= word.start {
            return Some(format!("karaoke.words[{index}] must start at or after the word before it ends and end after it starts"));
        }
        previous_end = word.end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn karaoke_text(content: &str) -> TextContent {
        let mut text = TextContent::new(content, 0.1);
        text.karaoke = Some(TextKaraoke { mode: KaraokeMode::Color, active: Some(KaraokeLook { color: Some([255, 0, 0, 255]), ..KaraokeLook::default() }), upcoming: Some(KaraokeLook { opacity: Some(0.5), ..KaraokeLook::default() }), ..TextKaraoke::default() });
        text
    }

    #[test]
    fn words_are_runs_between_white_space() {
        assert_eq!(word_ranges("  ab c\nde "), vec![2..4, 5..6, 7..9]);
        assert!(word_ranges(" \n ").is_empty());
    }

    #[test]
    fn a_cue_is_shared_by_characters() {
        let words = split_by_characters(&karaoke_text("a bbb"), Time::ZERO, Time::from_ticks(400));
        assert_eq!(words.iter().map(|w| (w.start.ticks(), w.end.ticks())).collect::<Vec<_>>(), [(0, 100), (100, 400)]);
    }

    #[test]
    fn words_are_found_in_the_text_after_its_case_change() {
        // "ß" becomes "SS": the second word starts later in the shaped text than in the content.
        let mut text = karaoke_text("straße ab");
        text.transform = crate::project::TextTransform::Uppercase;
        let shaped = text.shaped_content();
        let ranges = word_ranges(&shaped);
        assert_eq!(&shaped[ranges[1].clone()], "AB");
        let words = split_by_characters(&text, Time::ZERO, Time::from_ticks(900));
        // STRASSE has 7 characters, AB 2.
        assert_eq!(words[0].end.ticks(), 700);
    }

    #[test]
    fn the_look_changes_in_steps() {
        let words = split_by_characters(&karaoke_text("one two"), Time::ZERO, Time::from_ticks(600));
        let steps = step_times(&words);
        assert_eq!(steps.len(), 3, "start, the shared edge, end");
        assert_eq!(step_at(&steps, Time::from_ticks(100)), 1);
        assert_eq!(step_at(&steps, Time::from_ticks(300)), 2);
        assert_eq!(step_states(&words, &steps, 2), [WordState::Spoken, WordState::Active]);
        assert_eq!(step_states(&words, &steps, 0), [WordState::Upcoming, WordState::Upcoming]);
        assert_eq!(step_states(&words, &steps, 3), [WordState::Spoken, WordState::Spoken]);
    }

    #[test]
    fn each_state_takes_its_look() {
        let text = karaoke_text("one two three");
        let words = split_by_characters(&text, Time::ZERO, Time::from_ticks(900));
        let styles = word_styles(&text, &words, &[WordState::Spoken, WordState::Active, WordState::Upcoming]);
        assert_eq!(styles[0].color, [255, 255, 255, 255]);
        assert_eq!(styles[1].color, [255, 0, 0, 255]);
        assert_eq!(styles[2].color, [255, 255, 255, 128]);
        assert!(styles.iter().all(|style| style.background.is_none()), "color mode draws no box");
    }

    #[test]
    fn box_mode_puts_a_box_behind_the_active_word_only() {
        let mut text = karaoke_text("one two");
        text.karaoke.as_mut().unwrap().mode = KaraokeMode::Box;
        let words = split_by_characters(&text, Time::ZERO, Time::from_ticks(600));
        let styles = word_styles(&text, &words, &[WordState::Active, WordState::Upcoming]);
        assert!(styles[0].background.is_some());
        assert_eq!(styles[1].background, None);
    }

    #[test]
    fn word_times_must_match_the_words_and_keep_order() {
        let mut text = karaoke_text("one two");
        assert_eq!(karaoke_problem(&text), None);
        let word = |start: i64, end: i64| KaraokeWord { start: Time::from_ticks(start), end: Time::from_ticks(end), emphasis: false };
        text.karaoke.as_mut().unwrap().words = vec![word(0, 10)];
        assert!(karaoke_problem(&text).unwrap().contains("2 words"));
        text.karaoke.as_mut().unwrap().words = vec![word(0, 10), word(5, 20)];
        assert!(karaoke_problem(&text).unwrap().contains("words[1]"));
        text.karaoke.as_mut().unwrap().words = vec![word(0, 10), word(10, 20)];
        assert_eq!(karaoke_problem(&text), None);
        text.karaoke.as_mut().unwrap().active.as_mut().unwrap().opacity = Some(2.0);
        assert!(karaoke_problem(&text).unwrap().contains("opacity"));
    }

    #[test]
    fn a_project_without_karaoke_reads_as_before() {
        let text: TextContent = serde_json::from_str(r#"{"content":"Hi","size":0.1}"#).unwrap();
        assert_eq!(text.karaoke, None);
        assert!(!serde_json::to_string(&text).unwrap().contains("karaoke"));
    }
    #[test]
    fn the_cue_ends_where_the_clip_source_ends() {
        let mut clip = crate::edit::text_clip(Time::ZERO, Time::from_ticks(3), karaoke_text("a b"));
        clip.speed = 1.5;
        assert_eq!(cue_end(&clip), clip.source_end());
    }

    #[test]
    fn the_box_does_not_take_the_colour_of_the_word() {
        let mut text = karaoke_text("one two");
        text.karaoke.as_mut().unwrap().mode = KaraokeMode::Box;
        let words = split_by_characters(&text, Time::ZERO, Time::from_ticks(600));
        let styles = word_styles(&text, &words, &[WordState::Active]);
        assert_eq!(styles[0].background, Some(DEFAULT_KARAOKE_BOX), "the default box when no background is named");
        text.karaoke.as_mut().unwrap().active.as_mut().unwrap().background = Some([0, 0, 255, 255]);
        assert_eq!(word_styles(&text, &words, &[WordState::Active])[0].background, Some([0, 0, 255, 255]));
    }
}
