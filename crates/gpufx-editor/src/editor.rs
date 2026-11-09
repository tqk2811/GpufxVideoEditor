//! Editor state without a window: the project history, the selection and the status line. The UI
//! calls these methods and draws the result; every timeline change goes through an engine command.

use crate::export::{EncoderOption, ExportProgress};
use crate::i18n::{ClipAction, Language, Message, StreamKind};
use crate::media::{choose_track, item_from_probe, MediaItem, MediaLibrary, TrackChoice};
use crate::packages::{Catalog, PickerState};
use crate::playback::plan::{start_time, TransportCommand};
use crate::timeline::drag::{clamp_trim, ClipDrag, EditCommand};
use crate::timeline::view::TimelineView;
use engine::{
    add_clip, add_effect, add_track, add_transition, audio_sample_count, clip_at, delete_clip, nearest_free_start, remove_effect, remove_transition, set_effect_params, set_fades, set_gain, set_muted,
    set_transition_params, split_clip, trim_end, trim_start, AudioOutput, Clip, ClipKey, EditError, EditHistory, Effect, Output, Project, Time, TrackKind, Transition, SUPPORTED_VERSION,
};
use d3d12_backend::MediaProbe;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

const UNDO_DEPTH: usize = 200;

/// Length given to a clip whose file states none, and to a clip added without a probe.
pub const DEFAULT_CLIP_DURATION: Time = Time::from_ticks(900_000);
/// Length given to a new transition, until the inspector changes it.
pub const DEFAULT_TRANSITION_DURATION: Time = Time::from_ticks(90_000);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Idle,
    Info(Message),
    Refused(Message),
    Error(Message),
}

pub struct EditorState {
    history: EditHistory,
    selection: Option<ClipKey>,
    playhead: Time,
    status: Status,
    /// Counts the changes of the project; equal values mean an equal project.
    revision: u64,
    /// The revision of the last change audio must hear.
    audio_revision: u64,
    /// The file the project was last saved to or opened from.
    project_file: Option<PathBuf>,
    /// The revision that is on disk.
    saved_revision: u64,
    preview_error: Option<String>,
    playing: bool,
    transport: Vec<TransportCommand>,
    /// Where a held ruler press last sought to, so holding it does not seek every frame.
    scrub_target: Option<Time>,
    playback_message: Option<String>,
    silence_ms: u64,
    /// Why playback runs without sound; stays until a device opens.
    no_sound: Option<String>,
    pub view: TimelineView,
    pub drag: Option<ClipDrag>,
    /// The name of the file a background load is reading.
    loading: Option<String>,
    pub catalog: Catalog,
    pub picker: PickerState,
    pub encoders: Vec<EncoderOption>,
    pub encoder: usize,
    /// The running export, which turns editing, playback and scrubbing off.
    export: Option<ExportProgress>,
    /// A control being dragged; see [`LiveEdit`].
    live: Option<LiveEdit>,
    /// Escape or a lost window focus dropped a held control: it keeps reporting the pointer, so
    /// its changes are ignored until the button is released.
    hold_cancelled: bool,
    /// What this session imported and what the open project uses.
    pub library: MediaLibrary,
    pub ui: UiState,
    language: Language,
}

/// One held control: the project shows `shown` while `history` stays where it was, and the release
/// makes it one undo step.
struct LiveEdit {
    shown: Project,
    /// Whether the project was clean when the drag began.
    was_clean: bool,
}

/// The tab of the left panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LeftTab {
    #[default]
    Media,
    Audio,
    Effects,
}

/// What the panels remember between frames.
#[derive(Debug, Clone, Default)]
pub struct UiState {
    pub left_tab: LeftTab,
    pub media_search: String,
    pub audio_search: String,
    pub fullscreen: bool,
    /// A time field being dragged; the clip changes on release.
    pub clip_field: Option<ClipField>,
    /// The clip the inspector showed last, to notice when that changes.
    pub inspector_clip: Option<(ClipKey, String, i64)>,
}

/// What may be chosen in the inspector's time fields. Waits for the release of a drag, as a clip
/// edit renumbers clips.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipField {
    Start(f64),
    Duration(f64),
}

/// A clip of the whole default length at `start`, with no effects.
pub fn new_clip(source: &str, start: Time, duration: Time) -> Clip {
    Clip {
        source: source.to_owned(),
        start,
        source_in: Time::ZERO,
        duration,
        effects: Vec::new(),
        gain_db: 0.0,
        muted: false,
        fade_in: Time::ZERO,
        fade_out: Time::ZERO,
    }
}

fn default_project() -> Project {
    Project {
        version: SUPPORTED_VERSION,
        output: Output {
            width: 1920,
            height: 1080,
            frame_duration: Time::from_ticks(3000),
            audio: Some(AudioOutput { sample_rate: 48_000, channels: 2, bitrate: 192_000 }),
        },
        tracks: Vec::new(),
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new(default_project())
    }
}

impl EditorState {
    pub fn new(project: Project) -> Self {
        Self {
            history: EditHistory::new(project, UNDO_DEPTH),
            selection: None,
            playhead: Time::ZERO,
            status: Status::Idle,
            revision: 0,
            audio_revision: 0,
            project_file: None,
            saved_revision: 0,
            preview_error: None,
            playing: false,
            transport: Vec::new(),
            scrub_target: None,
            playback_message: None,
            silence_ms: 0,
            no_sound: None,
            view: TimelineView::default(),
            drag: None,
            loading: None,
            catalog: Catalog::default(),
            picker: PickerState::default(),
            encoders: Vec::new(),
            encoder: 0,
            export: None,
            live: None,
            hold_cancelled: false,
            library: MediaLibrary::default(),
            ui: UiState::default(),
            language: Language::default(),
        }
    }

    /// The project as shown: during a live edit, its preview.
    pub fn project(&self) -> &Project {
        self.live.as_ref().map_or_else(|| self.history.current(), |live| &live.shown)
    }

    pub fn selection(&self) -> Option<ClipKey> {
        self.selection
    }

    pub fn playhead(&self) -> Time {
        self.playhead
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn set_language(&mut self, language: Language) {
        self.language = language;
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn preview_error(&self) -> Option<&str> {
        self.preview_error.as_deref()
    }

    pub fn project_file(&self) -> Option<&Path> {
        self.project_file.as_deref()
    }

    /// Whether the project changed since it was last saved or opened.
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    /// Records a successful save to `file`.
    pub fn mark_saved(&mut self, file: PathBuf) {
        self.status = Status::Info(Message::Saved(file.display().to_string()));
        self.project_file = Some(file);
        self.saved_revision = self.revision;
    }

    pub fn report_error(&mut self, message: Message) {
        self.status = Status::Error(message);
    }

    pub fn report_info(&mut self, message: Message) {
        self.status = Status::Info(message);
    }

    pub fn report_refused(&mut self, message: Message) {
        self.status = Status::Refused(message);
    }

    pub fn loading(&self) -> Option<&str> {
        self.loading.as_deref()
    }

    pub fn set_loading(&mut self, name: Option<String>) {
        self.loading = name;
    }

    /// Replaces the project with a new or opened one: a fresh history, and a new revision so preview
    /// and audio rebuild. The new project counts as saved.
    pub fn replace_project(&mut self, project: Project, file: Option<PathBuf>) {
        if self.playing {
            self.transport.push(TransportCommand::Stop);
        }
        self.live = None;
        self.ui.clip_field = None;
        self.library.adopt_project_media(&project);
        self.history = EditHistory::new(project, UNDO_DEPTH);
        self.revision += 1;
        self.audio_revision = self.revision;
        self.saved_revision = self.revision;
        self.status = match &file {
            Some(file) => Status::Info(Message::Opened(file.display().to_string())),
            None => Status::Idle,
        };
        self.project_file = file;
        self.selection = None;
        self.playhead = Time::ZERO;
        self.playing = false;
        self.scrub_target = None;
        self.preview_error = None;
        self.playback_message = None;
        self.silence_ms = 0;
        self.no_sound = None;
        self.view = TimelineView::default();
        self.drag = None;
    }

    /// An empty project, replacing the current one.
    pub fn new_project(&mut self) {
        self.replace_project(default_project(), None);
    }

    pub fn is_exporting(&self) -> bool {
        self.export.is_some()
    }

    pub fn export_progress(&self) -> Option<ExportProgress> {
        self.export
    }

    /// Stops playback and turns editing off until [`Self::end_export`].
    pub fn begin_export(&mut self, total: i64) {
        if self.playing {
            self.transport.push(TransportCommand::Stop);
        }
        self.playing = false;
        self.scrub_target = None;
        self.drag = None;
        self.cancel_live();
        self.ui.clip_field = None;
        self.export = Some(ExportProgress { done: 0, total });
        self.status = Status::Info(Message::Exporting);
    }

    pub fn set_export_progress(&mut self, progress: ExportProgress) {
        self.export = Some(progress);
    }

    pub fn end_export(&mut self, status: Status) {
        self.export = None;
        self.status = status;
    }

    pub fn set_preview_error(&mut self, error: Option<String>) {
        self.preview_error = error;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn playback_message(&self) -> Option<&str> {
        self.playback_message.as_deref()
    }

    pub fn no_sound(&self) -> Option<&str> {
        self.no_sound.as_deref()
    }

    pub fn set_no_sound(&mut self, reason: Option<String>) {
        self.no_sound = reason;
    }

    pub fn silence_ms(&self) -> u64 {
        self.silence_ms
    }

    pub fn set_silence_ms(&mut self, silence_ms: u64) {
        self.silence_ms = silence_ms;
    }

    /// The audio thread was replaced while the state still says playing: plays on from the playhead.
    pub fn reopen_playback(&mut self) {
        if !self.playing {
            return;
        }
        let from = start_time(self.playhead, audio_sample_count(self.project()));
        self.playhead = from;
        self.transport.push(TransportCommand::Play { from });
    }

    /// Space: starts from the playhead or stops where it is. A timeline with no samples cannot play.
    pub fn toggle_playback(&mut self) {
        if self.is_exporting() {
            self.status = Status::Info(Message::ExportRunning);
            return;
        }
        if self.playing {
            self.playing = false;
            self.transport.push(TransportCommand::Stop);
            return;
        }
        let end = audio_sample_count(self.project());
        if end <= 0 {
            self.status = Status::Info(Message::NothingToPlay);
            return;
        }
        let from = start_time(self.playhead, end);
        self.playhead = from;
        self.playing = true;
        self.playback_message = None;
        self.transport.push(TransportCommand::Play { from });
    }

    /// The commands of this frame, for the window to carry out.
    pub fn take_transport(&mut self) -> Vec<TransportCommand> {
        std::mem::take(&mut self.transport)
    }

    /// A ruler press at `time`. Moves the playhead and, while playing, seeks once per new place.
    pub fn scrub_to(&mut self, time: Time) {
        if self.is_exporting() {
            return;
        }
        self.set_playhead(time);
        let time = self.playhead;
        if self.playing && self.scrub_target != Some(time) {
            self.transport.push(TransportCommand::Seek(time));
        }
        self.scrub_target = Some(time);
    }

    pub fn end_scrub(&mut self) {
        self.scrub_target = None;
    }

    /// The playhead while playing, from the audio clock.
    pub fn follow_clock(&mut self, time: Time) {
        if self.playing {
            self.set_playhead(time);
        }
    }

    /// Playback ended by itself: at the end, or with a message for the status line.
    pub fn playback_ended(&mut self, message: Option<String>) {
        self.playing = false;
        self.scrub_target = None;
        if message.is_some() {
            self.playback_message = message;
        }
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn set_playhead(&mut self, time: Time) {
        self.playhead = time.max(Time::ZERO);
    }

    pub fn select(&mut self, selection: Option<ClipKey>) {
        self.selection = selection;
    }

    fn refuse(&mut self, error: &EditError) {
        self.status = Status::Refused(Message::Technical(error.to_string()));
    }

    /// An export renders the project it started with, so edits wait for it. A drag holds a clip key,
    /// and any edit could make that key name another clip, so edits wait for the drop; the same
    /// goes for a held inspector control. True, with a status, while any is in progress.
    fn edits_blocked(&mut self) -> bool {
        if self.export.is_some() {
            self.status = Status::Info(Message::ExportRunning);
            return true;
        }
        if !self.is_holding() {
            return false;
        }
        self.status = Status::Info(Message::FinishDragFirst);
        true
    }

    /// Runs a command. A refusal goes to the status line and the result is false.
    fn run<F>(&mut self, command: F) -> bool
    where
        F: FnOnce(&Project) -> Result<Project, EditError>,
    {
        if self.edits_blocked() {
            return false;
        }
        let before = self.project().clone();
        match self.history.apply(command) {
            Ok(after) => {
                if *after != before {
                    self.revision += 1;
                    self.audio_revision = self.revision;
                }
                self.status = Status::Idle;
                true
            }
            Err(error) => {
                self.refuse(&error);
                false
            }
        }
    }

    pub fn add_track(&mut self, kind: TrackKind) {
        self.run(|project| add_track(project, kind));
    }

    /// Adds a clip of `duration` for `path` to `track`, at the free position nearest the playhead
    /// and selected. False, with a status, when it could not be added.
    fn place_clip(&mut self, track: usize, path: &Path, duration: Time) -> bool {
        if self.edits_blocked() {
            return false;
        }
        let Some(target) = self.project().tracks.get(track) else {
            self.refuse(&EditError::TrackNotFound { track });
            return false;
        };
        let Some(start) = nearest_free_start(target, None, duration, self.playhead) else {
            self.refuse(&EditError::NoRoom { track });
            return false;
        };
        let clip = new_clip(&path.to_string_lossy(), start, duration);
        if !self.run(|project| add_clip(project, track, clip)) {
            return false;
        }
        self.library.adopt_project_media(self.history.current());
        self.selection = clip_at(self.project(), track, start);
        true
    }

    /// Adds a clip of [`DEFAULT_CLIP_DURATION`] for `path` to `track`.
    pub fn add_clip_from_file(&mut self, track: usize, path: &Path) {
        if self.place_clip(track, path, DEFAULT_CLIP_DURATION) {
            self.status = Status::Info(Message::Added(clip_label(&path.to_string_lossy()).to_owned()));
        }
    }

    /// Adds a file a background probe has read to `track`, as long as the stream it runs on. A file
    /// lacking the stream the track plays is refused: on an audio track a project holding one would
    /// not validate (`SourceHasNoAudio`), and a video track would have nothing to show. The status
    /// says why the stream is unusable when the probe knows.
    pub fn add_probed_file(&mut self, track: usize, path: &Path, probe: &MediaProbe) {
        let name = clip_label(&path.to_string_lossy()).to_owned();
        let Some(kind) = self.project().tracks.get(track).map(|target| target.kind) else {
            self.refuse(&EditError::TrackNotFound { track });
            return;
        };
        let (present, stated, needed, problem) = match kind {
            TrackKind::Video => (probe.has_video, probe.video_duration, StreamKind::Video, probe.video_problem.as_deref()),
            TrackKind::Audio => (probe.has_audio, probe.audio_duration, StreamKind::Audio, probe.audio_problem.as_deref()),
        };
        if !present {
            self.status = Status::Refused(match problem {
                Some(reason) => Message::StreamUnusable { name, stream: needed, reason: reason.to_owned() },
                None => Message::StreamMissing { name, stream: needed },
            });
            return;
        }
        if let Some(item) = item_from_probe(&path.to_string_lossy(), probe) {
            self.library.add(item);
        }
        let length = stated.filter(|length| *length > Time::ZERO);
        if self.place_clip(track, path, length.unwrap_or(DEFAULT_CLIP_DURATION)) {
            self.status = Status::Info(match length {
                Some(length) => Message::AddedWithLength { name, seconds: format!("{:.2}", length.ticks() as f64 / engine::TICKS_PER_SECOND as f64) },
                None => Message::AddedNoLength { name, seconds: DEFAULT_CLIP_DURATION.ticks() / engine::TICKS_PER_SECOND },
            });
        }
    }

    /// Whether a drag of an inspector control is previewing.
    pub fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// Whether a timeline drag, a live edit or a time field drag is in progress.
    pub fn is_holding(&self) -> bool {
        self.drag.is_some() || self.live.is_some() || self.ui.clip_field.is_some()
    }

    /// Refuses, with a status, what must not see the preview of a live edit as the project: saving
    /// it or starting from its revision. True while one is shown.
    pub fn refuse_while_live(&mut self) -> bool {
        if self.live.is_none() {
            return false;
        }
        self.status = Status::Info(Message::FinishDragFirst);
        true
    }

    /// Whether a held control was cancelled and the button is still down.
    pub fn hold_cancelled(&self) -> bool {
        self.hold_cancelled
    }

    /// No pointer button is down any more: ends what a hold left behind.
    pub fn pointer_released(&mut self) {
        self.commit_live();
        self.ui.clip_field = None;
        self.hold_cancelled = false;
        if self.shows_hold_refusal() {
            self.status = Status::Idle;
        }
    }

    /// Whether the status says a request has to wait for the hold, which only the release ends.
    fn shows_hold_refusal(&self) -> bool {
        self.status == Status::Info(Message::FinishDragFirst)
    }

    /// The revision audio has to follow: live previews do not count, the release does.
    pub fn audio_revision(&self) -> u64 {
        self.audio_revision
    }

    /// Shows the result of `command` on the project before the drag, without touching the history.
    /// A refused step goes to the status line and the last good one stays shown.
    pub fn live_edit<F>(&mut self, command: F)
    where
        F: FnOnce(&Project) -> Result<Project, EditError>,
    {
        if self.hold_cancelled {
            return;
        }
        if self.live.is_none() && (self.export.is_some() || self.drag.is_some()) {
            self.edits_blocked();
            return;
        }
        match command(self.history.current()) {
            Ok(shown) => {
                let changed = shown != *self.project();
                let was_clean = self.live.as_ref().map_or(!self.is_dirty(), |live| live.was_clean);
                self.live = Some(LiveEdit { shown, was_clean });
                if changed {
                    self.revision += 1;
                }
                if !self.shows_hold_refusal() {
                    self.status = Status::Idle;
                }
            }
            Err(error) => self.refuse(&error),
        }
    }

    /// Ends the live edit, leaving at most one undo step.
    pub fn commit_live(&mut self) {
        let Some(live) = self.live.take() else { return };
        let unchanged = live.shown == *self.history.current();
        let shown = live.shown;
        let applied = self.history.apply(move |_| Ok(shown)).is_ok();
        debug_assert!(applied, "a live edit is already validated");
        self.revision += 1;
        self.audio_revision = self.revision;
        if unchanged && live.was_clean {
            self.saved_revision = self.revision;
        }
    }

    /// Ends the live edit and shows the project as it was before the drag.
    pub fn cancel_live(&mut self) {
        let Some(live) = self.live.take() else { return };
        self.revision += 1;
        // Playback opened during the drag holds the preview, so audio has to follow this too.
        self.audio_revision = self.revision;
        if live.was_clean {
            self.saved_revision = self.revision;
        }
    }

    /// Issues the command of a finished drag and selects the clip where it landed.
    pub fn finish_drag(&mut self, command: EditCommand) {
        self.drag = None;
        let landing = command.landing(self.project());
        if self.run(|project| command.apply(project)) {
            self.selection = landing.and_then(|span| clip_at(self.project(), span.track, span.start));
        }
    }

    /// The Start or Duration field of the selected clip, once its value is final.
    pub fn apply_clip_field(&mut self, field: ClipField) {
        let Some(key) = self.selected_for(ClipAction::Change) else { return };
        let Some(clip) = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)) else { return };
        let command = match field {
            ClipField::Start(seconds) => EditCommand::Move { key, track: key.track, desired_start: crate::timecode::from_seconds(seconds) },
            ClipField::Duration(seconds) => EditCommand::TrimEnd { key, new_end: clip.start.saturating_add(crate::timecode::from_seconds(seconds)) },
        };
        let command = clamp_trim(self.project(), command);
        self.finish_drag(command);
    }

    pub fn set_muted(&mut self, muted: bool) {
        let Some(key) = self.selected_for(ClipAction::Mute) else { return };
        self.run(|project| set_muted(project, key, muted));
    }

    pub fn live_gain(&mut self, gain_db: f64) {
        let Some(key) = self.selection else { return };
        self.live_edit(|project| set_gain(project, key, gain_db));
    }

    pub fn live_fades(&mut self, fade_in: Time, fade_out: Time) {
        let Some(key) = self.selection else { return };
        self.live_edit(|project| set_fades(project, key, fade_in, fade_out));
    }

    pub fn live_effect_params(&mut self, effect: usize, params: Map<String, Value>) {
        let Some(key) = self.selection else { return };
        self.live_edit(|project| set_effect_params(project, key, effect, params));
    }

    pub fn live_transition(&mut self, track: usize, after_clip: usize, params: Map<String, Value>, duration: Time) {
        self.live_edit(|project| set_transition_params(project, track, after_clip, params, duration));
    }

    /// Trims the selected clip so it starts at the playhead.
    pub fn trim_start_to_playhead(&mut self) {
        let Some(key) = self.selected_for(ClipAction::Trim) else { return };
        let at = self.playhead;
        if !self.playhead_inside(key) {
            self.status = Status::Info(Message::NothingAtPlayhead);
            return;
        }
        if self.run(|project| trim_start(project, key, at)) {
            self.selection = clip_at(self.project(), key.track, at);
        }
    }

    /// Trims the selected clip so it ends at the playhead.
    pub fn trim_end_to_playhead(&mut self) {
        let Some(key) = self.selected_for(ClipAction::Trim) else { return };
        let at = self.playhead;
        if !self.playhead_inside(key) {
            self.status = Status::Info(Message::NothingAtPlayhead);
            return;
        }
        self.run(|project| trim_end(project, key, at));
    }

    fn playhead_inside(&self, key: ClipKey) -> bool {
        let at = self.playhead;
        self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)).is_some_and(|clip| at > clip.start && clip.end().is_some_and(|end| at < end))
    }

    /// Adds `item` at the playhead, on a track with room or a new one, as one undo step.
    pub fn add_media_at_playhead(&mut self, item: &MediaItem) {
        if self.edits_blocked() {
            return;
        }
        let duration = item.duration.unwrap_or(DEFAULT_CLIP_DURATION);
        let start = self.playhead;
        let track_kind = item.kind.track_kind();
        let choice = choose_track(self.project(), track_kind, duration, start);
        let track = match choice {
            TrackChoice::Existing(index) => index,
            TrackChoice::New => self.project().tracks.len(),
        };
        let clip = new_clip(&item.source, start, duration);
        let added = self.run(|project| {
            let with_track = if choice == TrackChoice::New { add_track(project, track_kind)? } else { project.clone() };
            add_clip(&with_track, track, clip)
        });
        if added {
            self.selection = clip_at(self.project(), track, start);
            self.status = Status::Info(Message::Added(item.name.clone()));
        }
    }

    /// Records a probed file, whether or not a clip is made from it.
    pub fn import_probed(&mut self, path: &Path, probe: &MediaProbe) {
        match item_from_probe(&path.to_string_lossy(), probe) {
            Some(item) => {
                self.status = Status::Info(Message::Imported(item.name.clone()));
                self.library.add(item);
            }
            None => self.status = Status::Refused(Message::NoUsableStream(clip_label(&path.to_string_lossy()).to_owned())),
        }
    }

    /// Splits the selected clip at the playhead and selects the right half.
    pub fn split_selected(&mut self) {
        let Some(key) = self.selection else {
            self.status = Status::Info(Message::SelectClip(ClipAction::Split));
            return;
        };
        let at = self.playhead;
        if self.run(|project| split_clip(project, key, at)) {
            self.selection = Some(ClipKey { track: key.track, clip: key.clip + 1 });
        }
    }

    /// The selected clip, or a status asking for one.
    fn selected_for(&mut self, action: ClipAction) -> Option<ClipKey> {
        if self.selection.is_none() {
            self.status = Status::Info(Message::SelectClip(action));
        }
        self.selection
    }

    /// Adds effect `package` to the selected clip with its default parameters.
    pub fn add_effect_package(&mut self, package: &str) {
        let Some(key) = self.selected_for(ClipAction::AddEffect) else { return };
        let effect = Effect { package: package.to_owned(), params: Map::new() };
        self.run(|project| add_effect(project, key, effect));
    }

    pub fn remove_effect_at(&mut self, effect: usize) {
        let Some(key) = self.selected_for(ClipAction::RemoveEffect) else { return };
        self.run(|project| remove_effect(project, key, effect));
    }

    /// Adds transition `package` where the selected clip touches a neighbour on its track.
    pub fn add_transition_package(&mut self, package: &str) {
        let Some(key) = self.selected_for(ClipAction::AddTransition) else { return };
        let Some(after_clip) = touching_pair(self.project(), key) else {
            self.status = Status::Refused(Message::NoNeighbour);
            return;
        };
        let transition = Transition { after_clip, package: package.to_owned(), duration: DEFAULT_TRANSITION_DURATION, params: Map::new() };
        self.run(|project| add_transition(project, key.track, transition));
    }

    pub fn remove_transition_after(&mut self, track: usize, after_clip: usize) {
        self.run(|project| remove_transition(project, track, after_clip));
    }

    pub fn delete_selected(&mut self) {
        let Some(key) = self.selection else {
            self.status = Status::Info(Message::SelectClip(ClipAction::Delete));
            return;
        };
        if self.run(|project| delete_clip(project, key)) {
            self.selection = None;
        }
    }

    /// Drops the drag in progress without any edit.
    pub fn cancel_drag(&mut self) {
        self.hold_cancelled |= self.live.is_some() || self.ui.clip_field.is_some();
        self.cancel_live();
        self.ui.clip_field = None;
        if self.drag.take().is_some() {
            self.status = Status::Idle;
        }
    }

    pub fn undo(&mut self) {
        if self.edits_blocked() {
            return;
        }
        if self.history.undo() {
            self.after_history_step();
        } else {
            self.status = Status::Info(Message::NothingToUndo);
        }
    }

    pub fn redo(&mut self) {
        if self.edits_blocked() {
            return;
        }
        if self.history.redo() {
            self.after_history_step();
        } else {
            self.status = Status::Info(Message::NothingToRedo);
        }
    }

    /// Clip keys name clips of one project, so the selection does not survive a step back or
    /// forward. There is no drag to drop here, because both steps wait for it.
    fn after_history_step(&mut self) {
        self.revision += 1;
        self.audio_revision = self.revision;
        self.selection = None;
        self.status = Status::Idle;
    }
}

/// The first clip of a touching pair that includes `key`, preferring a pair with no transition yet.
pub fn touching_pair(project: &Project, key: ClipKey) -> Option<usize> {
    let track = project.tracks.get(key.track)?;
    let touches = |first: usize| track.clips.get(first + 1).is_some_and(|next| track.clips[first].end() == Some(next.start));
    let pairs: Vec<usize> = [Some(key.clip), key.clip.checked_sub(1)].into_iter().flatten().filter(|&first| touches(first)).collect();
    let free = |first: &&usize| !track.transitions.iter().any(|transition| transition.after_clip == **first);
    pairs.iter().find(free).or(pairs.first()).copied()
}

/// How long the transition after clip `after_clip` of `track` may be: at least a frame, at most
/// twice the shorter usable side. `None` when there is no such transition or room.
pub fn transition_duration_limits(project: &Project, track: usize, after_clip: usize) -> Option<(Time, Time)> {
    let target = project.tracks.get(track)?;
    let (first, second) = (target.clips.get(after_clip)?, target.clips.get(after_clip + 1)?);
    let head_used = |clip_index: usize| target.transitions.iter().find(|tr| tr.after_clip + 1 == clip_index).map_or(Time::ZERO, |tr| tr.duration.saturating_sub(tr.duration.half_floor()));
    let tail_used = |clip_index: usize| target.transitions.iter().find(|tr| tr.after_clip == clip_index).map_or(Time::ZERO, |tr| tr.duration.half_floor());
    let usable_first = first.duration.saturating_sub(head_used(after_clip));
    let usable_second = second.duration.saturating_sub(tail_used(after_clip + 1));
    let longest = Time::from_ticks(usable_first.min(usable_second).ticks().saturating_mul(2));
    let shortest = project.output.frame_duration;
    (longest >= shortest).then_some((shortest, longest))
}

/// What sits on a clip: its effects by position and the transitions on either side, by the index of
/// the clip before them.
pub fn applied_to(project: &Project, key: ClipKey) -> (Vec<String>, Vec<(usize, String)>) {
    let Some(track) = project.tracks.get(key.track) else { return (Vec::new(), Vec::new()) };
    let effects = track.clips.get(key.clip).map_or_else(Vec::new, |clip| clip.effects.iter().map(|effect| effect.package.clone()).collect());
    let transitions = track.transitions.iter().filter(|tr| tr.after_clip == key.clip || tr.after_clip.checked_add(1) == Some(key.clip)).map(|tr| (tr.after_clip, tr.package.clone())).collect();
    (effects, transitions)
}

/// The file name of a clip source, for its label.
pub fn clip_label(source: &str) -> &str {
    Path::new(source).file_name().and_then(|name| name.to_str()).unwrap_or(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::drag::{ClipDrag, DragKind, EditCommand};
    use engine::{Project, TICKS_PER_SECOND};

    fn seconds(value: i64) -> Time {
        Time::from_seconds(value).unwrap()
    }

    fn state_with_clip() -> EditorState {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        state.add_track(TrackKind::Audio);
        state.add_clip_from_file(0, Path::new("C:/media/a.mp4"));
        state
    }

    fn key(clip: usize) -> ClipKey {
        ClipKey { track: 0, clip }
    }

    fn clips(project: &Project, track: usize) -> Vec<(Time, Time)> {
        project.tracks[track].clips.iter().map(|clip| (clip.start, clip.duration)).collect()
    }

    #[test]
    fn a_new_clip_is_selected_and_uses_the_default_length() {
        let state = state_with_clip();
        assert_eq!(state.selection(), Some(key(0)));
        assert_eq!(clips(state.project(), 0), vec![(Time::ZERO, DEFAULT_CLIP_DURATION)]);
        assert_eq!(state.status(), &Status::Info(Message::Added("a.mp4".to_owned())));
    }

    #[test]
    fn a_second_clip_lands_after_the_first() {
        let mut state = state_with_clip();
        state.add_clip_from_file(0, Path::new("b.mp4"));
        assert_eq!(clips(state.project(), 0)[1].0, DEFAULT_CLIP_DURATION);
        assert_eq!(state.selection(), Some(key(1)));
    }

    fn video_probe(video: Option<i64>, audio: Option<i64>) -> MediaProbe {
        MediaProbe {
            has_video: video.is_some(),
            has_audio: audio.is_some(),
            video_duration: video.map(seconds),
            audio_duration: audio.map(seconds),
            video_problem: None,
            audio_problem: None,
        }
    }

    #[test]
    fn a_file_without_sound_is_refused_on_an_audio_track_only() {
        let mut state = state_with_clip();
        let before = state.project().clone();
        state.add_probed_file(1, Path::new("silent.mp4"), &video_probe(Some(3), None));
        assert_eq!(state.project(), &before);
        assert_eq!(state.status(), &Status::Refused(Message::StreamMissing { name: "silent.mp4".to_owned(), stream: StreamKind::Audio }));
        state.add_probed_file(1, Path::new("sound.wav"), &video_probe(None, Some(3)));
        assert_eq!(state.project().tracks[1].clips.len(), 1);
        state.add_probed_file(0, Path::new("silent.mp4"), &video_probe(Some(3), None));
        assert_eq!(state.project().tracks[0].clips.len(), 2, "a video track does not need sound");
    }

    #[test]
    fn an_audio_only_file_is_refused_on_a_video_track() {
        let mut state = state_with_clip();
        let before = state.project().clone();
        state.add_probed_file(0, Path::new("music.mp3"), &video_probe(None, Some(4)));
        assert_eq!(state.project(), &before);
        assert_eq!(state.status(), &Status::Refused(Message::StreamMissing { name: "music.mp3".to_owned(), stream: StreamKind::Video }));
    }

    #[test]
    fn an_unusable_stream_is_refused_with_its_real_reason() {
        let mut state = state_with_clip();
        let probe = MediaProbe { video_problem: Some("no decoder for its video codec hevc".to_owned()), ..video_probe(None, Some(4)) };
        state.add_probed_file(0, Path::new("movie.mkv"), &probe);
        assert_eq!(state.status(), &Status::Refused(Message::StreamUnusable { name: "movie.mkv".to_owned(), stream: StreamKind::Video, reason: "no decoder for its video codec hevc".to_owned() }));
    }

    #[test]
    fn a_probed_clip_takes_the_length_of_the_stream_its_track_plays() {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        state.add_track(TrackKind::Audio);
        state.add_probed_file(0, Path::new("a.mp4"), &video_probe(Some(5), Some(8)));
        assert_eq!(state.project().tracks[0].clips[0].duration, seconds(5));
        assert_eq!(state.status(), &Status::Info(Message::AddedWithLength { name: "a.mp4".to_owned(), seconds: "5.00".to_owned() }));
        state.add_probed_file(1, Path::new("a.mp4"), &video_probe(Some(5), Some(8)));
        assert_eq!(state.project().tracks[1].clips[0].duration, seconds(8));
    }

    #[test]
    fn a_probed_clip_with_no_stated_length_gets_the_default_and_says_so() {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        let unknown = MediaProbe { has_video: true, has_audio: false, video_duration: None, audio_duration: None, video_problem: None, audio_problem: None };
        state.add_probed_file(0, Path::new("raw.h264"), &unknown);
        assert_eq!(state.project().tracks[0].clips[0].duration, DEFAULT_CLIP_DURATION);
        assert!(matches!(state.status(), Status::Info(Message::AddedNoLength { .. })), "{:?}", state.status());
    }

    #[test]
    fn a_missing_track_is_reported_not_panicked() {
        let mut state = EditorState::default();
        state.add_clip_from_file(7, Path::new("a.mp4"));
        assert!(matches!(state.status(), Status::Refused(_)));
    }

    #[test]
    fn split_selects_the_right_half_even_when_the_cut_moves() {
        let mut moved = 0;
        for offset in 1000..1040 {
            let project = {
                let mut base = default_project();
                base.tracks.push(engine::Track { kind: TrackKind::Video, clips: Vec::new(), transitions: Vec::new() });
                let mut clip = new_clip("a.mp4", Time::ZERO, DEFAULT_CLIP_DURATION);
                clip.source_in = Time::from_ticks(1);
                base.tracks[0].clips.push(clip);
                base
            };
            let mut state = EditorState::new(project);
            state.select(Some(key(0)));
            let at = Time::from_ticks(offset);
            state.set_playhead(at);
            state.split_selected();

            assert_eq!(state.selection(), Some(key(1)), "cut at {at}");
            let parts = clips(state.project(), 0);
            assert_eq!(parts.len(), 2);
            assert_eq!(parts[1].0.ticks() + parts[1].1.ticks(), DEFAULT_CLIP_DURATION.ticks());
            assert!((parts[1].0.ticks() - offset).abs() <= 14);
            if parts[1].0 != at {
                moved += 1;
            }
        }
        assert!(moved > 0, "no cut moved, so the test would not catch a lookup by time");
    }

    #[test]
    fn split_outside_the_clip_is_refused_with_a_status() {
        let mut state = state_with_clip();
        state.set_playhead(seconds(50));
        let before = state.project().clone();
        state.split_selected();
        assert!(matches!(state.status(), Status::Refused(_)));
        assert_eq!(state.project(), &before);
    }

    #[test]
    fn split_and_delete_without_selection_explain_themselves() {
        let mut state = EditorState::default();
        state.split_selected();
        assert!(matches!(state.status(), Status::Info(_)));
        state.delete_selected();
        assert!(matches!(state.status(), Status::Info(_)));
    }

    #[test]
    fn delete_clears_the_selection_and_undo_restores_the_clip() {
        let mut state = state_with_clip();
        let with_clip = state.project().clone();
        state.delete_selected();
        assert_eq!(state.selection(), None);
        assert!(state.project().tracks[0].clips.is_empty());
        state.undo();
        assert_eq!(state.project(), &with_clip);
        state.redo();
        assert!(state.project().tracks[0].clips.is_empty());
    }

    #[test]
    fn undo_and_redo_drop_stale_selection() {
        let mut state = state_with_clip();
        state.undo();
        assert_eq!(state.selection(), None);
        state.select(Some(key(0)));
        state.redo();
        assert_eq!(state.selection(), None);
    }

    #[test]
    fn undo_with_an_empty_history_reports_it() {
        let mut state = EditorState::default();
        state.undo();
        assert_eq!(state.status(), &Status::Info(Message::NothingToUndo));
        state.redo();
        assert_eq!(state.status(), &Status::Info(Message::NothingToRedo));
    }

    #[test]
    fn a_drop_on_a_taken_place_lands_next_to_it_and_stays_selected() {
        let mut state = state_with_clip();
        state.set_playhead(seconds(30));
        state.add_clip_from_file(0, Path::new("b.mp4"));
        // The clip at 30 s asked to sit at 5 s: the free spot next to the first clip is 10 s.
        state.finish_drag(EditCommand::Move { key: key(1), track: 0, desired_start: seconds(5) });
        assert_eq!(state.status(), &Status::Idle);
        assert_eq!(clips(state.project(), 0)[1].0, seconds(10));
        assert_eq!(state.selection(), Some(key(1)));
        assert_eq!(state.drag, None);
    }

    #[test]
    fn a_drop_on_another_kind_of_track_is_refused_with_a_status() {
        let mut state = state_with_clip();
        let before = state.project().clone();
        state.finish_drag(EditCommand::Move { key: key(0), track: 1, desired_start: Time::ZERO });
        assert!(matches!(state.status(), Status::Refused(_)));
        assert_eq!(state.project(), &before);
    }

    #[test]
    fn a_drop_on_the_same_place_adds_no_undo_step() {
        let mut state = state_with_clip();
        state.add_clip_from_file(0, Path::new("b.mp4"));
        state.undo();
        assert!(state.can_redo(), "the redo step must be alive before the drop");
        state.select(Some(key(0)));
        state.finish_drag(EditCommand::Move { key: key(0), track: 0, desired_start: Time::ZERO });
        assert!(state.can_redo(), "a drop that changes nothing must not clear the redo stack");
        assert_eq!(state.selection(), Some(key(0)));
    }

    fn state_with_two_clips_and_a_drag_on_the_first() -> EditorState {
        let mut state = state_with_clip();
        state.set_playhead(seconds(30));
        state.add_clip_from_file(0, Path::new("b.mp4"));
        state.select(Some(key(0)));
        state.drag = Some(ClipDrag { key: key(0), kind: DragKind::Move, grab_offset: Time::ZERO });
        state
    }

    #[test]
    fn editing_during_a_drag_changes_nothing_and_says_why() {
        let mut state = state_with_two_clips_and_a_drag_on_the_first();
        let before = state.project().clone();
        let drag = state.drag;
        state.delete_selected();
        assert_eq!(state.status(), &Status::Info(Message::FinishDragFirst));
        state.set_playhead(seconds(5));
        state.split_selected();
        state.add_track(TrackKind::Video);
        state.add_clip_from_file(0, Path::new("c.mp4"));
        state.undo();
        state.redo();
        assert_eq!(state.project(), &before);
        assert_eq!(state.drag, drag);
        assert_eq!(state.selection(), Some(key(0)));
        assert_eq!(state.status(), &Status::Info(Message::FinishDragFirst));
    }

    #[test]
    fn undo_and_redo_do_not_cancel_a_drag() {
        let mut state = state_with_two_clips_and_a_drag_on_the_first();
        let drag = state.drag;
        state.undo();
        assert_eq!(state.drag, drag);
        state.redo();
        assert_eq!(state.drag, drag);
    }

    #[test]
    fn a_key_in_range_that_names_another_clip_does_not_move_that_clip() {
        let mut state = state_with_two_clips_and_a_drag_on_the_first();
        // Delete pressed in the middle of the drag of a.mp4. If it went through, key 0 would name
        // b.mp4 and the release would move that clip instead.
        state.delete_selected();
        state.finish_drag(EditCommand::Move { key: key(0), track: 0, desired_start: seconds(60) });
        let placed: Vec<(String, Time)> = state.project().tracks[0].clips.iter().map(|clip| (clip.source.clone(), clip.start)).collect();
        assert_eq!(placed, vec![("b.mp4".to_owned(), seconds(30)), ("C:/media/a.mp4".to_owned(), seconds(60))]);
    }

    #[test]
    fn a_control_held_after_escape_is_ignored_until_the_release() {
        let mut state = state_with_clip();
        let before = state.project().clone();
        state.live_gain(-6.0);
        state.cancel_drag();
        state.live_gain(-3.0);
        assert!(!state.is_live(), "the slider still reports the pointer, which must not restart the edit");
        assert_eq!(state.project(), &before);
        state.pointer_released();
        state.live_gain(-3.0);
        assert!(state.is_live(), "after the release a new hold works");
    }

    #[test]
    fn a_refused_request_stays_on_the_status_line_until_the_hold_ends() {
        let mut state = state_with_clip();
        state.live_gain(-3.0);
        assert!(state.refuse_while_live());
        state.live_gain(-4.0);
        assert_eq!(state.status(), &Status::Info(Message::FinishDragFirst), "the next step of the drag must not wipe it");
        state.pointer_released();
        assert_eq!(state.status(), &Status::Idle, "the release ends what it was waiting for");
    }

    #[test]
    fn a_held_time_field_blocks_edits_and_escape_clears_it() {
        let mut state = state_with_clip();
        state.ui.clip_field = Some(ClipField::Start(1.0));
        let before = state.project().clone();
        state.delete_selected();
        state.split_selected();
        state.undo();
        assert_eq!(state.project(), &before, "every edit waits for the release");
        assert_eq!(state.status(), &Status::Info(Message::FinishDragFirst));
        state.cancel_drag();
        assert_eq!(state.ui.clip_field, None);
        assert!(state.hold_cancelled(), "the field keeps reporting the pointer until the release");
        state.pointer_released();
        assert!(!state.hold_cancelled());
        state.delete_selected();
        assert!(state.project().tracks[0].clips.is_empty());
    }

    #[test]
    fn escape_tells_audio_to_drop_a_preview_it_may_hold() {
        let mut state = state_with_clip();
        state.live_gain(-6.0);
        let audio = state.audio_revision();
        state.cancel_drag();
        assert!(state.audio_revision() > audio, "playback opened during the hold plays the preview");
    }

    #[test]
    fn cancel_drag_drops_the_drag_without_an_edit() {
        let mut state = state_with_two_clips_and_a_drag_on_the_first();
        let before = state.project().clone();
        state.cancel_drag();
        assert_eq!(state.drag, None);
        assert_eq!(state.project(), &before);
    }

    #[test]
    fn a_finished_drag_still_edits() {
        let mut state = state_with_two_clips_and_a_drag_on_the_first();
        state.finish_drag(EditCommand::Move { key: key(0), track: 0, desired_start: seconds(60) });
        assert_eq!(state.drag, None);
        assert_eq!(clips(state.project(), 0)[1].0, seconds(60));
    }

    #[test]
    fn a_trim_past_the_source_start_is_refused() {
        let mut state = state_with_clip();
        state.set_playhead(seconds(30));
        state.add_clip_from_file(0, Path::new("b.mp4"));
        state.finish_drag(EditCommand::TrimStart { key: key(1), new_start: seconds(29) });
        assert!(matches!(state.status(), Status::Refused(_)));
    }

    #[test]
    fn a_trim_end_keeps_the_clip_selected() {
        let mut state = state_with_clip();
        state.finish_drag(EditCommand::TrimEnd { key: key(0), new_end: seconds(4) });
        assert_eq!(clips(state.project(), 0), vec![(Time::ZERO, seconds(4))]);
        assert_eq!(state.selection(), Some(key(0)));
    }

    #[test]
    fn the_playhead_never_goes_negative() {
        let mut state = EditorState::default();
        state.set_playhead(Time::from_ticks(-TICKS_PER_SECOND));
        assert_eq!(state.playhead(), Time::ZERO);
    }

    #[test]
    fn every_project_change_advances_the_revision() {
        let mut state = EditorState::default();
        assert_eq!(state.revision(), 0);
        state.add_track(TrackKind::Video);
        assert_eq!(state.revision(), 1);
        state.add_clip_from_file(0, Path::new("C:/media/a.mp4"));
        assert_eq!(state.revision(), 2);
        state.undo();
        assert_eq!(state.revision(), 3);
        state.redo();
        assert_eq!(state.revision(), 4);
    }

    #[test]
    fn nothing_changing_leaves_the_revision_alone() {
        let mut state = state_with_clip();
        let revision = state.revision();
        state.set_playhead(seconds(1));
        state.select(None);
        state.finish_drag(EditCommand::Move { key: key(0), track: 0, desired_start: Time::ZERO });
        assert_eq!(state.revision(), revision, "a drop on the same place changes nothing");
        state.finish_drag(EditCommand::Move { key: key(0), track: 1, desired_start: Time::ZERO });
        assert_eq!(state.revision(), revision, "a refused drop changes nothing");
        let mut empty = EditorState::default();
        empty.undo();
        assert_eq!(empty.revision(), 0, "an empty undo changes nothing");
    }

    fn playable() -> EditorState {
        let mut state = state_with_clip();
        state.take_transport();
        state
    }

    #[test]
    fn space_starts_from_the_playhead_and_stops_where_it_is() {
        let mut state = playable();
        state.set_playhead(seconds(2));
        state.toggle_playback();
        assert!(state.is_playing());
        assert_eq!(state.take_transport(), vec![TransportCommand::Play { from: seconds(2) }]);
        state.follow_clock(seconds(3));
        state.toggle_playback();
        assert!(!state.is_playing());
        assert_eq!(state.take_transport(), vec![TransportCommand::Stop]);
        assert_eq!(state.playhead(), seconds(3), "the playhead stays where it stopped");
    }

    #[test]
    fn playing_from_the_end_starts_over() {
        let mut state = playable();
        state.set_playhead(seconds(10));
        state.toggle_playback();
        assert_eq!(state.take_transport(), vec![TransportCommand::Play { from: Time::ZERO }]);
        assert_eq!(state.playhead(), Time::ZERO);
    }

    #[test]
    fn an_empty_or_audio_only_timeline_explains_why_it_does_not_play() {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Audio);
        state.add_clip_from_file(0, Path::new("music.wav"));
        state.take_transport();
        state.toggle_playback();
        assert!(!state.is_playing());
        assert!(state.take_transport().is_empty());
        assert_eq!(state.status(), &Status::Info(Message::NothingToPlay));
    }

    #[test]
    fn a_reopened_playback_plays_on_from_the_playhead() {
        let mut state = playable();
        state.toggle_playback();
        state.take_transport();
        state.follow_clock(seconds(4));
        state.reopen_playback();
        assert!(state.is_playing());
        assert_eq!(state.take_transport(), vec![TransportCommand::Play { from: seconds(4) }]);
    }

    #[test]
    fn a_stopped_timeline_is_not_reopened() {
        let mut state = playable();
        state.reopen_playback();
        assert!(state.take_transport().is_empty());
        assert!(!state.is_playing());
    }

    #[test]
    fn a_stopped_ruler_press_only_moves_the_playhead() {
        let mut state = playable();
        state.scrub_to(seconds(4));
        assert_eq!(state.playhead(), seconds(4));
        assert!(state.take_transport().is_empty());
    }

    #[test]
    fn a_ruler_press_while_playing_seeks_once_per_new_place() {
        let mut state = playable();
        state.toggle_playback();
        state.take_transport();
        state.scrub_to(seconds(4));
        state.follow_clock(seconds(5));
        state.scrub_to(seconds(4));
        assert_eq!(state.take_transport(), vec![TransportCommand::Seek(seconds(4))], "a held press seeks once");
        state.scrub_to(seconds(6));
        assert_eq!(state.take_transport(), vec![TransportCommand::Seek(seconds(6))]);
        state.end_scrub();
        state.scrub_to(seconds(6));
        assert_eq!(state.take_transport(), vec![TransportCommand::Seek(seconds(6))], "a new press seeks again");
    }

    #[test]
    fn following_the_clock_only_applies_while_playing() {
        let mut state = playable();
        state.set_playhead(seconds(1));
        state.follow_clock(seconds(7));
        assert_eq!(state.playhead(), seconds(1));
    }

    #[test]
    fn an_end_with_a_message_reaches_the_status_and_stops() {
        let mut state = playable();
        state.toggle_playback();
        state.playback_ended(Some("broken".to_owned()));
        assert!(!state.is_playing());
        assert_eq!(state.playback_message(), Some("broken"));
        state.toggle_playback();
        assert_eq!(state.playback_message(), None, "a new start clears it");
    }

    fn two_touching_clips() -> EditorState {
        let mut state = state_with_clip();
        state.add_clip_from_file(0, Path::new("b.mp4"));
        state.select(Some(key(0)));
        state
    }

    #[test]
    fn effects_are_added_and_removed_through_commands_and_undo() {
        let mut state = two_touching_clips();
        state.add_effect_package("packages/gpufx/ffmpeg/grayscale");
        let (effects, _) = applied_to(state.project(), key(0));
        assert_eq!(effects, vec!["packages/gpufx/ffmpeg/grayscale".to_owned()]);
        assert_eq!(state.project().tracks[0].clips[0].effects[0].params, Map::new(), "default parameters");
        assert_eq!(state.selection(), Some(key(0)));
        state.undo();
        assert!(state.project().tracks[0].clips[0].effects.is_empty());
        state.redo();
        assert_eq!(state.project().tracks[0].clips[0].effects.len(), 1);
    }

    #[test]
    fn removing_an_effect_uses_its_index() {
        let mut state = two_touching_clips();
        state.add_effect_package("a");
        state.select(Some(key(0)));
        state.add_effect_package("b");
        state.select(Some(key(0)));
        state.remove_effect_at(0);
        assert_eq!(applied_to(state.project(), key(0)).0, vec!["b".to_owned()]);
        state.select(Some(key(0)));
        state.remove_effect_at(9);
        assert!(matches!(state.status(), Status::Refused(_)));
    }

    #[test]
    fn effects_need_a_selection_and_a_video_clip() {
        let mut state = state_with_clip();
        state.select(None);
        state.add_effect_package("a");
        assert!(matches!(state.status(), Status::Info(_)));
        let mut audio = EditorState::default();
        audio.add_track(TrackKind::Audio);
        audio.add_clip_from_file(0, Path::new("a.wav"));
        audio.add_effect_package("a");
        assert!(matches!(audio.status(), Status::Refused(_)), "audio tracks have no picture");
    }

    #[test]
    fn a_transition_joins_the_selected_clip_with_a_touching_neighbour() {
        let mut state = two_touching_clips();
        state.add_transition_package("mix");
        let track = &state.project().tracks[0];
        assert_eq!(track.transitions.len(), 1);
        assert_eq!(track.transitions[0].after_clip, 0);
        assert_eq!(track.transitions[0].duration, DEFAULT_TRANSITION_DURATION);
        state.undo();
        assert!(state.project().tracks[0].transitions.is_empty());
    }

    #[test]
    fn the_second_of_two_clips_also_finds_the_pair() {
        let mut state = two_touching_clips();
        state.select(Some(key(1)));
        state.add_transition_package("mix");
        assert_eq!(state.project().tracks[0].transitions[0].after_clip, 0);
        let (_, transitions) = applied_to(state.project(), key(1));
        assert_eq!(transitions, vec![(0, "mix".to_owned())]);
    }

    #[test]
    fn a_clip_with_no_neighbour_refuses_a_transition() {
        let mut state = state_with_clip();
        state.add_transition_package("mix");
        assert_eq!(state.status(), &Status::Refused(Message::NoNeighbour));
        assert!(state.project().tracks[0].transitions.is_empty());
    }

    #[test]
    fn a_second_transition_on_the_same_cut_is_refused() {
        let mut state = two_touching_clips();
        state.add_transition_package("mix");
        state.select(Some(key(0)));
        state.add_transition_package("wipe");
        assert!(matches!(state.status(), Status::Refused(_)));
        assert_eq!(state.project().tracks[0].transitions.len(), 1);
    }

    #[test]
    fn removing_a_transition_is_undoable() {
        let mut state = two_touching_clips();
        state.add_transition_package("mix");
        state.remove_transition_after(0, 0);
        assert!(state.project().tracks[0].transitions.is_empty());
        state.undo();
        assert_eq!(state.project().tracks[0].transitions.len(), 1);
    }

    #[test]
    fn a_change_makes_the_project_dirty_and_a_save_clean() {
        let mut state = EditorState::default();
        assert!(!state.is_dirty());
        state.add_track(TrackKind::Video);
        assert!(state.is_dirty());
        state.mark_saved(PathBuf::from("a.json"));
        assert!(!state.is_dirty());
        assert_eq!(state.project_file(), Some(Path::new("a.json")));
        state.add_track(TrackKind::Audio);
        assert!(state.is_dirty());
    }

    #[test]
    fn opening_a_project_replaces_the_history_and_counts_as_saved() {
        let mut state = state_with_clip();
        state.toggle_playback();
        state.take_transport();
        let revision = state.revision();
        let opened = state.project().clone();
        state.replace_project(opened.clone(), Some(PathBuf::from("b.json")));

        assert_eq!(state.revision(), revision + 1, "preview and audio must refresh");
        assert!(!state.can_undo() && !state.can_redo(), "a fresh history");
        assert!(!state.is_dirty());
        assert!(!state.is_playing());
        assert_eq!(state.take_transport(), vec![TransportCommand::Stop]);
        assert_eq!(state.selection(), None);
        assert_eq!(state.playhead(), Time::ZERO);
        assert_eq!(state.project(), &opened);
    }

    #[test]
    fn opening_a_project_clears_the_audio_notes() {
        let mut state = state_with_clip();
        state.set_no_sound(Some("no device".to_owned()));
        state.set_silence_ms(40);
        state.replace_project(default_project(), None);
        assert_eq!(state.no_sound(), None);
        assert_eq!(state.silence_ms(), 0);
    }

    #[test]
    fn a_new_project_has_no_file() {
        let mut state = state_with_clip();
        state.mark_saved(PathBuf::from("a.json"));
        state.new_project();
        assert_eq!(state.project_file(), None);
        assert!(state.project().tracks.is_empty());
    }

    #[test]
    fn an_export_stops_playback_and_turns_editing_off() {
        let mut state = two_touching_clips();
        state.toggle_playback();
        state.take_transport();
        let before = state.project().clone();
        state.begin_export(90);

        assert!(!state.is_playing());
        assert_eq!(state.take_transport(), vec![TransportCommand::Stop]);
        state.add_track(TrackKind::Video);
        state.add_clip_from_file(0, Path::new("c.mp4"));
        state.select(Some(key(0)));
        state.add_effect_package("a");
        state.split_selected();
        state.delete_selected();
        state.undo();
        state.redo();
        assert_eq!(state.project(), &before);
        assert_eq!(state.status(), &Status::Info(Message::ExportRunning));
        state.toggle_playback();
        assert!(!state.is_playing());
        state.scrub_to(seconds(4));
        assert_eq!(state.playhead(), Time::ZERO);

        state.end_export(Status::Idle);
        state.add_track(TrackKind::Video);
        assert_ne!(state.project(), &before);
    }

    #[test]
    fn export_progress_is_shown_until_the_export_ends() {
        let mut state = EditorState::default();
        state.begin_export(10);
        state.set_export_progress(ExportProgress { done: 3, total: 10 });
        assert_eq!(state.export_progress().unwrap().fraction(), 0.3);
        state.end_export(Status::Error(Message::ExportFailed("disk full".to_owned())));
        assert_eq!(state.export_progress(), None);
        assert_eq!(state.status(), &Status::Error(Message::ExportFailed("disk full".to_owned())));
    }

    fn with_effect() -> EditorState {
        let mut state = two_touching_clips();
        state.add_effect_package("blur");
        state.select(Some(key(0)));
        state
    }

    fn radius(value: f64) -> Map<String, Value> {
        serde_json::json!({ "radius": value }).as_object().unwrap().clone()
    }

    #[test]
    fn a_slider_drag_previews_without_history_and_commits_one_undo_step() {
        let mut state = with_effect();
        let before = state.project().clone();
        let base_revision = state.revision();
        for value in [1.0, 2.0, 3.0] {
            state.live_effect_params(0, radius(value));
            assert_eq!(state.project().tracks[0].clips[0].effects[0].params, radius(value));
        }
        assert!(state.is_live());
        assert_eq!(state.revision(), base_revision + 3, "each step refreshes the preview");
        assert_eq!(state.history.current(), &before, "the history has not moved");
        state.commit_live();
        assert!(!state.is_live());
        assert_eq!(state.project().tracks[0].clips[0].effects[0].params, radius(3.0));
        state.undo();
        assert_eq!(state.project(), &before, "one drag is one undo step");
        state.redo();
        assert_eq!(state.project().tracks[0].clips[0].effects[0].params, radius(3.0));
    }

    #[test]
    fn a_held_control_blocks_edits_like_a_timeline_drag() {
        let mut state = with_effect();
        state.live_effect_params(0, radius(5.0));
        let shown = state.project().clone();
        state.delete_selected();
        state.undo();
        state.split_selected();
        state.add_track(TrackKind::Video);
        assert_eq!(state.status(), &Status::Info(Message::FinishDragFirst));
        assert_eq!(state.project(), &shown);
        assert!(state.is_live());
    }

    #[test]
    fn a_timeline_drag_blocks_a_live_edit() {
        let mut state = state_with_two_clips_and_a_drag_on_the_first();
        let before = state.project().clone();
        state.live_gain(-6.0);
        assert!(!state.is_live());
        assert_eq!(state.project(), &before);
        assert_eq!(state.status(), &Status::Info(Message::FinishDragFirst));
    }

    #[test]
    fn cancelling_a_live_edit_restores_the_project_and_stays_clean() {
        let mut state = with_effect();
        state.mark_saved(PathBuf::from("a.json"));
        let before = state.project().clone();
        let revision = state.revision();
        state.live_effect_params(0, radius(5.0));
        assert!(state.is_dirty());
        state.cancel_live();
        assert_eq!(state.project(), &before);
        assert!(state.revision() > revision, "the preview must come back");
        assert!(!state.is_dirty());
    }

    #[test]
    fn a_commit_that_changed_nothing_leaves_a_clean_project_clean_and_adds_no_step() {
        let mut state = with_effect();
        state.undo();
        state.redo();
        state.mark_saved(PathBuf::from("a.json"));
        assert!(state.can_undo());
        state.select(Some(key(0)));
        let params = state.project().tracks[0].clips[0].effects[0].params.clone();
        state.live_effect_params(0, params);
        state.commit_live();
        assert!(!state.is_dirty());
        state.undo();
        assert_eq!(state.project().tracks[0].clips[0].effects.len(), 0, "the earlier step is still next");
    }

    #[test]
    fn audio_hears_a_live_edit_only_at_the_release() {
        let mut state = state_with_clip();
        let audio = state.audio_revision();
        state.live_gain(-6.0);
        assert_eq!(state.audio_revision(), audio);
        state.commit_live();
        assert!(state.audio_revision() > audio);
        assert_eq!(state.project().tracks[0].clips[0].gain_db, -6.0);
    }

    #[test]
    fn a_refused_live_step_keeps_the_last_good_one() {
        let mut state = state_with_clip();
        state.live_gain(-6.0);
        state.live_gain(99.0);
        assert!(matches!(state.status(), Status::Refused(_)));
        assert_eq!(state.project().tracks[0].clips[0].gain_db, -6.0);
        state.commit_live();
        assert_eq!(state.project().tracks[0].clips[0].gain_db, -6.0);
    }

    #[test]
    fn escape_ends_a_live_edit_without_a_change() {
        let mut state = state_with_clip();
        let before = state.project().clone();
        state.live_gain(-6.0);
        state.cancel_drag();
        assert_eq!(state.project(), &before);
        assert!(!state.is_live());
    }

    #[test]
    fn a_new_project_drops_a_live_edit() {
        let mut state = state_with_clip();
        state.live_gain(-6.0);
        state.new_project();
        assert!(!state.is_live());
        assert!(state.project().tracks.is_empty());
    }

    #[test]
    fn a_transition_duration_edit_is_one_step_and_stays_within_its_limits() {
        let mut state = two_touching_clips();
        state.add_transition_package("mix");
        let (shortest, longest) = transition_duration_limits(state.project(), 0, 0).unwrap();
        assert_eq!(shortest, state.project().output.frame_duration);
        assert_eq!(longest, Time::from_ticks(DEFAULT_CLIP_DURATION.ticks() * 2));
        state.live_transition(0, 0, Map::new(), seconds(4));
        state.commit_live();
        assert_eq!(state.project().tracks[0].transitions[0].duration, seconds(4));
        state.undo();
        assert_eq!(state.project().tracks[0].transitions[0].duration, DEFAULT_TRANSITION_DURATION);
        assert_eq!(transition_duration_limits(state.project(), 0, 5), None);
    }

    #[test]
    fn a_neighbouring_transition_takes_its_share_of_a_clip() {
        let mut state = two_touching_clips();
        state.add_clip_from_file(0, Path::new("c.mp4"));
        state.select(Some(key(0)));
        state.add_transition_package("mix");
        state.select(Some(key(1)));
        state.add_transition_package("mix");
        let (_, longest) = transition_duration_limits(state.project(), 0, 1).unwrap();
        let taken = DEFAULT_TRANSITION_DURATION.saturating_sub(DEFAULT_TRANSITION_DURATION.half_floor());
        assert_eq!(longest.ticks(), 2 * (DEFAULT_CLIP_DURATION.ticks() - taken.ticks()));
    }

    #[test]
    fn the_start_and_duration_fields_edit_and_keep_the_selection() {
        let mut state = state_with_clip();
        state.apply_clip_field(ClipField::Start(4.0));
        assert_eq!(clips(state.project(), 0), vec![(seconds(4), DEFAULT_CLIP_DURATION)]);
        assert_eq!(state.selection(), Some(key(0)));
        state.apply_clip_field(ClipField::Duration(2.0));
        assert_eq!(clips(state.project(), 0), vec![(seconds(4), seconds(2))]);
    }

    #[test]
    fn trimming_to_the_playhead_cuts_each_side() {
        let mut state = state_with_clip();
        state.set_playhead(seconds(2));
        state.trim_start_to_playhead();
        assert_eq!(clips(state.project(), 0), vec![(seconds(2), seconds(8))]);
        assert_eq!(state.selection(), Some(key(0)));
        state.set_playhead(seconds(7));
        state.trim_end_to_playhead();
        assert_eq!(clips(state.project(), 0), vec![(seconds(2), seconds(5))]);
    }

    #[test]
    fn trimming_with_the_playhead_outside_the_clip_explains_itself() {
        let mut state = state_with_clip();
        state.set_playhead(seconds(50));
        let before = state.project().clone();
        state.trim_start_to_playhead();
        state.trim_end_to_playhead();
        assert_eq!(state.project(), &before);
        assert_eq!(state.status(), &Status::Info(Message::NothingAtPlayhead));
    }

    fn media(source: &str, kind: crate::media::MediaKind, length: Option<i64>) -> MediaItem {
        MediaItem { source: source.to_owned(), name: source.to_owned(), kind, duration: length.map(seconds), estimated: false }
    }

    #[test]
    fn media_is_added_at_the_playhead_making_a_track_in_one_undo_step() {
        let mut state = EditorState::default();
        state.set_playhead(seconds(2));
        let before = state.project().clone();
        state.add_media_at_playhead(&media("a.mp4", crate::media::MediaKind::Video, Some(3)));
        assert_eq!(state.project().tracks.len(), 1);
        assert_eq!(clips(state.project(), 0), vec![(seconds(2), seconds(3))]);
        assert_eq!(state.selection(), Some(key(0)));
        state.undo();
        assert_eq!(state.project(), &before, "the track and the clip are one step");
    }

    #[test]
    fn media_with_no_room_at_the_playhead_goes_on_a_new_track() {
        let mut state = state_with_clip();
        state.set_playhead(seconds(1));
        state.add_media_at_playhead(&media("b.mp4", crate::media::MediaKind::Video, Some(3)));
        assert_eq!(state.project().tracks.len(), 3);
        assert_eq!(state.project().tracks[2].clips[0].source, "b.mp4");
        state.add_media_at_playhead(&media("m.wav", crate::media::MediaKind::Audio, None));
        assert_eq!(state.project().tracks[1].clips.len(), 1, "the free audio track takes it");
        assert_eq!(state.project().tracks[1].clips[0].duration, DEFAULT_CLIP_DURATION);
    }

    #[test]
    fn opening_a_project_lists_its_media() {
        let mut state = EditorState::default();
        let mut project = default_project();
        project.tracks.push(engine::Track { kind: TrackKind::Video, clips: vec![new_clip("C:/v/x.mp4", Time::ZERO, seconds(4))], transitions: Vec::new() });
        state.replace_project(project, Some(PathBuf::from("p.json")));
        let listed = state.library.visible(crate::media::MediaKind::Video, "");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "x.mp4");
    }

    #[test]
    fn a_probed_import_is_listed_without_touching_the_project() {
        let mut state = EditorState::default();
        let before = state.project().clone();
        state.import_probed(Path::new("C:/a.mp4"), &video_probe(Some(5), Some(5)));
        assert_eq!(state.project(), &before);
        assert_eq!(state.library.visible(crate::media::MediaKind::Video, "").len(), 1);
        state.import_probed(Path::new("C:/none.bin"), &video_probe(None, None));
        assert!(matches!(state.status(), Status::Refused(_)));
    }

    #[test]
    fn a_clip_is_labelled_by_its_file_name() {
        assert_eq!(clip_label("C:/media/a.mp4"), "a.mp4");
        assert_eq!(clip_label("b.wav"), "b.wav");
    }
}
