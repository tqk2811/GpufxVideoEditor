//! Editor state without a window: the project history, the selection and the status line. The UI
//! calls these methods and draws the result; every timeline change goes through an engine command.

use crate::export::{EncoderOption, ExportProgress};
use crate::packages::{Catalog, PickerState};
use crate::playback::plan::{start_time, TransportCommand};
use crate::timeline::drag::{ClipDrag, EditCommand};
use crate::timeline::view::TimelineView;
use engine::{
    add_clip, add_effect, add_track, add_transition, audio_sample_count, clip_at, delete_clip, nearest_free_start, remove_effect, remove_transition, split_clip, AudioOutput, Clip, ClipKey,
    EditError, EditHistory, Effect, Output, Project, Time, TrackKind, Transition, SUPPORTED_VERSION,
};
use serde_json::Map;
use std::path::{Path, PathBuf};

const UNDO_DEPTH: usize = 200;
const DRAG_FIRST: &str = "Finish the drag first";
const EXPORT_RUNNING: &str = "Editing is off while an export runs";
const NO_NEIGHBOUR: &str = "A transition needs a selected clip that touches another clip on its track";
const NOTHING_TO_PLAY: &str = "Nothing to play: playback length follows the video frames, and audio alone has none yet";

/// Length given to a new clip. The project does not record source lengths and no backend call
/// reports one without opening a decoder.
pub const DEFAULT_CLIP_DURATION: Time = Time::from_ticks(900_000);
/// Length given to a new transition; parameters and lengths are edited in a later phase.
pub const DEFAULT_TRANSITION_DURATION: Time = Time::from_ticks(90_000);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Idle,
    Info(String),
    Refused(String),
    Error(String),
}

pub struct EditorState {
    history: EditHistory,
    selection: Option<ClipKey>,
    playhead: Time,
    status: Status,
    /// Counts the changes of the project; equal values mean an equal project.
    revision: u64,
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
    pub catalog: Catalog,
    pub picker: PickerState,
    pub encoders: Vec<EncoderOption>,
    pub encoder: usize,
    /// The running export, which turns editing, playback and scrubbing off.
    export: Option<ExportProgress>,
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
            catalog: Catalog::default(),
            picker: PickerState::default(),
            encoders: Vec::new(),
            encoder: 0,
            export: None,
        }
    }

    pub fn project(&self) -> &Project {
        self.history.current()
    }

    pub fn selection(&self) -> Option<ClipKey> {
        self.selection
    }

    pub fn playhead(&self) -> Time {
        self.playhead
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
        self.status = Status::Info(format!("Saved {}", file.display()));
        self.project_file = Some(file);
        self.saved_revision = self.revision;
    }

    pub fn report_error(&mut self, message: String) {
        self.status = Status::Error(message);
    }

    /// Replaces the project with a new or opened one: a fresh history, and a new revision so preview
    /// and audio rebuild. The new project counts as saved.
    pub fn replace_project(&mut self, project: Project, file: Option<PathBuf>) {
        if self.playing {
            self.transport.push(TransportCommand::Stop);
        }
        self.history = EditHistory::new(project, UNDO_DEPTH);
        self.revision += 1;
        self.saved_revision = self.revision;
        self.status = match &file {
            Some(file) => Status::Info(format!("Opened {}", file.display())),
            None => Status::Idle,
        };
        self.project_file = file;
        self.selection = None;
        self.playhead = Time::ZERO;
        self.playing = false;
        self.scrub_target = None;
        self.preview_error = None;
        self.playback_message = None;
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
        self.export = Some(ExportProgress { done: 0, total });
        self.status = Status::Info("Exporting".to_owned());
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

    /// Space: starts from the playhead or stops where it is. A timeline with no samples cannot play.
    pub fn toggle_playback(&mut self) {
        if self.is_exporting() {
            self.status = Status::Info(EXPORT_RUNNING.to_owned());
            return;
        }
        if self.playing {
            self.playing = false;
            self.transport.push(TransportCommand::Stop);
            return;
        }
        let end = audio_sample_count(self.project());
        if end <= 0 {
            self.status = Status::Info(NOTHING_TO_PLAY.to_owned());
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
        self.status = Status::Refused(error.to_string());
    }

    /// An export renders the project it started with, so edits wait for it. A drag holds a clip key,
    /// and any edit could make that key name another clip, so edits wait for the drop. True, with a
    /// status, while either is in progress.
    fn edits_blocked(&mut self) -> bool {
        if self.export.is_some() {
            self.status = Status::Info(EXPORT_RUNNING.to_owned());
            return true;
        }
        if self.drag.is_none() {
            return false;
        }
        self.status = Status::Info(DRAG_FIRST.to_owned());
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

    /// Adds a clip of [`DEFAULT_CLIP_DURATION`] for `path` to `track`, at the free position nearest
    /// the playhead.
    pub fn add_clip_from_file(&mut self, track: usize, path: &Path) {
        if self.edits_blocked() {
            return;
        }
        let Some(target) = self.project().tracks.get(track) else {
            self.refuse(&EditError::TrackNotFound { track });
            return;
        };
        let Some(start) = nearest_free_start(target, None, DEFAULT_CLIP_DURATION, self.playhead) else {
            self.refuse(&EditError::NoRoom { track });
            return;
        };
        let clip = new_clip(&path.to_string_lossy(), start, DEFAULT_CLIP_DURATION);
        if self.run(|project| add_clip(project, track, clip)) {
            self.selection = clip_at(self.project(), track, start);
            self.status = Status::Info("Clip added with the default length of 10 s".to_owned());
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

    /// Splits the selected clip at the playhead and selects the right half.
    pub fn split_selected(&mut self) {
        let Some(key) = self.selection else {
            self.status = Status::Info("Select a clip to split".to_owned());
            return;
        };
        let at = self.playhead;
        if self.run(|project| split_clip(project, key, at)) {
            self.selection = Some(ClipKey { track: key.track, clip: key.clip + 1 });
        }
    }

    /// The selected clip, or a status asking for one.
    fn selected_for(&mut self, action: &str) -> Option<ClipKey> {
        if self.selection.is_none() {
            self.status = Status::Info(format!("Select a clip to {action}"));
        }
        self.selection
    }

    /// Adds effect `package` to the selected clip with its default parameters.
    pub fn add_effect_package(&mut self, package: &str) {
        let Some(key) = self.selected_for("add an effect") else { return };
        let effect = Effect { package: package.to_owned(), params: Map::new() };
        self.run(|project| add_effect(project, key, effect));
    }

    pub fn remove_effect_at(&mut self, effect: usize) {
        let Some(key) = self.selected_for("remove an effect") else { return };
        self.run(|project| remove_effect(project, key, effect));
    }

    /// Adds transition `package` where the selected clip touches a neighbour on its track.
    pub fn add_transition_package(&mut self, package: &str) {
        let Some(key) = self.selected_for("add a transition") else { return };
        let Some(after_clip) = touching_pair(self.project(), key) else {
            self.status = Status::Refused(NO_NEIGHBOUR.to_owned());
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
            self.status = Status::Info("Select a clip to delete".to_owned());
            return;
        };
        if self.run(|project| delete_clip(project, key)) {
            self.selection = None;
        }
    }

    /// Drops the drag in progress without any edit.
    pub fn cancel_drag(&mut self) {
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
            self.status = Status::Info("Nothing to undo".to_owned());
        }
    }

    pub fn redo(&mut self) {
        if self.edits_blocked() {
            return;
        }
        if self.history.redo() {
            self.after_history_step();
        } else {
            self.status = Status::Info("Nothing to redo".to_owned());
        }
    }

    /// Clip keys name clips of one project, so the selection does not survive a step back or
    /// forward. There is no drag to drop here, because both steps wait for it.
    fn after_history_step(&mut self) {
        self.revision += 1;
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
        assert_eq!(state.status(), &Status::Info("Clip added with the default length of 10 s".to_owned()));
    }

    #[test]
    fn a_second_clip_lands_after_the_first() {
        let mut state = state_with_clip();
        state.add_clip_from_file(0, Path::new("b.mp4"));
        assert_eq!(clips(state.project(), 0)[1].0, DEFAULT_CLIP_DURATION);
        assert_eq!(state.selection(), Some(key(1)));
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
        assert_eq!(state.status(), &Status::Info("Nothing to undo".to_owned()));
        state.redo();
        assert_eq!(state.status(), &Status::Info("Nothing to redo".to_owned()));
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
        assert_eq!(state.status(), &Status::Info(DRAG_FIRST.to_owned()));
        state.set_playhead(seconds(5));
        state.split_selected();
        state.add_track(TrackKind::Video);
        state.add_clip_from_file(0, Path::new("c.mp4"));
        state.undo();
        state.redo();
        assert_eq!(state.project(), &before);
        assert_eq!(state.drag, drag);
        assert_eq!(state.selection(), Some(key(0)));
        assert_eq!(state.status(), &Status::Info(DRAG_FIRST.to_owned()));
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
        assert_eq!(state.status(), &Status::Info(NOTHING_TO_PLAY.to_owned()));
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
        assert_eq!(state.status(), &Status::Refused(NO_NEIGHBOUR.to_owned()));
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
        assert_eq!(state.status(), &Status::Info(EXPORT_RUNNING.to_owned()));
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
        state.end_export(Status::Error("disk full".to_owned()));
        assert_eq!(state.export_progress(), None);
        assert_eq!(state.status(), &Status::Error("disk full".to_owned()));
    }

    #[test]
    fn a_clip_is_labelled_by_its_file_name() {
        assert_eq!(clip_label("C:/media/a.mp4"), "a.mp4");
        assert_eq!(clip_label("b.wav"), "b.wav");
    }
}
