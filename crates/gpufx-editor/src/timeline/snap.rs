//! Magnetic snapping of clip drags to the edges of other clips, the playhead and the start of the
//! timeline. Nothing here knows about egui.

use super::drag::{clamp_trim, EditCommand};
use engine::{nearest_free_start, ClipKey, Project, Time, TICKS_PER_SECOND};

/// How close, in points on screen, an edge has to come to a snap point to be pulled onto it. On
/// screen and not in time, so zooming in still allows exact placement.
pub const SNAP_DISTANCE: f64 = 8.0;

/// [`SNAP_DISTANCE`] as a time at `pixels_per_second`.
pub fn snap_tolerance(pixels_per_second: f64) -> Time {
    if pixels_per_second <= 0.0 || pixels_per_second.is_nan() {
        return Time::ZERO;
    }
    Time::from_ticks((SNAP_DISTANCE / pixels_per_second * TICKS_PER_SECOND as f64).round() as i64)
}

/// The times an edge may snap to: the start of the timeline, the playhead, and the start and end of
/// every clip except `ignore`. Locked and hidden tracks count, their clips are on screen. Sorted
/// and without repeats.
pub fn snap_points(project: &Project, ignore: Option<ClipKey>, playhead: Time) -> Vec<Time> {
    let mut points = vec![Time::ZERO, playhead];
    for (track_index, track) in project.tracks.iter().enumerate() {
        for (clip_index, clip) in track.clips.iter().enumerate() {
            if ignore == Some(ClipKey { track: track_index, clip: clip_index }) {
                continue;
            }
            points.push(clip.start);
            if let Some(end) = clip.end() {
                points.push(end);
            }
        }
    }
    points.sort();
    points.dedup();
    points
}

/// A command after snapping, and the time to draw a guide line at when it snapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapped {
    pub command: EditCommand,
    pub guide: Option<Time>,
}

/// One way to retime a command so that an edge sits on a point.
struct Offer {
    distance: u64,
    command: EditCommand,
    point: Time,
    /// For a move, the start the clip has to land at for the edge to be on the point.
    start: Time,
}

/// Pulls the dragged edge of `command` onto the nearest of `points` within `tolerance`. A move has
/// two edges that compete, the start winning a tie. Only a retiming the engine would really put
/// there is taken, so a clip is never refused, nor moved further off, because of snapping; when
/// nothing fits the result is the plain [`clamp_trim`] of `command`.
pub fn snap_command(project: &Project, command: EditCommand, points: &[Time], tolerance: Time) -> Snapped {
    let unsnapped = Snapped { command: clamp_trim(project, command), guide: None };
    let key = command.key();
    let Some(clip) = project.tracks.get(key.track).and_then(|track| track.clips.get(key.clip)) else { return unsnapped };
    let limit = tolerance.ticks().max(0) as u64;
    let near = |edge: Time, point: Time| {
        let distance = edge.ticks().abs_diff(point.ticks());
        (distance <= limit).then_some(distance)
    };
    let mut offers = Vec::new();
    match command {
        EditCommand::Move { track, desired_start, .. } => {
            let end = desired_start.saturating_add(clip.duration);
            for &point in points {
                if let Some(distance) = near(desired_start, point) {
                    offers.push(Offer { distance, command: EditCommand::Move { key, track, desired_start: point }, point, start: point });
                }
            }
            for &point in points {
                let start = point.saturating_sub(clip.duration);
                if start < Time::ZERO {
                    continue;
                }
                if let Some(distance) = near(end, point) {
                    offers.push(Offer { distance, command: EditCommand::Move { key, track, desired_start: start }, point, start });
                }
            }
        }
        EditCommand::TrimStart { new_start, .. } => {
            for &point in points {
                if let Some(distance) = near(new_start, point) {
                    offers.push(Offer { distance, command: EditCommand::TrimStart { key, new_start: point }, point, start: point });
                }
            }
        }
        EditCommand::TrimEnd { new_end, .. } => {
            for &point in points {
                if let Some(distance) = near(new_end, point) {
                    offers.push(Offer { distance, command: EditCommand::TrimEnd { key, new_end: point }, point, start: point });
                }
            }
        }
    }
    // Stable, so at the same distance the start edge of a move, offered first, wins.
    offers.sort_by_key(|offer| offer.distance);
    for offer in offers {
        let candidate = clamp_trim(project, offer.command);
        let Some(span) = candidate.landing(project) else { continue };
        let lands = candidate.apply(project).is_ok() && match candidate {
            EditCommand::Move { .. } => span.start == offer.start,
            EditCommand::TrimEnd { .. } => span.end == offer.point,
            // The engine rounds a start to the grid of the speed.
            EditCommand::TrimStart { .. } => span.start.ticks().abs_diff(offer.point.ticks()) < clip.speed_grid() as u64,
        };
        if lands {
            return Snapped { command: candidate, guide: Some(offer.point) };
        }
    }
    unsnapped
}

/// Every way to put the start edge or the end edge of a span of `duration` at `start` on a point
/// within `tolerance`, as the start the span would take and the point, nearest first. At the same
/// distance the start edge comes first. A start before zero is left out.
fn span_offers(points: &[Time], start: Time, duration: Time, tolerance: Time) -> Vec<(Time, Time)> {
    let limit = tolerance.ticks().max(0) as u64;
    let end = start.saturating_add(duration);
    let mut offers = Vec::new();
    for &point in points {
        let distance = start.ticks().abs_diff(point.ticks());
        if distance <= limit {
            offers.push((distance, point, point));
        }
    }
    for &point in points {
        let distance = end.ticks().abs_diff(point.ticks());
        let new_start = point.saturating_sub(duration);
        if distance <= limit && new_start >= Time::ZERO {
            offers.push((distance, new_start, point));
        }
    }
    offers.sort_by_key(|offer| offer.0);
    offers.into_iter().map(|(_, new_start, point)| (new_start, point)).collect()
}

/// For a placement with no collision check: the start that puts the nearer of the start edge and
/// the end edge of a span of `duration` at `start` on the nearest point within `tolerance`, with
/// that point. `None` when no edge is near one.
pub fn snap_span(points: &[Time], start: Time, duration: Time, tolerance: Time) -> Option<(Time, Time)> {
    span_offers(points, start, duration, tolerance).into_iter().next()
}

/// For a clip of `duration` put on `track` as near `start` as the other clips allow: the nearest
/// snap whose start the track really has free, else `start` as it is. A missing track gives `start`.
pub fn snap_onto_track(project: &Project, track: usize, points: &[Time], start: Time, duration: Time, tolerance: Time) -> Time {
    let Some(target) = project.tracks.get(track) else { return start };
    span_offers(points, start, duration, tolerance)
        .into_iter()
        .find(|(new_start, _)| nearest_free_start(target, None, duration, *new_start) == Some(*new_start))
        .map_or(start, |(new_start, _)| new_start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::new_clip;
    use engine::{add_clip, add_track, Output, TrackKind};

    const SECOND: i64 = 90_000;

    fn seconds(value: i64) -> Time {
        Time::from_seconds(value).unwrap()
    }

    /// Track 0 (video): a at 2 to 4, b at 10 to 12. Track 1 (video) empty. Track 2 (audio) empty.
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

    /// A tenth of a second.
    fn tolerance() -> Time {
        Time::from_ticks(SECOND / 10)
    }

    fn move_to(clip: usize, track: usize, ticks: i64) -> EditCommand {
        EditCommand::Move { key: key(clip), track, desired_start: Time::from_ticks(ticks) }
    }

    #[test]
    fn a_move_start_snaps_to_the_end_of_another_clip() {
        let project = project();
        let points = snap_points(&project, Some(key(1)), Time::ZERO);
        let snapped = snap_command(&project, move_to(1, 0, 4 * SECOND + 4500), &points, tolerance());
        assert_eq!(snapped.command, move_to(1, 0, 4 * SECOND));
        assert_eq!(snapped.guide, Some(seconds(4)));
    }

    #[test]
    fn a_move_end_snaps_to_the_start_of_another_clip() {
        let project = project();
        let points = snap_points(&project, Some(key(0)), Time::ZERO);
        // Clip a is 2 s long: its end at 9.95 s is pulled onto 10 s, so it starts at 8 s.
        let snapped = snap_command(&project, move_to(0, 0, 8 * SECOND - 4500), &points, tolerance());
        assert_eq!(snapped.command, move_to(0, 0, 8 * SECOND));
        assert_eq!(snapped.guide, Some(seconds(10)));
    }

    #[test]
    fn the_nearest_point_wins() {
        let project = project();
        let points = [seconds(5), Time::from_ticks(5 * SECOND + 5400)];
        let snapped = snap_command(&project, move_to(0, 1, 5 * SECOND + 4500), &points, tolerance());
        assert_eq!(snapped.command, move_to(0, 1, 5 * SECOND + 5400));
        assert_eq!(snapped.guide, Some(Time::from_ticks(5 * SECOND + 5400)));
    }

    #[test]
    fn at_the_same_distance_the_start_edge_wins() {
        let project = project();
        // Clip a is 2 s long: the start at 5.01 s and the end at 7.01 s are both 0.01 s from a point.
        let points = [seconds(5), seconds(7)];
        let snapped = snap_command(&project, move_to(0, 1, 5 * SECOND + 900), &points, tolerance());
        assert_eq!(snapped.command, move_to(0, 1, 5 * SECOND));
        assert_eq!(snapped.guide, Some(seconds(5)));
    }

    #[test]
    fn outside_the_tolerance_nothing_changes_and_there_is_no_guide() {
        let project = project();
        let points = snap_points(&project, Some(key(1)), Time::ZERO);
        let command = move_to(1, 0, 6 * SECOND);
        assert_eq!(snap_command(&project, command, &points, tolerance()), Snapped { command, guide: None });
    }

    #[test]
    fn a_point_that_would_overlap_gives_no_false_guide_and_the_next_offer_is_tried() {
        let project = project();
        // Clip b (2 s) wants 3 s, inside clip a (2 to 4 s): the engine would put it elsewhere.
        let command = move_to(1, 0, 3 * SECOND);
        let wide = Time::from_ticks(SECOND * 3 / 2);
        let only_inside = snap_command(&project, command, &[seconds(3)], wide);
        assert_eq!(only_inside, Snapped { command, guide: None });
        // With 4 s also a point, the move goes there and the guide is where it really sits.
        let snapped = snap_command(&project, command, &[seconds(3), seconds(4)], wide);
        assert_eq!(snapped.command, move_to(1, 0, 4 * SECOND));
        assert_eq!(snapped.guide, Some(seconds(4)));
    }

    #[test]
    fn the_dragged_clip_edges_are_not_points() {
        let project = project();
        let without = snap_points(&project, Some(key(0)), Time::ZERO);
        assert!(!without.contains(&seconds(2)) && !without.contains(&seconds(4)));
        assert!(without.contains(&seconds(10)) && without.contains(&seconds(12)));
        let with = snap_points(&project, None, Time::ZERO);
        assert!(with.contains(&seconds(2)) && with.contains(&seconds(4)));
    }

    #[test]
    fn hidden_and_locked_tracks_give_points() {
        let mut project = project();
        project = add_clip(&project, 1, new_clip("c.mp4", seconds(20), seconds(2))).unwrap();
        project = add_clip(&project, 2, new_clip("d.mp3", seconds(30), seconds(2))).unwrap();
        project.tracks[1].hidden = true;
        project.tracks[2].locked = true;
        let points = snap_points(&project, None, Time::ZERO);
        for second in [20, 22, 30, 32] {
            assert!(points.contains(&seconds(second)), "{second}");
        }
    }

    #[test]
    fn the_playhead_and_zero_are_points_without_repeats() {
        let project = project();
        let points = snap_points(&project, None, seconds(7));
        assert!(points.contains(&Time::ZERO) && points.contains(&seconds(7)));
        assert!(points.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(snap_points(&project, None, Time::ZERO).iter().filter(|point| **point == Time::ZERO).count(), 1);
    }

    #[test]
    fn a_trim_end_that_clamping_pulls_away_gets_no_guide() {
        let project = project();
        // Clip a may not end past 10 s, where clip b starts, so 11 s is not reachable.
        let command = EditCommand::TrimEnd { key: key(0), new_end: Time::from_ticks(11 * SECOND + 1800) };
        let snapped = snap_command(&project, command, &[seconds(11)], tolerance());
        assert_eq!(snapped.command, EditCommand::TrimEnd { key: key(0), new_end: seconds(10) });
        assert_eq!(snapped.guide, None);
    }

    #[test]
    fn a_trim_end_snaps_to_a_point() {
        let project = project();
        let command = EditCommand::TrimEnd { key: key(0), new_end: Time::from_ticks(6 * SECOND + 1800) };
        let snapped = snap_command(&project, command, &[seconds(6)], tolerance());
        assert_eq!(snapped.command, EditCommand::TrimEnd { key: key(0), new_end: seconds(6) });
        assert_eq!(snapped.guide, Some(seconds(6)));
    }

    #[test]
    fn a_start_trim_at_another_speed_lands_within_the_grid_and_shows_the_guide() {
        let mut project = project();
        // At 1.234x the grid is 500 ticks. Clip b has clip a before it, ending at 4 s.
        let clip = &mut project.tracks[0].clips[1];
        clip.speed = 1.234;
        let grid = clip.speed_grid();
        assert_eq!(grid, 500);
        // Not on the grid of the clip, which starts at 10 s.
        let point = Time::from_ticks(10 * SECOND + 1000 + 123);
        let command = EditCommand::TrimStart { key: key(1), new_start: Time::from_ticks(point.ticks() + 50) };
        let snapped = snap_command(&project, command, &[point], tolerance());
        assert_eq!(snapped.guide, Some(point));
        let span = snapped.command.landing(&project).unwrap();
        assert!(span.start.ticks().abs_diff(point.ticks()) < grid as u64);
        assert!(snapped.command.apply(&project).is_ok());
    }

    #[test]
    fn a_move_to_a_refused_track_stays_unsnapped() {
        let project = project();
        // Track 2 is audio and clip a is video.
        let command = move_to(0, 2, 8 * SECOND - 4500);
        let points = snap_points(&project, Some(key(0)), Time::ZERO);
        assert_eq!(snap_command(&project, command, &points, tolerance()), Snapped { command, guide: None });
    }

    #[test]
    fn a_stale_key_is_returned_as_it_is() {
        let project = project();
        let command = EditCommand::TrimEnd { key: key(9), new_end: seconds(5) };
        assert_eq!(snap_command(&project, command, &[seconds(5)], tolerance()), Snapped { command, guide: None });
    }

    #[test]
    fn the_tolerance_halves_when_the_zoom_doubles() {
        let near = snap_tolerance(100.0);
        assert_eq!(near.ticks(), 7200);
        assert_eq!(snap_tolerance(200.0).ticks() * 2, near.ticks());
    }

    #[test]
    fn a_span_snaps_by_its_nearer_edge_and_never_below_zero() {
        let points = [seconds(5), seconds(9)];
        assert_eq!(snap_span(&points, Time::from_ticks(5 * SECOND + 900), seconds(2), tolerance()), Some((seconds(5), seconds(5))));
        // The end, at 9.05 s, is nearer to 9 s than the start is to anything.
        assert_eq!(snap_span(&points, Time::from_ticks(7 * SECOND + 4500), seconds(2), tolerance()), Some((seconds(7), seconds(9))));
        assert_eq!(snap_span(&points, seconds(7), seconds(1), tolerance()), None);
        // A span 0.4 s long ending on a point at 0.3 s would have to start before zero.
        let short = Time::from_ticks(SECOND * 4 / 10);
        assert_eq!(snap_span(&[Time::from_ticks(SECOND * 3 / 10)], Time::from_ticks(SECOND / 100), short, tolerance()), None);
    }

    /// Only clip b, at 10 to 12 s, on track 0.
    fn only_b() -> Project {
        let mut project = project();
        project.tracks[0].clips.remove(0);
        project
    }

    #[test]
    fn a_drop_is_not_snapped_to_a_start_it_cannot_have() {
        let project = only_b();
        // A 3 s item dropped at 9.52 s can only fit before clip b, at 7 s. A mark at 9.45 s is no
        // place for it, so the drop stays as it is and the track does the pushing.
        let at = Time::from_ticks(9 * SECOND + SECOND * 52 / 100);
        let mark = Time::from_ticks(9 * SECOND + SECOND * 45 / 100);
        assert_eq!(snap_onto_track(&project, 0, &[mark], at, seconds(3), tolerance()), at);
    }

    #[test]
    fn a_nearer_occupied_offer_falls_through_to_a_farther_free_one() {
        let project = only_b();
        // A 1 s item at 11.93 s: the mark at 11.9 s is nearer but inside clip b; 12 s is free.
        let at = Time::from_ticks(11 * SECOND + SECOND * 93 / 100);
        let inside = Time::from_ticks(11 * SECOND + SECOND * 90 / 100);
        assert_eq!(snap_onto_track(&project, 0, &[inside, seconds(12)], at, seconds(1), tolerance()), seconds(12));
    }

    #[test]
    fn a_drop_onto_a_free_start_or_a_missing_track() {
        let project = only_b();
        let at = Time::from_ticks(12 * SECOND + 900);
        assert_eq!(snap_onto_track(&project, 0, &[seconds(12)], at, seconds(1), tolerance()), seconds(12));
        assert_eq!(snap_onto_track(&project, 9, &[seconds(12)], at, seconds(1), tolerance()), at);
    }

    #[test]
    fn a_trim_the_engine_would_refuse_is_not_snapped() {
        let mut project = project();
        project.tracks[0].transitions.push(engine::Transition {
            after_clip: 0,
            package: "fade".to_owned(),
            duration: Time::from_ticks(3000),
            params: Default::default(),
        });
        // The pointer is back on the old end of clip a, a transition edge, with a mark near it.
        let command = EditCommand::TrimEnd { key: key(0), new_end: seconds(4) };
        let mark = Time::from_ticks(4 * SECOND + 4500);
        // The engine refuses any trim of this edge, so snapping must not add a guide to it.
        assert!(command.apply(&project).is_err());
        assert_eq!(snap_command(&project, command, &[mark], tolerance()), Snapped { command, guide: None });
    }
}
