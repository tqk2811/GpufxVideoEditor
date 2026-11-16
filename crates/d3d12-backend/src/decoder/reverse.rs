//! Decoding for a clip that plays backwards, where every request asks for an earlier frame.
//!
//! A decoder only goes forward from a keyframe, so a frame is reached by seeking to the keyframe at
//! or before it and decoding up to it. Doing that per frame would decode a whole GOP for each one.
//! Instead each refill decodes from that keyframe up to the frame asked for and keeps the last
//! [`RING`] frames on the way: the frames before it, which the next requests want. A GOP longer
//! than the ring costs a refill per [`RING`] frames, each decoding from the keyframe again, so a
//! long GOP is decoded a few times over, never the whole file.
//!
//! The frames are references into the decoder's own texture pool, which is opened with room for
//! the ring on top of what the decoder needs. A request just after the ring, as a scrub forward
//! gives, goes on decoding from where the refill stopped.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::render::{Decoder, RenderError};
use engine::Time;
use windows::Win32::Graphics::Direct3D12::ID3D12Device;

use super::{stream_pts, DecodedFrame, PublishClock, SequentialDecoder};

/// Frames kept by a refill. About a second at 30 fps; a 4K P010 frame takes 24 MB of the pool.
pub(crate) const RING: usize = 32;
/// Seeks retried further back, by four times more each, when one lands after the frame asked for.
const SEEK_RETRIES: u32 = 4;

/// Answers "which frame is shown at this time" for times that mostly go down.
pub struct ReverseDecoder {
    inner: SequentialDecoder,
    /// Consecutive frames in presentation order.
    ring: VecDeque<DecodedFrame>,
    /// Whether the first frame of the ring is the first frame of the stream: earlier times show it.
    starts_stream: bool,
    /// Whether the last frame of the ring is the last frame of the stream: later times show it.
    ends_stream: bool,
    /// Whether the decoder stands right after the last frame of the ring, so decoding on extends it.
    continues: bool,
}

impl ReverseDecoder {
    /// Opens `path` for hardware decoding on `device`, with a pool large enough for the ring.
    pub fn open(path: &Path, device: &ID3D12Device) -> Result<Self, String> {
        let inner = SequentialDecoder::open_holding(path, device, RING as i32 + 1)?;
        Ok(Self { inner, ring: VecDeque::new(), starts_stream: false, ends_stream: false, continues: false })
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
        if self.ring.len() > RING {
            self.ring.pop_front();
            self.starts_stream = false;
        }
    }

    /// Decodes on from where the decoder stands until a frame after `target` or the end of the
    /// stream, keeping the last frames in the ring.
    fn decode_through(&mut self, target: i64) -> Result<(), String> {
        loop {
            self.inner.check_abort()?;
            let Some(frame) = self.inner.next_frame()? else {
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
        for retry in 0..=SEEK_RETRIES {
            self.inner.seek(point)?;
            self.inner.check_abort()?;
            let Some(first) = self.inner.next_frame()? else {
                // Nothing at all from there: the stream ended before its keyframe.
                self.ends_stream = true;
                return Ok(());
            };
            if first.pts <= target || retry == SEEK_RETRIES || point <= self.inner.start_pts {
                // A landing still after the target is the start of what can be decoded.
                self.starts_stream = first.pts > target || point <= self.inner.start_pts;
                self.push(first);
                break;
            }
            point = point.saturating_sub(second.saturating_mul(1 << (2 * retry)));
        }
        self.continues = true;
        if self.ring.back().is_some_and(|frame| frame.pts > target) {
            return Ok(());
        }
        self.decode_through(target)
    }

    fn frame_at_inner(&mut self, time: Time) -> Result<DecodedFrame, String> {
        self.inner.request_started = Instant::now();
        let target = floor_pts(time.ticks(), self.inner.time_base.num, self.inner.time_base.den, self.inner.start_pts);
        if let Some(index) = self.find(target) {
            return self.ring[index].reference();
        }
        let just_after = self.continues && self.ring.back().is_some_and(|last| target >= last.pts) && self.ring.front().is_some_and(|first| target >= first.pts);
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
    use super::floor_pts;

    #[test]
    fn the_tick_before_a_frame_boundary_stays_on_the_frame_before() {
        // 1/15360 s per unit: the frame at 2 s of a 30 fps stream is pts 30720 (90000 * 2 ticks).
        assert_eq!(floor_pts(180_000, 1, 15_360, 0), 30_720);
        assert_eq!(floor_pts(179_999, 1, 15_360, 0), 30_719);
        assert_eq!(floor_pts(3_000, 1, 90_000, 100), 3_100, "the stream start is added");
        assert_eq!(floor_pts(-1, 1, 15_360, 0), -1, "a time before the source stays before it");
    }
}
