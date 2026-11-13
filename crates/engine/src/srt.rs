//! Reading and writing SubRip (`.srt`) subtitles.
//!
//! [`parse_srt`] is lenient: it takes UTF-8 with or without a byte order mark, CRLF or LF line
//! ends, `,` or `.` before the milliseconds, a missing or wrong cue number and cues of several
//! lines. A cue it cannot read is left out and named in a [`SrtWarning`] with its line number, so
//! one broken cue never loses the rest of the file. Formatting tags (`<i>`, `<b>`, `<u>`, `<font>`)
//! and SSA overrides (`{\an8}`) are dropped: a subtitle clip takes its look from its style.
//!
//! [`write_srt`] writes cues back with CRLF line ends and `,` before the milliseconds.

use crate::time::{Time, TICKS_PER_SECOND};

/// One subtitle of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrtCue {
    /// When it shows.
    pub start: Time,
    /// When it goes; always after `start`.
    pub end: Time,
    /// The text without tags, lines separated by a line feed. Never empty.
    pub text: String,
    /// The 1-based line of the file where the cue's timing is.
    pub line: usize,
}

/// Why a cue was left out, or a note about one that was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrtWarning {
    /// The 1-based line of the file it is about.
    pub line: usize,
    pub kind: SrtWarningKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrtWarningKind {
    /// A block without a `start --> end` line; it is left out.
    MissingTiming,
    /// A timing line that does not read; the cue is left out.
    BadTiming(String),
    /// The end is not after the start; the cue is left out.
    EmptyDuration,
    /// The cue has no text once its tags are dropped; it is left out.
    EmptyText,
    /// The cue number is missing or is not the one after the previous cue; the cue is kept.
    BadNumber,
}

impl std::fmt::Display for SrtWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: ", self.line)?;
        match &self.kind {
            SrtWarningKind::MissingTiming => write!(f, "no timing line, block skipped"),
            SrtWarningKind::BadTiming(text) => write!(f, "unreadable timing `{text}`, cue skipped"),
            SrtWarningKind::EmptyDuration => write!(f, "the cue ends before it starts, skipped"),
            SrtWarningKind::EmptyText => write!(f, "the cue has no text, skipped"),
            SrtWarningKind::BadNumber => write!(f, "missing or out-of-order cue number, cue kept"),
        }
    }
}

/// What a file holds: its cues in file order and the warnings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SrtDocument {
    pub cues: Vec<SrtCue>,
    pub warnings: Vec<SrtWarning>,
}

/// Reads an SRT file's text.
pub fn parse_srt(input: &str) -> SrtDocument {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let lines: Vec<&str> = input.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).collect();
    let mut document = SrtDocument::default();
    let mut previous_number: Option<u64> = None;
    let mut index = 0;
    while index < lines.len() {
        if lines[index].trim().is_empty() {
            index += 1;
            continue;
        }
        let block_start = index;
        while index < lines.len() && !lines[index].trim().is_empty() {
            index += 1;
        }
        let block = &lines[block_start..index];
        // The timing is the first line with an arrow, which a number line may precede.
        let timing_at = block.iter().take(2).position(|line| line.contains("-->"));
        let number = if timing_at != Some(0) { block[0].trim().parse::<u64>().ok() } else { None };
        let expected = previous_number.map_or(1, |previous| previous + 1);
        previous_number = Some(number.unwrap_or(expected));
        let Some(timing_at) = timing_at else {
            document.warnings.push(SrtWarning { line: block_start + 1, kind: SrtWarningKind::MissingTiming });
            continue;
        };
        let timing_line = block_start + timing_at + 1;
        if number != Some(expected) {
            document.warnings.push(SrtWarning { line: block_start + 1, kind: SrtWarningKind::BadNumber });
        }
        let Some((start, end)) = parse_timing(block[timing_at]) else {
            document.warnings.push(SrtWarning { line: timing_line, kind: SrtWarningKind::BadTiming(block[timing_at].trim().to_owned()) });
            continue;
        };
        if end <= start {
            document.warnings.push(SrtWarning { line: timing_line, kind: SrtWarningKind::EmptyDuration });
            continue;
        }
        let text = block[timing_at + 1..].iter().map(|line| strip_tags(line).trim().to_owned()).filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n");
        if text.is_empty() {
            document.warnings.push(SrtWarning { line: timing_line, kind: SrtWarningKind::EmptyText });
            continue;
        }
        document.cues.push(SrtCue { start, end, text, line: timing_line });
    }
    document
}

/// `00:00:01,000 --> 00:00:02,500`, ignoring anything after the end time (position hints).
fn parse_timing(line: &str) -> Option<(Time, Time)> {
    let (start, rest) = line.split_once("-->")?;
    let end = rest.split_whitespace().next()?;
    Some((parse_timestamp(start.trim())?, parse_timestamp(end)?))
}

/// `hh:mm:ss,mmm`, `hh:mm:ss.mmm`, `mm:ss,mmm`, with one to three millisecond digits.
fn parse_timestamp(text: &str) -> Option<Time> {
    let (clock, fraction) = match text.find([',', '.']) {
        Some(at) => (&text[..at], &text[at + 1..]),
        None => (text, ""),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let number = |part: &str| part.parse::<i64>().ok();
    let (hours, minutes, seconds) = if parts.len() == 3 { (number(parts[0])?, number(parts[1])?, number(parts[2])?) } else { (0, number(parts[0])?, number(parts[1])?) };
    if minutes >= 60 || seconds >= 60 || fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let millis = if fraction.is_empty() { 0 } else { fraction.parse::<i64>().ok()? * 10_i64.pow(3 - fraction.len() as u32) };
    let total_millis = hours.checked_mul(3600)?.checked_add(minutes * 60 + seconds)?.checked_mul(1000)?.checked_add(millis)?;
    Some(Time::from_ticks(total_millis.checked_mul(TICKS_PER_SECOND / 1000)?))
}

/// The line without `<...>` tags and `{\...}` overrides. A `<` or `{` that opens nothing stays.
pub fn strip_tags(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(['<', '{']) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let (close, is_tag) = if tail.starts_with('<') { ('>', is_markup_tag(tail)) } else { ('}', tail.starts_with("{\\")) };
        match tail.find(close) {
            Some(end) if is_tag => rest = &tail[end + 1..],
            _ => {
                out.push(tail.chars().next().unwrap_or_default());
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn is_markup_tag(tail: &str) -> bool {
    let name = tail[1..].trim_start_matches('/');
    let name: String = name.chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_lowercase();
    matches!(name.as_str(), "i" | "b" | "u" | "s" | "font")
}

/// `hh:mm:ss,mmm` of `time`, rounded to the millisecond; negative times are written as 0.
pub fn format_timestamp(time: Time) -> String {
    let ticks_per_milli = TICKS_PER_SECOND / 1000;
    let millis = (time.ticks().max(0) + ticks_per_milli / 2) / ticks_per_milli;
    format!("{:02}:{:02}:{:02},{:03}", millis / 3_600_000, millis / 60_000 % 60, millis / 1000 % 60, millis % 1000)
}

/// An SRT file of `cues`, numbered from 1 in the order given, with CRLF line ends.
pub fn write_srt<'a>(cues: impl IntoIterator<Item = (Time, Time, &'a str)>) -> String {
    let mut out = String::new();
    for (number, (start, end, text)) in cues.into_iter().enumerate() {
        out.push_str(&format!("{}\r\n{} --> {}\r\n", number + 1, format_timestamp(start), format_timestamp(end)));
        for line in text.lines() {
            out.push_str(line);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: i64) -> Time {
        Time::from_ticks(millis * 90)
    }

    #[test]
    fn reads_a_plain_file() {
        let doc = parse_srt("1\n00:00:01,000 --> 00:00:02,500\nHello\n\n2\n00:00:03,000 --> 00:00:04,000\nTwo\nlines\n");
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert_eq!(doc.cues, vec![SrtCue { start: ms(1000), end: ms(2500), text: "Hello".into(), line: 2 }, SrtCue { start: ms(3000), end: ms(4000), text: "Two\nlines".into(), line: 6 }]);
    }

    #[test]
    fn reads_bom_crlf_dots_and_short_fractions() {
        let doc = parse_srt("\u{feff}1\r\n01:02:03.4 --> 01:02:03.45\r\nA\r\n\r\n");
        assert!(doc.warnings.is_empty());
        let base = (3600 + 120 + 3) * 1000;
        assert_eq!((doc.cues[0].start, doc.cues[0].end), (ms(base + 400), ms(base + 450)));
        assert_eq!(doc.cues[0].text, "A");
    }

    #[test]
    fn reads_minutes_only_and_position_hints() {
        let doc = parse_srt("1\n00:01,000 --> 00:02,000 X1:10 X2:20\nA\n");
        assert_eq!((doc.cues[0].start, doc.cues[0].end), (ms(1000), ms(2000)));
    }

    #[test]
    fn keeps_cues_with_missing_or_wrong_numbers() {
        let doc = parse_srt("00:00:01,000 --> 00:00:02,000\nA\n\n7\n00:00:03,000 --> 00:00:04,000\nB\n\n8\n00:00:05,000 --> 00:00:06,000\nC\n");
        assert_eq!(doc.cues.len(), 3);
        assert_eq!(doc.warnings, vec![SrtWarning { line: 1, kind: SrtWarningKind::BadNumber }, SrtWarning { line: 4, kind: SrtWarningKind::BadNumber }]);
    }

    #[test]
    fn skips_broken_cues_and_says_where() {
        let text = "1\n00:00:01,000 --> 00:00:02,000\nGood\n\n2\nno timing here\n\n3\n00:00:xx,000 --> 00:00:04,000\nBad\n\n4\n00:00:05,000 --> 00:00:05,000\nZero\n\n5\n00:00:06,000 --> 00:00:07,000\n<i></i>\n\n6\n00:00:08,000 --> 00:00:09,000\nLast\n";
        let doc = parse_srt(text);
        assert_eq!(doc.cues.iter().map(|cue| cue.text.as_str()).collect::<Vec<_>>(), vec!["Good", "Last"]);
        let kinds: Vec<(usize, &SrtWarningKind)> = doc.warnings.iter().map(|w| (w.line, &w.kind)).collect();
        assert_eq!(kinds, vec![(5, &SrtWarningKind::MissingTiming), (9, &SrtWarningKind::BadTiming("00:00:xx,000 --> 00:00:04,000".into())), (13, &SrtWarningKind::EmptyDuration), (17, &SrtWarningKind::EmptyText)]);
    }

    #[test]
    fn rejects_out_of_range_fields() {
        for timing in ["00:60:00,000 --> 00:61:00,000", "00:00:61,000 --> 00:00:62,000", "00:00:01,0000 --> 00:00:02,000", "1:2 --> 3", "-00:00:01,000 --> 00:00:02,000"] {
            assert!(parse_timing(timing).is_none(), "{timing}");
        }
    }

    #[test]
    fn drops_tags_and_overrides() {
        let doc = parse_srt("1\n00:00:01,000 --> 00:00:02,000\n{\\an8}<i>Hi</i> <font color=\"#ff0000\">there</font>\n<B>bold</B> a < b {not}\n");
        assert_eq!(doc.cues[0].text, "Hi there\nbold a < b {not}");
    }

    #[test]
    fn writes_and_reads_back() {
        let out = write_srt([(ms(1000), ms(2500), "Hello"), (ms(3_723_004), ms(3_724_000), "Two\nlines")]);
        assert_eq!(out, "1\r\n00:00:01,000 --> 00:00:02,500\r\nHello\r\n\r\n2\r\n01:02:03,004 --> 01:02:04,000\r\nTwo\r\nlines\r\n\r\n");
        let doc = parse_srt(&out);
        assert!(doc.warnings.is_empty());
        assert_eq!(doc.cues[1].text, "Two\nlines");
        assert_eq!(doc.cues[1].start, ms(3_723_004));
    }

    #[test]
    fn rounds_to_the_millisecond() {
        assert_eq!(format_timestamp(Time::from_ticks(44)), "00:00:00,000");
        assert_eq!(format_timestamp(Time::from_ticks(45)), "00:00:00,001");
        assert_eq!(format_timestamp(Time::from_ticks(-5)), "00:00:00,000");
    }

    #[test]
    fn empty_input_has_nothing() {
        assert_eq!(parse_srt(""), SrtDocument::default());
        assert_eq!(parse_srt("\u{feff}\r\n\r\n"), SrtDocument::default());
    }
}
