//! Editor state without a window: the project history, the selection and the status line. The UI
//! calls these methods and draws the result; every timeline change goes through an engine command.

use crate::timeline::drag::{ClipDrag, EditCommand};
use crate::timeline::view::TimelineView;
use engine::{
    add_clip, add_track, clip_at, delete_clip, nearest_free_start, split_clip, AudioOutput, Clip, ClipKey, EditError, EditHistory,
    Output, Project, Time, TrackKind, SUPPORTED_VERSION,
};
use std::path::Path;

const UNDO_DEPTH: usize = 200;
const DRAG_FIRST: &str = "Finish the drag first";

/// Length given to a new clip. The project does not record source lengths and no backend call
/// reports one without opening a decoder.
pub const DEFAULT_CLIP_DURATION: Time = Time::from_ticks(900_000);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Idle,
    Info(String),
    Refused(String),
}

pub struct EditorState {
    history: EditHistory,
    selection: Option<ClipKey>,
    playhead: Time,
    status: Status,
    pub view: TimelineView,
    pub drag: Option<ClipDrag>,
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
            view: TimelineView::default(),
            drag: None,
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

    /// A drag holds a clip key, and any edit could make that key name another clip, so edits wait
    /// for the drop. True, with a status, while a drag is in progress.
    fn drag_blocks_edits(&mut self) -> bool {
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
        if self.drag_blocks_edits() {
            return false;
        }
        match self.history.apply(command) {
            Ok(_) => {
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
        if self.drag_blocks_edits() {
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
        if self.drag_blocks_edits() {
            return;
        }
        if self.history.undo() {
            self.after_history_step();
        } else {
            self.status = Status::Info("Nothing to undo".to_owned());
        }
    }

    pub fn redo(&mut self) {
        if self.drag_blocks_edits() {
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
        self.selection = None;
        self.status = Status::Idle;
    }
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
    fn a_clip_is_labelled_by_its_file_name() {
        assert_eq!(clip_label("C:/media/a.mp4"), "a.mp4");
        assert_eq!(clip_label("b.wav"), "b.wav");
    }
}
