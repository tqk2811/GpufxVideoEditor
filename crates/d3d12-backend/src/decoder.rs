//! Sequential H.264 decoding with D3D12VA through libav*. Same approach as the POC's decoder,
//! reduced to what the renderer needs; there is deliberately no software path.
//!
//! [`SequentialDecoder::frame_at`] walks forward, and seeks when the time asked for cannot be reached
//! going forward or is far enough ahead that decoding everything in between would stall. Times
//! outside the source hold its first or last frame.

use std::ffi::{c_void, CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::render::{Decoder, RenderError};
use engine::Time;
use gpufx_core::{ChromaLocation, ColorInfo, Matrix, Range};
use windows::core::Interface;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence};

use crate::ffi::{self, check, describe_error};

/// Frames the consumer may hold on top of what the decoder needs for references.
const EXTRA_HW_FRAMES: i32 = 8;
const TICKS_PER_SECOND: i128 = engine::TICKS_PER_SECOND as i128;
/// How far ahead of the decoded position, in seconds of stream time, a request may be and still be
/// reached by decoding through instead of seeking.
const FORWARD_SEEK_SECONDS: i64 = 2;

/// The end of the error text of a decode stopped through [`SequentialDecoder::set_abort`].
const DECODE_CANCELLED: &str = "decoding was cancelled";

/// Whether `message`, possibly with context added in front of it, reports a cancelled decode.
pub fn is_cancelled(message: &str) -> bool {
    message.ends_with(DECODE_CANCELLED)
}

/// How long ago a frame may have been published for a newer request to still cancel the decode in
/// progress. Older than this, the screen is stale, so the decode runs on until it shows something:
/// a drag over a slow source then gets a picture about every this long, instead of none.
const PUBLISH_FRESH_WITHIN: Duration = Duration::from_millis(500);

/// When the last frame was published, shared between the compositor that publishes and the decoders
/// that decide whether a newer request may cancel them.
#[derive(Debug)]
pub struct PublishClock {
    origin: Instant,
    /// Milliseconds after `origin` of the last publish, or `u64::MAX` before the first.
    last: std::sync::atomic::AtomicU64,
}

impl Default for PublishClock {
    fn default() -> Self {
        Self { origin: Instant::now(), last: std::sync::atomic::AtomicU64::new(u64::MAX) }
    }
}

impl PublishClock {
    /// Records that a frame was published now.
    pub fn mark(&self) {
        self.last.store(self.origin.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// Whether a frame was published within [`PUBLISH_FRESH_WITHIN`].
    fn is_fresh(&self) -> bool {
        match self.last.load(Ordering::Relaxed) {
            u64::MAX => false,
            at => (self.origin.elapsed().as_millis() as u64).saturating_sub(at) <= PUBLISH_FRESH_WITHIN.as_millis() as u64,
        }
    }
}

/// Whether a decode that has run for `elapsed` should stop. Closing always stops it. A newer request
/// stops it once the grace has passed, but only while a frame was published recently: otherwise
/// every request of a slow drag would be cancelled in turn and nothing would ever be shown.
fn should_abort(newer_waits: bool, closing: bool, elapsed: Duration, grace: Duration, recently_published: bool) -> bool {
    closing || (newer_waits && elapsed >= grace && recently_published)
}

/// What `seek_target` should be after a request ended with `held` empty or not. A seek that
/// decoded no frame must not leave its target behind: the next request would take it for a landing
/// to continue from and report that no frame could be decoded.
fn seek_target_after_attempt(held_is_none: bool, seek_target: Option<i64>) -> Option<i64> {
    if held_is_none {
        None
    } else {
        seek_target
    }
}

/// The stream timestamp of `ticks` of source time, rounded to the nearest unit of the time base
/// `num / den` (flooring would pick the frame before when the stream rounds its timestamps up, as
/// MKV's 1/1000 does), added to `start_pts`.
fn stream_pts(ticks: i64, num: i32, den: i32, start_pts: i64) -> i64 {
    let k = TICKS_PER_SECOND * i128::from(num);
    let offset = (2 * i128::from(ticks) * i128::from(den) + k).div_euclid(2 * k);
    start_pts.saturating_add(offset as i64)
}

/// Whether seeking to `target` beats decoding forward from `position`, the newest decoded frame.
/// `keyframe` looks up the keyframe a seek would land on, when the demuxer's index knows it; a seek
/// landing at or behind `position` would decode frames already decoded. `min_distance` is the
/// distance below which a seek has already been seen to land behind the position.
fn forward_seek_pays(position: i64, target: i64, threshold: i64, min_distance: Option<i64>, keyframe: impl FnOnce() -> Option<i64>) -> bool {
    let distance = target.saturating_sub(position);
    if distance <= threshold {
        return false;
    }
    // The index is exact, so it comes first; the learned distance only stands in for a missing one.
    match keyframe() {
        Some(keyframe) => keyframe > position,
        None => !min_distance.is_some_and(|min| distance <= min),
    }
}

/// Decides what follows a forward seek to `point` for `target`, given the first decoded frame:
/// `Some(next)` seeks again to the earlier `next`, because the frame shown at `target` lies before
/// the landing and decoding forward would have shown it. `origin` is the frame held before the seek:
/// a keyframe not after it always exists, so going there ends the search.
fn forward_retry_point(landing: Option<i64>, target: i64, point: i64, origin: i64, threshold: i64) -> Option<i64> {
    if landing.is_some_and(|landing| landing <= target) || point <= origin {
        return None;
    }
    let retreat = target.saturating_sub(point).max(threshold).saturating_mul(2);
    Some(target.saturating_sub(retreat).max(origin))
}

unsafe extern "C" fn choose_d3d12_format(_context: *mut ffi::AVCodecContext, formats: *const ffi::AVPixelFormat) -> ffi::AVPixelFormat {
    let mut current = formats;
    while *current != ffi::AV_PIX_FMT_NONE {
        if *current == ffi::AV_PIX_FMT_D3D12 {
            return ffi::AV_PIX_FMT_D3D12;
        }
        current = current.add(1);
    }
    ffi::AV_PIX_FMT_NONE
}

/// Keeps a reference to the frame the decoder started last, so its fence can be waited for.
unsafe extern "C" fn track_started_frame(context: *mut ffi::AVCodecContext, frame: *mut ffi::AVFrame, flags: std::ffi::c_int) -> std::ffi::c_int {
    let status = ffi::avcodec_default_get_buffer2(context, frame, flags);
    if status >= 0 {
        let slot = (*context).opaque.cast::<*mut ffi::AVFrame>();
        ffi::av_frame_free(slot);
        *slot = ffi::av_frame_clone(frame);
    }
    status
}

/// A decoded frame: one reference to its D3D12 texture slice.
pub struct DecodedFrame {
    frame: *mut ffi::AVFrame,
    /// Presentation timestamp in the stream's time base.
    pub pts: i64,
}

impl DecodedFrame {
    fn descriptor(&self) -> &ffi::AVD3D12VAFrame {
        // SAFETY: frames from this decoder are AV_PIX_FMT_D3D12, whose data[0] is the descriptor,
        // and it lives as long as the frame reference held by self.
        unsafe { &*((*self.frame).data[0] as *const ffi::AVD3D12VAFrame) }
    }

    /// Another reference to the same frame.
    fn reference(&self) -> Result<Self, String> {
        // SAFETY: self.frame is valid; av_frame_clone makes a new reference or returns null.
        let frame = unsafe { ffi::av_frame_clone(self.frame) };
        if frame.is_null() {
            return Err("av_frame_clone failed".to_owned());
        }
        Ok(Self { frame, pts: self.pts })
    }

    /// `ID3D12Resource *` of the frame, borrowed.
    pub fn texture_ptr(&self) -> *mut c_void {
        self.descriptor().texture.cast()
    }

    /// Array slice of the frame in [`Self::texture_ptr`].
    pub fn subresource_index(&self) -> u32 {
        self.descriptor().subresource_index as u32
    }

    /// `ID3D12Fence *` of the frame, borrowed.
    pub fn fence_ptr(&self) -> *mut c_void {
        self.descriptor().sync_ctx.fence.cast()
    }

    /// The value [`Self::fence_ptr`] reaches when the last user of the frame has finished.
    pub fn fence_value(&self) -> u64 {
        self.descriptor().sync_ctx.fence_value
    }

    /// Records that a later user signals the fence at `value`, as FFmpeg's `sync_ctx` expects.
    pub fn raise_fence_value(&mut self, value: u64) {
        // SAFETY: data[0] is the descriptor of this frame, which FFmpeg lets the frame's user
        // update; every reference to the frame shares it.
        unsafe {
            let descriptor = (*self.frame).data[0] as *mut ffi::AVD3D12VAFrame;
            (*descriptor).sync_ctx.fence_value = (*descriptor).sync_ctx.fence_value.max(value);
        }
    }

    pub fn width(&self) -> u32 {
        // SAFETY: self.frame is valid for the lifetime of self.
        unsafe { (*self.frame).width as u32 }
    }

    pub fn height(&self) -> u32 {
        // SAFETY: self.frame is valid for the lifetime of self.
        unsafe { (*self.frame).height as u32 }
    }

    /// The colour description for gpufx, with the same defaults as FFmpeg's vf_gpufx.
    pub fn color_info(&self) -> Result<ColorInfo, String> {
        // SAFETY: self.frame is valid for the lifetime of self.
        let (colorspace, color_range, chroma_location) = unsafe { ((*self.frame).colorspace, (*self.frame).color_range, (*self.frame).chroma_location) };
        let matrix = match colorspace {
            ffi::AVCOL_SPC_BT709 => Matrix::Bt709,
            ffi::AVCOL_SPC_BT2020_NCL => Matrix::Bt2020,
            // Unknown matrices are BT.601 for swscale too.
            ffi::AVCOL_SPC_UNSPECIFIED | ffi::AVCOL_SPC_BT470BG | ffi::AVCOL_SPC_SMPTE170M => Matrix::Bt601,
            other => return Err(format!("colorspace {other} has no matrix in gpufx")),
        };
        let range = if color_range == ffi::AVCOL_RANGE_JPEG { Range::Full } else { Range::Limited };
        let chroma_location = match chroma_location {
            ffi::AVCHROMA_LOC_CENTER => ChromaLocation::Center,
            ffi::AVCHROMA_LOC_TOPLEFT => ChromaLocation::TopLeft,
            ffi::AVCHROMA_LOC_TOP => ChromaLocation::Top,
            ffi::AVCHROMA_LOC_BOTTOMLEFT => ChromaLocation::BottomLeft,
            ffi::AVCHROMA_LOC_BOTTOM => ChromaLocation::Bottom,
            ffi::AVCHROMA_LOC_LEFT => ChromaLocation::Left,
            // Unspecified: H.264 and MPEG-2 convention for limited range, JPEG convention for full.
            _ if range == Range::Full => ChromaLocation::Center,
            _ => ChromaLocation::Left,
        };
        Ok(ColorInfo { matrix, range, chroma_location, premultiplied: false })
    }
}

impl Drop for DecodedFrame {
    fn drop(&mut self) {
        // SAFETY: self.frame came from av_frame_alloc or av_frame_clone and is not used afterwards.
        unsafe { ffi::av_frame_free(&mut self.frame) };
    }
}

/// Decodes one source and answers "which frame is shown at this time".
pub struct SequentialDecoder {
    format_context: *mut ffi::AVFormatContext,
    codec_context: *mut ffi::AVCodecContext,
    packet: *mut ffi::AVPacket,
    /// Owned by the decoder; boxed because `codec_context.opaque` points at it.
    latest_started: Box<*mut ffi::AVFrame>,
    stream_index: i32,
    time_base: ffi::AVRational,
    start_pts: i64,
    draining: bool,
    /// The frame shown at the last time asked for: the latest frame not after that time.
    held: Option<DecodedFrame>,
    /// The next frame after `held`, decoded to find out where `held` ends.
    lookahead: Option<DecodedFrame>,
    end_of_stream: bool,
    /// The target of the last seek; `None` before the first request.
    seek_target: Option<i64>,
    /// The timestamp of the first frame decoded after the last seek.
    landing_pts: Option<i64>,
    /// The longest jump so far whose seek landed at or behind the decoded position, because the
    /// keyframes are further apart than it. Shorter jumps decode through instead.
    min_forward_seek: Option<i64>,
    /// Set by the owner to stop the decode in progress. Absent for the export path.
    abort: Option<Arc<AtomicBool>>,
    /// Set when the owner is closing: stops the decode at once.
    closing: Option<Arc<AtomicBool>>,
    /// Without it, a newer request cancels whatever the clock would say.
    publish_clock: Option<Arc<PublishClock>>,
    /// How long a request runs before the flag can stop it.
    abort_grace: Duration,
    request_started: Instant,
    path: String,
}

impl SequentialDecoder {
    /// Opens `path` for hardware decoding on `device`.
    pub fn open(path: &Path, device: &ID3D12Device) -> Result<Self, String> {
        let path_text = path.to_str().ok_or_else(|| format!("{}: the path is not valid UTF-8", path.display()))?;
        let c_path = CString::new(path_text).map_err(|_| format!("{path_text}: the path contains NUL"))?;
        let mut decoder = Self {
            format_context: ptr::null_mut(),
            codec_context: ptr::null_mut(),
            packet: ptr::null_mut(),
            latest_started: Box::new(ptr::null_mut()),
            stream_index: -1,
            time_base: ffi::AVRational { num: 0, den: 1 },
            start_pts: 0,
            draining: false,
            held: None,
            lookahead: None,
            end_of_stream: false,
            seek_target: None,
            landing_pts: None,
            min_forward_seek: None,
            abort: None,
            closing: None,
            publish_clock: None,
            abort_grace: Duration::ZERO,
            request_started: Instant::now(),
            path: path_text.to_owned(),
        };
        // SAFETY: on error, Drop releases whatever has been created so far.
        unsafe { decoder.initialize(&c_path, device) }.map_err(|e| format!("{path_text}: {e}"))?;
        Ok(decoder)
    }

    unsafe fn initialize(&mut self, path: &CStr, device: &ID3D12Device) -> Result<(), String> {
        check(ffi::avformat_open_input(&mut self.format_context, path.as_ptr(), ptr::null(), ptr::null_mut()), "avformat_open_input")?;
        check(ffi::avformat_find_stream_info(self.format_context, ptr::null_mut()), "avformat_find_stream_info")?;

        let mut codec: *const ffi::AVCodec = ptr::null();
        self.stream_index = check(ffi::av_find_best_stream(self.format_context, ffi::AVMEDIA_TYPE_VIDEO, -1, -1, &mut codec, 0), "av_find_best_stream")?;
        let stream = *(*self.format_context).streams.add(self.stream_index as usize);
        self.time_base = (*stream).time_base;
        self.start_pts = if (*stream).start_time == ffi::NO_PTS { 0 } else { (*stream).start_time };

        self.codec_context = ffi::avcodec_alloc_context3(codec);
        if self.codec_context.is_null() {
            return Err("avcodec_alloc_context3 failed".to_owned());
        }
        check(ffi::avcodec_parameters_to_context(self.codec_context, (*stream).codecpar), "avcodec_parameters_to_context")?;

        let mut device_reference = ffi::av_hwdevice_ctx_alloc(ffi::AV_HWDEVICE_TYPE_D3D12VA);
        if device_reference.is_null() {
            return Err("av_hwdevice_ctx_alloc(D3D12VA) failed: this FFmpeg has no D3D12VA".to_owned());
        }
        let device_context = (*device_reference).data as *mut ffi::AVHWDeviceContext;
        let d3d12_context = (*device_context).hwctx as *mut ffi::AVD3D12VADeviceContext;
        // FFmpeg releases this interface when the device context goes away, so give it its own reference.
        (*d3d12_context).device = device.clone().into_raw().cast();
        if let Err(message) = check(ffi::av_hwdevice_ctx_init(device_reference), "av_hwdevice_ctx_init(D3D12VA)") {
            ffi::av_buffer_unref(&mut device_reference);
            return Err(message);
        }

        // avcodec_free_context unrefs this; our own reference ends here.
        (*self.codec_context).hw_device_ctx = device_reference;
        (*self.codec_context).get_format = Some(choose_d3d12_format);
        (*self.codec_context).get_buffer2 = Some(track_started_frame);
        (*self.codec_context).opaque = (&raw mut *self.latest_started).cast();
        (*self.codec_context).extra_hw_frames = EXTRA_HW_FRAMES;
        // Frame threading adds latency and does nothing for hardware decoding.
        (*self.codec_context).thread_count = 1;
        check(ffi::avcodec_open2(self.codec_context, codec, ptr::null_mut()), "avcodec_open2")?;

        self.packet = ffi::av_packet_alloc();
        if self.packet.is_null() {
            return Err("av_packet_alloc failed".to_owned());
        }
        Ok(())
    }

    /// Makes `frame_at` stop with a cancelled error while `flag` is set, once a request has run for
    /// `grace`. The grace lets short requests finish during a continuous scrub, where a newer one
    /// is always about to arrive and would otherwise cancel every frame.
    pub fn set_abort(&mut self, flag: Arc<AtomicBool>, grace: Duration) {
        self.abort = Some(flag);
        self.abort_grace = grace;
    }

    /// Makes a request stop at once while `closing` is set, and lets a newer request (the flag of
    /// [`Self::set_abort`]) stop it only while `clock` says a frame was published lately.
    pub fn set_abort_policy(&mut self, closing: Option<Arc<AtomicBool>>, clock: Option<Arc<PublishClock>>) {
        self.closing = closing;
        self.publish_clock = clock;
    }

    fn check_abort(&self) -> Result<(), String> {
        let flag = self.abort.as_ref().is_some_and(|flag| flag.load(Ordering::Relaxed));
        let closing = self.closing.as_ref().is_some_and(|closing| closing.load(Ordering::Relaxed));
        let recently_published = self.publish_clock.as_ref().is_none_or(|clock| clock.is_fresh());
        if should_abort(flag, closing, self.request_started.elapsed(), self.abort_grace, recently_published) {
            return Err(DECODE_CANCELLED.to_owned());
        }
        Ok(())
    }

    /// The stream timestamp of the source time `time`.
    fn target_pts(&self, time: Time) -> i64 {
        stream_pts(time.ticks(), self.time_base.num, self.time_base.den, self.start_pts)
    }

    /// The next frame in presentation order, or `None` at the end of the stream.
    fn next_frame(&mut self) -> Result<Option<DecodedFrame>, String> {
        // SAFETY: the contexts are valid after open; frame is freed on every error path.
        unsafe {
            let mut frame = ffi::av_frame_alloc();
            if frame.is_null() {
                return Err("av_frame_alloc failed".to_owned());
            }
            match self.receive_into(frame) {
                Ok(true) => {
                    if (*frame).format != ffi::AV_PIX_FMT_D3D12 {
                        let format = (*frame).format;
                        ffi::av_frame_free(&mut frame);
                        return Err(format!("the decoder returned pixel format {format}, not a D3D12 frame"));
                    }
                    // Frame cropping offsets a D3D12 frame cannot express: only right and bottom
                    // padding is supported, as gpufx reads the top-left region.
                    let (crop_top, crop_left) = ((*frame).crop_top, (*frame).crop_left);
                    if crop_top != 0 || crop_left != 0 {
                        ffi::av_frame_free(&mut frame);
                        return Err(format!("the stream has a crop offset (top {crop_top}, left {crop_left}); only frames whose picture starts at the top-left corner are supported"));
                    }
                    let frames_context = (*(*frame).hw_frames_ctx).data as *const ffi::AVHWFramesContext;
                    if (*frames_context).sw_format != ffi::AV_PIX_FMT_NV12 {
                        let format = (*frames_context).sw_format;
                        ffi::av_frame_free(&mut frame);
                        return Err(format!("the stream decodes to pixel format {format}; only 8-bit 4:2:0 (NV12) is supported"));
                    }
                    let pts = if (*frame).best_effort_timestamp != ffi::NO_PTS { (*frame).best_effort_timestamp } else { (*frame).pts };
                    if pts == ffi::NO_PTS {
                        ffi::av_frame_free(&mut frame);
                        return Err("the decoded frame has neither a best-effort timestamp nor a pts; frames cannot be placed on the timeline".to_owned());
                    }
                    Ok(Some(DecodedFrame { frame, pts }))
                }
                Ok(false) => {
                    ffi::av_frame_free(&mut frame);
                    Ok(None)
                }
                Err(message) => {
                    ffi::av_frame_free(&mut frame);
                    Err(message)
                }
            }
        }
    }

    unsafe fn receive_into(&mut self, frame: *mut ffi::AVFrame) -> Result<bool, String> {
        loop {
            let status = ffi::avcodec_receive_frame(self.codec_context, frame);
            if status == 0 {
                return Ok(true);
            }
            if status == ffi::AVERROR_EOF {
                return Ok(false);
            }
            if status != ffi::AVERROR_EAGAIN {
                return Err(format!("avcodec_receive_frame failed: {}", describe_error(status)));
            }
            self.feed_decoder()?;
        }
    }

    unsafe fn feed_decoder(&mut self) -> Result<(), String> {
        loop {
            let status = ffi::av_read_frame(self.format_context, self.packet);
            if status == ffi::AVERROR_EOF {
                if !self.draining {
                    self.draining = true;
                    check(ffi::avcodec_send_packet(self.codec_context, ptr::null()), "avcodec_send_packet(drain)")?;
                }
                return Ok(());
            }
            check(status, "av_read_frame")?;
            let is_video = (*self.packet).stream_index == self.stream_index;
            let sent = if is_video { ffi::avcodec_send_packet(self.codec_context, self.packet) } else { 0 };
            ffi::av_packet_unref(self.packet);
            check(sent, "avcodec_send_packet")?;
            if is_video {
                return Ok(());
            }
        }
    }

    fn seek(&mut self, target: i64) -> Result<(), String> {
        // SAFETY: the contexts are valid after open.
        unsafe {
            check(ffi::av_seek_frame(self.format_context, self.stream_index, target, ffi::AVSEEK_FLAG_BACKWARD as i32), "av_seek_frame")?;
            ffi::avcodec_flush_buffers(self.codec_context);
        }
        self.draining = false;
        self.end_of_stream = false;
        self.held = None;
        self.lookahead = None;
        self.seek_target = Some(target);
        self.landing_pts = None;
        Ok(())
    }

    /// The timestamp of the keyframe a backward seek to `target` lands on, when the index knows it.
    /// The last entry does not count: an index still being built ends there and proves nothing.
    fn keyframe_before(&self, target: i64) -> Option<i64> {
        // SAFETY: the contexts are valid after open, stream_index names a stream of the file, and
        // the entry pointer is read before the index can change.
        unsafe {
            let stream = *(*self.format_context).streams.add(self.stream_index as usize);
            let count = ffi::avformat_index_get_entries_count(stream);
            let index = ffi::av_index_search_timestamp(stream, target, ffi::AVSEEK_FLAG_BACKWARD as i32);
            if index < 0 || index >= count - 1 {
                return None;
            }
            let entry = ffi::avformat_index_get_entry(stream, index);
            (!entry.is_null()).then(|| (*entry).timestamp)
        }
    }

    /// Decodes the next frame into `lookahead` when there is none, and returns its timestamp.
    fn fill_lookahead(&mut self) -> Result<Option<i64>, String> {
        if self.lookahead.is_none() && !self.end_of_stream {
            self.lookahead = self.next_frame()?;
            self.end_of_stream = self.lookahead.is_none();
        }
        Ok(self.lookahead.as_ref().map(|frame| frame.pts))
    }

    /// Seeks when `target` is far ahead. Afterwards the decoder is positioned as if it had decoded
    /// forward to just before `target`: a landing after `target` is retried further back.
    fn skip_ahead(&mut self, target: i64) -> Result<(), String> {
        let Some(origin) = self.held.as_ref().map(|held| held.pts) else { return Ok(()) };
        if self.end_of_stream {
            return Ok(());
        }
        let position = self.lookahead.as_ref().map_or(origin, |frame| frame.pts);
        let threshold = stream_pts(FORWARD_SEEK_SECONDS * engine::TICKS_PER_SECOND, self.time_base.num, self.time_base.den, 0);
        if !forward_seek_pays(position, target, threshold, self.min_forward_seek, || self.keyframe_before(target)) {
            return Ok(());
        }
        let mut point = target;
        let landing = loop {
            match self.seek(point) {
                // Nothing has changed yet, so decoding forward still works.
                Err(_) if point == target => return Ok(()),
                other => other?,
            }
            self.check_abort()?;
            let landing = self.fill_lookahead()?;
            match forward_retry_point(landing, target, point, origin, threshold) {
                Some(earlier) => point = earlier,
                None => break landing,
            }
        };
        if let Some(landing) = landing.filter(|landing| *landing <= position) {
            self.min_forward_seek = Some(self.min_forward_seek.unwrap_or(0).max(target - landing));
        }
        // A seek for target, so the first frame is not mistaken for the start of the source.
        self.seek_target = Some(target);
        Ok(())
    }

    /// Moves `held` to the frame shown at `target`: the last frame not after it.
    fn advance_to(&mut self, target: i64) -> Result<(), String> {
        loop {
            self.check_abort()?;
            self.fill_lookahead()?;
            match &self.lookahead {
                // The first frame after a seek is taken even if later than the target: the
                // source has nothing earlier (a time before the first frame holds that frame).
                Some(frame) if self.held.is_none() || frame.pts <= target => {
                    self.landing_pts.get_or_insert(frame.pts);
                    self.held = self.lookahead.take();
                }
                _ => return Ok(()),
            }
        }
    }

    fn frame_at_inner(&mut self, time: Time) -> Result<DecodedFrame, String> {
        self.request_started = Instant::now();
        let target = self.target_pts(time);
        let cannot_continue = match (&self.held, self.seek_target) {
            (_, None) => true,
            // A held frame later than the target is fine only when it is the first frame after
            // the seek and the target has not moved before that seek: the source starts there.
            (Some(held), Some(seek_target)) => held.pts > target && !(target >= seek_target && self.landing_pts == Some(held.pts)),
            (None, Some(_)) => false,
        };
        let moved = if cannot_continue { self.seek(target) } else { self.skip_ahead(target) };
        let moved = moved.and_then(|()| self.advance_to(target));
        // Nothing held after a seek, interrupted or not: the next request must seek again, not
        // continue from a landing that was meant for another target.
        self.seek_target = seek_target_after_attempt(self.held.is_none(), self.seek_target);
        moved?;
        let held = self.held.as_ref().ok_or("no frame could be decoded")?;
        held.reference()
    }
}

impl Decoder for SequentialDecoder {
    type Frame = DecodedFrame;

    fn frame_at(&mut self, time: Time) -> Result<DecodedFrame, RenderError> {
        let path = self.path.clone();
        self.frame_at_inner(time).map_err(|e| RenderError(format!("{path}: {e}")))
    }

    /// Blocks until every decode submitted so far is done. FFmpeg returns from a decode without
    /// waiting for it and keeps reference frames in VIDEO_DECODE_READ meanwhile, so with B-frames
    /// a frame may still be referenced after it is output. A queue finishes in order, so the
    /// newest decode's fence covers the earlier ones.
    fn wait_idle(&mut self) -> Result<(), RenderError> {
        let latest = *self.latest_started;
        if latest.is_null() {
            return Ok(());
        }
        // SAFETY: latest is the frame reference held by self; D3D12 frames carry their descriptor in data[0].
        let sync = unsafe { &(*((*latest).data[0] as *const ffi::AVD3D12VAFrame)).sync_ctx };
        let raw = sync.fence.cast::<c_void>();
        // SAFETY: raw is a live ID3D12Fence held by the frame.
        let fence = unsafe { ID3D12Fence::from_raw_borrowed(&raw) }.ok_or("the frame has no fence")?;
        // SAFETY: a null event makes SetEventOnCompletion block until the value is reached.
        unsafe { fence.SetEventOnCompletion(sync.fence_value, HANDLE::default()) }.map_err(|e| RenderError(format!("decoder idle wait failed: {e}")))
    }
}

impl Drop for SequentialDecoder {
    fn drop(&mut self) {
        self.held = None;
        self.lookahead = None;
        // SAFETY: each pointer is null or owned by self, and these calls accept null.
        unsafe {
            ffi::av_frame_free(&mut *self.latest_started);
            ffi::av_packet_free(&mut self.packet);
            ffi::avcodec_free_context(&mut self.codec_context);
            ffi::avformat_close_input(&mut self.format_context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: i64 = 2000;

    #[test]
    fn a_newer_request_stops_a_decode_only_after_the_grace() {
        let grace = Duration::from_millis(100);
        assert!(!should_abort(false, false, Duration::from_secs(9), grace, true));
        assert!(!should_abort(true, false, Duration::from_millis(99), grace, true));
        assert!(should_abort(true, false, Duration::from_millis(100), grace, true));
    }

    #[test]
    fn a_newer_request_does_not_stop_a_decode_while_nothing_was_published_lately() {
        let grace = Duration::from_millis(100);
        // Dragging back on a long-GOP clip: every request is cancelled at the grace and the screen
        // never changes, unless a request that has run past the grace is left to finish.
        assert!(!should_abort(true, false, Duration::from_secs(9), grace, false));
        assert!(should_abort(true, false, Duration::from_secs(9), grace, true));
    }

    #[test]
    fn closing_stops_a_decode_at_once_whatever_was_published() {
        let grace = Duration::from_millis(100);
        assert!(should_abort(false, true, Duration::ZERO, grace, false));
        assert!(should_abort(true, true, Duration::from_millis(1), grace, false));
        assert!(should_abort(false, true, Duration::ZERO, grace, true));
    }

    #[test]
    fn a_request_with_nothing_newer_and_not_closing_is_never_stopped() {
        assert!(!should_abort(false, false, Duration::from_secs(60), Duration::ZERO, true));
        assert!(!should_abort(false, false, Duration::from_secs(60), Duration::ZERO, false));
    }

    #[test]
    fn a_seek_that_decoded_nothing_forgets_its_target() {
        assert_eq!(seek_target_after_attempt(true, Some(5000)), None, "the next request must seek again");
        assert_eq!(seek_target_after_attempt(true, None), None);
        assert_eq!(seek_target_after_attempt(false, Some(5000)), Some(5000), "a held frame keeps decoding forward");
        assert_eq!(seek_target_after_attempt(false, None), None);
    }

    #[test]
    fn a_far_target_seeks_when_the_index_says_it_lands_ahead_even_after_a_landing_behind() {
        // An earlier far landing set min_distance; a later keyframe the index knows to be ahead wins.
        assert!(pays(5000, 8000, Some(5001), Some(3000)));
        assert!(!pays(5000, 8000, Some(5000), Some(1)), "the index says behind: no seek");
        assert!(!pays(5000, 8000, None, Some(3000)), "no index, so the learned distance decides");
    }

    #[test]
    fn a_cancellation_is_recognised_under_added_context() {
        assert!(is_cancelled(&format!("frame 3, track 0: c:/a.mp4: {DECODE_CANCELLED}")));
        assert!(!is_cancelled("frame 3: avcodec_receive_frame failed"));
    }

    fn pays(position: i64, target: i64, keyframe: Option<i64>, min_distance: Option<i64>) -> bool {
        forward_seek_pays(position, target, THRESHOLD, min_distance, || keyframe)
    }

    #[test]
    fn a_near_target_is_decoded_through() {
        assert!(!pays(1000, 1000 + THRESHOLD, Some(2500), None), "exactly the threshold is near");
        assert!(!pays(1000, 500, None, None), "a backward target is not a forward seek");
    }

    #[test]
    fn a_far_target_seeks() {
        assert!(pays(1000, 1000 + THRESHOLD + 1, Some(2500), None));
        assert!(pays(1000, 3_600_000, None, None), "no index: try it");
    }

    #[test]
    fn a_keyframe_behind_the_position_is_not_worth_seeking_to() {
        // A keyframe interval longer than the threshold: the seek would redo decoded frames.
        assert!(!pays(5000, 8000, Some(5000), None));
        assert!(!pays(5000, 8000, Some(1000), None));
        assert!(pays(5000, 8000, Some(5001), None));
    }

    #[test]
    fn a_jump_as_short_as_one_that_landed_behind_does_not_seek_again() {
        assert!(!pays(5000, 8000, None, Some(3000)));
        assert!(pays(5000, 8001, None, Some(3000)), "a longer jump may reach the next keyframe");
    }

    #[test]
    fn a_landing_not_after_the_target_is_accepted() {
        assert_eq!(forward_retry_point(Some(9000), 10_000, 10_000, 1000, THRESHOLD), None);
        assert_eq!(forward_retry_point(Some(10_000), 10_000, 10_000, 1000, THRESHOLD), None);
    }

    #[test]
    fn a_landing_after_the_target_is_retried_further_back() {
        let first = forward_retry_point(Some(10_500), 10_000, 10_000, 1000, THRESHOLD).expect("retry");
        assert!(first < 10_000 && first >= 1000, "{first}");
        let second = forward_retry_point(Some(10_500), 10_000, first, 1000, THRESHOLD).expect("retry");
        assert!(second < first, "each retry goes further back: {first} then {second}");
    }

    #[test]
    fn a_retry_stops_at_the_held_frame() {
        assert_eq!(forward_retry_point(Some(10_500), 10_000, 9800, 9500, THRESHOLD), Some(9500));
        assert_eq!(forward_retry_point(Some(10_500), 10_000, 9500, 9500, THRESHOLD), None, "nothing earlier to try");
    }

    #[test]
    fn a_seek_that_decodes_nothing_is_retried_from_the_held_frame() {
        assert!(forward_retry_point(None, 10_000, 10_000, 1000, THRESHOLD).is_some());
        assert_eq!(forward_retry_point(None, 10_000, 1000, 1000, THRESHOLD), None);
    }

    #[test]
    fn rounds_to_the_nearest_unit_of_a_rounded_time_base() {
        // 29.97 fps in a 1/1000 time base: frame 2 is at 66.73 ms and stored as 67.
        assert_eq!(stream_pts(2 * 3003, 1, 1000, 0), 67);
        assert_eq!(stream_pts(3003, 1, 1000, 0), 33);
        assert_eq!(stream_pts(0, 1, 1000, 0), 0);
    }

    #[test]
    fn an_exact_time_base_keeps_its_timestamps() {
        // 30 fps in the 1/15360 time base of MP4: one frame is 512 units.
        assert_eq!(stream_pts(3000, 1, 15360, 0), 512);
        assert_eq!(stream_pts(3000 * 45, 1, 15360, 0), 512 * 45);
    }

    #[test]
    fn adds_the_start_pts_and_handles_negative_times() {
        assert_eq!(stream_pts(3000, 1, 15360, 1024), 1536);
        assert_eq!(stream_pts(-3000, 1, 15360, 0), -512);
    }
}
