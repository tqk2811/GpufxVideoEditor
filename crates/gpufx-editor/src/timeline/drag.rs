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

/// Pulls a trim into the range the engine accepts, so a drag that overshoots lands on the limit
/// instead of being refused whole. A move is returned as it is. A transition on the edge is not
/// handled here: the engine refuses that and the editor shows why.
pub fn clamp_trim(project: &Project, command: EditCommand) -> EditCommand {
    let key = command.key();
    let Some(track) = project.tracks.get(key.track) else { return command };
    let Some(clip) = track.clips.get(key.clip) else { return command };
    let Some(end) = clip.end() else { return command };
    let minimum = clip.fade_in.saturating_add(clip.fade_out).max(project.output.frame_duration);
    match command {
        EditCommand::TrimStart { new_start, .. } => {
            let previous_end = key.clip.checked_sub(1).and_then(|index| track.clips.get(index)).and_then(|previous| previous.end()).unwrap_or(Time::ZERO);
            // The source cannot be read before its first frame.
            let source_limit = clip.start.saturating_sub(clip.timeline_ticks(clip.source_in)).max(Time::ZERO);
            let lower = previous_end.max(source_limit);
            // A clip already shorter than the minimum is not shrunk further. The engine rounds a
            // start up to the grid of the speed, so the limit is the last grid position before it.
            let room = end.saturating_sub(minimum).saturating_sub(clip.start).ticks().max(0);
            let upper = clip.start.saturating_add(Time::from_ticks(room - room.rem_euclid(clip.speed_grid())));
            EditCommand::TrimStart { key, new_start: new_start.max(lower).min(upper) }
        }
        EditCommand::TrimEnd { new_end, .. } => {
            let lower = clip.start.saturating_add(minimum).min(end);
            let mut clamped = new_end.max(lower);
            if let Some(next) = track.clips.get(key.clip + 1) {
                clamped = clamped.min(next.start);
            }
            EditCommand::TrimEnd { key, new_end: clamped }
        }
        EditCommand::Move { .. } => command,
    }
}

/// `wanted` as the source position clip `key` may begin at: not before the source, not so far back
/// that it runs into the next clip, and leaving it at least as long as a trim would. On the speed
/// grid, so the engine takes it as it is.
pub fn clamp_source_in(project: &Project, key: ClipKey, wanted: Time) -> Option<Time> {
    let track = project.tracks.get(key.track)?;
    let clip = track.clips.get(key.clip)?;
    let end = clip.end()?;
    let grid = clip.speed_grid();
    let minimum = clip.fade_in.saturating_add(clip.fade_out).max(project.output.frame_duration);
    let room = track.clips.get(key.clip + 1).map_or(i64::MAX, |next| next.start.saturating_sub(end).ticks()).max(0);
    let back = clip.timeline_ticks(clip.source_in).ticks().min(room).max(0);
    if wanted == Time::ZERO && back == clip.timeline_ticks(clip.source_in).ticks() {
        return Some(Time::ZERO);
    }
    let lowest = -(back - back.rem_euclid(grid));
    let forward = clip.duration.saturating_sub(minimum).ticks().max(0);
    let highest = forward - forward.rem_euclid(grid);
    // Not `clamp`: a clip already shorter than the minimum may have nothing to give either way.
    let delta = clip.timeline_ticks(wanted.saturating_sub(clip.source_in)).ticks().max(lowest).min(highest);
    Some(clip.source_in.saturating_add(clip.source_offset(Time::from_ticks(delta - delta.rem_euclid(grid)))).max(Time::ZERO))
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
    /// or a locked one lands nowhere, because the engine refuses it.
    pub fn landing(&self, project: &Project) -> Option<ClipSpan> {
        let key = self.key();
        let source = project.tracks.get(key.track)?;
        if source.locked {
            return None;
        }
        let clip = source.clips.get(key.clip)?;
        let end = clip.start.saturating_add(clip.duration);
        match *self {
            EditCommand::Move { track, desired_start, .. } => {
                let target = project.tracks.get(track)?;
                if target.kind != source.kind || target.locked {
                    return None;
                }
                let ignore = (track == key.track).then_some(key.clip);
                let start = nearest_free_start(target, ignore, clip.duration, desired_start)?;
                Some(ClipSpan { track, start, end: start.saturating_add(clip.duration) })
            }
            EditCommand::TrimStart { new_start, .. } => Some(ClipSpan { track: key.track, start: clip.snapped_start(new_start), end }),
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
    fn a_start_trim_stops_where_the_source_starts_at_the_speed_of_the_clip() {
        let mut project = project();
        // Clip b at 10 to 12 with one source second before its in point, at 2x: that second is half
        // a second of timeline.
        let clip = &mut project.tracks[0].clips[1];
        clip.source_in = seconds(1);
        clip.speed = 2.0;
        let command = clamp_trim(&project, EditCommand::TrimStart { key: key(1), new_start: seconds(5) });
        assert_eq!(command, EditCommand::TrimStart { key: key(1), new_start: Time::from_ticks(9 * 90_000 + 45_000) });
        assert!(command.apply(&project).is_ok(), "the limit is a position the engine accepts");
    }

    #[test]
    fn a_start_trim_lands_where_the_engine_puts_it_at_another_speed() {
        let mut project = project();
        project.tracks[0].clips[1].speed = 1.5;
        let command = EditCommand::TrimStart { key: key(1), new_start: Time::from_ticks(10 * 90_000 + 1) };
        let span = command.landing(&project).unwrap();
        let next = command.apply(&project).unwrap();
        assert_eq!(next.tracks[0].clips[1].start, span.start);
        assert_eq!(engine::clip_at(&next, 0, span.start), Some(key(1)), "the clip is found where it landed");
    }

    #[test]
    fn landing_refuses_a_track_of_another_kind() {
        let project = project();
        let command = EditCommand::Move { key: key(0), track: 2, desired_start: seconds(1) };
        assert_eq!(command.landing(&project), None);
        assert!(matches!(command.apply(&project), Err(EditError::TrackKindMismatch { .. })));
    }

    #[test]
    fn nothing_lands_on_or_from_a_locked_track() {
        let mut project = project();
        project.tracks[1].locked = true;
        let onto = EditCommand::Move { key: key(0), track: 1, desired_start: seconds(1) };
        assert_eq!(onto.landing(&project), None);
        assert!(matches!(onto.apply(&project), Err(EditError::TrackLocked { track: 1 })));
        project.tracks[1].locked = false;
        project.tracks[0].locked = true;
        assert_eq!(EditCommand::TrimEnd { key: key(0), new_end: seconds(3) }.landing(&project), None);
    }

    fn limits_project() -> Project {
        // Track 0: a at 2 to 4 (source_in 1 s), b at 6 to 8 (source_in 0), c at 10 to 12.
        let mut project = project();
        let clips = &mut project.tracks[0].clips;
        clips[0].source_in = seconds(1);
        let mut middle = new_clip("m.mp4", seconds(6), seconds(2));
        middle.source_in = seconds(5);
        clips.insert(1, middle);
        project
    }

    #[test]
    fn a_start_trim_stops_at_the_previous_clip_and_at_the_source_start() {
        let project = limits_project();
        // The middle clip cannot reach back past the end of the clip before it (4 s).
        let command = EditCommand::TrimStart { key: key(1), new_start: seconds(3) };
        assert_eq!(clamp_trim(&project, command), EditCommand::TrimStart { key: key(1), new_start: seconds(4) });
        assert!(clamp_trim(&project, command).apply(&project).is_ok());
        // The first clip has 1 s of source before it, so it can start at 1 s and not before.
        let command = EditCommand::TrimStart { key: key(0), new_start: Time::ZERO };
        assert_eq!(clamp_trim(&project, command), EditCommand::TrimStart { key: key(0), new_start: seconds(1) });
        assert!(clamp_trim(&project, command).apply(&project).is_ok());
    }

    #[test]
    fn a_start_trim_keeps_the_minimum_duration() {
        let project = limits_project();
        let command = EditCommand::TrimStart { key: key(1), new_start: seconds(9) };
        let clamped = clamp_trim(&project, command);
        assert_eq!(clamped, EditCommand::TrimStart { key: key(1), new_start: seconds(8).saturating_sub(Time::from_ticks(3000)) });
        assert!(clamped.apply(&project).is_ok());
    }

    #[test]
    fn the_minimum_duration_covers_both_fades() {
        let mut project = limits_project();
        project.tracks[0].clips[1].fade_in = Time::from_ticks(9000);
        project.tracks[0].clips[1].fade_out = Time::from_ticks(6000);
        let command = EditCommand::TrimEnd { key: key(1), new_end: seconds(6) };
        assert_eq!(clamp_trim(&project, command), EditCommand::TrimEnd { key: key(1), new_end: seconds(6).saturating_add(Time::from_ticks(15_000)) });
    }

    #[test]
    fn an_end_trim_stops_at_the_next_clip_and_has_no_upper_bound_on_the_last() {
        let project = limits_project();
        let command = EditCommand::TrimEnd { key: key(1), new_end: seconds(11) };
        assert_eq!(clamp_trim(&project, command), EditCommand::TrimEnd { key: key(1), new_end: seconds(10) });
        assert!(clamp_trim(&project, command).apply(&project).is_ok());
        let last = EditCommand::TrimEnd { key: key(2), new_end: seconds(500) };
        assert_eq!(clamp_trim(&project, last), last);
        let short = EditCommand::TrimEnd { key: key(2), new_end: seconds(10) };
        assert_eq!(clamp_trim(&project, short), EditCommand::TrimEnd { key: key(2), new_end: seconds(10).saturating_add(Time::from_ticks(3000)) });
    }

    #[test]
    fn a_trim_inside_the_limits_and_a_move_pass_through() {
        let project = limits_project();
        let trim = EditCommand::TrimEnd { key: key(1), new_end: seconds(7) };
        assert_eq!(clamp_trim(&project, trim), trim);
        let moved = EditCommand::Move { key: key(1), track: 0, desired_start: seconds(99) };
        assert_eq!(clamp_trim(&project, moved), moved);
        let stale = EditCommand::TrimEnd { key: key(9), new_end: seconds(7) };
        assert_eq!(clamp_trim(&project, stale), stale);
    }

    #[test]
    fn a_transition_on_the_edge_is_still_refused_after_clamping() {
        let mut project = limits_project();
        project.tracks[0].transitions.push(engine::Transition {
            after_clip: 1,
            package: "fade".to_owned(),
            duration: Time::from_ticks(3000),
            params: Default::default(),
        });
        let command = clamp_trim(&project, EditCommand::TrimEnd { key: key(1), new_end: seconds(7) });
        assert!(matches!(command.apply(&project), Err(EditError::TransitionOnEdge { .. })));
    }

    /// At 1.234x the grid is 500 ticks, and the end minus the fades is not a multiple of it.
    #[test]
    fn a_start_trim_to_the_limit_with_fades_lands_on_the_grid_and_is_accepted() {
        let mut project = limits_project();
        let clip = &mut project.tracks[0].clips[1];
        clip.speed = 1.234;
        clip.fade_in = Time::from_ticks(9000);
        clip.fade_out = Time::from_ticks(6100);
        let start = clip.start;
        let command = clamp_trim(&project, EditCommand::TrimStart { key: key(1), new_start: seconds(9) });
        let EditCommand::TrimStart { new_start, .. } = command else { panic!("a start trim") };
        assert_eq!(new_start.saturating_sub(start).ticks().rem_euclid(500), 0, "on the grid: {new_start:?}");
        let next = command.apply(&project).expect("the limit is a position the engine accepts");
        assert_eq!(next.tracks[0].clips[1].start, new_start);
    }

    #[test]
    fn begin_drag_rejects_a_stale_key() {
        assert_eq!(begin_drag(&project(), ClipHit { key: key(9), zone: ClipZone::Body }, seconds(1)), None);
    }
}
