//! What a hovered tile of the picker shows, as pure functions of the time since the hover began,
//! and the tracker that decides when a hover has lasted long enough to start.

use crate::packages::PackageKind;
use d3d12_backend::PREVIEW_FPS;

/// How long the pointer must stay on one tile before it plays, so that sweeping across the grid
/// renders no loops.
pub const HOVER_DELAY: f64 = 0.25;
/// How long the last frame of a transition is held before it plays again.
pub const TRANSITION_HOLD: f64 = 0.4;
/// How long a static effect shows the picture without the effect.
pub const STATIC_ORIGINAL: f64 = 0.5;
/// How long a static effect shows the picture with the effect.
pub const STATIC_PROCESSED: f64 = 1.0;
/// The shortest wait before the next change, so that a rounding error cannot spin the repaints.
const MIN_WAIT: f64 = 0.001;

/// What a tile shows at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// The still, which has the effect applied.
    Still,
    /// Picture A as it is, without the effect.
    Original,
    /// This frame of the loop.
    Frame(u32),
}

/// What a tile shows now, and in how many seconds that changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    pub shown: Shown,
    pub next_in: f64,
}

/// The frame of a loop of `frames` frames at `elapsed` seconds. An effect plays the loop over and
/// over; a transition holds its last frame for [`TRANSITION_HOLD`] before it plays again.
pub fn loop_step(kind: PackageKind, frames: u32, elapsed: f64) -> Step {
    let frames = frames.max(1);
    let fps = f64::from(PREVIEW_FPS);
    let play = f64::from(frames) / fps;
    let elapsed = elapsed.max(0.0);
    let cycle = match kind {
        PackageKind::Effect => play,
        PackageKind::Transition => play + TRANSITION_HOLD,
    };
    let phase = elapsed % cycle;
    if phase < play {
        let index = ((phase * fps).floor() as u32).min(frames - 1);
        Step { shown: Shown::Frame(index), next_in: ((f64::from(index) + 1.0) / fps - phase).max(MIN_WAIT) }
    } else {
        Step { shown: Shown::Frame(frames - 1), next_in: (cycle - phase).max(MIN_WAIT) }
    }
}

/// What an effect that does not change over time shows at `elapsed` seconds: the picture without
/// the effect, then the still with it, over and over.
pub fn static_step(elapsed: f64) -> Step {
    let cycle = STATIC_ORIGINAL + STATIC_PROCESSED;
    let phase = elapsed.max(0.0) % cycle;
    if phase < STATIC_ORIGINAL {
        Step { shown: Shown::Original, next_in: (STATIC_ORIGINAL - phase).max(MIN_WAIT) }
    } else {
        Step { shown: Shown::Still, next_in: (cycle - phase).max(MIN_WAIT) }
    }
}

/// How a tile stands with the pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hover {
    /// The pointer is not on it.
    Idle,
    /// The pointer is on it, for `remaining` more seconds before it plays.
    Waiting { remaining: f64 },
    /// It plays, and began `elapsed` seconds ago.
    Playing { elapsed: f64 },
}

/// Which tile the pointer has stayed on, and since when. Lives in the editor's UI state; a tile that
/// is not drawn in a frame is not hovered in it.
#[derive(Debug, Clone, Default)]
pub struct TileHover {
    /// The package under the pointer and the time it arrived.
    candidate: Option<(String, f64)>,
    seen_this_frame: bool,
    playing: Option<String>,
}

impl TileHover {
    /// Call before drawing the tiles of a frame.
    pub fn begin_frame(&mut self) {
        self.seen_this_frame = false;
    }

    /// Call for each drawn tile with whether the pointer is on it, at time `now` in seconds.
    pub fn observe(&mut self, package: &str, hovered: bool, now: f64) -> Hover {
        if !hovered {
            return Hover::Idle;
        }
        self.seen_this_frame = true;
        let since = match &self.candidate {
            Some((current, since)) if current == package => *since,
            _ => {
                self.candidate = Some((package.to_owned(), now));
                now
            }
        };
        let waited = now - since;
        if waited < HOVER_DELAY {
            self.playing = None;
            Hover::Waiting { remaining: HOVER_DELAY - waited }
        } else {
            self.playing = Some(package.to_owned());
            Hover::Playing { elapsed: waited - HOVER_DELAY }
        }
    }

    /// Call after drawing the tiles. When no drawn tile was hovered, the hover is over, so that
    /// hovering again starts from the beginning.
    pub fn end_frame(&mut self) {
        if !self.seen_this_frame {
            self.clear();
        }
    }

    /// Forgets the hover: the picker is closed or not drawn.
    pub fn clear(&mut self) {
        self.candidate = None;
        self.playing = None;
        self.seen_this_frame = false;
    }

    /// The tile that has been hovered long enough to ask for its loop.
    pub fn playing(&self) -> Option<&str> {
        self.playing.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f64 = 1.0 / 20.0;

    fn index(step: Step) -> u32 {
        match step.shown {
            Shown::Frame(index) => index,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_effect_plays_its_frames_at_the_preview_rate_and_wraps_around() {
        assert_eq!(index(loop_step(PackageKind::Effect, 10, 0.0)), 0);
        assert_eq!(index(loop_step(PackageKind::Effect, 10, FRAME * 3.5)), 3);
        assert_eq!(index(loop_step(PackageKind::Effect, 10, FRAME * 9.5)), 9);
        assert_eq!(index(loop_step(PackageKind::Effect, 10, FRAME * 10.5)), 0, "back to the start");
        assert_eq!(index(loop_step(PackageKind::Effect, 10, FRAME * 23.5)), 3);
        let step = loop_step(PackageKind::Effect, 10, FRAME * 3.25);
        assert!((step.next_in - FRAME * 0.75).abs() < 1e-9, "{}", step.next_in);
    }

    #[test]
    fn a_transition_holds_its_last_frame_before_it_plays_again() {
        let play = FRAME * 10.0;
        assert_eq!(index(loop_step(PackageKind::Transition, 10, FRAME * 8.5)), 8);
        assert_eq!(index(loop_step(PackageKind::Transition, 10, FRAME * 9.5)), 9);
        let held = loop_step(PackageKind::Transition, 10, play + 0.1);
        assert_eq!(held.shown, Shown::Frame(9));
        assert!((held.next_in - 0.3).abs() < 1e-9, "{}", held.next_in);
        assert_eq!(index(loop_step(PackageKind::Transition, 10, play + 0.39)), 9);
        assert_eq!(index(loop_step(PackageKind::Transition, 10, play + TRANSITION_HOLD + 0.01)), 0, "plays again");
        assert_eq!(index(loop_step(PackageKind::Transition, 10, 2.0 * (play + TRANSITION_HOLD) + FRAME * 2.5)), 2);
    }

    #[test]
    fn the_last_frame_of_a_transition_waits_for_the_end_of_its_frame_and_the_hold() {
        let step = loop_step(PackageKind::Transition, 10, FRAME * 9.5);
        assert!((step.next_in - FRAME * 0.5).abs() < 1e-9, "{}", step.next_in);
    }

    #[test]
    fn a_loop_of_one_frame_and_a_negative_time_are_safe() {
        assert_eq!(loop_step(PackageKind::Effect, 1, 7.0).shown, Shown::Frame(0));
        assert_eq!(loop_step(PackageKind::Effect, 0, 7.0).shown, Shown::Frame(0));
        assert_eq!(loop_step(PackageKind::Transition, 5, -3.0).shown, Shown::Frame(0));
        assert!(loop_step(PackageKind::Effect, 10, 0.123).next_in > 0.0);
    }

    #[test]
    fn a_static_effect_shows_the_original_then_the_still_and_repeats() {
        assert_eq!(static_step(0.0).shown, Shown::Original);
        assert_eq!(static_step(0.49).shown, Shown::Original);
        assert_eq!(static_step(0.5).shown, Shown::Still);
        assert_eq!(static_step(1.49).shown, Shown::Still);
        assert_eq!(static_step(1.5).shown, Shown::Original, "the cycle repeats");
        assert_eq!(static_step(3.7).shown, Shown::Still);
        assert!((static_step(0.2).next_in - 0.3).abs() < 1e-9);
        assert!((static_step(0.7).next_in - 0.8).abs() < 1e-9);
    }

    #[test]
    fn a_tile_plays_only_after_the_pointer_stayed_on_it_for_the_delay() {
        let mut hover = TileHover::default();
        hover.begin_frame();
        assert_eq!(hover.observe("a", false, 0.0), Hover::Idle);
        hover.end_frame();
        hover.begin_frame();
        match hover.observe("a", true, 1.0) {
            Hover::Waiting { remaining } => assert!((remaining - HOVER_DELAY).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
        hover.end_frame();
        assert_eq!(hover.playing(), None);
        hover.begin_frame();
        assert!(matches!(hover.observe("a", true, 1.2), Hover::Waiting { .. }));
        hover.end_frame();
        hover.begin_frame();
        match hover.observe("a", true, 1.35) {
            Hover::Playing { elapsed } => assert!((elapsed - 0.1).abs() < 1e-9, "{elapsed}"),
            other => panic!("{other:?}"),
        }
        hover.end_frame();
        assert_eq!(hover.playing(), Some("a"));
    }

    #[test]
    fn sweeping_across_tiles_plays_none_of_them() {
        let mut hover = TileHover::default();
        for (step, package) in ["a", "b", "c", "d"].into_iter().enumerate() {
            hover.begin_frame();
            assert!(matches!(hover.observe(package, true, 0.2 * step as f64), Hover::Waiting { .. }));
            hover.end_frame();
            assert_eq!(hover.playing(), None);
        }
    }

    #[test]
    fn leaving_the_tile_stops_it_and_hovering_again_starts_from_the_beginning() {
        let mut hover = TileHover::default();
        for now in [0.0, 0.3] {
            hover.begin_frame();
            hover.observe("a", true, now);
            hover.end_frame();
        }
        assert_eq!(hover.playing(), Some("a"));
        hover.begin_frame();
        hover.end_frame();
        assert_eq!(hover.playing(), None);
        hover.begin_frame();
        assert!(matches!(hover.observe("a", true, 5.0), Hover::Waiting { .. }), "the delay starts again");
        hover.end_frame();
        hover.begin_frame();
        assert!(matches!(hover.observe("a", true, 5.3), Hover::Playing { elapsed } if (elapsed - 0.05).abs() < 1e-9));
        hover.end_frame();
    }

    #[test]
    fn moving_to_another_tile_restarts_the_delay_and_clear_forgets_everything() {
        let mut hover = TileHover::default();
        for now in [0.0, 0.3] {
            hover.begin_frame();
            hover.observe("a", true, now);
            hover.end_frame();
        }
        hover.begin_frame();
        assert!(matches!(hover.observe("b", true, 0.4), Hover::Waiting { .. }));
        hover.end_frame();
        assert_eq!(hover.playing(), None);
        hover.clear();
        hover.begin_frame();
        assert!(matches!(hover.observe("b", true, 0.5), Hover::Waiting { .. }));
    }
}
