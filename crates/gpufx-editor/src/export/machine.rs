//! What one `gpufx-render` child said and what that adds up to. No process here.

use std::path::{Path, PathBuf};

use d3d12_backend::{ExportReport, ProgressEvent};

use crate::i18n::Message;

/// How an export ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportOutcome {
    Finished(ExportReport),
    Cancelled,
    Failed(Message),
}

/// The events of one child, in the order it wrote them.
#[derive(Debug, Default)]
pub struct ChildMachine {
    reserved: Vec<PathBuf>,
    /// Only the first end counts.
    end: Option<ProgressEvent>,
    malformed: u64,
    frames: i64,
}

impl ChildMachine {
    /// Takes one line of stdout. Anything that is not an event is counted and otherwise ignored.
    pub fn take_line(&mut self, line: &[u8]) {
        match std::str::from_utf8(line).ok().and_then(ProgressEvent::parse) {
            Some(event) => self.take_event(event),
            None => self.malformed += 1,
        }
    }

    fn take_event(&mut self, event: ProgressEvent) {
        if self.end.is_some() {
            return;
        }
        match event {
            ProgressEvent::OutputReserved { path } => self.reserved.push(PathBuf::from(path)),
            ProgressEvent::Progress { frame, .. } => self.frames = frame,
            event if event.is_terminal() => self.end = Some(event),
            _ => {}
        }
    }

    /// Whether the child has reported how the export ended.
    pub fn ended(&self) -> bool {
        self.end.is_some()
    }

    pub fn frames(&self) -> i64 {
        self.frames
    }

    pub fn malformed(&self) -> u64 {
        self.malformed
    }

    /// The outcome once the child is gone, and the files that are now the caller's to remove.
    /// After an end the child has dealt with its own files, and may have kept a finished one.
    pub fn finish(self, cancel_requested: bool, exit_code: Option<i32>, stderr_tail: &str) -> (ExportOutcome, Vec<PathBuf>) {
        match self.end {
            Some(ProgressEvent::Finished { report }) => (ExportOutcome::Finished(report), Vec::new()),
            Some(ProgressEvent::Error { message }) => (ExportOutcome::Failed(Message::ExportFailed(message)), Vec::new()),
            Some(_) => (ExportOutcome::Cancelled, Vec::new()),
            None if cancel_requested => (ExportOutcome::Cancelled, self.reserved),
            None => (ExportOutcome::Failed(Message::ExportChildCrashed { code: exit_text(exit_code), stderr: stderr_tail.trim().to_owned() }), self.reserved),
        }
    }
}

/// A Windows exit code, in hexadecimal when it is an NTSTATUS such as 0xC0000005.
fn exit_text(code: Option<i32>) -> String {
    match code {
        Some(code) if code < 0 => format!("0x{code:08X}"),
        Some(code) => code.to_string(),
        None => "?".to_owned(),
    }
}

/// Whether `path` could be a temporary file of this export: a `.tmp` name beside an output.
pub fn is_own_partial(path: &Path, outputs: &[&Path]) -> bool {
    let named_tmp = path.file_name().is_some_and(|name| name.to_string_lossy().contains(".tmp"));
    named_tmp && outputs.iter().any(|output| output.parent().is_some() && output.parent() == path.parent())
}

#[cfg(test)]
mod tests {
    use super::*;
    use d3d12_backend::PROGRESS_PROTOCOL;

    fn report() -> ExportReport {
        ExportReport { encoder: "x".into(), frames: 3, audio_samples: 0, clamped_samples: 0, pcm_sha256: None }
    }

    fn feed(machine: &mut ChildMachine, events: &[ProgressEvent]) {
        for event in events {
            machine.take_line(event.to_line().as_bytes());
        }
    }

    fn reserved(path: &str) -> ProgressEvent {
        ProgressEvent::OutputReserved { path: path.to_owned() }
    }

    #[test]
    fn a_normal_run_finishes_and_leaves_nothing_to_remove() {
        let mut machine = ChildMachine::default();
        feed(&mut machine, &[ProgressEvent::Started { protocol: PROGRESS_PROTOCOL, adapter: "a".into(), total: 3, out: "o".into() }, reserved("o.tmp.mp4"), ProgressEvent::Progress { frame: 3, total: 3, elapsed_ms: 1 }, ProgressEvent::Finished { report: report() }]);
        assert_eq!(machine.frames(), 3);
        assert_eq!(machine.finish(false, Some(0), ""), (ExportOutcome::Finished(report()), Vec::new()));
    }

    #[test]
    fn malformed_lines_are_counted_and_do_not_stop_the_run() {
        let mut machine = ChildMachine::default();
        for line in [&b"gpu: x"[..], b"{", b"\xff\xfe", br#"{"event":"unknown"}"#] {
            machine.take_line(line);
        }
        feed(&mut machine, &[ProgressEvent::Finished { report: report() }]);
        assert_eq!(machine.malformed(), 4);
        assert!(matches!(machine.finish(false, Some(0), "").0, ExportOutcome::Finished(_)));
    }

    #[test]
    fn a_child_error_is_passed_on_and_its_files_are_left_alone() {
        let mut machine = ChildMachine::default();
        feed(&mut machine, &[reserved("kept.tmp.mp4"), ProgressEvent::Error { message: "the finished file was kept".into() }]);
        let (outcome, remove) = machine.finish(false, Some(1), "ignored");
        assert_eq!(outcome, ExportOutcome::Failed(Message::ExportFailed("the finished file was kept".into())));
        assert!(remove.is_empty(), "the file may be the only copy of the render");
    }

    #[test]
    fn a_crash_is_reported_with_the_stderr_tail_and_its_files_are_removed() {
        let mut machine = ChildMachine::default();
        feed(&mut machine, &[reserved("a.tmp.mp4")]);
        let (outcome, remove) = machine.finish(false, Some(-1073741819), "  boom\n");
        assert_eq!(outcome, ExportOutcome::Failed(Message::ExportChildCrashed { code: "0xC0000005".into(), stderr: "boom".into() }));
        assert_eq!(remove, [PathBuf::from("a.tmp.mp4")]);
    }

    #[test]
    fn exiting_cleanly_without_an_end_is_still_a_failure() {
        let (outcome, _) = ChildMachine::default().finish(false, Some(0), "");
        assert_eq!(outcome, ExportOutcome::Failed(Message::ExportChildCrashed { code: "0".into(), stderr: String::new() }));
    }

    #[test]
    fn a_killed_child_after_cancel_is_cancelled_and_cleaned_up() {
        let mut machine = ChildMachine::default();
        feed(&mut machine, &[reserved("a.tmp.mp4")]);
        assert_eq!(machine.finish(true, Some(1), ""), (ExportOutcome::Cancelled, vec![PathBuf::from("a.tmp.mp4")]));
    }

    #[test]
    fn an_end_that_beat_the_cancel_stands() {
        let mut machine = ChildMachine::default();
        feed(&mut machine, &[ProgressEvent::Finished { report: report() }]);
        assert!(matches!(machine.finish(true, Some(0), "").0, ExportOutcome::Finished(_)));
    }

    #[test]
    fn nothing_after_the_first_end_counts() {
        let mut machine = ChildMachine::default();
        feed(&mut machine, &[ProgressEvent::Cancelled, ProgressEvent::Error { message: "late".into() }, reserved("late.tmp.mp4")]);
        assert_eq!(machine.finish(false, Some(1), ""), (ExportOutcome::Cancelled, Vec::new()));
    }

    #[test]
    fn only_a_tmp_name_beside_an_output_may_be_removed() {
        let out = Path::new(r"D:\v\out.mp4");
        assert!(is_own_partial(Path::new(r"D:\v\out.12-0.tmp.mp4"), &[out]));
        assert!(!is_own_partial(Path::new(r"D:\v\holiday.mp4"), &[out]));
        assert!(!is_own_partial(Path::new(r"C:\Windows\a.tmp.dll"), &[out]));
    }
}
