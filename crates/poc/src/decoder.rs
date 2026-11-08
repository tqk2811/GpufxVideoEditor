//! H.264 decoding with D3D12VA through libav*, on a caller-supplied `ID3D12Device`.
//!
//! There is deliberately no software path: if D3D12VA cannot be used, opening fails.

use std::ffi::{c_void, CStr, CString};
use std::path::Path;
use std::ptr;

use gpufx_core::{ChromaLocation, ColorInfo, Matrix, Range};
use windows::core::Interface;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence, ID3D12Resource};

use crate::ffi;

/// Frames the consumer may hold on top of what the decoder needs for references.
const EXTRA_HW_FRAMES: i32 = 8;

pub fn describe_error(code: i32) -> String {
    let mut buffer = [0 as std::ffi::c_char; 256];
    // SAFETY: the buffer outlives the call and its length is passed.
    let status = unsafe { ffi::av_strerror(code, buffer.as_mut_ptr(), buffer.len()) };
    if status < 0 {
        return format!("FFmpeg error {code}");
    }
    // SAFETY: av_strerror NUL-terminates what it writes.
    let text = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    format!("{} ({code})", text.to_string_lossy())
}

fn check(code: i32, action: &str) -> Result<i32, String> {
    if code < 0 {
        Err(format!("{action} failed: {}", describe_error(code)))
    } else {
        Ok(code)
    }
}

/// Offers only D3D12; returning NONE makes the decoder fail instead of falling back to software.
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

/// How the decoder allocated a frame's texture; gpufx copies frames that are array slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureLayout {
    pub array_size: u32,
    pub texture_width: u64,
    pub texture_height: u32,
    pub resource_flags: i32,
    pub frames_context_is_array: bool,
    pub texture_array_flag: bool,
}

/// A frame copied to memory, for verification.
pub struct Nv12Image {
    pub width: usize,
    pub height: usize,
    pub luma: Vec<u8>,
    /// Interleaved U and V, half the luma size in each direction.
    pub chroma: Vec<u8>,
}

/// A decoded frame. The GPU may still be writing it until [`DecodedFrame::wait_until_ready`].
pub struct DecodedFrame {
    frame: *mut ffi::AVFrame,
    pub pts: i64,
}

impl DecodedFrame {
    fn descriptor(&self) -> &ffi::AVD3D12VAFrame {
        // SAFETY: frames from this decoder are AV_PIX_FMT_D3D12, whose data[0] is the descriptor,
        // and it lives as long as the frame reference held by self.
        unsafe { &*((*self.frame).data[0] as *const ffi::AVD3D12VAFrame) }
    }

    fn frames_context(&self) -> &ffi::AVHWFramesContext {
        // SAFETY: a hardware frame always carries its frames context, kept alive by self.frame.
        unsafe { &*((*(*self.frame).hw_frames_ctx).data as *const ffi::AVHWFramesContext) }
    }

    /// `ID3D12Resource *` (`AVD3D12VAFrame.texture`), borrowed from this frame.
    pub fn texture_ptr(&self) -> *mut c_void {
        self.descriptor().texture.cast()
    }

    /// Array slice of the frame in [`Self::texture_ptr`] (`AVD3D12VAFrame.subresource_index`).
    pub fn subresource_index(&self) -> u32 {
        self.descriptor().subresource_index as u32
    }

    /// `ID3D12Fence *` (`AVD3D12VAFrame.sync_ctx.fence`), borrowed from this frame.
    pub fn fence_ptr(&self) -> *mut c_void {
        self.descriptor().sync_ctx.fence.cast()
    }

    /// The value [`Self::fence_ptr`] reaches when the last user of this frame has finished.
    pub fn fence_value(&self) -> u64 {
        self.descriptor().sync_ctx.fence_value
    }

    /// Records that a later user of the frame signals its fence at `value`, as FFmpeg's
    /// `sync_ctx` expects, so the pool does not reuse the surface before that user is done.
    pub fn raise_fence_value(&mut self, value: u64) {
        // SAFETY: data[0] is the descriptor of this frame (see `descriptor`), which FFmpeg lets
        // the frame's user update; self.frame stays valid for the lifetime of self.
        unsafe {
            let descriptor = (*self.frame).data[0] as *mut ffi::AVD3D12VAFrame;
            (*descriptor).sync_ctx.fence_value = (*descriptor).sync_ctx.fence_value.max(value);
        }
    }

    /// The colour description of the frame for gpufx, with the same defaults as FFmpeg's vf_gpufx.
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

    pub fn width(&self) -> u32 {
        // SAFETY: self.frame is valid for the lifetime of self.
        unsafe { (*self.frame).width as u32 }
    }

    pub fn height(&self) -> u32 {
        // SAFETY: self.frame is valid for the lifetime of self.
        unsafe { (*self.frame).height as u32 }
    }

    pub fn pixel_format_name(&self) -> String {
        // SAFETY: self.frame is valid for the lifetime of self.
        pixel_format_name(unsafe { (*self.frame).format })
    }

    pub fn software_format_name(&self) -> String {
        pixel_format_name(self.frames_context().sw_format)
    }

    pub fn texture_layout(&self) -> Result<TextureLayout, String> {
        let raw = self.texture_ptr();
        // SAFETY: raw is a live ID3D12Resource held by the frame.
        let resource = unsafe { ID3D12Resource::from_raw_borrowed(&raw) }.ok_or("the frame has no texture")?;
        // SAFETY: GetDesc only reads the resource description.
        let description = unsafe { resource.GetDesc() };
        // SAFETY: the hwctx of a D3D12 frames context is an AVD3D12VAFramesContext.
        let frames = unsafe { &*(self.frames_context().hwctx as *const ffi::AVD3D12VAFramesContext) };
        Ok(TextureLayout {
            array_size: u32::from(description.DepthOrArraySize),
            texture_width: description.Width,
            texture_height: description.Height,
            resource_flags: description.Flags.0,
            frames_context_is_array: !frames.texture_array.is_null(),
            texture_array_flag: self.descriptor().flags & ffi::AV_D3D12VA_FRAME_FLAG_TEXTURE_ARRAY != 0,
        })
    }

    /// Blocks the CPU until the GPU has finished decoding this frame.
    pub fn wait_until_ready(&self) -> Result<(), String> {
        let raw = self.fence_ptr();
        // SAFETY: raw is a live ID3D12Fence held by the frame.
        let fence = unsafe { ID3D12Fence::from_raw_borrowed(&raw) }.ok_or("the frame has no fence")?;
        // SAFETY: a null event makes SetEventOnCompletion block until the value is reached.
        unsafe { fence.SetEventOnCompletion(self.fence_value(), HANDLE::default()) }.map_err(|e| format!("fence wait failed: {e}"))
    }

    /// Copies the frame to system memory and hands it to `read` (slow; verification only).
    fn with_software_frame<T>(&self, read: impl FnOnce(&ffi::AVFrame) -> Result<T, String>) -> Result<T, String> {
        // SAFETY: software is freshly allocated and freed on every path; transfer fills it.
        unsafe {
            let mut software = ffi::av_frame_alloc();
            if software.is_null() {
                return Err("av_frame_alloc failed".to_owned());
            }
            let result = check(ffi::av_hwframe_transfer_data(software, self.frame, 0), "av_hwframe_transfer_data").and_then(|_| {
                if (*software).format != ffi::AV_PIX_FMT_NV12 {
                    return Err(format!("unexpected read-back format {}", pixel_format_name((*software).format)));
                }
                read(&*software)
            });
            ffi::av_frame_free(&mut software);
            result
        }
    }

    /// Copies the luma of row `y` to memory (a slow read-back, for verification only).
    pub fn read_luma_row(&self, y: u32) -> Result<Vec<u8>, String> {
        self.with_software_frame(|software| {
            // SAFETY: y is inside the frame and each luma row holds at least `width` bytes.
            unsafe {
                let row = software.data[0].offset(y as isize * software.linesize[0] as isize);
                Ok(std::slice::from_raw_parts(row, software.width as usize).to_vec())
            }
        })
    }

    /// Copies the whole frame to memory (a slow read-back, for verification only).
    pub fn read_nv12(&self) -> Result<Nv12Image, String> {
        self.with_software_frame(|software| {
            let width = software.width as usize;
            let height = software.height as usize;
            let mut luma = Vec::with_capacity(width * height);
            let mut chroma = Vec::with_capacity(width * height / 2);
            // SAFETY: NV12 has `height` luma rows and `height / 2` chroma rows of at least `width`
            // bytes at the given line sizes.
            unsafe {
                for y in 0..height {
                    luma.extend_from_slice(std::slice::from_raw_parts(software.data[0].offset(y as isize * software.linesize[0] as isize), width));
                }
                for y in 0..height / 2 {
                    chroma.extend_from_slice(std::slice::from_raw_parts(software.data[1].offset(y as isize * software.linesize[1] as isize), width));
                }
            }
            Ok(Nv12Image { width, height, luma, chroma })
        })
    }
}

impl Drop for DecodedFrame {
    fn drop(&mut self) {
        // SAFETY: self.frame came from av_frame_alloc and is not used afterwards.
        unsafe { ffi::av_frame_free(&mut self.frame) };
    }
}

fn pixel_format_name(format: i32) -> String {
    // SAFETY: av_get_pix_fmt_name returns a static string or null.
    let name = unsafe { ffi::av_get_pix_fmt_name(format) };
    if name.is_null() {
        return format!("unknown({format})");
    }
    // SAFETY: checked non-null above; the string is static and NUL-terminated.
    unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned()
}

pub struct SeekResult {
    pub frame: DecodedFrame,
    /// Frames decoded and discarded on the way to the requested position.
    pub skipped_frames: u32,
    pub target_pts: i64,
}

pub struct D3d12Decoder {
    format_context: *mut ffi::AVFormatContext,
    codec_context: *mut ffi::AVCodecContext,
    packet: *mut ffi::AVPacket,
    /// Owned by the decoder; boxed because `codec_context.opaque` points at it.
    latest_started: Box<*mut ffi::AVFrame>,
    stream_index: i32,
    time_base: ffi::AVRational,
    start_pts: i64,
    frame_duration_pts: i64,
    duration_seconds: f64,
    draining: bool,
}

impl D3d12Decoder {
    pub fn open(path: &Path, device: &ID3D12Device) -> Result<Self, String> {
        let path_text = path.to_str().ok_or("the path is not valid UTF-8")?;
        let c_path = CString::new(path_text).map_err(|_| "the path contains NUL")?;
        let mut decoder = Self {
            format_context: ptr::null_mut(),
            codec_context: ptr::null_mut(),
            packet: ptr::null_mut(),
            latest_started: Box::new(ptr::null_mut()),
            stream_index: -1,
            time_base: ffi::AVRational { num: 0, den: 1 },
            start_pts: 0,
            frame_duration_pts: 1,
            duration_seconds: 0.0,
            draining: false,
        };
        // SAFETY: on error, Drop releases whatever has been created so far.
        unsafe { decoder.initialize(&c_path, device)? };
        Ok(decoder)
    }

    unsafe fn initialize(&mut self, path: &CStr, device: &ID3D12Device) -> Result<(), String> {
        check(ffi::avformat_open_input(&mut self.format_context, path.as_ptr(), ptr::null(), ptr::null_mut()), "avformat_open_input")?;
        check(ffi::avformat_find_stream_info(self.format_context, ptr::null_mut()), "avformat_find_stream_info")?;

        let mut codec: *const ffi::AVCodec = ptr::null();
        self.stream_index =
            check(ffi::av_find_best_stream(self.format_context, ffi::AVMEDIA_TYPE_VIDEO, -1, -1, &mut codec, 0), "av_find_best_stream")?;
        let stream = *(*self.format_context).streams.add(self.stream_index as usize);
        self.time_base = (*stream).time_base;
        self.start_pts = if (*stream).start_time == ffi::NO_PTS { 0 } else { (*stream).start_time };
        let frame_rate = (*stream).avg_frame_rate;
        if frame_rate.num <= 0 || frame_rate.den <= 0 {
            return Err("the stream has no average frame rate".to_owned());
        }
        self.frame_duration_pts =
            (i64::from(frame_rate.den) * i64::from(self.time_base.den) / (i64::from(frame_rate.num) * i64::from(self.time_base.num))).max(1);
        self.duration_seconds = (*stream).duration as f64 * f64::from(self.time_base.num) / f64::from(self.time_base.den);

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
        // FFmpeg always releases this interface when the device context goes away, so give it its own reference.
        (*d3d12_context).device = device.clone().into_raw().cast();
        let initialized = check(ffi::av_hwdevice_ctx_init(device_reference), "av_hwdevice_ctx_init(D3D12VA)");
        if let Err(message) = initialized {
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

    /// Blocks until every decode submitted so far is done; returns whether one was still running.
    ///
    /// FFmpeg returns from a decode without waiting for it and keeps reference frames in
    /// VIDEO_DECODE_READ meanwhile. With B-frames a frame is output after later frames that
    /// reference it were submitted, so its own fence does not show that nothing else uses it.
    /// A queue finishes in order, so the newest decode's fence covers the earlier ones.
    pub fn wait_until_idle(&self) -> Result<bool, String> {
        let latest = *self.latest_started;
        if latest.is_null() {
            return Ok(false);
        }
        // SAFETY: latest is the frame reference held by self; D3D12 frames carry their descriptor in data[0].
        let sync = unsafe { &(*((*latest).data[0] as *const ffi::AVD3D12VAFrame)).sync_ctx };
        let raw = sync.fence.cast::<c_void>();
        // SAFETY: raw is a live ID3D12Fence held by the frame.
        let fence = unsafe { ID3D12Fence::from_raw_borrowed(&raw) }.ok_or("the frame has no fence")?;
        // SAFETY: plain COM calls; a null event makes SetEventOnCompletion block until the value is reached.
        unsafe {
            let running = fence.GetCompletedValue() < sync.fence_value;
            fence.SetEventOnCompletion(sync.fence_value, HANDLE::default()).map_err(|e| format!("decoder idle wait failed: {e}"))?;
            Ok(running)
        }
    }

    pub fn duration_seconds(&self) -> f64 {
        self.duration_seconds
    }

    /// Frame number of the timestamp `pts`, assuming the constant frame rate of the POC's clips.
    pub fn frame_index_of(&self, pts: i64) -> i64 {
        (pts - self.start_pts + self.frame_duration_pts / 2) / self.frame_duration_pts
    }

    pub fn frame_duration_pts(&self) -> i64 {
        self.frame_duration_pts
    }

    /// The next frame in presentation order, or `None` at the end of the stream.
    pub fn next_frame(&mut self) -> Result<Option<DecodedFrame>, String> {
        // SAFETY: the contexts are valid after open; frame is freed on every error path.
        unsafe {
            let mut frame = ffi::av_frame_alloc();
            if frame.is_null() {
                return Err("av_frame_alloc failed".to_owned());
            }
            let received = self.receive_into(frame);
            match received {
                Ok(true) => {
                    if (*frame).format != ffi::AV_PIX_FMT_D3D12 {
                        let name = pixel_format_name((*frame).format);
                        ffi::av_frame_free(&mut frame);
                        return Err(format!("the decoder returned {name}, not a D3D12 frame"));
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

    /// Seeks to `seconds` and decodes forward to the frame displayed at that time.
    pub fn seek_accurate(&mut self, seconds: f64) -> Result<SeekResult, String> {
        let offset = (seconds * f64::from(self.time_base.den) / f64::from(self.time_base.num)) as i64;
        let target_pts = self.start_pts + offset;
        // SAFETY: the contexts are valid after open.
        unsafe {
            check(
                ffi::av_seek_frame(self.format_context, self.stream_index, target_pts, ffi::AVSEEK_FLAG_BACKWARD as i32),
                "av_seek_frame",
            )?;
            ffi::avcodec_flush_buffers(self.codec_context);
        }
        self.draining = false;

        let mut skipped_frames = 0;
        let mut previous = None;
        while let Some(frame) = self.next_frame()? {
            if frame.pts + self.frame_duration_pts > target_pts {
                return Ok(SeekResult { frame, skipped_frames, target_pts });
            }
            skipped_frames += 1;
            previous = Some(frame);
        }
        // Past the last frame: the last frame is the one still on screen at that time.
        let frame = previous.ok_or_else(|| format!("no frame decoded after seeking to {seconds} s"))?;
        Ok(SeekResult { frame, skipped_frames, target_pts })
    }
}

impl Drop for D3d12Decoder {
    fn drop(&mut self) {
        // SAFETY: each pointer is null or owned by self, and these calls accept null.
        unsafe {
            ffi::av_frame_free(&mut *self.latest_started);
            ffi::av_packet_free(&mut self.packet);
            ffi::avcodec_free_context(&mut self.codec_context);
            ffi::avformat_close_input(&mut self.format_context);
        }
    }
}
