//! Decoding for a clip that plays backwards, where every request asks for an earlier frame.
//!
//! A decoder only goes forward from a keyframe, so a frame is reached by seeking to the keyframe at
//! or before it and decoding up to it. Doing that per frame would decode a whole GOP for each one.
//! Instead each refill decodes from that keyframe up to the frame asked for and keeps the last
//! frames on the way in a ring: the frames before it, which the next requests want. A GOP longer
//! than the ring would cost a refill per ring of frames, each decoding from the keyframe again:
//! about N² / 2R frames for a GOP of N and a ring of R. So the frames a refill pushes out of the
//! ring are copied to system memory, up to [`CACHE_BUDGET`], and once the ring is played through
//! the frames before it come back from there, uploaded into the pool, instead of being decoded
//! again. A GOP that fits in the ring and the cache together is decoded about once; a longer one
//! costs a refill per ring and cache of frames, and [`ReverseDecoder::slow_gop`] says so, as a
//! proxy with short GOPs plays it far cheaper.
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

use windows::core::Interface;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::ID3D12Fence;

use super::{forward_seek_pays, stream_pts, DecodedFrame, PixelFormat, PublishClock, SequentialDecoder, FORWARD_SEEK_SECONDS};
use crate::ffi::{self, check};

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

/// System memory the frames before the ring of one reversed clip may take.
pub(crate) const CACHE_BUDGET: u64 = 768 << 20;

/// A refill that decodes more than this many times the frames it keeps is reported by
/// [`ReverseDecoder::slow_gop`]: the GOP is several times longer than the ring and the cache.
const SLOW_REFILL: usize = 3;

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
    /// Copies in system memory of the consecutive frames right before the first one of the ring.
    below: VecDeque<CpuFrame>,
    /// The most frames `below` keeps.
    below_capacity: usize,
    /// Whether the first frame of `below` is the first frame of the stream.
    below_starts_stream: bool,
    /// The timestamp of the frame right after the last one of the ring, when it is known without
    /// the ring holding it: the ring then ends there, as the ring uploaded from `below` does.
    next_pts: Option<i64>,
    /// The step between consecutive frames, from the stream's frame rate and then the smallest
    /// gap seen, to tell which frames a refill keeps. While unknown nothing is copied.
    step: Option<i64>,
    /// Whether a refill decoded several times the frames the ring and the cache keep.
    slow_gop: bool,
    /// Frames copied to system memory so far.
    downloaded: u64,
}

/// A decoded frame copied to system memory, with the properties of the frame it came from.
struct CpuFrame {
    frame: *mut ffi::AVFrame,
    pts: i64,
    format: PixelFormat,
}

impl Drop for CpuFrame {
    fn drop(&mut self) {
        // SAFETY: self.frame came from av_frame_alloc and is not used afterwards.
        unsafe { ffi::av_frame_free(&mut self.frame) };
    }
}

impl ReverseDecoder {
    /// Opens `path` for hardware decoding on `device`, with a pool large enough for the ring.
    pub fn open(path: &Path, device: &ID3D12Device) -> Result<Self, String> {
        Self::open_with_cache(path, device, CACHE_BUDGET)
    }

    /// [`ReverseDecoder::open`] with `cache_budget` bytes of system memory for the frames before
    /// the ring instead of [`CACHE_BUDGET`]; 0 keeps none, to measure what the cache saves.
    pub fn open_with_cache(path: &Path, device: &ID3D12Device, cache_budget: u64) -> Result<Self, String> {
        let (mut capacity, mut below_capacity) = (MAX_RING, 0);
        let inner = SequentialDecoder::open_holding(path, device, &mut |size| {
            let bytes = frame_bytes(size.width, size.height, size.ten_bit);
            capacity = ring_frames(bytes);
            below_capacity = usize::try_from(cache_budget / bytes.max(1)).unwrap_or(usize::MAX);
            capacity as i32 + 1
        })?;
        let step = Some(inner.frame_step).filter(|step| *step > 0);
        Ok(Self {
            inner,
            capacity,
            ring: VecDeque::new(),
            starts_stream: false,
            ends_stream: false,
            continues: false,
            decoded: 0,
            below: VecDeque::new(),
            below_capacity,
            below_starts_stream: false,
            next_pts: None,
            step,
            slow_gop: false,
            downloaded: 0,
        })
    }

    /// Whether a refill had to decode several times the frames it could keep: the source's GOPs
    /// are much longer than the ring and the cache, so playing it backwards decodes each frame
    /// many times. A proxy with short GOPs plays it far cheaper.
    pub fn slow_gop(&self) -> bool {
        self.slow_gop
    }

    /// How many frames this decoder has decoded, to measure what a way of playing costs.
    pub fn decoded_frames(&self) -> u64 {
        self.decoded
    }

    /// How many frames this decoder has copied to system memory for the cache.
    pub fn downloaded_frames(&self) -> u64 {
        self.downloaded
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
        let known_end = self.ends_stream || self.next_pts.is_some_and(|next| target < next);
        (after < self.ring.len() || known_end).then_some(after - 1)
    }

    /// Adds `frame` after the last one of the ring. The frame it pushes out goes to `below` when
    /// it is not before `keep_from`, as a refill for that target keeps no frames before it.
    fn push(&mut self, frame: DecodedFrame, keep_from: Option<i64>) -> Result<(), String> {
        if let Some(last) = self.ring.back() {
            let gap = frame.pts - last.pts;
            if gap > 0 {
                self.step = Some(self.step.map_or(gap, |step| step.min(gap)));
            }
        }
        self.ring.push_back(frame);
        if self.ring.len() <= self.capacity {
            return Ok(());
        }
        let out = self.ring.pop_front().expect("the ring is over its capacity");
        let starts_stream = std::mem::take(&mut self.starts_stream);
        // keep_from is unknown until the step is: copying then would download frames far from the
        // target that the cache pushes out again.
        if self.below_capacity == 0 || keep_from.is_none() && self.step.is_none() || keep_from.is_some_and(|from| out.pts < from) {
            // Not kept: what is below would no longer be right before the ring.
            self.below.clear();
            self.below_starts_stream = false;
            return Ok(());
        }
        let copy = download(&out)?;
        self.downloaded += 1;
        if self.below.is_empty() {
            self.below_starts_stream = starts_stream;
        }
        self.below.push_back(copy);
        if self.below.len() > self.below_capacity {
            self.below.pop_front();
            self.below_starts_stream = false;
        }
        Ok(())
    }

    /// Decodes on from where the decoder stands until a frame after `target` or the end of the
    /// stream, keeping the last frames in the ring and the ones before them below it.
    fn decode_through(&mut self, target: i64, keep_from: Option<i64>) -> Result<(), String> {
        self.next_pts = None;
        loop {
            self.inner.check_abort()?;
            let Some(frame) = self.next_frame()? else {
                self.ends_stream = true;
                self.continues = false;
                return Ok(());
            };
            let past = frame.pts > target;
            self.push(frame, keep_from)?;
            if past {
                return Ok(());
            }
        }
    }

    /// Moves the frames of `below` up to the one shown at `target` into the ring, when `target`
    /// is before the ring and `below` has it. Returns whether it did.
    fn take_from_below(&mut self, target: i64) -> Result<bool, String> {
        let Some(front) = self.ring.front().map(|frame| frame.pts) else { return Ok(false) };
        let covered = self.below.front().is_some_and(|first| first.pts <= target || self.below_starts_stream);
        if target >= front || !covered {
            return Ok(false);
        }
        // The frame shown at target and as many before it as the ring keeps.
        let last = self.below.partition_point(|frame| frame.pts <= target).saturating_sub(1);
        let first = (last + 1).saturating_sub(self.capacity);
        let next = self.below.get(last + 1).map_or(front, |frame| frame.pts);
        let moved: Vec<CpuFrame> = self.below.drain(first..).take(last + 1 - first).collect();
        // The ring's frames go back to the pool, which the uploads take them from.
        self.ring.clear();
        self.continues = false;
        self.ends_stream = false;
        for copy in &moved {
            let frame = upload(&self.inner, copy)?;
            self.ring.push_back(frame);
        }
        self.starts_stream = self.below.is_empty() && self.below_starts_stream;
        if self.below.is_empty() {
            self.below_starts_stream = false;
        }
        self.next_pts = Some(next);
        Ok(true)
    }

    /// Fills the ring with the frames up to the one shown at `target`, from the keyframe before it.
    fn refill(&mut self, target: i64) -> Result<(), String> {
        self.ring.clear();
        self.below.clear();
        self.below_starts_stream = false;
        self.next_pts = None;
        self.starts_stream = false;
        self.ends_stream = false;
        self.continues = false;
        let decoded_before = self.decoded;
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
                    self.push(first, None)?;
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
        // Frames earlier than the ring and the cache can keep are only decoded, not copied.
        let kept = (self.capacity + self.below_capacity) as i64;
        let keep_from = self.step.map(|step| target.saturating_sub(step.saturating_mul(kept)));
        self.decode_through(target, keep_from)?;
        let decoded = (self.decoded - decoded_before) as usize;
        if decoded > SLOW_REFILL * (self.capacity + self.below_capacity) {
            self.slow_gop = true;
        }
        Ok(())
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
        let moved = match self.take_from_below(target) {
            Ok(true) => Ok(()),
            Ok(false) if just_after => self.decode_through(target, None),
            Ok(false) => self.refill(target),
            Err(message) => Err(message),
        };
        if moved.is_err() {
            // An interrupted refill leaves a ring that may not reach the target; start over next time.
            self.ring.clear();
            self.below.clear();
            self.next_pts = None;
            self.continues = false;
        }
        moved?;
        let index = self.find(target).ok_or("no frame could be decoded")?;
        self.ring[index].reference()
    }
}

/// A copy of `frame` in system memory.
fn download(frame: &DecodedFrame) -> Result<CpuFrame, String> {
    // SAFETY: frame.frame is a valid D3D12 frame; the copy is freed by CpuFrame on every path.
    unsafe {
        let copy = CpuFrame { frame: ffi::av_frame_alloc(), pts: frame.pts, format: frame.format };
        if copy.frame.is_null() {
            return Err("av_frame_alloc failed".to_owned());
        }
        // FFmpeg waits for the frame's fence before copying it.
        check(ffi::av_hwframe_transfer_data(copy.frame, frame.frame, 0), "av_hwframe_transfer_data (download)")?;
        check(ffi::av_frame_copy_props(copy.frame, frame.frame), "av_frame_copy_props")?;
        (*copy.frame).width = (*frame.frame).width;
        (*copy.frame).height = (*frame.frame).height;
        Ok(copy)
    }
}

/// `copy` uploaded into a free frame of the pool of `decoder`.
fn upload(decoder: &SequentialDecoder, copy: &CpuFrame) -> Result<DecodedFrame, String> {
    // SAFETY: the codec context has a frames context once it has decoded a frame, which it did
    // for `copy`; the new frame is freed by DecodedFrame on every path.
    unsafe {
        let frames = (*decoder.codec_context).hw_frames_ctx;
        if frames.is_null() {
            return Err("the decoder has no frame pool to upload into".to_owned());
        }
        let frame = DecodedFrame { frame: ffi::av_frame_alloc(), pts: copy.pts, format: copy.format, turns: decoder.turns };
        if frame.frame.is_null() {
            return Err("av_frame_alloc failed".to_owned());
        }
        check(ffi::av_hwframe_get_buffer(frames, frame.frame, 0), "av_hwframe_get_buffer")?;
        // The surface may still be read by work on its last picture, which FFmpeg's upload does
        // not wait for.
        let sync = &frame.descriptor().sync_ctx;
        let raw = sync.fence.cast::<std::ffi::c_void>();
        if let Some(fence) = ID3D12Fence::from_raw_borrowed(&raw) {
            fence.SetEventOnCompletion(sync.fence_value, HANDLE::default()).map_err(|e| format!("waiting for a pool surface failed: {e}"))?;
        }
        check(ffi::av_hwframe_transfer_data(frame.frame, copy.frame, 0), "av_hwframe_transfer_data (upload)")?;
        check(ffi::av_frame_copy_props(frame.frame, copy.frame), "av_frame_copy_props")?;
        (*frame.frame).width = (*copy.frame).width;
        (*frame.frame).height = (*copy.frame).height;
        Ok(frame)
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
