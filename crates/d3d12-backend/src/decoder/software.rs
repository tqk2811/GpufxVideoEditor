//! Frames the renderer cannot take as decoded: pictures decoded in software (D3D12VA has no
//! decoder for the stream, as for H.264 4:2:2, ProRes or FFV1) and D3D12 surfaces in a layout
//! other than NV12 or P010 (a GPU that decodes HEVC 4:2:2 or 4:4:4 gives Y210, Y410 or AYUV).
//!
//! gpufx reads D3D12 inputs as NV12 or P010 only, so such a picture is converted by libswscale to
//! NV12 (8 bits) or P010 (more than 8 bits) and uploaded into a D3D12 surface of a pool kept here.
//! The chroma of 4:2:2 and 4:4:4 is halved on the way; the matrix, range and transfer are kept as
//! they are, since the samples are only moved, so the frame's colour description still holds.
//! The pool and the scaler follow the size of each picture, so a source whose size changes
//! midway goes on with surfaces of the new size.

use std::ptr;

use gpufx_core::PixelFormat;

use crate::ffi::{self, check};

/// `SWS_BILINEAR | SWS_ACCURATE_RND | SWS_FULL_CHR_H_INP`: the chroma is resampled from the full
/// input, and rounding is exact so a picture already 4:2:0 comes through unchanged.
const SWS_FLAGS: i32 = 2 | 0x40000 | 0x4000;

/// The format libswscale reads `format` as, and what it converts to. The JPEG variants are read as
/// their plain twins, so a full-range picture is not squeezed into limited range on the way: the
/// frame's `color_range` says full range and the shader expands it.
pub(super) fn conversion(format: ffi::AVPixelFormat) -> Result<(ffi::AVPixelFormat, PixelFormat), String> {
    // SAFETY: av_pix_fmt_desc_get accepts any value and returns null for an unknown one.
    let descriptor = unsafe { ffi::av_pix_fmt_desc_get(format) };
    if descriptor.is_null() {
        return Err(format!("the stream decodes to unknown pixel format {format}"));
    }
    // SAFETY: checked non-null; descriptors are static.
    let descriptor = unsafe { &*descriptor };
    if descriptor.flags & ffi::AV_PIX_FMT_FLAG_HWACCEL != 0 {
        return Err(format!("the stream decodes to hardware pixel format {format}"));
    }
    let source = match format {
        ffi::AV_PIX_FMT_YUVJ420P => ffi::AV_PIX_FMT_YUV420P,
        ffi::AV_PIX_FMT_YUVJ422P => ffi::AV_PIX_FMT_YUV422P,
        ffi::AV_PIX_FMT_YUVJ444P => ffi::AV_PIX_FMT_YUV444P,
        ffi::AV_PIX_FMT_YUVJ440P => ffi::AV_PIX_FMT_YUV440P,
        other => other,
    };
    let target = if descriptor.comp[0].depth > 8 { PixelFormat::P010 } else { PixelFormat::Nv12 };
    Ok((source, target))
}

/// Whether `format` stores RGB samples, which the conversion turns into BT.709 limited range YUV.
pub(super) fn is_rgb(format: ffi::AVPixelFormat) -> bool {
    // SAFETY: as in `conversion`.
    let descriptor = unsafe { ffi::av_pix_fmt_desc_get(format) };
    // SAFETY: checked non-null; descriptors are static.
    !descriptor.is_null() && unsafe { (*descriptor).flags } & ffi::AV_PIX_FMT_FLAG_RGB != 0
}

/// The FFmpeg format of a target.
fn av_format(target: PixelFormat) -> ffi::AVPixelFormat {
    match target {
        PixelFormat::P010 => ffi::AV_PIX_FMT_P010LE,
        _ => ffi::AV_PIX_FMT_NV12,
    }
}

/// The size of the surface for a picture of `side` pixels: NV12 and P010 need even sides.
pub(super) fn surface_side(side: i32) -> i32 {
    (side.max(1) + 1) & !1
}

/// Converts pictures to NV12 or P010 and uploads them into D3D12 surfaces of its own pool.
pub(super) struct FrameConverter {
    /// A reference to the D3D12 device the pool is made on.
    device: *mut ffi::AVBufferRef,
    /// The pool of D3D12 surfaces, for `pool_key`.
    pool: *mut ffi::AVBufferRef,
    pool_key: (i32, i32, ffi::AVPixelFormat),
    scaler: *mut ffi::SwsContext,
    /// Source width, height and format, surface width and height and target format: what
    /// `scaler` was made for.
    scaler_key: (i32, i32, ffi::AVPixelFormat, i32, i32, ffi::AVPixelFormat),
    /// The converted picture in system memory, reused while the size holds.
    staging: *mut ffi::AVFrame,
    /// A D3D12 surface of another layout copied to system memory.
    downloaded: *mut ffi::AVFrame,
}

// SAFETY: the converter owns its FFmpeg objects and is used by one thread at a time, as the
// decoder that holds it.
unsafe impl Send for FrameConverter {}

impl FrameConverter {
    /// A converter that uploads to the device of `device`, a reference it takes its own of.
    pub(super) fn new(device: *mut ffi::AVBufferRef) -> Result<Self, String> {
        if device.is_null() {
            return Err("the decoder has no D3D12 device to upload to".to_owned());
        }
        // SAFETY: device is a valid buffer reference; both frames are freed by Drop.
        let converter = unsafe {
            Self {
                device: ffi::av_buffer_ref(device),
                pool: ptr::null_mut(),
                pool_key: (0, 0, ffi::AV_PIX_FMT_NONE),
                scaler: ptr::null_mut(),
                scaler_key: (0, 0, ffi::AV_PIX_FMT_NONE, 0, 0, ffi::AV_PIX_FMT_NONE),
                staging: ffi::av_frame_alloc(),
                downloaded: ffi::av_frame_alloc(),
            }
        };
        if converter.device.is_null() || converter.staging.is_null() || converter.downloaded.is_null() {
            return Err("allocating the frame converter failed".to_owned());
        }
        Ok(converter)
    }

    /// A new D3D12 frame holding `frame` as NV12 or P010, with its properties, and its format.
    ///
    /// # Safety
    /// `frame` must be a valid decoded frame, in system memory or a D3D12 surface.
    pub(super) unsafe fn convert(&mut self, frame: *const ffi::AVFrame) -> Result<(*mut ffi::AVFrame, PixelFormat), String> {
        let mut source = frame;
        if (*frame).format == ffi::AV_PIX_FMT_D3D12 {
            ffi::av_frame_unref(self.downloaded);
            // FFmpeg waits for the frame's fence before copying it.
            check(ffi::av_hwframe_transfer_data(self.downloaded, frame, 0), "av_hwframe_transfer_data (download)")?;
            source = self.downloaded;
        }
        let (width, height) = ((*source).width, (*source).height);
        let (read_as, target) = conversion((*source).format)?;
        let rgb = is_rgb((*source).format);
        let (surface_width, surface_height) = (surface_side(width), surface_side(height));
        let target_format = av_format(target);

        let key = (width, height, read_as, surface_width, surface_height, target_format);
        if self.scaler.is_null() || self.scaler_key != key {
            ffi::sws_freeContext(self.scaler);
            self.scaler = ffi::sws_getContext(width, height, read_as, surface_width, surface_height, target_format, SWS_FLAGS, ptr::null_mut(), ptr::null_mut(), ptr::null());
            if self.scaler.is_null() {
                return Err(format!("sws_getContext failed for pixel format {} at {width}x{height}", (*source).format));
            }
            if rgb {
                // RGB has no matrix of its own: BT.709 in limited range, as the frame is tagged.
                let table = ffi::sws_getCoefficients(ffi::SWS_CS_ITU709);
                ffi::sws_setColorspaceDetails(self.scaler, table, 1, table, 0, 0, 1 << 16, 1 << 16);
            }
            self.scaler_key = key;
            ffi::av_frame_unref(self.staging);
        }
        if (*self.staging).data[0].is_null() {
            (*self.staging).format = target_format;
            (*self.staging).width = surface_width;
            (*self.staging).height = surface_height;
            check(ffi::av_frame_get_buffer(self.staging, 0), "av_frame_get_buffer (conversion)")?;
        }
        let scaled = ffi::sws_scale(
            self.scaler,
            (*source).data.as_ptr().cast(),
            (*source).linesize.as_ptr(),
            0,
            height,
            (*self.staging).data.as_ptr(),
            (*self.staging).linesize.as_ptr(),
        );
        if scaled <= 0 {
            return Err(format!("sws_scale failed ({scaled})"));
        }

        let pool = self.pool(surface_width, surface_height, target_format)?;
        let mut uploaded = ffi::av_frame_alloc();
        if uploaded.is_null() {
            return Err("av_frame_alloc failed".to_owned());
        }
        let result = (|| {
            check(ffi::av_hwframe_get_buffer(pool, uploaded, 0), "av_hwframe_get_buffer")?;
            super::wait_for_surface(uploaded)?;
            check(ffi::av_hwframe_transfer_data(uploaded, self.staging, 0), "av_hwframe_transfer_data (upload)")?;
            check(ffi::av_frame_copy_props(uploaded, frame), "av_frame_copy_props")?;
            (*uploaded).width = width;
            (*uploaded).height = height;
            if rgb {
                (*uploaded).colorspace = ffi::AVCOL_SPC_BT709;
                (*uploaded).color_range = ffi::AVCOL_RANGE_MPEG;
            }
            Ok(())
        })();
        if let Err(message) = result {
            ffi::av_frame_free(&mut uploaded);
            return Err(message);
        }
        Ok((uploaded, target))
    }

    /// The pool of surfaces of this size and format, made anew when either changes. Frames from
    /// the pool before keep it alive through their own references.
    unsafe fn pool(&mut self, width: i32, height: i32, format: ffi::AVPixelFormat) -> Result<*mut ffi::AVBufferRef, String> {
        if !self.pool.is_null() && self.pool_key == (width, height, format) {
            return Ok(self.pool);
        }
        ffi::av_buffer_unref(&mut self.pool);
        let mut pool = ffi::av_hwframe_ctx_alloc(self.device);
        if pool.is_null() {
            return Err("av_hwframe_ctx_alloc failed".to_owned());
        }
        let frames = (*pool).data as *mut ffi::AVHWFramesContext;
        (*frames).format = ffi::AV_PIX_FMT_D3D12;
        (*frames).sw_format = format;
        (*frames).width = width;
        (*frames).height = height;
        // Surfaces are made as they are asked for: the consumer may hold any number of them.
        (*frames).initial_pool_size = 0;
        if let Err(message) = check(ffi::av_hwframe_ctx_init(pool), "av_hwframe_ctx_init (upload pool)") {
            ffi::av_buffer_unref(&mut pool);
            return Err(message);
        }
        self.pool = pool;
        self.pool_key = (width, height, format);
        Ok(pool)
    }
}

impl Drop for FrameConverter {
    fn drop(&mut self) {
        // SAFETY: each pointer is null or owned by self, and these calls accept null.
        unsafe {
            ffi::av_frame_free(&mut self.staging);
            ffi::av_frame_free(&mut self.downloaded);
            ffi::sws_freeContext(self.scaler);
            ffi::av_buffer_unref(&mut self.pool);
            ffi::av_buffer_unref(&mut self.device);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_formats_go_to_p010_and_others_to_nv12() {
        assert_eq!(conversion(ffi::AV_PIX_FMT_YUV422P10LE).map(|c| c.1), Ok(PixelFormat::P010));
        assert_eq!(conversion(ffi::AV_PIX_FMT_YUV444P10LE).map(|c| c.1), Ok(PixelFormat::P010));
        assert_eq!(conversion(ffi::AV_PIX_FMT_YUV422P).map(|c| c.1), Ok(PixelFormat::Nv12));
        assert_eq!(conversion(ffi::AV_PIX_FMT_YUV444P).map(|c| c.1), Ok(PixelFormat::Nv12));
    }

    #[test]
    fn jpeg_formats_are_read_as_their_plain_twins_to_keep_full_range_samples() {
        assert_eq!(conversion(ffi::AV_PIX_FMT_YUVJ422P).map(|c| c.0), Ok(ffi::AV_PIX_FMT_YUV422P));
        assert_eq!(conversion(ffi::AV_PIX_FMT_YUVJ444P).map(|c| c.0), Ok(ffi::AV_PIX_FMT_YUV444P));
        assert_eq!(conversion(ffi::AV_PIX_FMT_YUV422P10LE).map(|c| c.0), Ok(ffi::AV_PIX_FMT_YUV422P10LE));
    }

    #[test]
    fn hardware_formats_are_not_converted() {
        assert!(conversion(ffi::AV_PIX_FMT_D3D12).is_err());
    }

    #[test]
    fn surfaces_have_even_sides() {
        assert_eq!(surface_side(1920), 1920);
        assert_eq!(surface_side(1081), 1082);
        assert_eq!(surface_side(0), 2);
    }
}
