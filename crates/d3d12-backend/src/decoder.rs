//! Sequential H.264 decoding with D3D12VA through libav*. Same approach as the POC's decoder,
//! reduced to what the renderer needs; there is deliberately no software path.
//!
//! [`SequentialDecoder::frame_at`] walks forward and seeks only when the time asked for cannot be
//! reached going forward. Times outside the source hold its first or last frame.

use std::ffi::{c_void, CStr, CString};
use std::path::Path;
use std::ptr;

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

    /// The stream timestamp of the source time `time`.
    fn target_pts(&self, time: Time) -> i64 {
        let offset = i128::from(time.ticks()) * i128::from(self.time_base.den) / (TICKS_PER_SECOND * i128::from(self.time_base.num));
        self.start_pts.saturating_add(offset as i64)
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

    /// Moves `held` to the frame shown at `target`: the last frame not after it.
    fn advance_to(&mut self, target: i64) -> Result<(), String> {
        loop {
            if self.lookahead.is_none() && !self.end_of_stream {
                self.lookahead = self.next_frame()?;
                self.end_of_stream = self.lookahead.is_none();
            }
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
        let target = self.target_pts(time);
        let cannot_continue = match (&self.held, self.seek_target) {
            (_, None) => true,
            // A held frame later than the target is fine only when it is the first frame after
            // the seek and the target has not moved before that seek: the source starts there.
            (Some(held), Some(seek_target)) => held.pts > target && !(target >= seek_target && self.landing_pts == Some(held.pts)),
            (None, Some(_)) => false,
        };
        if cannot_continue {
            self.seek(target)?;
        }
        self.advance_to(target)?;
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
