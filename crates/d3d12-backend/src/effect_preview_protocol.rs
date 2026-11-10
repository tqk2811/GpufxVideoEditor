//! The lines of `gpufx-render --effect-previews`, shared by the helper and the editor that spawns it: one JSON
//! object per line, in both directions. The helper documents how it uses them.

use serde::{Deserialize, Serialize};

use crate::progress::bounded_line;

/// Changes when an existing line loses or changes a field. New fields and events do not change it.
pub const PREVIEW_PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewJobKind {
    /// A `PREVIEW_SIZE` square PNG.
    Still,
    /// One PNG with the frames of the loop stacked top to bottom.
    Loop,
}

/// What the parent writes to the helper's stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewJob {
    pub id: u64,
    pub kind: PreviewJobKind,
    /// The package folder, as `Path::to_str` gave it: a path that is not UTF-8 cannot be sent.
    pub package: String,
    /// The file to write, as `Path::to_str` gave it.
    pub out: String,
}

/// A line that is not a job. `id` is there when the line had a readable one, so the parent can tell which
/// request it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidJob {
    pub id: Option<u64>,
    pub message: String,
}

impl PreviewJob {
    /// The job as one line, without the line end.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("a job has only strings and numbers")
    }

    pub fn parse(line: &str) -> Result<Self, InvalidJob> {
        let value: serde_json::Value = serde_json::from_str(line.trim()).map_err(|e| InvalidJob { id: None, message: format!("not JSON: {e}") })?;
        let id = value.get("id").and_then(serde_json::Value::as_u64);
        serde_json::from_value(value).map_err(|e| InvalidJob { id, message: format!("not a job: {e}") })
    }
}

/// Who a failed job is to be blamed on, which tells the parent whether to try again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewBlame {
    /// The GPU or its driver; another package may well work.
    Device,
    /// The package does not render.
    Package,
    /// The preview rendered, but encoding it or writing `out` failed.
    Output,
}

/// What the helper writes to its stdout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PreviewEvent {
    /// The renderer exists. The first line.
    Ready { protocol: u32, device: String },
    /// A job went well. `static` is true for a loop that does not change over time.
    Done {
        id: u64,
        #[serde(rename = "static")]
        is_static: bool,
    },
    Failed { id: u64, blame: PreviewBlame, message: String },
    /// A line that was not a job; `id` is there when it was readable.
    Invalid {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        message: String,
    },
    /// No renderer; the helper exits with a non-zero code.
    Fatal { message: String },
}

impl PreviewEvent {
    /// The event as one line, without the line end. The `message` of `failed`, `invalid` and `fatal` is cut to
    /// keep the line within `MAX_EVENT_LINE`, so the reader always gets it.
    pub fn to_line(&self) -> String {
        match self {
            Self::Failed { id, blame, message } => bounded_line(self, Some(message), |message| Self::Failed { id: *id, blame: *blame, message }),
            Self::Invalid { id, message } => bounded_line(self, Some(message), |message| Self::Invalid { id: *id, message }),
            Self::Fatal { message } => bounded_line(self, Some(message), |message| Self::Fatal { message }),
            _ => bounded_line(self, None, |_| unreachable!("there is no message to cut")),
        }
    }

    /// The event on `line`, or `None` for anything else.
    pub fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line.trim()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::MAX_EVENT_LINE;

    #[test]
    fn events_round_trip_and_name_themselves() {
        for event in [
            PreviewEvent::Ready { protocol: PREVIEW_PROTOCOL, device: "GPU \"1\"".into() },
            PreviewEvent::Done { id: 1, is_static: true },
            PreviewEvent::Failed { id: 2, blame: PreviewBlame::Output, message: "line one\nline two".into() },
            PreviewEvent::Invalid { id: Some(3), message: "x".into() },
            PreviewEvent::Invalid { id: None, message: "x".into() },
            PreviewEvent::Fatal { message: "no GPU".into() },
        ] {
            let line = event.to_line();
            assert!(!line.contains('\n'), "one event is one line: {line}");
            assert_eq!(PreviewEvent::parse(&line).as_ref(), Some(&event));
        }
        assert_eq!(PreviewEvent::Done { id: 1, is_static: false }.to_line(), r#"{"event":"done","id":1,"static":false}"#);
        assert_eq!(PreviewEvent::Invalid { id: None, message: "m".into() }.to_line(), r#"{"event":"invalid","message":"m"}"#);
        assert_eq!(PreviewEvent::Failed { id: 1, blame: PreviewBlame::Package, message: "m".into() }.to_line(), r#"{"event":"failed","id":1,"blame":"package","message":"m"}"#);
    }

    #[test]
    fn a_job_round_trips_and_an_invalid_one_keeps_its_id() {
        let job = PreviewJob { id: 7, kind: PreviewJobKind::Loop, package: "p".into(), out: "o.tmp".into() };
        assert_eq!(job.to_line(), r#"{"id":7,"kind":"loop","package":"p","out":"o.tmp"}"#);
        assert_eq!(PreviewJob::parse(&job.to_line()), Ok(job));
        let unknown = PreviewJob::parse(r#"{"id":4,"kind":"zoom","package":"p","out":"o"}"#).expect_err("unknown kind");
        assert_eq!(unknown.id, Some(4));
        assert!(unknown.message.contains("zoom"));
        assert_eq!(PreviewJob::parse("{").expect_err("not JSON").id, None);
        assert_eq!(PreviewJob::parse(r#"{"id":-1}"#).expect_err("negative").id, None);
    }

    #[test]
    fn a_message_too_long_for_a_line_stays_a_valid_event() {
        for piece in ["a", "ế", "\"", "\n", "\u{1}"] {
            let message = piece.repeat(MAX_EVENT_LINE);
            for event in [
                PreviewEvent::Failed { id: 1, blame: PreviewBlame::Device, message: message.clone() },
                PreviewEvent::Invalid { id: Some(2), message: message.clone() },
                PreviewEvent::Fatal { message: message.clone() },
            ] {
                let line = event.to_line();
                assert!(line.len() <= MAX_EVENT_LINE, "{piece:?}: {} bytes", line.len());
                let cut = PreviewEvent::parse(&line).unwrap_or_else(|| panic!("{piece:?}: not an event"));
                let (PreviewEvent::Failed { message, .. } | PreviewEvent::Invalid { message, .. } | PreviewEvent::Fatal { message }) = cut else { panic!("{piece:?}: another event") };
                assert!(message.ends_with("...") && message.starts_with(piece), "{piece:?}");
            }
        }
    }
}
