//! Dialogs and file loads that run off the UI thread, as logic with no window: which one may start,
//! what follows when one ends, and when a finished load may touch the project. The window runs the
//! returned [`Action`]s and feeds results back as [`Arrival`]s.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use d3d12_backend::MediaProbe;
use engine::Project;

use crate::editor::{clip_label, EditorState};

const DIALOG_OPEN: &str = "Close the open dialog first";
const LOAD_RUNNING: &str = "Wait for the current load to finish";
const LOAD_DROPPED: &str = "The load in progress was dropped";
const OPEN_CANCELLED: &str = "The project changed while it was being read, so Open was cancelled";

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

/// Which task a failure belongs to. A load carries the id it was started with, so the result of
/// one that was dropped is never taken for the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskId {
    /// The file dialog or prompt that is up; there is only one.
    Modal,
    Load(u64),
}

/// A result delivered back to the UI thread.
#[derive(Debug)]
pub enum Arrival {
    FilePicked { dialog: FileDialog, path: Option<PathBuf> },
    PromptAnswered { prompt: Prompt, accepted: bool },
    ClipProbed { load: u64, track: usize, path: PathBuf, result: Result<MediaProbe, String> },
    ProjectRead { load: u64, file: PathBuf, result: Result<Project, String> },
    /// The thread of a task panicked, so it will never deliver a result.
    TaskFailed { task: TaskId, message: String },
}

/// Runs `work` and returns what it delivers. A panic becomes [`Arrival::TaskFailed`], so the slot
/// the task held is freed and the user is told, instead of the app waiting for a result that never
/// comes.
pub fn run_guarded(name: &str, task: TaskId, work: impl FnOnce() -> Arrival) -> Arrival {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|payload| Arrival::TaskFailed { task, message: format!("the {name} task stopped unexpectedly: {}", panic_text(payload.as_ref())) })
}

fn panic_text(payload: &(dyn Any + Send)) -> String {
    payload.downcast_ref::<&str>().map(|text| (*text).to_owned()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "no message".to_owned())
}

/// What the window must do next. The project itself was already updated here.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    ShowFileDialog(FileDialog),
    ShowPrompt(Prompt),
    ProbeClip { load: u64, track: usize, path: PathBuf },
    ReadProject { load: u64, file: PathBuf },
    SaveTo(PathBuf),
    StartExport(PathBuf),
    /// The project was replaced, so playback of the old one must go.
    ProjectReplaced,
    Exit,
}

/// A dialog or prompt that is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modal {
    Dialog,
    Prompt,
}

/// The load in the background.
#[derive(Debug, Clone, Copy)]
struct Load {
    id: u64,
    /// For an Open: the revision the project had when the decision to replace it was taken.
    base_revision: Option<u64>,
}

/// A load that finished while the project could not be touched.
#[derive(Debug)]
enum Finished {
    Clip { track: usize, path: PathBuf, result: Result<MediaProbe, String> },
    Project { file: PathBuf, result: Result<Project, String>, base_revision: Option<u64> },
}

/// One dialog or prompt at a time, and one load; a prompt may be up while a load runs.
#[derive(Debug, Default)]
pub struct Tasks {
    modal: Option<Modal>,
    load: Option<Load>,
    finished: Option<Finished>,
    last_load: u64,
    /// The revision when Open was allowed to replace the project; it goes to the load once a file is picked.
    open_base: Option<u64>,
}

fn name_of(path: &Path) -> String {
    clip_label(&path.to_string_lossy()).to_owned()
}

impl Tasks {
    pub fn is_idle(&self) -> bool {
        self.modal.is_none() && self.load.is_none()
    }

    /// Whether a finished load waits for a drag, an export or a prompt to end.
    pub fn has_waiting_load(&self) -> bool {
        self.finished.is_some()
    }

    fn start_prompt(&mut self, prompt: Prompt) -> Action {
        self.modal = Some(Modal::Prompt);
        Action::ShowPrompt(prompt)
    }

    fn start_dialog(&mut self, dialog: FileDialog) -> Action {
        self.modal = Some(Modal::Dialog);
        Action::ShowFileDialog(dialog)
    }

    fn start_load(&mut self, state: &mut EditorState, path: &Path, base_revision: Option<u64>) -> u64 {
        self.last_load += 1;
        self.load = Some(Load { id: self.last_load, base_revision });
        state.set_loading(Some(name_of(path)));
        self.last_load
    }

    /// Forgets the load, so a result it still delivers is ignored.
    fn drop_load(&mut self, state: &mut EditorState) {
        self.load = None;
        self.finished = None;
        state.set_loading(None);
    }

    /// Starts `request`, or says on the status line why it has to wait.
    pub fn begin(&mut self, state: &mut EditorState, request: Request) -> Action {
        if self.modal.is_some() {
            state.report_info(DIALOG_OPEN.to_owned());
            return Action::Nothing;
        }
        if self.load.is_some() {
            state.report_info(LOAD_RUNNING.to_owned());
            return Action::Nothing;
        }
        match request {
            Request::NewProject if state.is_dirty() => self.start_prompt(Prompt::DiscardFor(Discard::NewProject)),
            Request::NewProject => {
                state.new_project();
                Action::ProjectReplaced
            }
            Request::OpenProject if state.is_dirty() => self.start_prompt(Prompt::DiscardFor(Discard::OpenProject)),
            Request::OpenProject => {
                self.open_base = Some(state.revision());
                self.start_dialog(FileDialog::OpenProject)
            }
            Request::SaveProjectAs => self.start_dialog(FileDialog::SaveProject),
            Request::PickClip { track } => self.start_dialog(FileDialog::PickClip { track }),
            Request::Export => self.start_dialog(FileDialog::Export),
        }
    }

    /// The app is leaving: a load in the background is dropped, since it may hang on a slow file
    /// and must never keep the app open.
    fn exit(&mut self, state: &mut EditorState) -> Action {
        if self.load.is_some() {
            self.drop_load(state);
            state.report_info(LOAD_DROPPED.to_owned());
        }
        Action::Exit
    }

    /// The window was asked to close. A dialog that is up has to be answered first, so nothing is
    /// lost by a close the user cannot see the outcome of. A load goes on while the user is still
    /// asked, because the answer may be No; it is dropped only when the app really exits.
    pub fn close_requested(&mut self, state: &mut EditorState) -> Action {
        if self.modal.is_some() {
            state.report_info(DIALOG_OPEN.to_owned());
            return Action::Nothing;
        }
        if state.is_exporting() {
            self.start_prompt(Prompt::CancelExportForClose)
        } else if state.is_dirty() {
            self.start_prompt(Prompt::DiscardFor(Discard::Close))
        } else {
            self.exit(state)
        }
    }

    /// A dialog or load could not be started at all.
    pub fn start_failed(&mut self, state: &mut EditorState, message: String) {
        self.modal = None;
        self.open_base = None;
        self.drop_load(state);
        state.report_error(message);
    }

    pub fn on_arrival(&mut self, state: &mut EditorState, arrival: Arrival) -> Action {
        match arrival {
            Arrival::FilePicked { dialog, path } => {
                self.modal = None;
                let Some(path) = path else {
                    self.open_base = None;
                    return Action::Nothing;
                };
                match dialog {
                    FileDialog::PickClip { track } => {
                        let load = self.start_load(state, &path, None);
                        Action::ProbeClip { load, track, path }
                    }
                    FileDialog::OpenProject => {
                        let base = self.open_base.take();
                        let load = self.start_load(state, &path, base);
                        Action::ReadProject { load, file: path }
                    }
                    FileDialog::SaveProject => Action::SaveTo(path),
                    FileDialog::Export => Action::StartExport(path),
                }
            }
            Arrival::PromptAnswered { prompt, accepted } => {
                self.modal = None;
                if !accepted {
                    return Action::Nothing;
                }
                match prompt {
                    Prompt::DiscardFor(Discard::NewProject) => {
                        state.new_project();
                        Action::ProjectReplaced
                    }
                    Prompt::DiscardFor(Discard::OpenProject) => {
                        self.open_base = Some(state.revision());
                        self.start_dialog(FileDialog::OpenProject)
                    }
                    Prompt::DiscardFor(Discard::Close) => self.exit(state),
                    Prompt::CancelExportForClose if state.is_dirty() => self.start_prompt(Prompt::DiscardFor(Discard::Close)),
                    Prompt::CancelExportForClose => self.exit(state),
                }
            }
            Arrival::ClipProbed { load, track, path, result } => self.load_finished(state, load, Finished::Clip { track, path, result }),
            Arrival::ProjectRead { load, file, result } => {
                let base_revision = self.load.filter(|running| running.id == load).and_then(|running| running.base_revision);
                self.load_finished(state, load, Finished::Project { file, result, base_revision })
            }
            Arrival::TaskFailed { task, message } => {
                match task {
                    TaskId::Modal if self.modal.is_some() => {
                        self.modal = None;
                        self.open_base = None;
                    }
                    TaskId::Load(id) if self.load.is_some_and(|running| running.id == id) => self.drop_load(state),
                    _ => return Action::Nothing,
                }
                state.report_error(message);
                Action::Nothing
            }
        }
    }

    fn load_finished(&mut self, state: &mut EditorState, id: u64, finished: Finished) -> Action {
        // The load was dropped, or this is the result of an earlier one.
        if self.load.is_none_or(|running| running.id != id) {
            return Action::Nothing;
        }
        self.finished = Some(finished);
        self.poll(state)
    }

    /// Applies a finished load once nothing is in the way: an export, a drag, or a prompt whose
    /// answer decides whether the app goes on.
    pub fn poll(&mut self, state: &mut EditorState) -> Action {
        if state.is_exporting() || state.drag.is_some() || self.modal.is_some() {
            return Action::Nothing;
        }
        let Some(finished) = self.finished.take() else { return Action::Nothing };
        self.load = None;
        state.set_loading(None);
        match finished {
            Finished::Clip { track, path, result } => {
                match result {
                    Ok(probe) => state.add_probed_file(track, &path, &probe),
                    Err(message) => state.report_refused(message),
                }
                Action::Nothing
            }
            Finished::Project { file, result, base_revision } => match result {
                Ok(_) if base_revision.is_some_and(|base| base != state.revision()) => {
                    state.report_refused(OPEN_CANCELLED.to_owned());
                    Action::Nothing
                }
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

    /// Picks `path` for track 0 and returns the id of the load that starts.
    fn start_load(tasks: &mut Tasks, state: &mut EditorState, path: &str) -> u64 {
        assert_eq!(tasks.begin(state, Request::PickClip { track: 0 }), Action::ShowFileDialog(PICK));
        match tasks.on_arrival(state, picked(PICK, path)) {
            Action::ProbeClip { load, track: 0, path: started } if started == PathBuf::from(path) => load,
            other => panic!("expected a probe of {path}, got {other:?}"),
        }
    }

    fn probed(load: u64, path: &str, result: Result<MediaProbe, String>) -> Arrival {
        Arrival::ClipProbed { load, track: 0, path: PathBuf::from(path), result }
    }

    /// Picks the project `path` in the Open dialog and returns the id of the read that starts.
    fn start_read(tasks: &mut Tasks, state: &mut EditorState, path: &str) -> u64 {
        match tasks.on_arrival(state, picked(FileDialog::OpenProject, path)) {
            Action::ReadProject { load, file } if file == PathBuf::from(path) => load,
            other => panic!("expected a read of {path}, got {other:?}"),
        }
    }

    fn read(load: u64, path: &str, result: Result<Project, String>) -> Arrival {
        Arrival::ProjectRead { load, file: PathBuf::from(path), result }
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
        let load = start_load(&mut tasks, &mut state, "C:/media/a.mp4");
        assert_eq!(state.loading(), Some("a.mp4"));
        assert_eq!(tasks.begin(&mut state, Request::OpenProject), Action::Nothing, "one load at a time");
        assert_eq!(state.status(), &Status::Info(LOAD_RUNNING.to_owned()));

        assert_eq!(tasks.on_arrival(&mut state, probed(load, "C:/media/a.mp4", Ok(probe(Some(7), Some(9))))), Action::Nothing);
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
        let load = start_load(&mut tasks, &mut state, "bad.mp4");
        tasks.on_arrival(&mut state, probed(load, "bad.mp4", Err("bad.mp4: it has no video or audio stream".to_owned())));
        assert!(matches!(state.status(), Status::Refused(message) if message.contains("no video or audio")), "{:?}", state.status());
        assert!(state.project().tracks[0].clips.is_empty());
        assert!(tasks.is_idle());
    }

    #[test]
    fn a_track_that_went_away_refuses_cleanly() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        let load = start_load(&mut tasks, &mut state, "a.mp4");
        state.undo();
        state.undo();
        tasks.on_arrival(&mut state, probed(load, "a.mp4", Ok(probe(Some(5), None))));
        assert!(matches!(state.status(), Status::Refused(_)), "{:?}", state.status());
        assert!(state.project().tracks.is_empty());
        assert!(tasks.is_idle());
    }

    #[test]
    fn a_result_during_a_drag_waits_for_the_drop() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_clip_from_file(0, Path::new("old.mp4"));
        let drag = Some(ClipDrag { key: ClipKey { track: 0, clip: 0 }, kind: DragKind::Move, grab_offset: Time::ZERO });
        let load = start_load(&mut tasks, &mut state, "new.mp4");
        state.drag = drag;
        tasks.on_arrival(&mut state, probed(load, "new.mp4", Ok(probe(Some(5), None))));
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
    fn an_opened_project_replaces_after_the_prompt_and_a_failure_leaves_it() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_track(TrackKind::Video);
        let before = state.project().clone();
        let prompt = Prompt::DiscardFor(Discard::OpenProject);
        assert_eq!(tasks.begin(&mut state, Request::OpenProject), Action::ShowPrompt(prompt));
        assert_eq!(tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt, accepted: true }), Action::ShowFileDialog(FileDialog::OpenProject));
        let load = start_read(&mut tasks, &mut state, "b.json");
        assert_eq!(state.loading(), Some("b.json"));
        tasks.on_arrival(&mut state, read(load, "b.json", Err("broken".to_owned())));
        assert_eq!(state.project(), &before, "a failed open leaves the project");
        assert_eq!(state.status(), &Status::Error("broken".to_owned()));
        assert!(state.is_dirty());

        tasks.begin(&mut state, Request::OpenProject);
        tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt, accepted: true });
        let load = start_read(&mut tasks, &mut state, "b.json");
        let opened = Project { tracks: Vec::new(), ..before.clone() };
        assert_eq!(tasks.on_arrival(&mut state, read(load, "b.json", Ok(opened.clone()))), Action::ProjectReplaced);
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
        let load = start_load(&mut tasks, &mut state, "a.mp4");
        assert_eq!(tasks.close_requested(&mut state), Action::Exit);
        assert_eq!(state.loading(), None);
        assert_eq!(tasks.on_arrival(&mut state, probed(load, "a.mp4", Ok(probe(Some(5), None)))), Action::Nothing);
        assert!(state.project().tracks[0].clips.is_empty());
    }

    #[test]
    fn a_late_result_of_a_dropped_load_is_not_taken_for_the_next_one() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        let first = start_load(&mut tasks, &mut state, "a.mp4");
        tasks.start_failed(&mut state, "the prompt thread did not start".to_owned());
        let second = start_load(&mut tasks, &mut state, "b.mp4");
        assert_ne!(first, second);
        tasks.on_arrival(&mut state, probed(first, "a.mp4", Ok(probe(Some(5), None))));
        assert!(state.project().tracks[0].clips.is_empty(), "the old result is not the new load's");
        assert_eq!(state.loading(), Some("b.mp4"));
        assert!(!tasks.is_idle());
        tasks.on_arrival(&mut state, probed(second, "b.mp4", Ok(probe(Some(5), None))));
        assert_eq!(state.project().tracks[0].clips.len(), 1);
        assert!(tasks.is_idle());
    }

    #[test]
    fn closing_with_unsaved_changes_keeps_the_load_while_the_prompt_is_up() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_track(TrackKind::Video);
        let load = start_load(&mut tasks, &mut state, "a.mp4");
        let prompt = Prompt::DiscardFor(Discard::Close);
        assert_eq!(tasks.close_requested(&mut state), Action::ShowPrompt(prompt));
        assert_eq!(state.loading(), Some("a.mp4"), "the answer is still open, so the load goes on");
        let tracks = state.project().tracks.len();
        tasks.on_arrival(&mut state, probed(load, "a.mp4", Ok(probe(Some(5), None))));
        assert!(state.project().tracks[0].clips.is_empty(), "nothing lands while the prompt is up");
        assert_eq!(tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt, accepted: false }), Action::Nothing);
        assert_eq!(tasks.poll(&mut state), Action::Nothing);
        assert_eq!(state.project().tracks.len(), tracks);
        assert_eq!(state.project().tracks[0].clips.len(), 1, "No keeps the app, and the load lands");
        assert!(tasks.is_idle());
        assert_eq!(state.loading(), None);
    }

    #[test]
    fn exiting_after_the_close_prompt_drops_the_load() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_track(TrackKind::Video);
        start_load(&mut tasks, &mut state, "a.mp4");
        let prompt = Prompt::DiscardFor(Discard::Close);
        tasks.close_requested(&mut state);
        assert_eq!(tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt, accepted: true }), Action::Exit);
        assert_eq!(state.loading(), None);
        assert!(tasks.is_idle(), "exit does not wait for the load");
    }

    #[test]
    fn an_open_is_cancelled_when_the_project_changed_while_it_was_read() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        assert_eq!(tasks.begin(&mut state, Request::OpenProject), Action::ShowFileDialog(FileDialog::OpenProject));
        let load = start_read(&mut tasks, &mut state, "b.json");
        state.add_track(TrackKind::Video);
        let edited = state.project().clone();
        let opened = Project { tracks: Vec::new(), ..edited.clone() };
        let action = tasks.on_arrival(&mut state, read(load, "b.json", Ok(opened)));
        assert_eq!(action, Action::Nothing);
        assert_eq!(state.project(), &edited, "the edit made while reading is kept");
        assert!(matches!(state.status(), Status::Refused(message) if message.contains("changed while")), "{:?}", state.status());
        assert!(tasks.is_idle());
        assert_eq!(state.loading(), None);
    }

    #[test]
    fn an_open_is_cancelled_when_the_project_changed_after_the_discard_prompt_was_answered() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_track(TrackKind::Video);
        let prompt = Prompt::DiscardFor(Discard::OpenProject);
        tasks.begin(&mut state, Request::OpenProject);
        tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt, accepted: true });
        state.add_track(TrackKind::Audio);
        let load = start_read(&mut tasks, &mut state, "b.json");
        let edited = state.project().clone();
        let opened = Project { tracks: Vec::new(), ..edited.clone() };
        tasks.on_arrival(&mut state, read(load, "b.json", Ok(opened)));
        assert_eq!(state.project(), &edited);
        assert!(matches!(state.status(), Status::Refused(message) if message.contains("changed while")), "{:?}", state.status());
    }

    #[test]
    fn an_unchanged_project_is_replaced_by_the_one_that_was_read() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        tasks.begin(&mut state, Request::OpenProject);
        let load = start_read(&mut tasks, &mut state, "b.json");
        let opened = Project { tracks: Vec::new(), ..state.project().clone() };
        let action = tasks.on_arrival(&mut state, read(load, "b.json", Ok(opened.clone())));
        assert_eq!(action, Action::ProjectReplaced);
        assert_eq!(state.project(), &opened);
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

    #[test]
    fn a_failed_load_frees_the_slot_and_reports() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        let load = start_load(&mut tasks, &mut state, "a.mp4");
        let failed = Arrival::TaskFailed { task: TaskId::Load(load), message: "the clip-probe task stopped unexpectedly: boom".to_owned() };
        assert_eq!(tasks.on_arrival(&mut state, failed), Action::Nothing);
        assert!(tasks.is_idle());
        assert_eq!(state.loading(), None);
        assert!(matches!(state.status(), Status::Error(message) if message.contains("boom")), "{:?}", state.status());
        assert_eq!(tasks.begin(&mut state, Request::OpenProject), Action::ShowFileDialog(FileDialog::OpenProject), "a new request can start");
    }

    #[test]
    fn a_failed_dialog_frees_the_slot_so_the_app_can_close() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_track(TrackKind::Video);
        let prompt = Prompt::DiscardFor(Discard::Close);
        assert_eq!(tasks.close_requested(&mut state), Action::ShowPrompt(prompt));
        assert_eq!(tasks.close_requested(&mut state), Action::Nothing, "stuck while the prompt is up");
        tasks.on_arrival(&mut state, Arrival::TaskFailed { task: TaskId::Modal, message: "prompt failed".to_owned() });
        assert!(tasks.is_idle());
        assert_eq!(tasks.close_requested(&mut state), Action::ShowPrompt(prompt), "the close can be asked again");
    }

    #[test]
    fn a_failure_of_a_dropped_load_does_not_free_the_next_one() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        let first = start_load(&mut tasks, &mut state, "a.mp4");
        tasks.start_failed(&mut state, "no thread".to_owned());
        start_load(&mut tasks, &mut state, "b.mp4");
        tasks.on_arrival(&mut state, Arrival::TaskFailed { task: TaskId::Load(first), message: "late".to_owned() });
        assert!(!tasks.is_idle());
        assert_eq!(state.loading(), Some("b.mp4"));
    }

    #[test]
    fn a_panic_in_the_work_of_a_task_becomes_a_failure() {
        let arrival = run_guarded("clip-probe", TaskId::Load(3), || -> Arrival { panic!("denominator is zero") });
        match arrival {
            Arrival::TaskFailed { task: TaskId::Load(3), message } => assert!(message.contains("clip-probe") && message.contains("denominator is zero"), "{message}"),
            other => panic!("{other:?}"),
        }
        let arrival = run_guarded("prompt", TaskId::Modal, || -> Arrival { panic!("{}", String::from("formatted")) });
        assert!(matches!(arrival, Arrival::TaskFailed { task: TaskId::Modal, message } if message.contains("formatted")));
        let fine = run_guarded("prompt", TaskId::Modal, || Arrival::FilePicked { dialog: FileDialog::Export, path: None });
        assert!(matches!(fine, Arrival::FilePicked { .. }), "{fine:?}");
    }
}

