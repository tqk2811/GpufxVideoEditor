//! Editor state without a window: the project history, the selection and the status line. The UI
//! calls these methods and draws the result; every timeline change goes through an engine command.

use crate::export::{EncoderOption, ExportOutcome, ExportProgress};
use crate::i18n::{ClipAction, ClipEdge, Language, Message, StreamKind, TrackLabel};
use crate::media::{choose_track, item_from_probe, MediaItem, MediaLibrary, TrackChoice};
use crate::packages::{Catalog, PickerState};
use crate::project_lock::ProjectLock;
use crate::playback::plan::{start_time, TransportCommand};
use crate::timeline::drag::{begin_drag, clamp_source_in, clamp_trim, drag_command, ClipDrag, EditCommand};
use crate::timeline::hit::ClipHit;
use crate::timeline::snap::{snap_command, snap_onto_track, snap_points, snap_span, snap_tolerance, Snapped};
use crate::timeline::view::TimelineView;
use crate::effect_previews::PreviewManager;
use crate::visuals::ClipVisuals;
use engine::{
    add_clip, add_effect, add_track, add_transition, audio_sample_count, clip_at, delete_clip, move_clip, nearest_free_start, remove_effect, remove_transition, set_effect_params, set_fades, set_gain, set_frame_duration, set_muted, set_output_size, set_placement, set_speed,
    set_track_hidden, set_transition_params, split_clip, trim_end, trim_source_in, trim_start, AudioOutput, Clip, ClipKey, EditError, EditHistory, Effect, Output, Project, Time, TrackKind, Transition, SUPPORTED_VERSION,
};
use d3d12_backend::MediaProbe;
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::layout::{PanelSizes, SplitterDrag};

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
    /// Whether agents may reach this window, and whether one is connected now.
    pub agent_access: bool,
    pub agent_connected: bool,
    /// The last export an agent started, which it can ask about later.
    agent_export: Option<AgentExport>,
    agent_job_count: u64,
    /// Keeps other processes off the project file while it is open here.
    project_lock: Option<ProjectLock>,
    pub ui: UiState,
    /// Thumbnails and level bars; drawn from `&self`, so behind a cell.
    pub visuals: RefCell<ClipVisuals>,
    /// Previews of the effect packages; asked for while drawing, so behind a cell.
    pub previews: RefCell<PreviewManager>,
    language: Language,
}

/// An export started by an agent. `outcome` stays `None` while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentExport {
    pub job: u64,
    pub out: PathBuf,
    pub outcome: Option<ExportOutcome>,
}

/// One held control: the project shows `shown` while `history` stays where it was, and the release
/// makes it one undo step.
struct LiveEdit {
    shown: Project,
    /// Whether the project was clean when the drag began.
    was_clean: bool,
}

fn stream_of(kind: TrackKind) -> StreamKind {
    match kind {
        TrackKind::Video => StreamKind::Video,
        TrackKind::Audio => StreamKind::Audio,
    }
}

/// Track `index` as the timeline labels it, numbered within its kind from 1.
fn track_label(project: &Project, index: usize) -> TrackLabel {
    let Some(track) = project.tracks.get(index) else { return TrackLabel::Position(index + 1) };
    let number = project.tracks[..=index].iter().filter(|other| other.kind == track.kind).count();
    match track.kind {
        TrackKind::Video => TrackLabel::Video(number),
        TrackKind::Audio => TrackLabel::Audio(number),
    }
}

fn seconds_text(time: Time) -> String {
    format!("{:.2}", crate::timecode::to_seconds(time))
}

/// What the status line says for a refused edit. No `_` arm: a new reason must be given a message.
fn refusal_message(project: &Project, error: &EditError) -> Message {
    let label = |track: usize| track_label(project, track);
    match error {
        EditError::TrackNotFound { track } => Message::EditTrackNotFound(track + 1),
        EditError::ClipNotFound { key } => Message::EditClipNotFound { track: label(key.track), clip: key.clip + 1 },
        EditError::EffectNotFound { key, effect } => Message::EditEffectNotFound { track: label(key.track), clip: key.clip + 1, effect: effect + 1 },
        EditError::TransitionNotFound { track, after_clip } => Message::EditTransitionNotFound { track: label(*track), after_clip: after_clip + 1 },
        EditError::TrackKindMismatch { from, to } => Message::EditTrackKindMismatch { from: stream_of(*from), to: stream_of(*to) },
        EditError::NoRoom { track } => Message::EditNoRoom(label(*track)),
        EditError::Overlap { track, start, end } => Message::EditOverlap { track: label(*track), start: seconds_text(*start), end: seconds_text(*end) },
        EditError::TimeOverflow { key } => Message::EditTimeOverflow { track: label(key.track), clip: key.clip + 1 },
        EditError::TransitionOnEdge { key, edge } => Message::EditTransitionOnEdge { track: label(key.track), edge: if *edge == "start" { ClipEdge::Start } else { ClipEdge::End } },
        EditError::BeforeSourceStart { key, .. } => Message::EditBeforeSourceStart(label(key.track)),
        EditError::SplitOutsideClip { key, at } => Message::EditSplitOutsideClip { track: label(key.track), at: seconds_text(*at) },
        EditError::SplitInsideFade { key, at } => Message::EditSplitInsideFade { track: label(key.track), at: seconds_text(*at) },
        EditError::SplitInsideTransition { key, at } => Message::EditSplitInsideTransition { track: label(key.track), at: seconds_text(*at) },
        EditError::TrackLocked { track } => Message::EditTrackLocked(label(*track)),
        EditError::SpeedOverlap { key, end, next_start } => Message::EditSpeedOverlap { track: label(key.track), end: seconds_text(*end), next: seconds_text(*next_start) },
        EditError::Invalid(errors) => Message::EditInvalid(errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")),
    }
}

/// The tab of the left panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LeftTab {
    #[default]
    Media,
    Audio,
    Effects,
    Transitions,
}

/// What the panels remember between frames.
#[derive(Debug, Clone, Default)]
pub struct UiState {
    pub left_tab: LeftTab,
    pub media_search: String,
    pub audio_search: String,
    pub fullscreen: bool,
    /// When the pointer or a key last did something in fullscreen; the controls hide after a while.
    pub fullscreen_activity: Option<f64>,
    /// A time field being dragged; the clip changes on release.
    pub clip_field: Option<ClipField>,
    /// The clip the inspector showed last, to notice when that changes.
    pub inspector_clip: Option<(ClipKey, String, i64)>,
    pub panel_sizes: PanelSizes,
    pub theme: crate::ui::theme::ThemeChoice,
    /// The splitter in the user's hand. Not a hold: edits go on while it moves.
    pub splitter: Option<SplitterDrag>,
    /// Files the window was given, with where the pointer was in physical pixels when known.
    pub files_dropped: Vec<(PathBuf, Option<egui::Pos2>)>,
    /// Drops on the timeline, the first being read one file at a time.
    pub drop_batches: VecDeque<DropBatch>,
    /// Snapping to clip edges is off. The default, false, keeps it on.
    pub snap_off: bool,
    /// The effect picker tile the pointer stays on; cleared when the picker is not drawn.
    pub tile_hover: crate::effect_previews::play::TileHover,
}

/// The files of one drop on the timeline and the place the next of them goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropBatch {
    pub files: VecDeque<PathBuf>,
    /// The track of the clips so far, or the row dropped on; `None` makes a new track.
    pub track: Option<usize>,
    pub at: Time,
}

impl UiState {
    /// Puts the panels back as they were before the drag, if one was going on.
    pub fn cancel_splitter(&mut self) {
        if let Some(drag) = self.splitter.take() {
            self.panel_sizes = drag.before;
        }
    }
}

/// What may be chosen in the inspector's time fields. Waits for the release of a drag, as a clip
/// edit renumbers clips.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipField {
    Start(f64),
    /// Where the clip begins in its source.
    SourceIn(f64),
    /// How much of the source the clip plays; its length on the timeline follows from the speed.
    Duration(f64),
}

/// A speed as the factor the project stores, without trailing zeros: `1.237`, `2`.
pub fn speed_text(speed: f64) -> String {
    let text = format!("{speed:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A speed as the factor shown to the user, for example `2x` or `1.237x`.
pub fn format_speed(speed: f64) -> String {
    format!("{}x", speed_text(speed))
}

/// The first value in `low..=high` where `holds` is true, for a `holds` that stays true once it
/// is, or `high + 1` when there is none.
fn first_holding(mut low: i64, mut high: i64, holds: impl Fn(i64) -> bool) -> i64 {
    let end = high + 1;
    let mut found = end;
    while low <= high {
        let middle = low + (high - low) / 2;
        if holds(middle) {
            found = middle;
            high = middle - 1;
        } else {
            low = middle + 1;
        }
    }
    found
}

/// The speeds clip `key` may take where it is, as `(lowest, highest)` in project units: the lowest
/// keeps it clear of the next clip and the highest leaves room for its fades and the transition at
/// its start. Always holds the current speed. `None` when the clip has a transition on its end,
/// where the speed cannot change at all. Mirrors the rounding of [`engine::set_speed`].
pub fn speed_limits(project: &Project, key: ClipKey) -> Option<(f64, f64)> {
    let track = project.tracks.get(key.track)?;
    let clip = track.clips.get(key.clip)?;
    if track.transitions.iter().any(|transition| transition.after_clip == key.clip) {
        return None;
    }
    let source_length = i128::from(clip.duration.ticks()) * i128::from(clip.speed_milli());
    let duration = |milli: i64| i64::try_from(((source_length + i128::from(milli) / 2) / i128::from(milli)).max(1)).unwrap_or(i64::MAX);
    let room = track.clips.get(key.clip + 1).map(|next| next.start.saturating_sub(clip.start).ticks());
    let head = key.clip.checked_sub(1).and_then(|previous| track.transitions.iter().find(|transition| transition.after_clip == previous)).map_or(Time::ZERO, |transition| transition.duration.saturating_sub(transition.duration.half_floor()));
    let shortest = clip.fade_in.saturating_add(clip.fade_out).max(head).ticks().max(1);
    let lowest = room.map_or(100, |room| first_holding(100, 10_000, |milli| duration(milli) <= room));
    // The speeds that still fit form the range below the first that no longer does.
    let highest = first_holding(100, 10_000, |milli| duration(milli) < shortest) - 1;
    let current = clip.speed_milli();
    let scale = engine::SPEED_SCALE as f64;
    Some((lowest.min(current) as f64 / scale, highest.max(current) as f64 / scale))
}

/// The sizes offered for the output, widest shapes first.
pub const OUTPUT_PRESETS: [(u32, u32); 6] = [(3840, 2160), (2560, 1440), (1920, 1080), (1280, 720), (1080, 1920), (1080, 1080)];
const MAX_OUTPUT_SIDE: u32 = 7680;
/// The frame rates offered for the output, as ticks per frame: 24, 25, 29.97, 30, 50 and 60 fps.
/// Only rates with a whole number of ticks per frame can be kept exactly, which leaves out 23.976 and 59.94.
pub const OUTPUT_FRAME_DURATIONS: [Time; 6] = [Time::from_ticks(3750), Time::from_ticks(3600), Time::from_ticks(3003), Time::from_ticks(3000), Time::from_ticks(1800), Time::from_ticks(1500)];

/// `side` as an output side: encoders of 4:2:0 video need it even.
fn even_side(side: u32) -> u32 {
    side.clamp(2, MAX_OUTPUT_SIDE) & !1
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
        speed: 1.0,
        placement: None,
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
            agent_access: false,
            agent_connected: false,
            agent_export: None,
            agent_job_count: 0,
            project_lock: None,
            ui: UiState::default(),
            visuals: RefCell::new(ClipVisuals::default()),
            previews: RefCell::new(PreviewManager::default()),
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
        self.ui.drop_batches.clear();
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
        self.status = Status::Refused(refusal_message(self.project(), error));
    }

    /// An export renders the project it started with, so edits wait for it. A drag holds a clip key,
    /// and any edit could make that key name another clip, so edits wait for the drop; the same
    /// goes for a held inspector control. True, with a status, while any is in progress.
    pub(crate) fn edits_blocked(&mut self) -> bool {
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

    /// Runs a command on an unblocked state. The revisions follow only a real change.
    fn apply_command<F>(&mut self, command: F) -> Result<(), EditError>
    where
        F: FnOnce(&Project) -> Result<Project, EditError>,
    {
        let before = self.project().clone();
        let after = self.history.apply(command)?;
        if *after != before {
            self.revision += 1;
            self.audio_revision = self.revision;
        }
        self.status = Status::Idle;
        Ok(())
    }

    /// Runs a command. A refusal goes to the status line and the result is false.
    fn run<F>(&mut self, command: F) -> bool
    where
        F: FnOnce(&Project) -> Result<Project, EditError>,
    {
        if self.edits_blocked() {
            return false;
        }
        match self.apply_command(command) {
            Ok(()) => true,
            Err(error) => {
                self.refuse(&error);
                false
            }
        }
    }

    /// Whether an edit from an agent has to wait, the same rule as [`Self::edits_blocked`] for a
    /// person, and why.
    pub fn agent_busy_reason(&self) -> Option<&'static str> {
        if self.export.is_some() {
            Some("an export is running")
        } else if self.is_holding() {
            Some("the user is dragging a control")
        } else {
            None
        }
    }

    /// Runs a command of an agent through the path a person's edit takes. The selection is dropped
    /// after a real change, because clip keys name clips of one project.
    pub fn apply_agent_command<F>(&mut self, command: F) -> Result<(), EditError>
    where
        F: FnOnce(&Project) -> Result<Project, EditError>,
    {
        let revision = self.revision;
        self.apply_command(command)?;
        if self.revision != revision {
            self.selection = None;
            self.library.adopt_project_media(self.history.current());
        }
        Ok(())
    }

    /// Sets a track's lock. Like the button, not an undo step and not heard by the audio.
    pub fn set_track_locked(&mut self, track: usize, locked: bool) -> Result<(), EditError> {
        if self.is_track_locked(track) == locked && track < self.project().tracks.len() {
            return Ok(());
        }
        self.history.set_track_locked(track, locked)?;
        self.revision += 1;
        self.status = Status::Idle;
        Ok(())
    }

    /// Goes back one step for an agent; false when there is none.
    pub fn agent_undo(&mut self) -> bool {
        let stepped = self.history.undo();
        if stepped {
            self.after_history_step();
        }
        stepped
    }

    /// Goes forward one step for an agent; false when there is none.
    pub fn agent_redo(&mut self) -> bool {
        let stepped = self.history.redo();
        if stepped {
            self.after_history_step();
        }
        stepped
    }

    /// Takes the lock of `file`, keeping one already held for it. False when another process has it.
    pub fn claim_project_lock(&mut self, file: &Path) -> bool {
        if self.project_lock.as_ref().is_some_and(|lock| d3d12_backend::same_file(lock.file(), file)) {
            return true;
        }
        match ProjectLock::acquire(file) {
            Ok(Some(lock)) => {
                self.project_lock = Some(lock);
                true
            }
            Ok(None) => false,
            // A folder that takes no lock file is no reason to refuse the project.
            Err(_) => {
                self.project_lock = None;
                true
            }
        }
    }

    /// Records an export an agent has just started and returns its job number.
    pub fn begin_agent_export(&mut self, out: PathBuf) -> u64 {
        self.agent_job_count += 1;
        self.agent_export = Some(AgentExport { job: self.agent_job_count, out, outcome: None });
        self.agent_job_count
    }

    /// Records how the running export ended, for the agent that started it.
    pub fn finish_agent_export(&mut self, outcome: &ExportOutcome) {
        if let Some(record) = self.agent_export.as_mut().filter(|record| record.outcome.is_none()) {
            record.outcome = Some(outcome.clone());
        }
    }

    pub fn agent_export(&self) -> Option<&AgentExport> {
        self.agent_export.as_ref()
    }

    /// Lets go of the project lock, for a project with no file.
    pub fn release_project_lock(&mut self) {
        self.project_lock = None;
    }

    pub fn add_track(&mut self, kind: TrackKind) {
        self.run(|project| add_track(project, kind));
    }

    pub fn is_track_locked(&self, track: usize) -> bool {
        self.project().tracks.get(track).is_some_and(|target| target.locked)
    }

    /// Whether the selected clip sits on a locked track.
    pub fn selection_locked(&self) -> bool {
        self.selection.is_some_and(|key| self.is_track_locked(key.track))
    }

    /// Not an undo step. The project is dirty, because the flag is saved with it, but nothing that
    /// is heard changed, so the audio ring stays.
    pub fn toggle_track_locked(&mut self, track: usize) {
        if self.edits_blocked() {
            return;
        }
        let locked = self.is_track_locked(track);
        match self.history.set_track_locked(track, !locked) {
            Ok(()) => {
                self.revision += 1;
                self.status = Status::Idle;
            }
            Err(error) => self.refuse(&error),
        }
    }

    pub fn toggle_track_hidden(&mut self, track: usize) {
        let hidden = self.project().tracks.get(track).is_some_and(|target| target.hidden);
        self.run(|project| set_track_hidden(project, track, !hidden));
    }

    /// Starts dragging the clip at `hit`, unless its track is locked, which the status then says.
    pub fn begin_clip_drag(&mut self, hit: ClipHit, press_time: Time) {
        if self.is_track_locked(hit.key.track) {
            self.refuse(&EditError::TrackLocked { track: hit.key.track });
            return;
        }
        self.drag = begin_drag(self.project(), hit, press_time);
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
        let Some(length) = self.stream_length(kind, &name, probe) else { return };
        if let Some(item) = item_from_probe(&path.to_string_lossy(), probe) {
            self.library.add(item);
        }
        if self.place_clip(track, path, length.unwrap_or(DEFAULT_CLIP_DURATION)) {
            self.status = Status::Info(match length {
                Some(length) => Message::AddedWithLength { name, seconds: format!("{:.2}", length.ticks() as f64 / engine::TICKS_PER_SECOND as f64) },
                None => Message::AddedNoLength { name, seconds: DEFAULT_CLIP_DURATION.ticks() / engine::TICKS_PER_SECOND },
            });
        }
    }

    /// The stated length of the stream a track of `kind` plays, `Some(None)` when it states none.
    /// `None`, with a status, when the file has no such stream.
    fn stream_length(&mut self, kind: TrackKind, name: &str, probe: &MediaProbe) -> Option<Option<Time>> {
        let (present, stated, needed, problem) = match kind {
            TrackKind::Video => (probe.has_video, probe.video_duration, StreamKind::Video, probe.video_problem.as_deref()),
            TrackKind::Audio => (probe.has_audio, probe.audio_duration, StreamKind::Audio, probe.audio_problem.as_deref()),
        };
        if !present {
            self.status = Status::Refused(match problem {
                Some(reason) => Message::StreamUnusable { name: name.to_owned(), stream: needed, reason: reason.to_owned() },
                None => Message::StreamMissing { name: name.to_owned(), stream: needed },
            });
            return None;
        }
        Some(stated.filter(|length| *length > Time::ZERO))
    }

    /// Adds a dropped file that has been read, at the place its batch has reached. A file that
    /// cannot go there leaves the place as it is, for the next file of the batch.
    pub fn add_dropped_file(&mut self, path: &Path, probe: &MediaProbe) -> Option<ClipKey> {
        let (track, at) = self.ui.drop_batches.front().map(|batch| (batch.track, batch.at))?;
        let name = clip_label(&path.to_string_lossy()).to_owned();
        let own = if probe.has_video { TrackKind::Video } else { TrackKind::Audio };
        let kind = track.and_then(|index| self.project().tracks.get(index)).map_or(own, |target| target.kind);
        let duration = self.stream_length(kind, &name, probe)?;
        if let Some(item) = item_from_probe(&path.to_string_lossy(), probe) {
            self.library.add(item);
        }
        let media_kind = if kind == TrackKind::Video { crate::media::MediaKind::Video } else { crate::media::MediaKind::Audio };
        let item = MediaItem { source: path.to_string_lossy().into_owned(), name: name.clone(), kind: media_kind, duration, estimated: false };
        let key = self.drop_media(&item, track, at)?;
        if duration.is_none() {
            self.status = Status::Info(Message::AddedNoLength { name, seconds: DEFAULT_CLIP_DURATION.ticks() / engine::TICKS_PER_SECOND });
        }
        let end = self.project().tracks[key.track].clips[key.clip].end();
        if let (Some(batch), Some(end)) = (self.ui.drop_batches.front_mut(), end) {
            batch.track = Some(key.track);
            batch.at = end;
        }
        Some(key)
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

    /// The command of `drag` with the pointer at `pointer_time`, pulled onto the edges of other
    /// clips, the playhead and zero, and the time to draw a guide line at. `free` turns snapping off
    /// for this moment, as holding Shift does. The preview and the release both use this, so they
    /// always agree.
    pub fn snapped_drag(&self, drag: &ClipDrag, pointer_time: Time, pointer_track: Option<usize>, free: bool) -> Snapped {
        let command = drag_command(drag, pointer_time, pointer_track);
        if self.ui.snap_off || free {
            return Snapped { command: clamp_trim(self.project(), command), guide: None };
        }
        let points = snap_points(self.project(), Some(drag.key), self.playhead());
        snap_command(self.project(), command, &points, snap_tolerance(self.view.pixels_per_second()))
    }

    /// Where a library item dropped with its start at `at` begins: pulled onto a snap point by its
    /// start or end unless snapping is off or `free`. On `track` only a start the track really has
    /// free counts; `None` is a new track, where anything does. An unknown `duration` counts as the
    /// default.
    pub fn snapped_drop_time(&self, at: Time, duration: Option<Time>, track: Option<usize>, free: bool) -> Time {
        if self.ui.snap_off || free {
            return at;
        }
        let duration = duration.unwrap_or(DEFAULT_CLIP_DURATION);
        let points = snap_points(self.project(), None, self.playhead());
        let tolerance = snap_tolerance(self.view.pixels_per_second());
        match track {
            Some(track) => snap_onto_track(self.project(), track, &points, at, duration, tolerance),
            None => snap_span(&points, at, duration, tolerance).map_or(at, |(start, _)| start),
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

    /// A time field of the selected clip, once its value is final.
    pub fn apply_clip_field(&mut self, field: ClipField) {
        let Some(key) = self.selected_for(ClipAction::Change) else { return };
        let Some(clip) = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)) else { return };
        let command = match field {
            ClipField::SourceIn(seconds) => {
                if let Some(source_in) = clamp_source_in(self.project(), key, crate::timecode::from_seconds(seconds)) {
                    self.run(|project| trim_source_in(project, key, source_in));
                }
                return;
            }
            ClipField::Start(seconds) => EditCommand::Move { key, track: key.track, desired_start: crate::timecode::from_seconds(seconds) },
            ClipField::Duration(seconds) => EditCommand::TrimEnd { key, new_end: clip.start.saturating_add(clip.timeline_ticks(crate::timecode::from_seconds(seconds))) },
        };
        let command = clamp_trim(self.project(), command);
        self.finish_drag(command);
    }

    /// Puts the selected clip back where it fits the frame, as one undo step.
    pub fn reset_placement(&mut self) {
        let Some(key) = self.selected_for(ClipAction::Change) else { return };
        self.run(|project| set_placement(project, key, None));
    }

    /// The source time from where clip `key` begins to the end of its stream, when a probe stated
    /// the length of that stream.
    pub fn source_remaining(&self, key: ClipKey) -> Option<Time> {
        let track = self.project().tracks.get(key.track)?;
        let clip = track.clips.get(key.clip)?;
        let length = self.library.probed_duration(&clip.source, track.kind)?;
        Some(length.saturating_sub(clip.source_in)).filter(|left| *left > Time::ZERO)
    }

    /// Sets the size of the output, as the nearest size encoders take.
    pub fn set_output_size(&mut self, width: u32, height: u32) {
        let (width, height) = (even_side(width), even_side(height));
        self.run(|project| set_output_size(project, width, height));
    }

    /// Sets how long one output frame lasts.
    pub fn set_frame_duration(&mut self, frame_duration: Time) {
        self.run(|project| set_frame_duration(project, frame_duration));
    }

    pub fn set_muted(&mut self, muted: bool) {
        let Some(key) = self.selected_for(ClipAction::Mute) else { return };
        self.run(|project| set_muted(project, key, muted));
    }

    pub fn live_gain(&mut self, gain_db: f64) {
        let Some(key) = self.selection else { return };
        self.live_edit(|project| set_gain(project, key, gain_db));
    }

    /// Sets the speed of the selected clip, keeping its source range: see [`engine::set_speed`]. A
    /// drag that overshoots lands on the limit of [`speed_limits`] instead of being refused. The
    /// limit comes from the project before the drag, which is what the edit starts from.
    pub fn live_speed(&mut self, speed: f64) {
        let Some(key) = self.selection else { return };
        let speed = speed_limits(self.history.current(), key).map_or(speed, |(lowest, highest)| speed.max(lowest).min(highest));
        self.live_edit(|project| set_speed(project, key, speed));
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
        // At another speed the start rounds up to a grid, which from the last grid of a clip is
        // at or past its end: nothing would be left of it.
        let lands_inside = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)).is_some_and(|clip| clip.end().is_some_and(|end| clip.snapped_start(at) < end));
        if !self.playhead_inside(key) || !lands_inside {
            self.status = Status::Info(Message::NothingAtPlayhead);
            return;
        }
        if self.run(|project| trim_start(project, key, at)) {
            // A trim keeps the order of the clips, and at another speed lands near `at`, not on it.
            self.selection = Some(key);
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

    /// Adds `item` where it was dropped: on `track`, as near `at` as the other clips allow, or on a
    /// new track of its kind when `track` is `None`, which is one undo step. A track of the other
    /// kind or a locked one refuses it. The key of the new clip when it was added.
    pub fn drop_media(&mut self, item: &MediaItem, track: Option<usize>, at: Time) -> Option<ClipKey> {
        if self.edits_blocked() {
            return None;
        }
        let duration = item.duration.unwrap_or(DEFAULT_CLIP_DURATION);
        let kind = item.kind.track_kind();
        let at = at.max(Time::ZERO);
        let (index, start) = match track {
            Some(index) => {
                let found = self.project().tracks.get(index).map(|target| (target.kind, target.locked, nearest_free_start(target, None, duration, at)));
                let error = match found {
                    None => EditError::TrackNotFound { track: index },
                    Some((to, _, _)) if to != kind => EditError::TrackKindMismatch { from: kind, to },
                    Some((_, true, _)) => EditError::TrackLocked { track: index },
                    Some((_, false, Some(start))) => {
                        return self.add_dropped(item, index, start, false, kind, duration);
                    }
                    Some((_, false, None)) => EditError::NoRoom { track: index },
                };
                self.refuse(&error);
                return None;
            }
            None => (self.project().tracks.len(), at),
        };
        self.add_dropped(item, index, start, true, kind, duration)
    }

    fn add_dropped(&mut self, item: &MediaItem, track: usize, start: Time, new_track: bool, kind: TrackKind, duration: Time) -> Option<ClipKey> {
        let clip = new_clip(&item.source, start, duration);
        let added = self.run(|project| {
            let with_track = if new_track { add_track(project, kind)? } else { project.clone() };
            add_clip(&with_track, track, clip)
        });
        if !added {
            return None;
        }
        self.library.adopt_project_media(self.history.current());
        self.selection = clip_at(self.project(), track, start);
        self.status = Status::Info(Message::Added(item.name.clone()));
        self.selection
    }

    /// Ends a move dropped outside the track rows: the clip goes to a new track of its kind, as
    /// one undo step. A drag that is not a move ends as it would anywhere else.
    /// `free` turns snapping off for this drop, as holding Shift does.
    pub fn finish_drag_on_new_track(&mut self, drag: ClipDrag, pointer_time: Time, free: bool) {
        let command = drag_command(&drag, pointer_time, None);
        let EditCommand::Move { key, mut desired_start, .. } = command else {
            let snapped = self.snapped_drag(&drag, pointer_time, None, free);
            self.finish_drag(snapped.command);
            return;
        };
        if !self.ui.snap_off && !free {
            let duration = self.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)).map(|clip| clip.duration);
            let points = snap_points(self.project(), Some(key), self.playhead());
            if let Some((start, _)) = duration.and_then(|duration| snap_span(&points, desired_start, duration, snap_tolerance(self.view.pixels_per_second()))) {
                desired_start = start;
            }
        }
        self.drag = None;
        let Some(kind) = self.project().tracks.get(key.track).map(|track| track.kind) else { return };
        let track = self.project().tracks.len();
        if self.run(|project| move_clip(&add_track(project, kind)?, key, track, desired_start)) {
            self.selection = clip_at(self.project(), track, desired_start);
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

    #[test]
    fn the_own_lock_counts_whatever_the_case_of_the_path() {
        let dir = std::env::temp_dir().join(format!("gpufx-editor-lock-case-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut state = EditorState::default();
        assert!(state.claim_project_lock(&dir.join("p.json")));
        assert!(state.claim_project_lock(&dir.join("P.JSON")), "the file it already holds");
        state.release_project_lock();
        let _ = std::fs::remove_dir_all(&dir);
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

    fn locked_clip_state() -> EditorState {
        let mut state = state_with_clip();
        state.select(Some(key(0)));
        state.toggle_track_locked(0);
        state
    }

    #[test]
    fn locking_refuses_edits_of_the_track_with_a_translated_reason() {
        let mut state = locked_clip_state();
        assert!(state.is_track_locked(0));
        let before = state.project().clone();
        state.set_playhead(seconds(5));
        state.split_selected();
        assert_eq!(state.project(), &before);
        assert_eq!(state.status(), &Status::Refused(Message::EditTrackLocked(TrackLabel::Video(1))));
        state.delete_selected();
        assert_eq!(state.project(), &before, "Delete is refused too");
        state.set_muted(true);
        assert_eq!(state.project(), &before);
    }

    #[test]
    fn a_locked_clip_can_still_be_selected_and_its_refused_edit_leaves_the_selection() {
        let mut state = locked_clip_state();
        state.select(None);
        state.select(Some(key(0)));
        assert_eq!(state.selection(), Some(key(0)));
        state.delete_selected();
        assert_eq!(state.selection(), Some(key(0)));
    }

    #[test]
    fn hiding_is_an_undoable_step_and_the_lock_stays_through_it() {
        let mut state = state_with_clip();
        state.toggle_track_locked(0);
        state.toggle_track_hidden(0);
        let revision = state.revision();
        assert!(state.project().tracks[0].locked && state.project().tracks[0].hidden);
        state.undo();
        assert!(state.revision() > revision);
        assert!(state.project().tracks[0].locked && !state.project().tracks[0].hidden);
    }

    #[test]
    fn ctrl_z_after_locking_undoes_the_earlier_edit_and_leaves_the_lock() {
        let mut state = state_with_clip();
        state.toggle_track_hidden(0);
        state.toggle_track_locked(0);
        state.undo();
        assert!(!state.project().tracks[0].hidden, "the step before the lock");
        assert!(state.project().tracks[0].locked, "the lock is not undone");
    }

    #[test]
    fn locking_does_not_clear_redo_and_undo_keeps_the_lock_state_of_now() {
        let mut state = state_with_clip();
        state.toggle_track_hidden(0);
        state.undo();
        state.toggle_track_locked(0);
        assert!(state.can_redo(), "the lock is not an edit that replaces the future");
        state.redo();
        assert!(state.project().tracks[0].hidden);
        assert!(state.project().tracks[0].locked);
    }

    #[test]
    fn a_lock_dirties_the_project_survives_save_and_reopen_and_stays_out_of_the_audio() {
        let mut state = state_with_clip();
        state.mark_saved(PathBuf::from("a.json"));
        let audio = state.audio_revision();
        state.toggle_track_locked(0);
        assert!(state.is_dirty());
        assert_eq!(state.audio_revision(), audio);
        let folder = std::env::temp_dir().join(format!("gpufx-editor-lock-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("p.json");
        crate::project_file::save_project_file(state.project(), &file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        let reopened = crate::project_file::parse_project(&text, &file, &engine::AcceptAll).unwrap();
        assert!(reopened.tracks[0].locked);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn locking_waits_for_a_held_control() {
        let mut state = state_with_clip();
        state.select(Some(key(0)));
        state.live_gain(-6.0);
        state.toggle_track_locked(0);
        assert!(!state.is_track_locked(0));
        assert_eq!(state.status(), &Status::Info(Message::FinishDragFirst));
    }

    #[test]
    fn hiding_a_track_makes_preview_and_audio_follow() {
        let mut state = state_with_clip();
        let (revision, audio) = (state.revision(), state.audio_revision());
        state.toggle_track_hidden(0);
        assert!(state.revision() > revision, "the preview is asked for again");
        assert!(state.audio_revision() > audio, "playback gets the new project");
        assert!(state.is_dirty());
    }

    #[test]
    fn toggling_lock_leaves_the_audio_revision_alone_but_toggling_hide_changes_it() {
        let mut state = state_with_clip();
        let (revision, audio) = (state.revision(), state.audio_revision());
        state.toggle_track_locked(0);
        assert!(state.revision() > revision, "the project changed and is dirty");
        assert_eq!(state.audio_revision(), audio, "a lock is not heard");
        state.toggle_track_locked(0);
        assert_eq!(state.audio_revision(), audio);
        state.toggle_track_hidden(0);
        assert!(state.audio_revision() > audio, "a hidden track is not heard");
    }

    #[test]
    fn a_locked_track_can_still_be_hidden_and_unlocked() {
        let mut state = locked_clip_state();
        state.toggle_track_hidden(0);
        assert!(state.project().tracks[0].hidden);
        state.toggle_track_locked(0);
        assert!(!state.project().tracks[0].locked);
    }

    #[test]
    fn a_drag_on_a_locked_track_does_not_start_and_says_why() {
        let mut state = locked_clip_state();
        state.begin_clip_drag(crate::timeline::hit::ClipHit { key: key(0), zone: crate::timeline::hit::ClipZone::Body }, seconds(1));
        assert_eq!(state.drag, None);
        assert_eq!(state.status(), &Status::Refused(Message::EditTrackLocked(TrackLabel::Video(1))));
    }

    #[test]
    fn dropping_a_clip_on_a_locked_track_is_refused_and_the_clip_stays() {
        let mut state = state_with_clip();
        state.add_track(TrackKind::Video);
        state.toggle_track_locked(2);
        let before = state.project().clone();
        state.finish_drag(EditCommand::Move { key: key(0), track: 2, desired_start: seconds(1) });
        assert_eq!(state.project(), &before);
        assert_eq!(state.status(), &Status::Refused(Message::EditTrackLocked(TrackLabel::Video(2))));
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
    fn a_refused_edit_is_told_in_the_language_of_the_editor() {
        let mut state = EditorState::default();
        state.set_language(Language::Vietnamese);
        state.add_clip_from_file(7, Path::new("a.mp4"));
        assert_eq!(state.status(), &Status::Refused(Message::EditTrackNotFound(8)));
        assert_eq!(Language::Vietnamese.text(&Message::EditTrackNotFound(8)), "Track 8 không tồn tại");
    }

    #[test]
    fn every_edit_refusal_has_text_and_names_its_track_as_the_timeline_does() {
        let mut project = EditorState::default().project().clone();
        for kind in [TrackKind::Video, TrackKind::Audio, TrackKind::Video] {
            project.tracks.push(engine::Track { kind, clips: Vec::new(), transitions: Vec::new(), locked: false, hidden: false });
        }
        let key = ClipKey { track: 2, clip: 0 };
        let at = Time::from_seconds(3).unwrap();
        let errors = [
            EditError::ClipNotFound { key },
            EditError::EffectNotFound { key, effect: 0 },
            EditError::TransitionNotFound { track: 2, after_clip: 0 },
            EditError::NoRoom { track: 2 },
            EditError::Overlap { track: 2, start: at, end: at },
            EditError::TimeOverflow { key },
            EditError::TransitionOnEdge { key, edge: "start" },
            EditError::TransitionOnEdge { key, edge: "end" },
            EditError::BeforeSourceStart { key, source_in: at },
            EditError::SplitOutsideClip { key, at },
            EditError::SplitInsideFade { key, at },
            EditError::SplitInsideTransition { key, at },
            EditError::SpeedOverlap { key, end: at, next_start: at },
        ];
        for error in &errors {
            let message = refusal_message(&project, error);
            assert!(!matches!(message, Message::Technical(_)), "{error:?}");
            assert!(Language::English.text(&message).starts_with("Video track 2"), "{error:?}");
            assert!(Language::Vietnamese.text(&message).starts_with("Track video 2"), "{error:?}");
        }
        let start = refusal_message(&project, &EditError::TransitionOnEdge { key, edge: "start" });
        let end = refusal_message(&project, &EditError::TransitionOnEdge { key, edge: "end" });
        assert_ne!(Language::Vietnamese.text(&start), Language::Vietnamese.text(&end));

        let mismatch = refusal_message(&project, &EditError::TrackKindMismatch { from: TrackKind::Audio, to: TrackKind::Video });
        assert_eq!(Language::English.text(&mismatch), "A clip cannot move from an audio track to a video track");
        assert_eq!(Language::Vietnamese.text(&mismatch), "Không thể chuyển clip từ track âm thanh sang track video");
        assert_eq!(refusal_message(&project, &EditError::TrackNotFound { track: 9 }), Message::EditTrackNotFound(10));
        let invalid = refusal_message(&project, &EditError::Invalid(Vec::new()));
        assert!(Language::Vietnamese.text(&invalid).starts_with("Kết quả không hợp lệ"));
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
                base.tracks.push(engine::Track { kind: TrackKind::Video, clips: Vec::new(), transitions: Vec::new(), locked: false, hidden: false });
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
    fn trimming_a_sped_clip_to_the_playhead_keeps_it_selected() {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        state.add_clip_from_file(0, Path::new("a.mp4"));
        let key = ClipKey { track: 0, clip: 0 };
        state.select(Some(key));
        state.live_speed(1.5);
        state.commit_live();
        state.scrub_to(Time::from_ticks(90_001));
        state.trim_start_to_playhead();
        assert_ne!(state.project().tracks[0].clips[0].start, Time::ZERO);
        assert_eq!(state.selection(), Some(key));
    }

    #[test]
    fn a_speed_shows_every_decimal_the_project_stores_and_no_more() {
        assert_eq!(format_speed(1.237), "1.237x");
        assert_eq!(format_speed(1.004), "1.004x");
        assert_eq!(format_speed(2.0), "2x");
        assert_eq!(format_speed(0.1), "0.1x");
        assert_eq!(format_speed(10.0), "10x");
        assert_eq!(speed_text(1.5), "1.5");
    }

    #[test]
    fn trimming_to_a_playhead_in_the_last_grid_of_a_sped_clip_says_nothing_is_there() {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        state.add_clip_from_file(0, Path::new("a.mp4"));
        let key = ClipKey { track: 0, clip: 0 };
        state.select(Some(key));
        state.live_speed(1.234);
        state.commit_live();
        let before = state.project().clone();
        let end = before.tracks[0].clips[0].end().unwrap();
        state.scrub_to(end.saturating_sub(Time::from_ticks(1)));
        state.trim_start_to_playhead();
        assert_eq!(state.status(), &Status::Info(Message::NothingAtPlayhead), "not a raw refusal");
        assert_eq!(state.project(), &before);
    }

    #[test]
    fn a_speed_drag_is_one_undo_step_and_can_be_cancelled() {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        state.add_clip_from_file(0, Path::new("a.mp4"));
        let key = ClipKey { track: 0, clip: 0 };
        state.select(Some(key));
        let before = state.project().tracks[0].clips[0].clone();
        state.live_speed(2.0);
        state.live_speed(4.0);
        assert_eq!(state.project().tracks[0].clips[0].speed, 4.0, "the preview follows the drag");
        state.commit_live();
        let done = state.project().tracks[0].clips[0].clone();
        assert_eq!((done.speed, done.source_in, done.start), (4.0, before.source_in, before.start));
        assert_eq!(done.source_end(), before.source_end(), "the same part of the source");
        state.undo();
        assert_eq!(state.project().tracks[0].clips[0], before, "one undo step");

        state.live_speed(2.0);
        state.cancel_live();
        assert_eq!(state.project().tracks[0].clips[0], before, "Esc restores the clip");
    }

    #[test]
    fn a_speed_against_a_neighbour_stays_where_it_is_with_no_refusal() {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        state.add_clip_from_file(0, Path::new("a.mp4"));
        state.add_clip_from_file(0, Path::new("b.mp4"));
        let key = ClipKey { track: 0, clip: 0 };
        state.select(Some(key));
        let before = state.project().tracks[0].clips[0].clone();
        state.live_speed(0.5);
        state.commit_live();
        assert_eq!(state.project().tracks[0].clips[0], before, "the next clip touches it");
        assert!(!matches!(state.status(), Status::Refused(_)), "{:?}", state.status());
    }

    /// Two clips with a gap: the first at 0, the second where the first would need less than
    /// twice its length.
    fn state_with_a_gap() -> (EditorState, ClipKey, Time) {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        state.add_clip_from_file(0, Path::new("a.mp4"));
        let length = state.project().tracks[0].clips[0].duration;
        let next = Time::from_ticks(length.ticks() * 3 / 2);
        state.set_playhead(next);
        state.add_clip_from_file(0, Path::new("b.mp4"));
        let key = ClipKey { track: 0, clip: 0 };
        state.select(Some(key));
        (state, key, next)
    }

    #[test]
    fn a_fast_drag_lands_exactly_on_the_next_clip_with_no_refusal() {
        let (mut state, key, next) = state_with_a_gap();
        state.live_speed(0.1);
        assert!(!matches!(state.status(), Status::Refused(_)), "{:?}", state.status());
        state.commit_live();
        let clip = state.project().tracks[0].clips[0].clone();
        assert!(clip.end().unwrap() <= next, "stays clear of the next clip");
        assert_ne!(clip.speed, 1.0, "the clip did change");
        // One step slower no longer fits: this is the limit, not a stop short of it.
        assert!(matches!(set_speed(state.project(), key, clip.speed - 0.001), Err(EditError::SpeedOverlap { .. })));
    }

    #[test]
    fn a_fast_drag_to_a_short_length_stops_where_the_fades_still_fit() {
        let (mut state, key, _) = state_with_a_gap();
        let length = state.project().tracks[0].clips[0].duration;
        state.live_fades(Time::from_ticks(length.ticks() / 2), Time::from_ticks(length.ticks() / 4));
        state.commit_live();
        state.live_speed(10.0);
        assert!(!matches!(state.status(), Status::Refused(_)), "{:?}", state.status());
        state.commit_live();
        let clip = state.project().tracks[0].clips[0].clone();
        assert!(clip.speed < 10.0 && clip.speed > 1.0, "{}", clip.speed);
        assert!(clip.duration >= clip.fade_in.saturating_add(clip.fade_out));
        assert!(set_speed(state.project(), key, clip.speed + 0.001).is_err(), "one step faster does not fit");
    }

    #[test]
    fn speed_limits_follow_the_gap_the_fades_and_the_start_transition() {
        let (state, key, _) = state_with_a_gap();
        let (lowest, highest) = speed_limits(state.project(), key).unwrap();
        assert!((lowest - 2.0 / 3.0).abs() < 0.002, "{lowest}");
        assert_eq!(highest, 10.0);
        let second = ClipKey { track: 0, clip: 1 };
        assert_eq!(speed_limits(state.project(), second), Some((0.1, 10.0)), "nothing after the last clip");
        let mut joined = state.project().clone();
        let transition = engine::Transition { after_clip: 0, package: "fade".to_owned(), duration: joined.tracks[0].clips[1].duration, params: Default::default() };
        joined.tracks[0].transitions.push(transition);
        assert_eq!(speed_limits(&joined, key), None, "a transition on its end");
        let (_, highest) = speed_limits(&joined, second).unwrap();
        assert!((highest - 2.0).abs() < 0.002, "half of the transition sits on the head of the clip: {highest}");
        assert_eq!(speed_limits(state.project(), ClipKey { track: 5, clip: 0 }), None);
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
    fn the_output_size_is_even_within_limits_and_undoes() {
        let mut state = state_with_clip();
        let before = state.project().clone();
        state.set_output_size(1281, 719);
        assert_eq!((state.project().output.width, state.project().output.height), (1280, 718));
        state.set_output_size(1, 99_999);
        assert_eq!((state.project().output.width, state.project().output.height), (2, 7680));
        state.undo();
        state.undo();
        assert_eq!(state.project(), &before);
    }

    #[test]
    fn the_frame_rate_changes_and_undoes() {
        let mut state = state_with_clip();
        let before = state.project().clone();
        state.set_frame_duration(OUTPUT_FRAME_DURATIONS[5]);
        assert_eq!(state.project().output.frame_duration, Time::from_ticks(1500));
        state.undo();
        assert_eq!(state.project(), &before);
    }

    #[test]
    fn a_drop_with_no_track_makes_one_of_its_kind_in_one_undo_step() {
        let mut state = EditorState::default();
        let at = seconds(2);
        let added = state.drop_media(&media("a.mp4", crate::media::MediaKind::Video, Some(3)), None, at).unwrap();
        assert_eq!(added, ClipKey { track: 0, clip: 0 });
        assert_eq!(state.project().tracks[0].kind, TrackKind::Video);
        assert_eq!(clips(state.project(), 0), vec![(at, seconds(3))]);
        assert_eq!(state.selection(), Some(added));
        state.undo();
        assert!(state.project().tracks.is_empty(), "the track goes with the clip");
    }

    #[test]
    fn a_drop_on_a_track_lands_at_the_nearest_free_place() {
        let mut state = state_with_clip();
        let before = clips(state.project(), 0);
        let added = state.drop_media(&media("b.mp4", crate::media::MediaKind::Video, Some(1)), Some(0), seconds(1)).unwrap();
        assert_eq!(state.project().tracks.len(), 2, "no new track");
        assert_eq!(added.track, 0);
        assert_eq!(state.project().tracks[0].clips[added.clip].start, before[0].1, "after the clip it was dropped on");
    }

    #[test]
    fn a_drop_on_the_wrong_kind_or_a_locked_track_is_refused_and_makes_no_track() {
        let mut state = state_with_clip();
        let before = state.project().clone();
        assert_eq!(state.drop_media(&media("b.mp4", crate::media::MediaKind::Video, Some(1)), Some(1), seconds(1)), None);
        assert!(matches!(state.status(), Status::Refused(_)));
        state.toggle_track_locked(0);
        let locked = state.project().clone();
        assert_eq!(state.drop_media(&media("b.mp4", crate::media::MediaKind::Video, Some(1)), Some(0), seconds(30)), None);
        assert_eq!(state.project(), &locked);
        assert_eq!(locked.tracks.len(), before.tracks.len());
    }

    #[test]
    fn a_clip_moved_outside_the_rows_gets_a_new_track_in_one_undo_step() {
        let mut state = state_with_clip();
        let before = state.project().clone();
        let hit = ClipHit { key: key(0), zone: crate::timeline::hit::ClipZone::Body };
        let drag = begin_drag(state.project(), hit, Time::ZERO).unwrap();
        state.finish_drag_on_new_track(drag, seconds(4), false);
        assert_eq!(state.project().tracks.len(), 3);
        assert_eq!(state.project().tracks[2].kind, TrackKind::Video);
        assert_eq!(clips(state.project(), 2), vec![(seconds(4), DEFAULT_CLIP_DURATION)]);
        assert_eq!(state.selection(), Some(ClipKey { track: 2, clip: 0 }));
        assert!(state.project().tracks[0].clips.is_empty());
        state.undo();
        assert_eq!(state.project(), &before);
    }

    #[test]
    fn a_clip_of_a_locked_track_stays_when_moved_outside_the_rows() {
        let mut state = state_with_clip();
        let hit = ClipHit { key: key(0), zone: crate::timeline::hit::ClipZone::Body };
        let drag = begin_drag(state.project(), hit, Time::ZERO).unwrap();
        state.toggle_track_locked(0);
        let before = state.project().clone();
        state.finish_drag_on_new_track(drag, seconds(4), false);
        assert_eq!(state.project(), &before);
    }

    /// Two clips on track 0: a at 0 for the default 10 s, and b after a gap, at 20 s.
    fn state_with_two_clips() -> EditorState {
        let mut state = state_with_clip();
        state.add_clip_from_file(0, Path::new("C:/media/b.mp4"));
        let clip = state.project().tracks[0].clips[1].clone();
        let moved = EditCommand::Move { key: key(1), track: 0, desired_start: seconds(20) };
        assert_eq!(clip.start, DEFAULT_CLIP_DURATION);
        state.finish_drag(moved);
        state
    }

    /// Half the snap tolerance at the default zoom: close enough to snap.
    fn nudge() -> Time {
        Time::from_ticks(snap_tolerance(state_view_pps()).ticks() / 2)
    }

    fn state_view_pps() -> f64 {
        EditorState::default().view.pixels_per_second()
    }

    #[test]
    fn snapped_drag_pulls_to_an_edge_unless_free_or_off() {
        let mut state = state_with_two_clips();
        let hit = ClipHit { key: key(1), zone: crate::timeline::hit::ClipZone::Body };
        let drag = begin_drag(state.project(), hit, seconds(20)).unwrap();
        let near = DEFAULT_CLIP_DURATION.saturating_add(nudge());
        let snapped = state.snapped_drag(&drag, near, Some(0), false);
        assert_eq!(snapped.command, EditCommand::Move { key: key(1), track: 0, desired_start: DEFAULT_CLIP_DURATION });
        assert_eq!(snapped.guide, Some(DEFAULT_CLIP_DURATION));
        let free = state.snapped_drag(&drag, near, Some(0), true);
        assert_eq!(free, Snapped { command: EditCommand::Move { key: key(1), track: 0, desired_start: near }, guide: None });
        state.ui.snap_off = true;
        assert_eq!(state.snapped_drag(&drag, near, Some(0), false), free);
    }

    #[test]
    fn a_clip_moved_outside_the_rows_snaps_unless_free() {
        let mut state = state_with_two_clips();
        let hit = ClipHit { key: key(1), zone: crate::timeline::hit::ClipZone::Body };
        let drag = begin_drag(state.project(), hit, seconds(20)).unwrap();
        let near = DEFAULT_CLIP_DURATION.saturating_add(nudge());
        let before = state.project().clone();
        state.finish_drag_on_new_track(drag, near, true);
        assert_eq!(clips(state.project(), 2)[0].0, near);
        state.undo();
        assert_eq!(state.project(), &before);
        state.finish_drag_on_new_track(drag, near, false);
        assert_eq!(clips(state.project(), 2)[0].0, DEFAULT_CLIP_DURATION);
    }

    #[test]
    fn snapped_drop_time_pulls_the_item_to_an_edge_unless_free_or_off() {
        let mut state = state_with_two_clips();
        let near = DEFAULT_CLIP_DURATION.saturating_add(nudge());
        assert_eq!(state.snapped_drop_time(near, Some(seconds(3)), Some(0), false), DEFAULT_CLIP_DURATION);
        // By its end: 3 s long, ending a little past where b starts.
        let end_near = seconds(20).saturating_add(nudge()).saturating_sub(seconds(3));
        assert_eq!(state.snapped_drop_time(end_near, Some(seconds(3)), Some(0), false), seconds(17));
        assert_eq!(state.snapped_drop_time(end_near, Some(seconds(3)), None, false), seconds(17));
        assert_eq!(state.snapped_drop_time(near, Some(seconds(3)), Some(0), true), near);
        state.ui.snap_off = true;
        assert_eq!(state.snapped_drop_time(near, None, Some(0), false), near);
    }

    #[test]
    fn the_source_in_field_cuts_the_head_and_goes_back_to_the_first_frame() {
        let mut state = state_with_clip();
        let whole = DEFAULT_CLIP_DURATION;
        state.apply_clip_field(ClipField::SourceIn(2.0));
        let clip = &state.project().tracks[0].clips[0];
        assert_eq!((clip.start, clip.source_in, clip.duration), (Time::ZERO, seconds(2), whole.saturating_sub(seconds(2))));
        assert_eq!(state.selection(), Some(key(0)));
        state.apply_clip_field(ClipField::SourceIn(0.0));
        let clip = &state.project().tracks[0].clips[0];
        assert_eq!((clip.source_in, clip.duration), (Time::ZERO, whole));
    }

    #[test]
    fn resetting_the_placement_clears_it_in_one_undo_step() {
        let mut state = state_with_clip();
        state.select(Some(key(0)));
        let placed = Some(engine::ClipPlacement { x: 0.1, y: 0.2, width: 0.5 });
        assert!(state.run(|project| set_placement(project, key(0), placed)));
        state.reset_placement();
        assert_eq!(state.project().tracks[0].clips[0].placement, None);
        state.undo();
        assert_eq!(state.project().tracks[0].clips[0].placement, placed, "one undo brings it back");
        state.undo();
        assert_eq!(state.project().tracks[0].clips[0].placement, None, "and the step before it was the clip without one");
    }

    #[test]
    fn resetting_the_placement_of_a_clip_on_a_locked_track_changes_nothing() {
        let mut state = state_with_clip();
        state.select(Some(key(0)));
        let placed = Some(engine::ClipPlacement { x: 0.1, y: 0.2, width: 0.5 });
        assert!(state.run(|project| set_placement(project, key(0), placed)));
        state.toggle_track_locked(0);
        let before = state.project().clone();
        state.reset_placement();
        assert_eq!(state.project(), &before);
    }

    #[test]
    fn the_source_in_field_stops_at_the_next_clip_and_at_the_shortest_length() {
        let mut state = state_with_clip();
        state.apply_clip_field(ClipField::SourceIn(1_000.0));
        let frame = state.project().output.frame_duration;
        assert_eq!(state.project().tracks[0].clips[0].duration, frame, "a cut leaves one frame");
        state.apply_clip_field(ClipField::SourceIn(0.0));
        assert_eq!(state.project().tracks[0].clips[0].source_in, Time::ZERO);

        state.apply_clip_field(ClipField::SourceIn(2.0));
        let end = state.project().tracks[0].clips[0].end().unwrap();
        let second = new_clip("b.mp4", end.saturating_add(seconds(1)), seconds(1));
        assert!(state.run(|project| add_clip(project, 0, second)));
        state.apply_clip_field(ClipField::SourceIn(0.0));
        let clip = &state.project().tracks[0].clips[0];
        assert_eq!(clip.source_in, seconds(1), "only the gap of one second can be given back");
        assert_eq!(clip.end(), Some(end.saturating_add(seconds(1))));
    }

    #[test]
    fn the_duration_field_counts_source_time_at_another_speed() {
        let mut state = state_with_clip();
        assert!(state.run(|project| set_speed(project, key(0), 2.0)));
        state.apply_clip_field(ClipField::Duration(4.0));
        assert_eq!(state.project().tracks[0].clips[0].duration, seconds(2));
    }

    #[test]
    fn what_remains_of_the_source_needs_a_probed_length_and_counts_the_cut() {
        let mut state = state_with_clip();
        assert_eq!(state.source_remaining(key(0)), None, "nothing stated the length of the file");
        let source = state.project().tracks[0].clips[0].source.clone();
        state.library.add(crate::media::MediaItem { source, name: "a".to_owned(), kind: crate::media::MediaKind::Video, duration: Some(seconds(20)), estimated: false });
        assert_eq!(state.source_remaining(key(0)), Some(seconds(20)));
        state.apply_clip_field(ClipField::SourceIn(2.0));
        assert_eq!(state.source_remaining(key(0)), Some(seconds(18)));
    }

    #[test]
    fn the_source_in_field_reaches_the_first_frame_at_a_speed_off_the_grid() {
        let mut state = state_with_clip();
        state.apply_clip_field(ClipField::SourceIn(2.0));
        assert!(state.run(|project| set_speed(project, key(0), 1.237)));
        state.apply_clip_field(ClipField::SourceIn(0.0));
        assert_eq!(state.project().tracks[0].clips[0].source_in, Time::ZERO);
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
        project.tracks.push(engine::Track { kind: TrackKind::Video, clips: vec![new_clip("C:/v/x.mp4", Time::ZERO, seconds(4))], transitions: Vec::new(), locked: false, hidden: false });
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
