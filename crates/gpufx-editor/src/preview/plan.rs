//! Decisions of the preview that need no thread, window or GPU.

use engine::{Project, Time};

/// The output frame shown at `time`.
pub fn frame_index(project: &Project, time: Time) -> i64 {
    let duration = project.output.frame_duration.ticks();
    if duration <= 0 {
        return 0;
    }
    time.ticks().div_euclid(duration)
}

/// Remembers the last (revision, frame) asked for, so an unchanged wish sends nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct RequestTracker {
    sent: Option<(u64, i64)>,
}

impl RequestTracker {
    /// True when this wish differs from the last one, which it then becomes.
    pub fn should_send(&mut self, revision: u64, frame: i64) -> bool {
        let wish = Some((revision, frame));
        let changed = self.sent != wish;
        self.sent = wish;
        changed
    }
}

/// The size of a picture of `image` pixels fitted inside `available` points, keeping its aspect
/// ratio. Zero when either has no area.
pub fn fit_size(image: (u32, u32), available: (f32, f32)) -> (f32, f32) {
    if image.0 == 0 || image.1 == 0 || available.0 <= 0.0 || available.1 <= 0.0 {
        return (0.0, 0.0);
    }
    let (width, height) = (image.0 as f32, image.1 as f32);
    let scale = (available.0 / width).min(available.1 / height);
    (width * scale, height * scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{Output, SUPPORTED_VERSION};

    fn project(frame_duration: i64) -> Project {
        Project {
            version: SUPPORTED_VERSION,
            output: Output { width: 16, height: 9, frame_duration: Time::from_ticks(frame_duration), audio: None },
            tracks: Vec::new(),
        }
    }

    #[test]
    fn a_time_inside_a_frame_gives_that_frame() {
        let p = project(3000);
        assert_eq!(frame_index(&p, Time::ZERO), 0);
        assert_eq!(frame_index(&p, Time::from_ticks(2999)), 0);
        assert_eq!(frame_index(&p, Time::from_ticks(3000)), 1);
        assert_eq!(frame_index(&p, Time::from_ticks(90_000)), 30);
    }

    #[test]
    fn a_bad_frame_duration_does_not_divide_by_zero() {
        assert_eq!(frame_index(&project(0), Time::from_ticks(5)), 0);
    }

    #[test]
    fn the_tracker_sends_only_changes() {
        let mut tracker = RequestTracker::default();
        assert!(tracker.should_send(0, 0));
        assert!(!tracker.should_send(0, 0));
        assert!(tracker.should_send(0, 1));
        assert!(tracker.should_send(1, 1), "a new revision needs a new frame");
        assert!(tracker.should_send(0, 1), "going back to an older wish is a change too");
    }

    #[test]
    fn a_wide_image_is_limited_by_the_width() {
        assert_eq!(fit_size((1920, 1080), (960.0, 1000.0)), (960.0, 540.0));
    }

    #[test]
    fn a_tall_area_limit_keeps_the_aspect() {
        let (width, height) = fit_size((1920, 1080), (4000.0, 540.0));
        assert_eq!((width, height), (960.0, 540.0));
    }

    #[test]
    fn an_empty_area_or_image_has_no_size() {
        assert_eq!(fit_size((1920, 1080), (0.0, 500.0)), (0.0, 0.0));
        assert_eq!(fit_size((0, 1080), (500.0, 500.0)), (0.0, 0.0));
    }
}
