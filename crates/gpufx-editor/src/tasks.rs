//! Dialogs and file loads that run off the UI thread, as logic with no window: which one may start,
//! what follows when one ends, and when a finished load may touch the project. The window runs the
//! returned [`Action`]s and feeds results back as [`Arrival`]s.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use d3d12_backend::MediaProbe;
use engine::{ClipKey, Project};

use crate::editor::{clip_label, EditorState, LibraryTarget};
use crate::i18n::Message;
use crate::media::MediaItem;
use crate::package_import::{ImportOutcome, ImportProblem};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDialog {
    PickClip { track: usize },
    /// A font file for the text clip, while the project is still at `revision`: a clip key names
    /// a clip of one project only, so a change while the dialog is up drops the choice.
    PickFont { clip: ClipKey, revision: u64 },
    ImportMedia { audio_only: bool },
    ImportPackage,
    OpenProject,
    SaveProject,
    Export,
    /// An SRT file to import as a subtitle track.
    ImportSubtitles,
    /// Where to save subtitle track `track` as an SRT file. Tracks are only ever appended, so the
    /// index still names it when the dialog closes.
    ExportSubtitles { track: usize },
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
    /// A package with the name in [`Tasks::package_to_overwrite`] is installed; replace it?
    OverwritePackage,
}

impl Prompt {
    /// The question to ask. `overwriting` is the name of the package an overwrite would replace.
    pub fn question(self, overwriting: Option<&str>) -> Message {
        match self {
            Self::OverwritePackage => Message::PromptOverwritePackage(overwriting.unwrap_or_default().to_owned()),
            Self::DiscardFor(Discard::NewProject) => Message::PromptDiscardForNew,
            Self::DiscardFor(Discard::OpenProject) => Message::PromptDiscardForOpen,
            Self::DiscardFor(Discard::Close) => Message::PromptDiscardForClose,
            Self::CancelExportForClose => Message::PromptCancelExport,
        }
    }
}

/// What the user asked for that may need a dialog or a load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// To the file it came from, or through the Save dialog when it has none.
    SaveProject,
    NewProject,
    OpenProject,
    SaveProjectAs,
    PickClip { track: usize },
    ImportMedia { audio_only: bool },
    ImportPackage,
    Export,
    /// A font file for the text clip, through the file dialog.
    PickFont { clip: ClipKey },
    ImportSubtitles,
    ExportSubtitles { track: usize },
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
    MediaProbed { load: u64, path: PathBuf, result: Result<MediaProbe, String> },
    DropProbed { load: u64, path: PathBuf, result: Result<MediaProbe, String> },
    ProjectRead { load: u64, file: PathBuf, result: Result<Project, String> },
    PackageImported { load: u64, path: PathBuf, result: Result<ImportOutcome, ImportProblem> },
    LibraryItemProbed { load: u64, item: MediaItem, target: LibraryTarget, result: Result<MediaProbe, String> },
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
    ProbeDrop { load: u64, path: PathBuf },
    ProbeMedia { load: u64, path: PathBuf },
    ReadProject { load: u64, file: PathBuf },
    ImportPackage { load: u64, path: PathBuf, overwrite: bool },
    /// A sound of the library must be measured before it becomes a clip.
    ProbeLibraryItem { load: u64, item: MediaItem, target: LibraryTarget },
    /// A package was installed: the list must be read again and what was built from it dropped.
    PackageInstalled { package: String },
    /// An audio pack was installed: the library must be read again.
    AudioPackInstalled,
    StickerPackInstalled,
    /// A clip animation pack was installed: the library must be read again.
    ClipAnimationPackInstalled,
    /// A text or subtitle style pack was installed: the library must be read again.
    TextStylePackInstalled,
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
    Media { path: PathBuf, result: Result<MediaProbe, String> },
    Drop { path: PathBuf, result: Result<MediaProbe, String> },
    Project { file: PathBuf, result: Result<Project, String>, base_revision: Option<u64> },
    Package { path: PathBuf, result: Result<ImportOutcome, ImportProblem> },
    LibraryItem { item: MediaItem, target: LibraryTarget, result: Result<MediaProbe, String> },
}

/// A zip waiting for the answer to whether it may replace the package installed under its name.
#[derive(Debug)]
struct PendingOverwrite {
    path: PathBuf,
    name: String,
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
    overwrite: Option<PendingOverwrite>,
}

fn name_of(path: &Path) -> String {
    clip_label(&path.to_string_lossy()).to_owned()
}

impl Tasks {
    pub fn is_idle(&self) -> bool {
        self.modal.is_none() && self.load.is_none()
    }

    /// The name of the installed package the overwrite prompt is about.
    pub fn package_to_overwrite(&self) -> Option<&str> {
        self.overwrite.as_ref().map(|pending| pending.name.as_str())
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
        // The project being held is only a preview, with a revision the release will change.
        if state.refuse_while_live() {
            return Action::Nothing;
        }
        if let (Request::SaveProject, Some(file)) = (request, state.project_file()) {
            return Action::SaveTo(file.to_path_buf());
        }
        if self.modal.is_some() {
            state.report_info(Message::DialogOpen);
            return Action::Nothing;
        }
        if self.load.is_some() {
            state.report_info(Message::LoadRunning);
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
            Request::SaveProject | Request::SaveProjectAs => self.start_dialog(FileDialog::SaveProject),
            Request::PickClip { track } => self.start_dialog(FileDialog::PickClip { track }),
            Request::ImportMedia { audio_only } => self.start_dialog(FileDialog::ImportMedia { audio_only }),
            Request::ImportPackage => self.start_dialog(FileDialog::ImportPackage),
            Request::Export => self.start_dialog(FileDialog::Export),
            Request::PickFont { clip } => self.start_dialog(FileDialog::PickFont { clip, revision: state.revision() }),
            Request::ImportSubtitles => self.start_dialog(FileDialog::ImportSubtitles),
            Request::ExportSubtitles { track } => self.start_dialog(FileDialog::ExportSubtitles { track }),
        }
    }

    /// Measures a library sound before it becomes a clip; a dialog or load that is running says so.
    pub fn begin_library_add(&mut self, state: &mut EditorState, item: MediaItem, target: LibraryTarget) -> Action {
        if state.refuse_while_live() {
            return Action::Nothing;
        }
        if self.modal.is_some() {
            state.report_info(Message::DialogOpen);
            return Action::Nothing;
        }
        if self.load.is_some() {
            state.report_info(Message::LoadRunning);
            return Action::Nothing;
        }
        let load = self.start_load(state, Path::new(&item.name), None);
        Action::ProbeLibraryItem { load, item, target }
    }

    /// Starts reading the next dropped file once nothing else is going on.
    pub fn next_file_drop(&mut self, state: &mut EditorState) -> Action {
        if self.modal.is_some() || self.load.is_some() || state.is_exporting() || state.is_holding() {
            return Action::Nothing;
        }
        // A batch stays in the queue while its last file is read, for the place it says.
        while state.ui.drop_batches.front().is_some_and(|batch| batch.files.is_empty()) {
            state.ui.drop_batches.pop_front();
        }
        let Some(path) = state.ui.drop_batches.front_mut().and_then(|batch| batch.files.pop_front()) else { return Action::Nothing };
        let load = self.start_load(state, &path, None);
        Action::ProbeDrop { load, path }
    }

    /// The app is leaving: a load in the background is dropped, since it may hang on a slow file
    /// and must never keep the app open.
    fn exit(&mut self, state: &mut EditorState) -> Action {
        if self.load.is_some() {
            self.drop_load(state);
            state.report_info(Message::LoadDropped);
        }
        Action::Exit
    }

    /// The window was asked to close. A dialog that is up has to be answered first, so nothing is
    /// lost by a close the user cannot see the outcome of. A load goes on while the user is still
    /// asked, because the answer may be No; it is dropped only when the app really exits.
    pub fn close_requested(&mut self, state: &mut EditorState) -> Action {
        if self.modal.is_some() {
            state.report_info(Message::DialogOpen);
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
        state.report_error(Message::TaskFailed(message));
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
                    FileDialog::ImportMedia { .. } => {
                        let load = self.start_load(state, &path, None);
                        Action::ProbeMedia { load, path }
                    }
                    FileDialog::PickFont { revision, .. } if revision != state.revision() => {
                        state.report_info(Message::FontTargetChanged);
                        Action::Nothing
                    }
                    FileDialog::PickFont { clip, .. } => {
                        state.set_text_font(clip, engine::FontRef::File { path: path.to_string_lossy().into_owned() });
                        Action::Nothing
                    }
                    FileDialog::ImportPackage if state.is_exporting() => {
                        state.report_refused(Message::ExportRunning);
                        Action::Nothing
                    }
                    FileDialog::ImportPackage => {
                        let load = self.start_load(state, &path, None);
                        Action::ImportPackage { load, path, overwrite: false }
                    }
                    FileDialog::OpenProject => {
                        let base = self.open_base.take();
                        let load = self.start_load(state, &path, base);
                        Action::ReadProject { load, file: path }
                    }
                    FileDialog::SaveProject if state.refuse_while_live() => Action::Nothing,
                    FileDialog::SaveProject => Action::SaveTo(path),
                    FileDialog::Export if state.is_exporting() => {
                        state.report_refused(Message::ExportAlreadyRunning);
                        Action::Nothing
                    }
                    FileDialog::Export => Action::StartExport(path),
                    FileDialog::ImportSubtitles => {
                        state.import_subtitles_file(&path);
                        Action::Nothing
                    }
                    FileDialog::ExportSubtitles { track } => {
                        state.export_subtitles_file(track, &path);
                        Action::Nothing
                    }
                }
            }
            Arrival::PromptAnswered { prompt, accepted } => {
                self.modal = None;
                let overwrite = self.overwrite.take();
                if !accepted {
                    return Action::Nothing;
                }
                match prompt {
                    Prompt::OverwritePackage if state.is_exporting() => {
                        state.report_refused(Message::ExportRunning);
                        Action::Nothing
                    }
                    Prompt::OverwritePackage => match overwrite {
                        Some(PendingOverwrite { path, .. }) => {
                            let load = self.start_load(state, &path, None);
                            Action::ImportPackage { load, path, overwrite: true }
                        }
                        None => Action::Nothing,
                    },
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
            Arrival::MediaProbed { load, path, result } => self.load_finished(state, load, Finished::Media { path, result }),
            Arrival::DropProbed { load, path, result } => self.load_finished(state, load, Finished::Drop { path, result }),
            Arrival::PackageImported { load, path, result } => self.load_finished(state, load, Finished::Package { path, result }),
            Arrival::LibraryItemProbed { load, item, target, result } => self.load_finished(state, load, Finished::LibraryItem { item, target, result }),
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
                state.report_error(Message::TaskFailed(message));
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
        if state.is_exporting() || state.is_holding() || self.modal.is_some() {
            return Action::Nothing;
        }
        let Some(finished) = self.finished.take() else { return Action::Nothing };
        self.load = None;
        state.set_loading(None);
        match finished {
            Finished::Clip { track, path, result } => {
                match result {
                    Ok(probe) => state.add_probed_file(track, &path, &probe),
                    Err(message) => state.report_refused(Message::Technical(message)),
                }
                Action::Nothing
            }
            Finished::Drop { path, result } => {
                match result {
                    Ok(probe) => {
                        state.add_dropped_file(&path, &probe);
                    }
                    Err(message) => state.report_refused(Message::Technical(message)),
                }
                Action::Nothing
            }
            Finished::Media { path, result } => {
                match result {
                    Ok(probe) => state.import_probed(&path, &probe),
                    Err(message) => state.report_refused(Message::Technical(message)),
                }
                Action::Nothing
            }
            Finished::Package { path, result } => match result {
                Ok(ImportOutcome::AlreadyInstalled { name }) => {
                    self.overwrite = Some(PendingOverwrite { path, name });
                    self.start_prompt(Prompt::OverwritePackage)
                }
                Ok(ImportOutcome::Installed(installed)) => {
                    state.show_installed_package(&installed);
                    Action::PackageInstalled { package: installed.package }
                }
                Ok(ImportOutcome::AudioInstalled(installed)) => {
                    state.show_installed_audio_pack(&installed);
                    Action::AudioPackInstalled
                }
                Ok(ImportOutcome::StickerInstalled(installed)) => {
                    state.show_installed_sticker_pack(&installed);
                    Action::StickerPackInstalled
                }
                Ok(ImportOutcome::ClipAnimationInstalled(installed)) => {
                    state.show_installed_clip_animation_pack(&installed);
                    Action::ClipAnimationPackInstalled
                }
                Ok(ImportOutcome::TextStyleInstalled(installed)) => {
                    state.show_installed_text_style_pack(&installed.name, installed.replaced);
                    Action::TextStylePackInstalled
                }
                Ok(ImportOutcome::SubtitleStyleInstalled(installed)) => {
                    state.show_installed_text_style_pack(&installed.name, installed.replaced);
                    Action::TextStylePackInstalled
                }
                Err(problem) => {
                    state.report_refused(Message::PackageImportRefused(problem));
                    Action::Nothing
                }
            },
            Finished::LibraryItem { item, target, result } => {
                match result {
                    Ok(probe) => state.add_probed_library_item(&item, target, &probe),
                    Err(message) => state.report_refused(Message::Technical(message)),
                }
                Action::Nothing
            }
            Finished::Project { file, result, base_revision } => match result {
                Ok(_) if base_revision.is_some_and(|base| base != state.revision()) => {
                    state.report_refused(Message::OpenCancelled);
                    Action::Nothing
                }
                Ok(project) => {
                    state.replace_project(project, Some(file));
                    Action::ProjectReplaced
                }
                Err(message) => {
                    state.report_error(Message::OpenFailed(message));
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

    /// Picks a zip in the dialog and returns the id of the load that installs it.
    fn start_package_import(tasks: &mut Tasks, state: &mut EditorState) -> u64 {
        assert_eq!(tasks.begin(state, Request::ImportPackage), Action::ShowFileDialog(FileDialog::ImportPackage));
        match tasks.on_arrival(state, picked(FileDialog::ImportPackage, "pack.zip")) {
            Action::ImportPackage { load, overwrite: false, .. } => load,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_import_picked_while_an_export_runs_is_refused_before_it_starts() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        assert_eq!(tasks.begin(&mut state, Request::ImportPackage), Action::ShowFileDialog(FileDialog::ImportPackage));
        state.begin_export(100);
        assert_eq!(tasks.on_arrival(&mut state, picked(FileDialog::ImportPackage, "pack.zip")), Action::Nothing);
        assert!(matches!(state.status(), Status::Refused(Message::ExportRunning)));
    }

    #[test]
    fn an_installed_audio_pack_asks_for_the_library_and_shows_the_audio_tab() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        let load = start_package_import(&mut tasks, &mut state);
        let installed = crate::asset_install::InstalledAudioPack { kind: crate::asset_install::AudioPackKind::Sfx, id: "p".to_owned(), name: "P".to_owned(), replaced: false };
        assert_eq!(tasks.on_arrival(&mut state, imported(load, Ok(ImportOutcome::AudioInstalled(installed)))), Action::AudioPackInstalled);
        assert_eq!(state.ui.left_tab, crate::editor::LeftTab::Audio);
    }

    #[test]
    fn an_installed_clip_animation_pack_asks_for_the_library() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        let load = start_package_import(&mut tasks, &mut state);
        let installed = crate::clip_animation_install::InstalledClipAnimationPack { id: "p".to_owned(), name: "P".to_owned(), replaced: false };
        assert_eq!(tasks.on_arrival(&mut state, imported(load, Ok(ImportOutcome::ClipAnimationInstalled(installed)))), Action::ClipAnimationPackInstalled);
    }

    #[test]
    fn an_installed_style_pack_asks_for_the_library_and_shows_the_text_tab() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        let load = start_package_import(&mut tasks, &mut state);
        let installed = crate::text_style_install::InstalledTextStylePack { id: "p".to_owned(), name: "P".to_owned(), replaced: false };
        assert_eq!(tasks.on_arrival(&mut state, imported(load, Ok(ImportOutcome::TextStyleInstalled(installed)))), Action::TextStylePackInstalled);
        assert_eq!(state.ui.left_tab, crate::editor::LeftTab::Text);
        let load = start_package_import(&mut tasks, &mut state);
        let installed = crate::subtitle_style_install::InstalledSubtitleStylePack { id: "s".to_owned(), name: "S".to_owned(), replaced: true };
        assert_eq!(tasks.on_arrival(&mut state, imported(load, Ok(ImportOutcome::SubtitleStyleInstalled(installed)))), Action::TextStylePackInstalled);
    }

    #[test]
    fn an_installed_sticker_pack_asks_for_the_library_and_shows_the_sticker_tab() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        let load = start_package_import(&mut tasks, &mut state);
        let installed = crate::sticker_install::InstalledStickerPack { id: "p".to_owned(), name: "P".to_owned(), replaced: false };
        assert_eq!(tasks.on_arrival(&mut state, imported(load, Ok(ImportOutcome::StickerInstalled(installed)))), Action::StickerPackInstalled);
        assert_eq!(state.ui.left_tab, crate::editor::LeftTab::Stickers);
    }

    #[test]
    fn a_library_sound_is_measured_then_placed_at_its_real_length() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        let manifest_length = Some(engine::Time::from_ticks(9000));
        let item = MediaItem { source: "asset:sfx/p/a.flac".to_owned(), name: "A".to_owned(), kind: crate::media::MediaKind::Audio, duration: manifest_length, estimated: true };
        let Action::ProbeLibraryItem { load, item, target } = tasks.begin_library_add(&mut state, item, LibraryTarget::Playhead) else { panic!("it is measured first") };
        assert_eq!(tasks.begin_library_add(&mut state, item.clone(), LibraryTarget::Playhead), Action::Nothing, "one load at a time");
        let arrived = Arrival::LibraryItemProbed { load, item, target, result: Ok(probe(None, Some(4))) };
        assert_eq!(tasks.on_arrival(&mut state, arrived), Action::Nothing);
        let clips: Vec<_> = state.project().tracks.iter().filter(|track| track.is_audio()).flat_map(|track| &track.clips).collect();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].source, "asset:sfx/p/a.flac", "the clip keeps the asset source");
        assert_eq!(clips[0].duration, seconds(4), "the probe, not the manifest");
    }

    fn imported(load: u64, result: Result<ImportOutcome, ImportProblem>) -> Arrival {
        Arrival::PackageImported { load, path: PathBuf::from("pack.zip"), result }
    }

    #[test]
    fn a_name_already_installed_is_asked_about_and_yes_imports_again_with_overwrite() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        let load = start_package_import(&mut tasks, &mut state);
        let action = tasks.on_arrival(&mut state, imported(load, Ok(ImportOutcome::AlreadyInstalled { name: "Iris".to_owned() })));
        assert_eq!(action, Action::ShowPrompt(Prompt::OverwritePackage));
        assert_eq!(Prompt::OverwritePackage.question(tasks.package_to_overwrite()), Message::PromptOverwritePackage("Iris".to_owned()));
        match tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt: Prompt::OverwritePackage, accepted: true }) {
            Action::ImportPackage { path, overwrite: true, .. } => assert_eq!(path, PathBuf::from("pack.zip")),
            other => panic!("{other:?}"),
        }
        assert_eq!(tasks.package_to_overwrite(), None);
    }

    #[test]
    fn yes_to_the_overwrite_during_an_export_installs_nothing() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        let load = start_package_import(&mut tasks, &mut state);
        tasks.on_arrival(&mut state, imported(load, Ok(ImportOutcome::AlreadyInstalled { name: "Iris".to_owned() })));
        state.begin_export(10);
        let action = tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt: Prompt::OverwritePackage, accepted: true });
        assert_eq!(action, Action::Nothing);
        assert_eq!(state.status(), &Status::Refused(Message::ExportRunning));
        assert!(tasks.is_idle());
        assert_eq!(tasks.package_to_overwrite(), None);
    }

    #[test]
    fn no_to_the_overwrite_installs_nothing_and_frees_the_slot() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        let load = start_package_import(&mut tasks, &mut state);
        tasks.on_arrival(&mut state, imported(load, Ok(ImportOutcome::AlreadyInstalled { name: "Iris".to_owned() })));
        assert_eq!(tasks.on_arrival(&mut state, Arrival::PromptAnswered { prompt: Prompt::OverwritePackage, accepted: false }), Action::Nothing);
        assert!(tasks.is_idle());
        assert_eq!(tasks.package_to_overwrite(), None);
    }

    #[test]
    fn an_installed_package_is_shown_in_its_list_and_a_refused_one_is_reported() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        let load = start_package_import(&mut tasks, &mut state);
        let installed = crate::package_import::InstalledPackage { package: "packages/gpufx/user/a.zip".to_owned(), name: "A".to_owned(), kind: crate::packages::PackageKind::Transition, replaced: false };
        assert_eq!(tasks.on_arrival(&mut state, imported(load, Ok(ImportOutcome::Installed(installed)))), Action::PackageInstalled { package: "packages/gpufx/user/a.zip".to_owned() });
        assert_eq!(state.ui.left_tab, crate::editor::LeftTab::Transitions);
        assert_eq!(state.picker.kind, crate::packages::PackageKind::Transition);
        assert_eq!(state.picker.selected.as_deref(), Some("packages/gpufx/user/a.zip"));

        let load = start_package_import(&mut tasks, &mut state);
        assert_eq!(tasks.on_arrival(&mut state, imported(load, Err(ImportProblem::NoManifest))), Action::Nothing);
        assert!(tasks.is_idle());
    }

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

    #[test]
    fn an_export_dialog_that_finishes_during_an_export_starts_nothing() {
        let mut state = with_tracks();
        let mut tasks = Tasks::default();
        state.begin_export(10);
        let action = tasks.on_arrival(&mut state, picked(FileDialog::Export, "second.mp4"));
        assert!(matches!(action, Action::Nothing), "{action:?}");
        assert_eq!(state.status(), &Status::Refused(Message::ExportAlreadyRunning));
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

    fn queue_drop(state: &mut EditorState, files: &[&str], track: Option<usize>, at: engine::Time) {
        state.ui.drop_batches.push_back(crate::editor::DropBatch { files: files.iter().map(PathBuf::from).collect(), track, at });
    }

    fn read_next(tasks: &mut Tasks, state: &mut EditorState, result: Result<MediaProbe, String>) {
        let Action::ProbeDrop { load, path } = tasks.next_file_drop(state) else { panic!("a file should be read") };
        assert_eq!(tasks.next_file_drop(state), Action::Nothing, "one read at a time");
        tasks.on_arrival(state, Arrival::DropProbed { load, path, result });
    }

    #[test]
    fn dropped_files_follow_each_other_on_one_track() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        queue_drop(&mut state, &["C:/a.mp4", "C:/b.mp4"], None, engine::Time::from_ticks(90_000));
        read_next(&mut tasks, &mut state, Ok(probe(Some(5), Some(5))));
        read_next(&mut tasks, &mut state, Ok(probe(Some(5), Some(5))));
        assert_eq!(state.project().tracks.len(), 1);
        let clips = &state.project().tracks[0].clips;
        assert_eq!(clips[0].start, engine::Time::from_ticks(90_000));
        assert_eq!(Some(clips[1].start), clips[0].end());
    }

    #[test]
    fn an_unreadable_file_in_the_middle_keeps_the_batch_together() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        queue_drop(&mut state, &["C:/a.mp4", "C:/bad.mp4", "C:/c.mp4"], None, engine::Time::ZERO);
        read_next(&mut tasks, &mut state, Ok(probe(Some(5), Some(5))));
        read_next(&mut tasks, &mut state, Err("unreadable".to_owned()));
        assert!(matches!(state.status(), crate::editor::Status::Refused(_)));
        read_next(&mut tasks, &mut state, Ok(probe(Some(5), Some(5))));
        assert_eq!(state.project().tracks.len(), 1, "the third file joins the first");
        let clips = &state.project().tracks[0].clips;
        assert_eq!(Some(clips[1].start), clips[0].end());
    }

    #[test]
    fn a_later_file_of_another_kind_is_refused_on_the_track_of_the_batch() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        queue_drop(&mut state, &["C:/a.mp4", "C:/m.wav", "C:/c.mp4"], None, engine::Time::ZERO);
        read_next(&mut tasks, &mut state, Ok(probe(Some(5), Some(5))));
        read_next(&mut tasks, &mut state, Ok(probe(None, Some(5))));
        assert!(matches!(state.status(), crate::editor::Status::Refused(_)));
        read_next(&mut tasks, &mut state, Ok(probe(Some(5), Some(5))));
        assert_eq!(state.project().tracks.len(), 1);
        assert_eq!(state.project().tracks[0].clips.len(), 2);
    }

    #[test]
    fn a_second_drop_keeps_its_own_place() {
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        queue_drop(&mut state, &["C:/a.mp4"], None, engine::Time::ZERO);
        queue_drop(&mut state, &["C:/b.mp4"], None, engine::Time::ZERO);
        read_next(&mut tasks, &mut state, Ok(probe(Some(5), Some(5))));
        read_next(&mut tasks, &mut state, Ok(probe(Some(5), Some(5))));
        assert_eq!(state.project().tracks.len(), 2, "each drop outside the rows makes its own track");
        assert_eq!(state.project().tracks[1].clips[0].start, engine::Time::ZERO);
    }

    #[test]
    fn a_new_project_forgets_the_drops_of_the_old_one() {
        let mut state = EditorState::default();
        queue_drop(&mut state, &["C:/a.mp4"], Some(3), engine::Time::ZERO);
        state.new_project();
        assert!(state.ui.drop_batches.is_empty());
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
    fn saving_goes_to_the_file_or_the_dialog() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        assert_eq!(tasks.begin(&mut state, Request::SaveProject), Action::SaveTo(PathBuf::from("p.json")));
        let (mut tasks, mut state) = (Tasks::default(), EditorState::default());
        assert_eq!(tasks.begin(&mut state, Request::SaveProject), Action::ShowFileDialog(FileDialog::SaveProject));
    }

    #[test]
    fn nothing_starts_from_the_preview_of_a_held_control() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_clip_from_file(0, Path::new("a.mp4"));
        state.live_gain(-6.0);
        for request in [Request::SaveProject, Request::SaveProjectAs, Request::OpenProject, Request::NewProject, Request::Export] {
            assert_eq!(tasks.begin(&mut state, request), Action::Nothing, "{request:?}");
        }
        assert!(tasks.is_idle(), "no dialog or prompt came up");
        assert_eq!(tasks.open_base, None, "no revision of the preview was remembered");
        assert!(state.is_live(), "the hold goes on");
    }

    #[test]
    fn a_second_dialog_cannot_open_while_one_is_up() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        assert_eq!(tasks.begin(&mut state, Request::PickClip { track: 0 }), Action::ShowFileDialog(PICK));
        for request in [Request::OpenProject, Request::NewProject, Request::SaveProjectAs, Request::Export, Request::PickClip { track: 1 }] {
            assert_eq!(tasks.begin(&mut state, request), Action::Nothing, "{request:?}");
        }
        assert_eq!(state.status(), &Status::Info(Message::DialogOpen));
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
        assert_eq!(state.status(), &Status::Info(Message::LoadRunning));

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
        assert_eq!(state.status(), &Status::Refused(Message::Technical("bad.mp4: it has no video or audio stream".to_owned())));
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
    fn a_result_during_a_held_inspector_control_waits_for_the_release() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        state.add_clip_from_file(0, Path::new("old.mp4"));
        let load = start_load(&mut tasks, &mut state, "new.mp4");
        state.select(Some(ClipKey { track: 0, clip: 0 }));
        state.live_gain(-3.0);
        tasks.on_arrival(&mut state, probed(load, "new.mp4", Ok(probe(Some(5), None))));
        assert_eq!(state.project().tracks[0].clips.len(), 1, "queued, the control holds a clip key");
        state.commit_live();
        tasks.poll(&mut state);
        assert_eq!(state.project().tracks[0].clips.len(), 2);
    }

    #[test]
    fn an_import_lists_the_file_and_adds_no_clip() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        let dialog = FileDialog::ImportMedia { audio_only: false };
        assert_eq!(tasks.begin(&mut state, Request::ImportMedia { audio_only: false }), Action::ShowFileDialog(dialog));
        let Action::ProbeMedia { load, path } = tasks.on_arrival(&mut state, picked(dialog, "C:/m/a.mp4")) else { panic!("expected a probe") };
        assert_eq!(state.loading(), Some("a.mp4"));
        let result = Arrival::MediaProbed { load, path, result: Ok(probe(Some(5), Some(5))) };
        assert_eq!(tasks.on_arrival(&mut state, result), Action::Nothing);
        assert_eq!(state.library.visible(crate::media::MediaKind::Video, "").len(), 1);
        assert!(state.project().tracks[0].clips.is_empty());
        assert!(tasks.is_idle());
    }

    #[test]
    fn a_failed_import_says_why_and_frees_the_slot() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        let dialog = FileDialog::ImportMedia { audio_only: true };
        tasks.begin(&mut state, Request::ImportMedia { audio_only: true });
        let Action::ProbeMedia { load, path } = tasks.on_arrival(&mut state, picked(dialog, "bad.wav")) else { panic!("expected a probe") };
        tasks.on_arrival(&mut state, Arrival::MediaProbed { load, path, result: Err("unreadable".to_owned()) });
        assert_eq!(state.status(), &Status::Refused(Message::Technical("unreadable".to_owned())));
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
        assert_eq!(state.status(), &Status::Error(Message::OpenFailed("broken".to_owned())));
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
        assert_eq!(state.status(), &Status::Info(Message::DialogOpen));
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
        assert_eq!(state.status(), &Status::Refused(Message::OpenCancelled));
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
        assert_eq!(state.status(), &Status::Refused(Message::OpenCancelled));
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
        assert_eq!(state.status(), &Status::Error(Message::TaskFailed("no thread".to_owned())));
    }

    #[test]
    fn a_failed_load_frees_the_slot_and_reports() {
        let (mut tasks, mut state) = (Tasks::default(), with_tracks());
        let load = start_load(&mut tasks, &mut state, "a.mp4");
        let failed = Arrival::TaskFailed { task: TaskId::Load(load), message: "the clip-probe task stopped unexpectedly: boom".to_owned() };
        assert_eq!(tasks.on_arrival(&mut state, failed), Action::Nothing);
        assert!(tasks.is_idle());
        assert_eq!(state.loading(), None);
        assert!(matches!(state.status(), Status::Error(Message::TaskFailed(detail)) if detail.contains("boom")), "{:?}", state.status());
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

    /// A state with a text clip, selected, and the key of that clip.
    fn with_text_clip() -> (EditorState, ClipKey) {
        let mut state = with_tracks();
        state.add_text_at_playhead();
        let key = state.selection().expect("the new text clip is selected");
        (state, key)
    }

    fn font_of(state: &EditorState, key: ClipKey) -> engine::FontRef {
        state.project().tracks[key.track].clips[key.clip].text.as_ref().unwrap().font.clone()
    }

    #[test]
    fn a_font_file_chosen_while_the_project_stays_the_same_goes_to_the_clip() {
        let (mut state, key) = with_text_clip();
        let mut tasks = Tasks::default();
        let dialog = FileDialog::PickFont { clip: key, revision: state.revision() };
        assert_eq!(tasks.begin(&mut state, Request::PickFont { clip: key }), Action::ShowFileDialog(dialog));
        assert_eq!(tasks.on_arrival(&mut state, picked(dialog, "C:/fonts/a.ttf")), Action::Nothing);
        assert_eq!(font_of(&state, key), engine::FontRef::File { path: "C:/fonts/a.ttf".to_owned() });
    }

    #[test]
    fn a_font_file_chosen_after_the_project_changed_is_not_applied() {
        let (mut state, key) = with_text_clip();
        let mut tasks = Tasks::default();
        let Action::ShowFileDialog(dialog) = tasks.begin(&mut state, Request::PickFont { clip: key }) else { panic!("no dialog") };
        // An agent edits the project while the dialog is up: the key may now name another clip.
        state.add_track(TrackKind::Video);
        let before = state.project().clone();
        assert_eq!(tasks.on_arrival(&mut state, picked(dialog, "C:/fonts/a.ttf")), Action::Nothing);
        assert_eq!(state.project(), &before, "nothing is changed");
        assert_eq!(state.status(), &Status::Info(Message::FontTargetChanged));
        assert!(tasks.is_idle());
    }
}
