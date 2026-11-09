//! Dialogs and file loads that run off the UI thread, as logic with no window: which one may start,
//! what follows when one ends, and when a finished load may touch the project. The window runs the
//! returned [`Action`]s and feeds results back as [`Arrival`]s.

use std::path::{Path, PathBuf};

use d3d12_backend::MediaProbe;
use engine::Project;

use crate::editor::{clip_label, EditorState};

const DIALOG_OPEN: &str = "Close the open dialog first";
const LOAD_RUNNING: &str = "Wait for the current load to finish";
const LOAD_DROPPED: &str = "The load in progress was dropped";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDialog {
    PickClip { track: usize },
    OpenProject,
    SaveProject,
    Export,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discard {
    NewProject,
    OpenProject,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// The project has unsaved changes; go on and lose them?
    DiscardFor(Discard),
    CancelExportForClose,
}

impl Prompt {
    pub fn question(self) -> String {
        match self {
            Self::DiscardFor(Discard::NewProject) => "The project has unsaved changes. Start a new project anyway and lose them?".to_owned(),
            Self::DiscardFor(Discard::OpenProject) => "The project has unsaved changes. Open another project anyway and lose them?".to_owned(),
            Self::DiscardFor(Discard::Close) => "The project has unsaved changes. Close anyway and lose them?".to_owned(),
            Self::CancelExportForClose => "An export is running. Cancel it and close?".to_owned(),
        }
    }
}

/// What the user asked for that may need a dialog or a load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    NewProject,
    OpenProject,
    SaveProjectAs,
    PickClip { track: usize },
    Export,
}

/// A result delivered back to the UI thread.
#[derive(Debug)]
pub enum Arrival {
    FilePicked { dialog: FileDialog, path: Option<PathBuf> },
    PromptAnswered { prompt: Prompt, accepted: bool },
    ClipProbed { track: usize, path: PathBuf, result: Result<MediaProbe, String> },
    ProjectRead { file: PathBuf, result: Result<Project, String> },
}

/// What the window must do next. The project itself was already updated here.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    ShowFileDialog(FileDialog),
    ShowPrompt(Prompt),
    ProbeClip { track: usize, path: PathBuf },
    ReadProject { file: PathBuf },
    SaveTo(PathBuf),
    StartExport(PathBuf),
    /// The project was replaced, so playback of the old one must go.
    ProjectReplaced,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    Dialog,
    Prompt,
    Loading,
}

/// A load that finished while the project could not be touched.
#[derive(Debug)]
enum Finished {
    Clip { track: usize, path: PathBuf, result: Result<MediaProbe, String> },
    Project { file: PathBuf, result: Result<Project, String> },
}

/// One dialog, prompt or load at a time.
#[derive(Debug, Default)]
pub struct Tasks {
    busy: Option<Busy>,
    finished: Option<Finished>,
}

fn name_of(path: &Path) -> String {
    clip_label(&path.to_string_lossy()).to_owned()
}

impl Tasks {
    pub fn is_idle(&self) -> bool {
        self.busy.is_none()
    }

    /// Whether a finished load waits for a drag or an export to end.
    pub fn has_waiting_load(&self) -> bool {
        self.finished.is_some()
    }

    fn start_prompt(&mut self, prompt: Prompt) -> Action {
        self.busy = Some(Busy::Prompt);
        Action::ShowPrompt(prompt)
    }

    fn start_dialog(&mut self, dialog: FileDialog) -> Action {
        self.busy = Some(Busy::Dialog);
        Action::ShowFileDialog(dialog)
    }

    /// Starts `request`, or says on the status line why it has to wait.
    pub fn begin(&mut self, state: &mut EditorState, request: Request) -> Action {
        if let Some(busy) = self.busy {
            state.report_info(if busy == Busy::Loading { LOAD_RUNNING } else { DIALOG_OPEN }.to_owned());
            return Action::Nothing;
        }
        match request {
            Request::NewProject if state.is_dirty() => self.start_prompt(Prompt::DiscardFor(Discard::NewProject)),
            Request::NewProject => {
                state.new_project();
                Action::ProjectReplaced
            }
            Request::OpenProject if state.is_dirty() => self.start_prompt(Prompt::DiscardFor(Discard::OpenProject)),
            Request::OpenProject => self.start_dialog(FileDialog::OpenProject),
            Request::SaveProjectAs => self.start_dialog(FileDialog::SaveProject),
            Request::PickClip { track } => self.start_dialog(FileDialog::PickClip { track }),
            Request::Export => self.start_dialog(FileDialog::Export),
        }
    }

    /// The window was asked to close. A dialog that is up has to be answered first, so nothing is
    /// lost by a close the user cannot see the outcome of; a load in the background is dropped,
    /// since it may hang on a slow file and must never keep the app open.
    pub fn close_requested(&mut self, state: &mut EditorState) -> Action {
        match self.busy {
            Some(Busy::Dialog | Busy::Prompt) => {
                state.report_info(DIALOG_OPEN.to_owned());
                return Action::Nothing;
            }
            Some(Busy::Loading) => {
                self.finished = None;
                self.busy = None;
                state.set_loading(None);
                state.report_info(LOAD_DROPPED.to_owned());
            }
            None => {}
        }
        if state.is_exporting() {
            self.start_prompt(Prompt::CancelExportForClose)
        } else if state.is_dirty() {
            self.start_prompt(Prompt::DiscardFor(Discard::Close))
        } else {
            Action::Exit
        }
    }

    /// A dialog or load could not be started at all.
    pub fn start_failed(&mut self, state: &mut EditorState, message: String) {
        self.busy = None;
        self.finished = None;
        state.set_loading(None);
        state.report_error(message);
    }

    pub fn on_arrival(&mut self, state: &mut EditorState, arrival: Arrival) -> Action {
        match arrival {
            Arrival::FilePicked { dialog, path } => {
                self.busy = None;
                let Some(path) = path else { return Action::Nothing };
                match dialog {
                    FileDialog::PickClip { track } => {
                        self.busy = Some(Busy::Loading);
                        state.set_loading(Some(name_of(&path)));
                        Action::ProbeClip { track, path }
                    }
                    FileDialog::OpenProject => {
                        self.busy = Some(Busy::Loading);
                        state.set_loading(Some(name_of(&path)));
                        Action::ReadProject { file: path }
                    }
                    FileDialog::SaveProject => Action::SaveTo(path),
                    FileDialog::Export => Action::StartExport(path),
                }
            }
            Arrival::PromptAnswered { prompt, accepted } => {
                self.busy = None;
                if !accepted {
                    return Action::Nothing;
                }
                match prompt {
                    Prompt::DiscardFor(Discard::NewProject) => {
                        state.new_project();
                        Action::ProjectReplaced
                    }
                    Prompt::DiscardFor(Discard::OpenProject) => self.start_dialog(FileDialog::OpenProject),
                    Prompt::DiscardFor(Discard::Close) => Action::Exit,
                    Prompt::CancelExportForClose if state.is_dirty() => self.start_prompt(Prompt::DiscardFor(Discard::Close)),
                    Prompt::CancelExportForClose => Action::Exit,
                }
            }
            Arrival::ClipProbed { track, path, result } => self.load_finished(state, Finished::Clip { track, path, result }),
            Arrival::ProjectRead { file, result } => self.load_finished(state, Finished::Project { file, result }),
        }
    }

    fn load_finished(&mut self, state: &mut EditorState, finished: Finished) -> Action {
        // A close dropped this load while it ran.
        if self.busy != Some(Busy::Loading) {
            return Action::Nothing;
        }
        self.finished = Some(finished);
        self.poll(state)
    }

    /// Applies a finished load once neither an export nor a drag is in the way.
    pub fn poll(&mut self, state: &mut EditorState) -> Action {
        if state.is_exporting() || state.drag.is_some() {
            return Action::Nothing;
        }
        let Some(finished) = self.finished.take() else { return Action::Nothing };
        self.busy = None;
        state.set_loading(None);
        match finished {
            Finished::Clip { track, path, result } => {
                match result {
                    Ok(probe) => state.add_probed_file(track, &path, &probe),
                    Err(message) => state.report_refused(message),
                }
                Action::Nothing
            }
            Finished::Project { file, result } => match result {
                Ok(project) => {
                    state.replace_project(project, Some(file));
                    Action::ProjectReplaced
                }
                Err(message) => {
                    state.report_error(message);
                    Action::Nothing
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Status;
    use crate::timeline::drag::{ClipDrag, DragKind};
    use engine::{ClipKey, Time, TrackKind};

    const PICK: FileDialog = FileDialog::PickClip { track: 0 };

    fn seconds(value: i64) -> Time {
        Time::from_seconds(value).unwrap()
    }

    fn probe(video: Option<i64>, audio: Option<i64>) -> MediaProbe {
        MediaProbe { has_video: video.is_some(), has_audio: audio.is_some(), video_duration: video.map(seconds), audio_duration: audio.map(seconds), video_problem: None, audio_problem: None }
    }

    fn with_tracks() -> EditorState {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        state.add_track(TrackKind::Audio);
        state.mark_saved(PathBuf::from("p.json"));
        state
    }

    fn picked(dialog: FileDialog, path: &str) -> Arrival {
        Arrival::FilePicked { dialog, path: Some(PathBuf::from(path)) }
    }

    fn start_load(tasks: &mut Tasks, state: &mut EditorState, path: &str) {
        assert_eq!(tasks.begin(state, Request::PickClip { track: 0 }), Action::ShowFileDialog(PICK));
        assert_eq!(tasks.on_arrival(state, picked(PICK, path)), Action::ProbeClip { track: 0, path: PathBuf::from(path) });
    }

    fn probed(path: &str, result: Result<MediaProbe, String>) -> Arrival {
        Arrival::ClipProbed { track: 0, path: PathBuf::from(path), result }
    }

    #[test]
    fn a_second_dialog_cannot_open_while_one_is_up() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        assert_eq!(tasks.begin(&mut state, Request::PickClip { track: 0 }), Action::ShowFileDialog(PICK));
        for request in [Request::OpenProject, Request::NewProject, Request::SaveProjectAs, Request::Export, Request::PickClip { track: 1 }] {
            assert_eq!(tasks.begin(&mut state, request), Action::Nothing, "{request:?}");
        }
        assert_eq!(state.status(), &Status::Info(DIALOG_OPEN.to_owned()));
    }

    #[test]
    fn a_cancelled_dialog_frees_the_slot() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        tasks.begin(&mut state, Request::PickClip { track: 0 });
        assert_eq!(tasks.on_arrival(&mut state, Arrival::FilePicked { dialog: PICK, path: None }), Action::Nothing);
        assert!(tasks.is_idle());
        assert_eq!(state.loading(), None);
    }

    #[test]
    fn a_picked_clip_loads_and_lands_as_one_undo_step() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        start_load(&mut tasks, &mut state, "C:/media/a.mp4");
        assert_eq!(state.loading(), Some("a.mp4"));
        assert_eq!(tasks.begin(&mut state, Request::OpenProject), Action::Nothing, "one load at a time");
        assert_eq!(state.status(), &Status::Info(LOAD_RUNNING.to_owned()));

        assert_eq!(tasks.on_arrival(&mut state, probed("C:/media/a.mp4", Ok(probe(Some(7), Some(9))))), Action::Nothing);
        assert_eq!(state.loading(), None);
        assert!(tasks.is_idle());
        assert_eq!(state.project().tracks[0].clips.len(), 1);
        assert_eq!(state.project().tracks[0].clips[0].duration, seconds(7), "the video length, on a video track");
        state.undo();
        assert!(state.project().tracks[0].clips.is_empty(), "a single undo step removes it");
    }

    #[test]
    fn an_unreadable_file_is_refused_with_the_reason() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        start_load(&mut tasks, &mut state, "bad.mp4");
        tasks.on_arrival(&mut state, probed("bad.mp4", Err("bad.mp4: it has no video or audio stream".to_owned())));
        assert!(matches!(state.status(), Status::Refused(message) if message.contains("no video or audio")), "{:?}", state.status());
        assert!(state.project().tracks[0].clips.is_empty());
        assert!(tasks.is_idle());
    }

    #[test]
    fn a_track_that_went_away_refuses_cleanly() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        start_load(&mut tasks, &mut state, "a.mp4");
        state.undo();
        state.undo();
        tasks.on_arrival(&mut state, probed("a.mp4", Ok(probe(Some(5), None))));
        assert!(matches!(state.status(), Status::Refused(_)), "{:?}", state.status());
        assert!(state.project().tracks.is_empty());
        assert!(tasks.is_idle());
    }

    #[test]
    fn a_result_during_a_drag_waits_for_the_drop() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_clip_from_file(0, Path::new("old.mp4"));
        let drag = Some(ClipDrag { key: ClipKey { track: 0, clip: 0 }, kind: DragKind::Move, grab_offset: Time::ZERO });
        start_load(&mut tasks, &mut state, "new.mp4");
        state.drag = drag;
        tasks.on_arrival(&mut state, probed("new.mp4", Ok(probe(Some(5), None))));
        assert_eq!(state.project().tracks[0].clips.len(), 1, "queued, the drag holds a clip key");
        assert_eq!(state.drag, drag, "the drag goes on");
        assert_eq!(state.loading(), Some("new.mp4"), "still shown as loading");
        assert!(!tasks.is_idle());
        tasks.poll(&mut state);
        assert_eq!(state.project().tracks[0].clips.len(), 1, "still dragging");

        state.drag = None;
        assert_eq!(tasks.poll(&mut state), Action::Nothing);
        assert_eq!(state.project().tracks[0].clips.len(), 2);
        assert!(tasks.is_idle());
    }

    #[test]
    fn a_result_during_an_export_waits_for_it() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        start_load(&mut tasks, &mut state, "new.mp4");
        state.begin_export(10);
        tasks.on_arrival(&mut state, probed("new.mp4", Ok(probe(Some(5), None))));
        let exporting = state.project().clone();
        tasks.poll(&mut state);
        assert_eq!(state.project(), &exporting);
        state.end_export(Status::Idle);
        tasks.poll(&mut state);
        assert_eq!(state.project().tracks[0].clips.len(), 1);
    }

    #[test]
    fn an_opened_project_replaces_after_the_prompt_and_a_failure_leaves_it() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_track(TrackKind::Video);
        let before = state.project().clone();
        let prompt = Prompt::DiscardFor(Discard::OpenProject);
        assert_eq!(tasks.begin(&mut state, Request::OpenProject), Action::ShowPrompt(prompt));
        assert_eq!(tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt, accepted: true }), Action::ShowFileDialog(FileDialog::OpenProject));
        assert_eq!(tasks.on_arrival(&mut state, picked(FileDialog::OpenProject, "b.json")), Action::ReadProject { file: PathBuf::from("b.json") });
        assert_eq!(state.loading(), Some("b.json"));
        tasks.on_arrival(&mut state, Arrival::ProjectRead { file: PathBuf::from("b.json"), result: Err("broken".to_owned()) });
        assert_eq!(state.project(), &before, "a failed open leaves the project");
        assert_eq!(state.status(), &Status::Error("broken".to_owned()));
        assert!(state.is_dirty());

        tasks.begin(&mut state, Request::OpenProject);
        tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt, accepted: true });
        tasks.on_arrival(&mut state, picked(FileDialog::OpenProject, "b.json"));
        let opened = Project { tracks: Vec::new(), ..before.clone() };
        assert_eq!(tasks.on_arrival(&mut state, Arrival::ProjectRead { file: PathBuf::from("b.json"), result: Ok(opened.clone()) }), Action::ProjectReplaced);
        assert_eq!(state.project(), &opened);
        assert!(!state.is_dirty());
    }

    #[test]
    fn declining_the_prompt_changes_nothing() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_track(TrackKind::Video);
        let prompt = Prompt::DiscardFor(Discard::NewProject);
        tasks.begin(&mut state, Request::NewProject);
        assert_eq!(tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt, accepted: false }), Action::Nothing);
        assert_eq!(state.project().tracks.len(), 3);
        assert!(tasks.is_idle());
    }

    #[test]
    fn a_clean_project_closes_at_once_and_a_dirty_one_asks_first() {
        let mut state = with_tracks();
        assert_eq!(Tasks::default().close_requested(&mut state), Action::Exit);
        state.add_track(TrackKind::Video);
        let mut tasks = Tasks::default();
        let prompt = Prompt::DiscardFor(Discard::Close);
        assert_eq!(tasks.close_requested(&mut state), Action::ShowPrompt(prompt));
        assert_eq!(tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt, accepted: true }), Action::Exit);
    }

    #[test]
    fn closing_with_a_dialog_up_is_refused_and_never_waits_for_it() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        tasks.begin(&mut state, Request::PickClip { track: 0 });
        assert_eq!(tasks.close_requested(&mut state), Action::Nothing);
        assert_eq!(state.status(), &Status::Info(DIALOG_OPEN.to_owned()));
    }

    #[test]
    fn closing_drops_a_load_so_a_late_result_does_nothing() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        start_load(&mut tasks, &mut state, "a.mp4");
        assert_eq!(tasks.close_requested(&mut state), Action::Exit);
        assert_eq!(state.loading(), None);
        assert_eq!(tasks.on_arrival(&mut state, probed("a.mp4", Ok(probe(Some(5), None)))), Action::Nothing);
        assert!(state.project().tracks[0].clips.is_empty());
    }

    #[test]
    fn closing_during_an_export_asks_to_cancel_it_then_about_unsaved_changes() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_track(TrackKind::Video);
        state.begin_export(10);
        let cancel = Prompt::CancelExportForClose;
        assert_eq!(tasks.close_requested(&mut state), Action::ShowPrompt(cancel));
        let discard = Prompt::DiscardFor(Discard::Close);
        assert_eq!(tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt: cancel, accepted: true }), Action::ShowPrompt(discard));
        assert_eq!(tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt: discard, accepted: true }), Action::Exit);
    }

    #[test]
    fn a_failed_start_frees_the_slot_and_reports() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        tasks.begin(&mut state, Request::PickClip { track: 0 });
        tasks.start_failed(&mut state, "no thread".to_owned());
        assert!(tasks.is_idle());
        assert_eq!(state.status(), &Status::Error("no thread".to_owned()));
    }
}
