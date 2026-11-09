//! Decisions of playback that need no device, thread or window.

use std::time::Duration;

use engine::{sample_position, sample_time, Time, MIX_CHANNELS, MIX_SAMPLE_RATE};

/// 1024 frames, about 21 ms: one mix step, and what a device buffer usually takes in a few pulls.
pub const BLOCK_FRAMES: usize = 1024;
/// 200 ms ready ahead of the device.
pub const LEAD_FRAMES: usize = 9_600;
pub const RING_BLOCKS: usize = ring_blocks(LEAD_FRAMES, BLOCK_FRAMES);
/// Used when the device gives no usable timestamps.
pub const FALLBACK_LATENCY: Duration = Duration::from_millis(30);

/// Twice the lead plus one: after a seek or edit, the stale blocks of a whole lead wait for the next
/// pull and a whole fresh lead still fits beside them, so stale audio never limits the new audio.
pub const fn ring_blocks(lead_frames: usize, block_frames: usize) -> usize {
    2 * lead_frames.div_ceil(block_frames) + 1
}

/// Frames to mix now, in whole blocks: what is missing from the lead, rounded up.
pub fn frames_to_prepare(prepared: usize, lead_frames: usize, block_frames: usize) -> usize {
    lead_frames.saturating_sub(prepared).div_ceil(block_frames) * block_frames
}

/// Whether the device may start: the lead is ready, or everything left of the timeline is.
pub fn prefill_done(prepared: usize, remaining: i64, lead_frames: usize) -> bool {
    prepared >= lead_frames || prepared as i64 >= remaining.max(0)
}

/// Whether the gate may open now: still playing, no edit waiting to replace what is prepared, and
/// the prefill of the current generation is done. Ask with the command lock held.
pub fn gate_may_open(playing: bool, edit_pending: bool, prepared: usize, remaining: i64, lead_frames: usize) -> bool {
    playing && !edit_pending && prefill_done(prepared, remaining, lead_frames)
}

/// Frames between the position the clock reports after a pull and what is heard at that moment: the
/// buffer just handed over, plus the time until its first frame reaches the speaker.
pub fn output_delay_frames(buffer_frames: usize, latency: Option<Duration>) -> u64 {
    let latency = latency.unwrap_or(FALLBACK_LATENCY);
    let latency_frames = latency.as_nanos() * u128::from(MIX_SAMPLE_RATE) / 1_000_000_000;
    buffer_frames as u64 + latency_frames as u64
}

/// The clock and device figures one playhead update needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClockReading {
    /// The sample the device consumes next.
    pub position: i64,
    /// The sample count of the timeline.
    pub end: i64,
    /// Frames of silence pulled since the clock reached the end. The clock stands still there, so
    /// these are what lets the last delayed samples count as heard.
    pub past_end: u64,
    /// See [`output_delay_frames`].
    pub delay: u64,
    /// Where playback last started or was sought to: what is heard before it is from before.
    pub anchor: i64,
}

impl ClockReading {
    /// The timeline sample being heard: the position minus the delay. A seek lands on its target
    /// without a step back, and the end is not passed.
    pub fn heard_sample(&self) -> i64 {
        let delay = i64::try_from(self.delay).unwrap_or(i64::MAX);
        let past_end = i64::try_from(self.past_end).unwrap_or(i64::MAX);
        let heard = self.position.saturating_add(past_end).saturating_sub(delay);
        heard.max(self.anchor).min(self.end).max(0)
    }

    /// True when the last sample has left the speaker.
    pub fn finished(&self) -> bool {
        self.position >= self.end && self.past_end >= self.delay
    }

    /// The playhead for this reading. Stays inside the last frame so the picture is not blank at
    /// the end.
    pub fn playhead(&self) -> Time {
        sample_time(self.heard_sample().min(self.end - 1).max(0))
    }
}

/// Where playback starts from `playhead`: at the end, or past it, it starts over.
pub fn start_time(playhead: Time, end: i64) -> Time {
    if sample_position(playhead) >= end - 1 {
        Time::ZERO
    } else {
        playhead
    }
}

/// What the transport asks the window to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportCommand {
    Play { from: Time },
    Stop,
    Seek(Time),
}

/// One format a device offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatRange {
    pub channels: u16,
    pub min_rate: u32,
    pub max_rate: u32,
    pub is_f32: bool,
}

/// The index of the first offer of 48 kHz stereo floats, or the message for the status line. No
/// other format is used: the mix would play at the wrong pitch or need converting.
pub fn choose_format(offers: &[FormatRange]) -> Result<usize, String> {
    let wanted = |offer: &FormatRange| offer.is_f32 && usize::from(offer.channels) == MIX_CHANNELS && (offer.min_rate..=offer.max_rate).contains(&MIX_SAMPLE_RATE);
    offers.iter().position(wanted).ok_or_else(|| {
        let offered: Vec<String> = offers.iter().map(|o| format!("{} ch {}-{} Hz{}", o.channels, o.min_rate, o.max_rate, if o.is_f32 { "" } else { " non-float" })).collect();
        format!("the audio device does not offer 48 kHz stereo floats (offers: {}), so nothing is played", offered.join(", "))
    })
}

/// The length of `frames` of the mix in milliseconds.
pub fn frames_to_ms(frames: u64) -> u64 {
    frames * 1000 / u64::from(MIX_SAMPLE_RATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading() -> ClockReading {
        ClockReading { position: 48_000, end: 96_000, past_end: 0, delay: 1_440, anchor: 0 }
    }

    #[test]
    fn the_ring_fits_a_stale_lead_and_a_fresh_one() {
        assert!(RING_BLOCKS * BLOCK_FRAMES >= 2 * LEAD_FRAMES);
        assert_eq!(ring_blocks(1024, 1024), 3);
        assert_eq!(ring_blocks(1025, 1024), 5);
    }

    #[test]
    fn the_lead_is_refilled_in_whole_blocks() {
        assert_eq!(frames_to_prepare(0, 9_600, 1024), 10 * 1024);
        assert_eq!(frames_to_prepare(9_500, 9_600, 1024), 1024);
        assert_eq!(frames_to_prepare(9_600, 9_600, 1024), 0);
        assert_eq!(frames_to_prepare(20_000, 9_600, 1024), 0);
    }

    #[test]
    fn the_device_starts_once_the_lead_or_the_rest_of_the_timeline_is_ready() {
        assert!(!prefill_done(100, 48_000, 9_600));
        assert!(prefill_done(9_600, 48_000, 9_600));
        assert!(prefill_done(500, 500, 9_600), "only 500 frames are left");
        assert!(prefill_done(0, 0, 9_600), "nothing is left");
        assert!(prefill_done(0, -5, 9_600));
    }

    #[test]
    fn the_gate_opens_only_while_playing_with_the_lead_and_no_edit_waiting() {
        assert!(gate_may_open(true, false, 9_600, 48_000, 9_600));
        assert!(!gate_may_open(false, false, 9_600, 48_000, 9_600), "stopped");
        assert!(!gate_may_open(true, true, 9_600, 48_000, 9_600), "the lead is about to be replaced");
        assert!(!gate_may_open(true, false, 0, 48_000, 9_600), "a new generation has no lead");
        assert!(gate_may_open(true, false, 500, 500, 9_600), "only 500 frames are left");
    }

    #[test]
    fn the_delay_is_the_buffer_plus_the_latency() {
        assert_eq!(output_delay_frames(480, Some(Duration::from_millis(10))), 960);
        assert_eq!(output_delay_frames(480, None), 480 + 1_440);
    }

    #[test]
    fn the_playhead_trails_the_clock_by_the_delay() {
        assert_eq!(reading().heard_sample(), 46_560);
        assert_eq!(reading().playhead(), sample_time(46_560));
    }

    #[test]
    fn a_seek_does_not_step_back_before_its_target() {
        let seeked = ClockReading { position: 10_000, anchor: 10_000, ..reading() };
        assert_eq!(seeked.heard_sample(), 10_000);
        let later = ClockReading { position: 12_000, anchor: 10_000, ..reading() };
        assert_eq!(later.heard_sample(), 10_560);
    }

    #[test]
    fn the_start_of_the_timeline_does_not_go_negative() {
        let start = ClockReading { position: 100, delay: 1_440, ..reading() };
        assert_eq!(start.heard_sample(), 0);
    }

    #[test]
    fn the_end_is_reached_only_when_the_last_samples_were_heard() {
        let at_end = ClockReading { position: 96_000, past_end: 0, ..reading() };
        assert!(!at_end.finished());
        assert_eq!(at_end.heard_sample(), 96_000 - 1_440);
        let playing_out = ClockReading { past_end: 1_439, ..at_end };
        assert!(!playing_out.finished());
        let done = ClockReading { past_end: 1_440, ..at_end };
        assert!(done.finished());
        assert_eq!(done.heard_sample(), 96_000);
    }

    #[test]
    fn the_playhead_stays_inside_the_last_frame() {
        let done = ClockReading { position: 96_000, past_end: 1_440, ..reading() };
        assert_eq!(done.playhead(), sample_time(95_999));
    }

    #[test]
    fn an_empty_timeline_reads_as_finished_at_zero() {
        let empty = ClockReading { position: 0, end: 0, past_end: 5, delay: 5, anchor: 0 };
        assert!(empty.finished());
        assert_eq!(empty.playhead(), Time::ZERO);
    }

    #[test]
    fn playing_starts_at_the_playhead_or_over_from_the_end() {
        let middle = sample_time(1_000);
        assert_eq!(start_time(middle, 96_000), middle);
        assert_eq!(start_time(sample_time(95_999), 96_000), Time::ZERO, "the resting place at the end");
        assert_eq!(start_time(sample_time(200_000), 96_000), Time::ZERO, "past the end");
    }

    fn offer(channels: u16, min_rate: u32, max_rate: u32, is_f32: bool) -> FormatRange {
        FormatRange { channels, min_rate, max_rate, is_f32 }
    }

    #[test]
    fn stereo_floats_at_48_khz_are_chosen() {
        let offers = [offer(6, 48_000, 48_000, true), offer(2, 44_100, 44_100, true), offer(2, 8_000, 192_000, true)];
        assert_eq!(choose_format(&offers), Ok(2));
    }

    #[test]
    fn another_rate_or_layout_is_refused_with_what_is_offered() {
        for offers in [vec![offer(2, 44_100, 44_100, true)], vec![offer(1, 48_000, 48_000, true)], vec![offer(2, 48_000, 48_000, false)]] {
            let message = choose_format(&offers).expect_err("refused");
            assert!(message.contains("nothing is played"), "{message}");
        }
        assert!(choose_format(&[]).is_err(), "a device with no offers");
    }

    #[test]
    fn a_frame_count_converts_to_milliseconds() {
        assert_eq!(frames_to_ms(48_000), 1000);
        assert_eq!(frames_to_ms(480), 10);
    }
}
