//! Turns a pointer drag into an engine edit command.

use super::hit::{ClipHit, ClipZone};
use engine::{move_clip, nearest_free_start, trim_end, trim_start, ClipKey, EditError, Project, Time};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragKind {
    Move,
    TrimStart,
    TrimEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipDrag {
    pub key: ClipKey,
    pub kind: DragKind,
    /// Pointer time at the press minus the time of the dragged edge, so the edge does not jump.
    pub grab_offset: Time,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditCommand {
    Move { key: ClipKey, track: usize, desired_start: Time },
    TrimStart { key: ClipKey, new_start: Time },
    TrimEnd { key: ClipKey, new_end: Time },
}

/// Where a command would put its clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipSpan {
    pub track: usize,
    pub start: Time,
    pub end: Time,
}

/// Starts a drag on `hit`, pressed at `press_time`. `None` when the clip no longer exists.
pub fn begin_drag(project: &Project, hit: ClipHit, press_time: Time) -> Option<ClipDrag> {
    let clip = project.tracks.get(hit.key.track)?.clips.get(hit.key.clip)?;
    let (kind, edge) = match hit.zone {
        ClipZone::Body => (DragKind::Move, clip.start),
        ClipZone::StartEdge => (DragKind::TrimStart, clip.start),
        ClipZone::EndEdge => (DragKind::TrimEnd, clip.start.saturating_add(clip.duration)),
    };
    Some(ClipDrag { key: hit.key, kind, grab_offset: press_time.saturating_sub(edge) })
}

/// The command for the pointer at `pointer_time`. `pointer_track` is the row under the pointer; a
/// move stays on its own track when the pointer is outside the rows.
pub fn drag_command(drag: &ClipDrag, pointer_time: Time, pointer_track: Option<usize>) -> EditCommand {
    let edge = pointer_time.saturating_sub(drag.grab_offset).max(Time::ZERO);
    match drag.kind {
        DragKind::Move => EditCommand::Move {
            key: drag.key,
            track: pointer_track.unwrap_or(drag.key.track),
            desired_start: edge,
        },
        DragKind::TrimStart => EditCommand::TrimStart { key: drag.key, new_start: edge },
        DragKind::TrimEnd => EditCommand::TrimEnd { key: drag.key, new_end: edge },
    }
}

impl EditCommand {
    pub fn apply(&self, project: &Project) -> Result<Project, EditError> {
        match *self {
            EditCommand::Move { key, track, desired_start } => move_clip(project, key, track, desired_start),
            EditCommand::TrimStart { key, new_start } => trim_start(project, key, new_start),
            EditCommand::TrimEnd { key, new_end } => trim_end(project, key, new_end),
        }
    }

    pub fn key(&self) -> ClipKey {
        match *self {
            EditCommand::Move { key, .. } | EditCommand::TrimStart { key, .. } | EditCommand::TrimEnd { key, .. } => key,
        }
    }

    /// Where the clip lands if the engine accepts the command. A move onto a track of another kind
    /// lands nowhere, because the engine refuses it.
    pub fn landing(&self, project: &Project) -> Option<ClipSpan> {
        let key = self.key();
        let clip = project.tracks.get(key.track)?.clips.get(key.clip)?;
        let end = clip.start.saturating_add(clip.duration);
        match *self {
            EditCommand::Move { track, desired_start, .. } => {
                let target = project.tracks.get(track)?;
                if target.kind != project.tracks[key.track].kind {
                    return None;
                }
                let ignore = (track == key.track).then_some(key.clip);
                let start = nearest_free_start(target, ignore, clip.duration, desired_start)?;
                Some(ClipSpan { track, start, end: start.saturating_add(clip.duration) })
            }
            EditCommand::TrimStart { new_start, .. } => Some(ClipSpan { track: key.track, start: new_start, end }),
            EditCommand::TrimEnd { new_end, .. } => Some(ClipSpan { track: key.track, start: clip.start, end: new_end }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::new_clip;
    use engine::{add_clip, add_track, Output, TrackKind};

    fn seconds(value: i64) -> Time {
        Time::from_seconds(value).unwrap()
    }

    fn project() -> Project {
        let empty = Project {
            version: engine::SUPPORTED_VERSION,
            output: Output { width: 1920, height: 1080, frame_duration: Time::from_ticks(3000), audio: None },
            tracks: Vec::new(),
        };
        let video = add_track(&empty, TrackKind::Video).unwrap();
        let other = add_track(&video, TrackKind::Video).unwrap();
        let audio = add_track(&other, TrackKind::Audio).unwrap();
        let a = add_clip(&audio, 0, new_clip("a.mp4", seconds(2), seconds(2))).unwrap();
        add_clip(&a, 0, new_clip("b.mp4", seconds(10), seconds(2))).unwrap()
    }

    fn key(clip: usize) -> ClipKey {
        ClipKey { track: 0, clip }
    }

    #[test]
    fn a_move_keeps_the_grab_point_under_the_pointer() {
        let project = project();
        let hit = ClipHit { key: key(0), zone: ClipZone::Body };
        let drag = begin_drag(&project, hit, seconds(3)).unwrap();
        assert_eq!(drag.grab_offset, seconds(1));
        let command = drag_command(&drag, seconds(6), Some(1));
        assert_eq!(command, EditCommand::Move { key: key(0), track: 1, desired_start: seconds(5) });
    }

    #[test]
    fn trims_follow_their_edge() {
        let project = project();
        let start = begin_drag(&project, ClipHit { key: key(0), zone: ClipZone::StartEdge }, seconds(2)).unwrap();
        assert_eq!(start.kind, DragKind::TrimStart);
        assert_eq!(drag_command(&start, seconds(3), None), EditCommand::TrimStart { key: key(0), new_start: seconds(3) });
        let end = begin_drag(&project, ClipHit { key: key(0), zone: ClipZone::EndEdge }, seconds(4)).unwrap();
        assert_eq!(end.kind, DragKind::TrimEnd);
        assert_eq!(drag_command(&end, seconds(5), None), EditCommand::TrimEnd { key: key(0), new_end: seconds(5) });
    }

    #[test]
    fn a_drag_cannot_ask_for_a_negative_time() {
        let project = project();
        let drag = begin_drag(&project, ClipHit { key: key(0), zone: ClipZone::Body }, seconds(3)).unwrap();
        let command = drag_command(&drag, Time::from_ticks(-90_000), Some(0));
        assert_eq!(command, EditCommand::Move { key: key(0), track: 0, desired_start: Time::ZERO });
    }

    #[test]
    fn a_pointer_outside_the_rows_keeps_the_track() {
        let drag = ClipDrag { key: key(0), kind: DragKind::Move, grab_offset: Time::ZERO };
        assert!(matches!(drag_command(&drag, seconds(1), None), EditCommand::Move { track: 0, .. }));
    }

    #[test]
    fn landing_matches_what_the_engine_does() {
        let project = project();
        // Asked to overlap the clip at 10 to 12: the engine puts it just before.
        let command = EditCommand::Move { key: key(0), track: 0, desired_start: seconds(9) };
        let span = command.landing(&project).unwrap();
        assert_eq!(span, ClipSpan { track: 0, start: seconds(8), end: seconds(10) });
        let next = command.apply(&project).unwrap();
        assert_eq!(engine::clip_at(&next, 0, span.start), Some(key(0)));
    }

    #[test]
    fn landing_refuses_a_track_of_another_kind() {
        let project = project();
        let command = EditCommand::Move { key: key(0), track: 2, desired_start: seconds(1) };
        assert_eq!(command.landing(&project), None);
        assert!(matches!(command.apply(&project), Err(EditError::TrackKindMismatch { .. })));
    }

    #[test]
    fn begin_drag_rejects_a_stale_key() {
        assert_eq!(begin_drag(&project(), ClipHit { key: key(9), zone: ClipZone::Body }, seconds(1)), None);
    }
}
