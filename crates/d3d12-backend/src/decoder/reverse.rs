//! Decoding for a clip that plays backwards, where every request asks for an earlier frame.
//!
//! A decoder only goes forward from a keyframe, so a frame is reached by seeking to the keyframe at
//! or before it and decoding up to it. Doing that per frame would decode a whole GOP for each one.
//! Instead each refill decodes from that keyframe up to the frame asked for and keeps the last
//! frames on the way in a ring: the frames before it, which the next requests want. A GOP longer
//! than the ring costs a refill per ring of frames, each decoding from the keyframe again, so a
//! long GOP is decoded a few times over, never the whole file.
//!
//! The frames are references into the decoder's own texture pool, which is opened with room for
//! the ring on top of what the decoder needs. The ring takes at most [`RING_BUDGET`] of video
//! memory: [`MAX_RING`] frames up to 1080p, about ten at 4K in 10 bits, and never fewer than
//! [`MIN_RING`], which a picture above 4K in 10 bits makes exceed the budget.
//!
//! A request just after the ring, as a scrub forward gives, goes on decoding from where the refill
//! stopped, unless a seek would get there with fewer frames decoded, as for a forward decoder.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::render::{Decoder, RenderError};
use engine::Time;
use windows::Win32::Graphics::Direct3D12::ID3D12Device;

use super::{forward_seek_pays, stream_pts, DecodedFrame, PublishClock, SequentialDecoder, FORWARD_SEEK_SECONDS};

/// Video memory the ring of one reversed clip may take, on top of what its decoder needs.
pub(crate) const RING_BUDGET: u64 = 256 << 20;
/// The most frames a ring keeps: about a second at 30 fps.
pub(crate) const MAX_RING: usize = 32;
/// The fewest frames a ring keeps, whatever their size, so a refill still serves several requests.
pub(crate) const MIN_RING: usize = 8;

/// Bytes of one decoded 4:2:0 picture, with the padding a decoder may give its surfaces.
fn frame_bytes(width: i32, height: i32, ten_bit: bool) -> u64 {
    let padded = |side: i32| (u64::from(side.max(1).unsigned_abs()) + 127) / 128 * 128;
    padded(width) * padded(height) * 3 / 2 * if ten_bit { 2 } else { 1 }
}

/// Frames of `bytes` each that fit in [`RING_BUDGET`], within [`MIN_RING`] and [`MAX_RING`].
fn ring_frames(bytes: u64) -> usize {
    usize::try_from(RING_BUDGET / bytes.max(1)).unwrap_or(MAX_RING).clamp(MIN_RING, MAX_RING)
}
/// Seeks retried further back, by four times more each, when one lands after the frame asked for.
const SEEK_RETRIES: u32 = 4;

/// Answers "which frame is shown at this time" for times that mostly go down.
pub struct ReverseDecoder {
    inner: SequentialDecoder,
    /// The most frames the ring keeps, from the size of the pictures.
    capacity: usize,
    /// Consecutive frames in presentation order.
    ring: VecDeque<DecodedFrame>,
    /// Whether the first frame of the ring is the first frame of the stream: earlier times show it.
    starts_stream: bool,
    /// Whether the last frame of the ring is the last frame of the stream: later times show it.
    ends_stream: bool,
    /// Whether the decoder stands right after the last frame of the ring, so decoding on extends it.
    continues: bool,
    /// Frames decoded so far.
    decoded: u64,
}

impl ReverseDecoder {
    /// Opens `path` for hardware decoding on `device`, with a pool large enough for the ring.
    pub fn open(path: &Path, device: &ID3D12Device) -> Result<Self, String> {
        let mut capacity = MAX_RING;
        let inner = SequentialDecoder::open_holding(path, device, &mut |size| {
            capacity = ring_frames(frame_bytes(size.width, size.height, size.ten_bit));
            capacity as i32 + 1
        })?;
        Ok(Self { inner, capacity, ring: VecDeque::new(), starts_stream: false, ends_stream: false, continues: false, decoded: 0 })
    }

    /// How many frames this decoder has decoded, to measure what a way of playing costs.
    pub fn decoded_frames(&self) -> u64 {
        self.decoded
    }

    /// The next frame from the decoder, counted.
    fn next_frame(&mut self) -> Result<Option<DecodedFrame>, String> {
        let frame = self.inner.next_frame()?;
        self.decoded += u64::from(frame.is_some());
        Ok(frame)
    }

    /// See [`SequentialDecoder::set_abort`].
    pub fn set_abort(&mut self, flag: Arc<AtomicBool>, grace: Duration) {
        self.inner.set_abort(flag, grace);
    }

    /// See [`SequentialDecoder::set_abort_policy`].
    pub fn set_abort_policy(&mut self, closing: Option<Arc<AtomicBool>>, clock: Option<Arc<PublishClock>>) {
        self.inner.set_abort_policy(closing, clock);
    }

    /// The index in the ring of the frame shown at stream time `target`, when the ring knows it.
    fn find(&self, target: i64) -> Option<usize> {
        let after = self.ring.partition_point(|frame| frame.pts <= target);
        if after == 0 {
            return (self.starts_stream && !self.ring.is_empty()).then_some(0);
        }
        (after < self.ring.len() || self.ends_stream).then_some(after - 1)
    }

    fn push(&mut self, frame: DecodedFrame) {
        self.ring.push_back(frame);
        if self.ring.len() > self.capacity {
            self.ring.pop_front();
            self.starts_stream = false;
        }
    }

    /// Decodes on from where the decoder stands until a frame after `target` or the end of the
    /// stream, keeping the last frames in the ring.
    fn decode_through(&mut self, target: i64) -> Result<(), String> {
        loop {
            self.inner.check_abort()?;
            let Some(frame) = self.next_frame()? else {
                self.ends_stream = true;
                self.continues = false;
                return Ok(());
            };
            let past = frame.pts > target;
            self.push(frame);
            if past {
                return Ok(());
            }
        }
    }

    /// Fills the ring with the frames up to the one shown at `target`, from the keyframe before it.
    fn refill(&mut self, target: i64) -> Result<(), String> {
        self.ring.clear();
        self.starts_stream = false;
        self.ends_stream = false;
        self.continues = false;
        let second = stream_pts(engine::TICKS_PER_SECOND, self.inner.time_base.num, self.inner.time_base.den, 0).max(1);
        let mut point = target;
        let mut retry = 0;
        loop {
            self.inner.seek(point)?;
            self.inner.check_abort()?;
            let Some(first) = self.next_frame()? else {
                // Nothing at all from there: the stream ended before its keyframe.
                self.ends_stream = true;
                return Ok(());
            };
            match after_landing(first.pts, target, point, retry, self.inner.start_pts, second) {
                Landing::Take { starts_stream } => {
                    self.starts_stream = starts_stream;
                    self.push(first);
                    break;
                }
                Landing::Retry(earlier) => point = earlier,
            }
            retry += 1;
        }
        self.continues = true;
        if self.ring.back().is_some_and(|frame| frame.pts > target) {
            return Ok(());
        }
        self.decode_through(target)
    }

    /// Whether a refill gets to `target` with fewer frames decoded than decoding on from `position`.
    fn seek_pays(&self, position: i64, target: i64) -> bool {
        let threshold = stream_pts(FORWARD_SEEK_SECONDS * engine::TICKS_PER_SECOND, self.inner.time_base.num, self.inner.time_base.den, 0);
        forward_seek_pays(position, target, threshold, None, || self.inner.keyframe_before(target))
    }

    fn frame_at_inner(&mut self, time: Time) -> Result<DecodedFrame, String> {
        self.inner.request_started = Instant::now();
        let target = floor_pts(time.ticks(), self.inner.time_base.num, self.inner.time_base.den, self.inner.start_pts);
        if let Some(index) = self.find(target) {
            return self.ring[index].reference();
        }
        let just_after = self.continues && self.ring.back().is_some_and(|last| target >= last.pts && !self.seek_pays(last.pts, target)) && self.ring.front().is_some_and(|first| target >= first.pts);
        let moved = if just_after { self.decode_through(target) } else { self.refill(target) };
        if moved.is_err() {
            // An interrupted refill leaves a ring that may not reach the target; start over next time.
            self.ring.clear();
            self.continues = false;
        }
        moved?;
        let index = self.find(target).ok_or("no frame could be decoded")?;
        self.ring[index].reference()
    }
}

/// What a refill does with the first frame decoded after seeking to `point` for `target`.
#[derive(Debug, PartialEq, Eq)]
enum Landing {
    /// Keep it as the first frame of the ring; `starts_stream` says no earlier frame exists.
    Take { starts_stream: bool },
    /// It came after the target: seek again to this earlier point.
    Retry(i64),
}

/// Decides [`Landing`] for a landing at `first`, the `retry`-th seek so far, in a stream that starts
/// at `start`. Each retry goes back four times further than the one before, starting at `second`;
/// the last one goes to the start of the stream, so a landing after the target is only ever taken
/// for the first frame of the stream, never served in place of the frames before it.
fn after_landing(first: i64, target: i64, point: i64, retry: u32, start: i64, second: i64) -> Landing {
    if first <= target || point <= start {
        return Landing::Take { starts_stream: point <= start };
    }
    if retry + 1 >= SEEK_RETRIES {
        return Landing::Retry(start);
    }
    Landing::Retry(point.saturating_sub(second.saturating_mul(1 << (2 * retry))).max(start))
}

/// The latest stream timestamp not after `ticks`. A reversed clip asks for the tick before the end
/// of a frame (see `engine::Clip::source_time_at`), which the rounding of the forward decoder would
/// carry over to the next frame, as ticks are finer than most stream time bases.
fn floor_pts(ticks: i64, num: i32, den: i32, start_pts: i64) -> i64 {
    let k = super::TICKS_PER_SECOND * i128::from(num.max(1));
    start_pts.saturating_add((i128::from(ticks) * i128::from(den)).div_euclid(k) as i64)
}

impl Decoder for ReverseDecoder {
    type Frame = DecodedFrame;

    fn frame_at(&mut self, time: Time) -> Result<DecodedFrame, RenderError> {
        let path = self.inner.path.clone();
        self.frame_at_inner(time).map_err(|e| RenderError(format!("{path}: {e}")))
    }

    fn wait_idle(&mut self) -> Result<(), RenderError> {
        self.inner.wait_idle()
    }
}


#[cfg(test)]
mod tests {
    use super::{after_landing, floor_pts, frame_bytes, ring_frames, Landing, MAX_RING, MIN_RING, RING_BUDGET, SEEK_RETRIES};

    #[test]
    fn the_ring_is_sized_to_its_video_memory_budget() {
        assert_eq!(ring_frames(frame_bytes(1920, 1080, false)), MAX_RING, "1080p keeps a full second");
        assert_eq!(ring_frames(frame_bytes(64, 64, true)), MAX_RING);
        let uhd = frame_bytes(3840, 2160, true);
        let frames = ring_frames(uhd);
        assert!((MIN_RING..MAX_RING).contains(&frames), "4K in 10 bits keeps {frames} frames");
        assert!(frames as u64 * uhd <= RING_BUDGET, "{} MB", frames as u64 * uhd >> 20);
        assert!((frames as u64 + 1) * uhd > RING_BUDGET, "as many as fit");
        assert_eq!(ring_frames(frame_bytes(7680, 4320, true)), MIN_RING, "8K still keeps a few");
    }

    #[test]
    fn a_landing_after_the_target_is_never_taken_before_the_stream_start() {
        let (target, second) = (100_000, 1_000);
        // A landing at or before the target is kept; from the start of the stream it starts it.
        assert_eq!(after_landing(90_000, target, target, 0, 0, second), Landing::Take { starts_stream: false });
        assert_eq!(after_landing(90_000, target, 0, 2, 0, second), Landing::Take { starts_stream: true });
        // Later than the target: back further each time, and at last to the start.
        let mut point = target;
        for retry in 0..SEEK_RETRIES {
            match after_landing(target + 1, target, point, retry, 0, second) {
                Landing::Retry(earlier) => {
                    assert!(earlier < point, "retry {retry} goes back");
                    point = earlier;
                }
                taken => panic!("retry {retry}: a frame after the target was taken as {taken:?}"),
            }
        }
        assert_eq!(point, 0, "the last retry seeks to the start of the stream");
        // Only there is a landing after the target the first frame of the stream.
        assert_eq!(after_landing(target + 1, target, point, SEEK_RETRIES, 0, second), Landing::Take { starts_stream: true });
    }

    #[test]
    fn the_tick_before_a_frame_boundary_stays_on_the_frame_before() {
        // 1/15360 s per unit: the frame at 2 s of a 30 fps stream is pts 30720 (90000 * 2 ticks).
        assert_eq!(floor_pts(180_000, 1, 15_360, 0), 30_720);
        assert_eq!(floor_pts(179_999, 1, 15_360, 0), 30_719);
        assert_eq!(floor_pts(3_000, 1, 90_000, 100), 3_100, "the stream start is added");
        assert_eq!(floor_pts(-1, 1, 15_360, 0), -1, "a time before the source stays before it");
    }
}
