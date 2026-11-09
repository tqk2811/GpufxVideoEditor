//! Which clip, and which part of it, is under the pointer.

use super::view::TimelineView;
use engine::{ClipKey, Project};

/// The narrowest a clip is drawn, so one thinner than this can still be grabbed where it shows.
pub const MIN_CLIP_WIDTH: f64 = 1.0;

/// Width of the zone at each clip edge that trims instead of moving.
pub const EDGE_GRAB: f64 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipZone {
    Body,
    StartEdge,
    EndEdge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipHit {
    pub key: ClipKey,
    pub zone: ClipZone,
}

/// The zone of `x` in a clip drawn from `left` to `right`, or `None` outside it. Edge zones take
/// at most a third of the clip each, so a narrow clip can still be grabbed by its body.
pub fn clip_zone(x: f64, left: f64, right: f64) -> Option<ClipZone> {
    if x < left || x >= right {
        return None;
    }
    let edge = EDGE_GRAB.min((right - left) / 3.0);
    Some(if x - left < edge {
        ClipZone::StartEdge
    } else if right - x < edge {
        ClipZone::EndEdge
    } else {
        ClipZone::Body
    })
}

/// The clip of `track` under `x`.
pub fn hit_test(project: &Project, view: &TimelineView, track: usize, x: f64) -> Option<ClipHit> {
    project.tracks.get(track)?.clips.iter().enumerate().find_map(|(clip, c)| {
        let left = view.time_to_x(c.start);
        let right = view.time_to_x(c.start.saturating_add(c.duration)).max(left + MIN_CLIP_WIDTH);
        clip_zone(x, left, right).map(|zone| ClipHit { key: ClipKey { track, clip }, zone })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::new_clip;
    use crate::timeline::view::ViewArea;
    use engine::{add_clip, add_track, Output, Time, TrackKind};

    fn project() -> Project {
        let empty = Project {
            version: engine::SUPPORTED_VERSION,
            output: Output { width: 1920, height: 1080, frame_duration: Time::from_ticks(3000), audio: None },
            tracks: Vec::new(),
        };
        let with_track = add_track(&empty, TrackKind::Video).unwrap();
        let first = add_clip(&with_track, 0, new_clip("a.mp4", Time::ZERO, Time::from_seconds(1).unwrap())).unwrap();
        add_clip(&first, 0, new_clip("b.mp4", Time::from_seconds(1).unwrap(), Time::from_seconds(1).unwrap())).unwrap()
    }

    #[test]
    fn zones_split_a_wide_clip_at_the_grab_width() {
        assert_eq!(clip_zone(100.0, 100.0, 200.0), Some(ClipZone::StartEdge));
        assert_eq!(clip_zone(105.9, 100.0, 200.0), Some(ClipZone::StartEdge));
        assert_eq!(clip_zone(106.0, 100.0, 200.0), Some(ClipZone::Body));
        assert_eq!(clip_zone(194.0, 100.0, 200.0), Some(ClipZone::Body));
        assert_eq!(clip_zone(194.1, 100.0, 200.0), Some(ClipZone::EndEdge));
        assert_eq!(clip_zone(200.0, 100.0, 200.0), None);
        assert_eq!(clip_zone(99.9, 100.0, 200.0), None);
    }

    #[test]
    fn a_narrow_clip_keeps_a_body() {
        assert_eq!(clip_zone(102.0, 100.0, 106.0), Some(ClipZone::Body));
        assert_eq!(clip_zone(100.5, 100.0, 106.0), Some(ClipZone::StartEdge));
        assert_eq!(clip_zone(105.5, 100.0, 106.0), Some(ClipZone::EndEdge));
    }

    #[test]
    fn hit_test_finds_the_clip_at_several_zooms() {
        let project = project();
        let area = ViewArea { content_end: Time::from_seconds(100).unwrap(), width: 1000.0, track_count: 1, rows_height: 100.0 };
        for factor in [1.0, 2.0, 12.5] {
            let mut view = TimelineView::default();
            view.zoom_at(factor, 0.0, &area);
            let second = view.time_to_x(Time::from_ticks(135_000));
            let hit = hit_test(&project, &view, 0, second).unwrap();
            assert_eq!(hit, ClipHit { key: ClipKey { track: 0, clip: 1 }, zone: ClipZone::Body });
            let left_edge = view.time_to_x(Time::from_seconds(1).unwrap());
            assert_eq!(hit_test(&project, &view, 0, left_edge).unwrap().zone, ClipZone::StartEdge);
            let past = view.time_to_x(Time::from_seconds(3).unwrap());
            assert_eq!(hit_test(&project, &view, 0, past), None);
        }
    }

    #[test]
    fn the_shared_boundary_belongs_to_the_later_clip() {
        let project = project();
        let view = TimelineView::default();
        let x = view.time_to_x(Time::from_seconds(1).unwrap());
        let hit = hit_test(&project, &view, 0, x).unwrap();
        assert_eq!(hit.key.clip, 1);
    }

    #[test]
    fn a_clip_thinner_than_a_point_can_still_be_hit() {
        let project = project();
        let view = TimelineView::default();
        let thin = engine::Time::from_ticks(1);
        let mut thin_project = project.clone();
        thin_project.tracks[0].clips[0].duration = thin;
        let x = view.time_to_x(engine::Time::ZERO) + 0.5;
        assert_eq!(hit_test(&thin_project, &view, 0, x).map(|hit| hit.key.clip), Some(0));
    }

    #[test]
    fn a_missing_track_hits_nothing() {
        assert_eq!(hit_test(&project(), &TimelineView::default(), 5, 10.0), None);
    }
}
