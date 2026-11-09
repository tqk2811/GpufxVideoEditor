//! What one `gpufx-render` child said and what that adds up to. No process here.

use std::path::{Path, PathBuf};

use d3d12_backend::{ExportReport, ProgressEvent, PROGRESS_PROTOCOL};

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
    /// The child said `started`, so it understood the command line.
    started: bool,
    /// The child speaks another protocol version.
    mismatched: bool,
    /// The last line of stdout was not an event.
    last_line_malformed: bool,
    total: i64,
    frames: i64,
}

/// What a `gpufx-render` older than the progress protocol prints for the flag it does not know.
const OLD_RENDERER_USAGE: &str = "unknown argument --progress";

impl ChildMachine {
    /// Takes one line of stdout. Anything that is not an event is ignored.
    pub fn take_line(&mut self, line: &[u8]) {
        let event = std::str::from_utf8(line).ok().and_then(ProgressEvent::parse);
        self.last_line_malformed = event.is_none();
        if let Some(event) = event {
            self.take_event(event);
        }
    }

    /// Every frame was written and the last line was unreadable: it may have been the end, and a
    /// file the child kept may be the only copy of the render.
    fn result_unreadable(&self) -> bool {
        self.last_line_malformed && self.total > 0 && self.frames >= self.total
    }

    fn take_event(&mut self, event: ProgressEvent) {
        if self.end.is_some() || self.mismatched {
            return;
        }
        match event {
            ProgressEvent::Started { protocol, total, .. } => {
                self.started = true;
                self.total = total;
                self.mismatched = protocol != PROGRESS_PROTOCOL;
            }
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

    /// Whether the child speaks another protocol version, so nothing it says can be trusted.
    pub fn mismatched(&self) -> bool {
        self.mismatched
    }

    pub fn frames(&self) -> i64 {
        self.frames
    }

    /// The outcome once the child is gone, and the files that are now the caller's to remove.
    /// After an end the child has dealt with its own files, and may have kept a finished one.
    pub fn finish(self, cancel_requested: bool, exit_code: Option<i32>, stderr_tail: &str) -> (ExportOutcome, Vec<PathBuf>) {
        if self.mismatched || (self.end.is_none() && !self.started && stderr_tail.contains(OLD_RENDERER_USAGE)) {
            return (ExportOutcome::Failed(Message::ExportRendererMismatch), self.reserved);
        }
        if self.end.is_none() && self.result_unreadable() {
            let files: Vec<String> = self.reserved.iter().map(|path| path.display().to_string()).collect();
            return (ExportOutcome::Failed(Message::ExportResultUnknown(files.join(", "))), Vec::new());
        }
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
    fn malformed_lines_do_not_stop_the_run() {
        let mut machine = ChildMachine::default();
        for line in [&b"gpu: x"[..], b"{", b"\xff\xfe", br#"{"event":"unknown"}"#] {
            machine.take_line(line);
        }
        feed(&mut machine, &[ProgressEvent::Finished { report: report() }]);
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
    fn another_protocol_version_is_refused_whatever_it_says_next() {
        let mut machine = ChildMachine::default();
        feed(&mut machine, &[ProgressEvent::Started { protocol: PROGRESS_PROTOCOL + 1, adapter: "a".into(), total: 3, out: "o".into() }, ProgressEvent::Finished { report: report() }]);
        assert!(machine.mismatched());
        assert_eq!(machine.finish(false, Some(0), "").0, ExportOutcome::Failed(Message::ExportRendererMismatch));
    }

    #[test]
    fn the_same_protocol_version_is_accepted() {
        let mut machine = ChildMachine::default();
        feed(&mut machine, &[ProgressEvent::Started { protocol: PROGRESS_PROTOCOL, adapter: "a".into(), total: 3, out: "o".into() }]);
        assert!(!machine.mismatched());
    }

    #[test]
    fn a_renderer_without_the_flag_is_asked_to_match() {
        let tail = "unknown argument --progress\nusage: gpufx-render ...\n";
        assert_eq!(ChildMachine::default().finish(false, Some(1), tail).0, ExportOutcome::Failed(Message::ExportRendererMismatch));
    }

    fn after_every_frame(machine: &mut ChildMachine) {
        feed(machine, &[ProgressEvent::Started { protocol: PROGRESS_PROTOCOL, adapter: "a".into(), total: 3, out: "o".into() }, reserved("a.tmp.mp4"), ProgressEvent::Progress { frame: 3, total: 3, elapsed_ms: 1 }]);
    }

    #[test]
    fn an_unreadable_last_line_after_every_frame_keeps_the_file() {
        for cancel_requested in [false, true] {
            let mut machine = ChildMachine::default();
            after_every_frame(&mut machine);
            machine.take_line(b"{\"event\":\"err");
            let (outcome, remove) = machine.finish(cancel_requested, Some(1), "");
            assert_eq!(outcome, ExportOutcome::Failed(Message::ExportResultUnknown("a.tmp.mp4".into())));
            assert!(remove.is_empty(), "it may be the only copy of the render");
        }
    }

    #[test]
    fn noise_before_the_last_frame_still_means_a_partial_file() {
        let mut machine = ChildMachine::default();
        feed(&mut machine, &[ProgressEvent::Started { protocol: PROGRESS_PROTOCOL, adapter: "a".into(), total: 3, out: "o".into() }, reserved("a.tmp.mp4"), ProgressEvent::Progress { frame: 2, total: 3, elapsed_ms: 1 }]);
        machine.take_line(b"library noise");
        let (outcome, remove) = machine.finish(false, Some(1), "");
        assert!(matches!(outcome, ExportOutcome::Failed(Message::ExportChildCrashed { .. })));
        assert_eq!(remove, [PathBuf::from("a.tmp.mp4")]);
    }

    #[test]
    fn a_readable_line_after_the_noise_clears_the_doubt() {
        let mut machine = ChildMachine::default();
        after_every_frame(&mut machine);
        machine.take_line(b"noise");
        feed(&mut machine, &[ProgressEvent::Encoder { name: "x".into() }]);
        assert_eq!(machine.finish(false, Some(1), "").1, [PathBuf::from("a.tmp.mp4")]);
    }

    #[test]
    fn only_a_tmp_name_beside_an_output_may_be_removed() {
        let out = Path::new(r"D:\v\out.mp4");
        assert!(is_own_partial(Path::new(r"D:\v\out.12-0.tmp.mp4"), &[out]));
        assert!(!is_own_partial(Path::new(r"D:\v\holiday.mp4"), &[out]));
        assert!(!is_own_partial(Path::new(r"C:\Windows\a.tmp.dll"), &[out]));
    }
}
