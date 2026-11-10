//! The machine-readable progress of `gpufx-render --progress json`: one JSON object per stdout line.
//! The protocol is documented in `docs/plans/spec-mcp-server-vi.md` (section "Giao thức tiến độ").

use serde::{Deserialize, Serialize};

use crate::export::ExportReport;

/// Changes when an existing event loses or changes a field. New fields and events do not change it.
pub const PROGRESS_PROTOCOL: u32 = 1;

/// The stdin line that asks a `--progress json` run to stop cleanly.
pub const CANCEL_LINE: &str = "cancel";

/// The longest line of the protocol, without its line end. A reader treats a longer one as noise.
pub const MAX_EVENT_LINE: usize = 64 * 1024;

/// What replaces the end of an `error` message that would not fit on a line.
const TRUNCATED: &str = "...";

/// `event` as one line, within [`MAX_EVENT_LINE`]. A line that is too long has `message` (the one free-text
/// field of the event) cut, with [`TRUNCATED`] at its end, and `shorten` builds the event again from it.
/// Without a `message` the line is returned as it is.
pub(crate) fn bounded_line<T: Serialize>(event: &T, message: Option<&str>, shorten: impl FnOnce(String) -> T) -> String {
    let line = serde_json::to_string(event).expect("an event has only strings and numbers");
    let Some(message) = message.filter(|_| line.len() > MAX_EVENT_LINE) else {
        return line;
    };
    let quoted_length = |text: &str| serde_json::to_string(text).expect("a string").len() - 2;
    let mut room = MAX_EVENT_LINE.saturating_sub(line.len() - quoted_length(message) + TRUNCATED.len());
    let mut keep = 0;
    for (at, character) in message.char_indices() {
        // A control character or a quote takes several bytes of JSON.
        let cost = quoted_length(character.encode_utf8(&mut [0; 4]));
        if cost > room {
            break;
        }
        room -= cost;
        keep = at + character.len_utf8();
    }
    serde_json::to_string(&shorten(format!("{}{TRUNCATED}", &message[..keep]))).expect("an event has only strings and numbers")
}

/// One line of the protocol. Exactly one of `Finished`, `Cancelled` and `Error` ends a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ProgressEvent {
    Started { protocol: u32, adapter: String, total: i64, out: String },
    /// A file was created under this temporary name. It is left behind only if the process dies
    /// before reporting an end, so whoever kills the process removes it.
    OutputReserved { path: String },
    Encoder { name: String },
    /// `frame` frames are done, of `total`.
    Progress { frame: i64, total: i64, elapsed_ms: u64 },
    Finished { report: ExportReport },
    Cancelled,
    Error { message: String },
}

impl ProgressEvent {
    /// The event as one line, without the line end. An `error` message is cut to keep the line
    /// within [`MAX_EVENT_LINE`], so the reader always gets it.
    pub fn to_line(&self) -> String {
        let message = match self {
            Self::Error { message } => Some(message.as_str()),
            _ => None,
        };
        bounded_line(self, message, |message| Self::Error { message })
    }

    /// The event on `line`, or `None` for anything else: other programs may write to stdout too.
    pub fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line.trim()).ok()
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. } | Self::Cancelled | Self::Error { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_round_trip_and_name_themselves() {
        let report = ExportReport { encoder: "h264_nvenc".into(), frames: 3, audio_samples: 0, clamped_samples: 0, pcm_sha256: None };
        for event in [
            ProgressEvent::Started { protocol: PROGRESS_PROTOCOL, adapter: "GPU \"1\"".into(), total: 9, out: "D:\\xuất\\a.mp4".into() },
            ProgressEvent::OutputReserved { path: "a.1-0.tmp.mp4".into() },
            ProgressEvent::Encoder { name: "libx264".into() },
            ProgressEvent::Progress { frame: 2, total: 9, elapsed_ms: 40 },
            ProgressEvent::Finished { report },
            ProgressEvent::Cancelled,
            ProgressEvent::Error { message: "line one\nline two".into() },
        ] {
            let line = event.to_line();
            assert!(!line.contains('\n'), "one event is one line: {line}");
            assert_eq!(ProgressEvent::parse(&line).as_ref(), Some(&event));
        }
        assert_eq!(ProgressEvent::Cancelled.to_line(), r#"{"event":"cancelled"}"#);
        assert_eq!(ProgressEvent::Progress { frame: 1, total: 2, elapsed_ms: 3 }.to_line(), r#"{"event":"progress","frame":1,"total":2,"elapsed_ms":3}"#);
    }

    #[test]
    fn an_error_too_long_for_a_line_stays_a_valid_event() {
        for piece in ["a", "ế", "\"", "\n", "\u{1}"] {
            let event = ProgressEvent::Error { message: piece.repeat(MAX_EVENT_LINE) };
            let line = event.to_line();
            assert!(line.len() <= MAX_EVENT_LINE, "{piece:?}: {} bytes", line.len());
            let Some(ProgressEvent::Error { message }) = ProgressEvent::parse(&line) else { panic!("{piece:?}: not an event") };
            assert!(message.ends_with("...") && message.starts_with(piece), "{piece:?}");
        }
    }

    #[test]
    fn a_message_that_fits_is_not_cut() {
        let event = ProgressEvent::Error { message: "x".repeat(1000) };
        assert_eq!(ProgressEvent::parse(&event.to_line()).as_ref(), Some(&event));
    }

    #[test]
    fn other_output_is_not_an_event() {
        for line in ["", "gpu: x", "{", "[1]", r#"{"event":"nonsense"}"#, r#"{"event":"progress","frame":"a"}"#, r#"{"frame":1}"#] {
            assert_eq!(ProgressEvent::parse(line), None, "{line}");
        }
    }

    #[test]
    fn only_the_three_ends_are_terminal() {
        assert!(ProgressEvent::Cancelled.is_terminal());
        assert!(ProgressEvent::Error { message: String::new() }.is_terminal());
        assert!(!ProgressEvent::Encoder { name: String::new() }.is_terminal());
    }
}
