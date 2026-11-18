//! The frames of an animated WebP, laid over each other as the WebP container spec says and as
//! libwebp's `WebPAnimDecoder` (and so browsers and Pillow) does.
//!
//! The `image-webp` crate decodes each frame's bitstream well but lays frames that cover part of
//! the canvas wrongly: a frame disposed to the background is not cleared to transparent as libwebp
//! does, so what it covered shows through later frames. So this reads the chunks itself, decodes
//! each frame's bitstream as a still WebP of its own, and lays the frames itself.

use std::io::Cursor;

use image::codecs::webp::WebPDecoder;
use image::{DynamicImage, ImageDecoder, Limits};

/// One frame of an animated WebP: the whole canvas as it is shown, straight RGBA, and how long.
pub(crate) struct CanvasFrame {
    pub rgba: Vec<u8>,
    pub duration_ms: u32,
}

/// An animated WebP read from its bytes.
pub(crate) struct AnimatedWebp<'a> {
    pub width: u32,
    pub height: u32,
    /// How many times the animation plays, from `ANIM`; 0 is forever.
    pub loops: u32,
    frames: Vec<AnmfChunk<'a>>,
}

struct AnmfChunk<'a> {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    duration_ms: u32,
    blend: bool,
    dispose: bool,
    /// The chunks of the frame's bitstream: `ALPH` and `VP8 `, or `VP8 `, or `VP8L`.
    bitstream: &'a [u8],
}

type Chunk<'a> = Result<([u8; 4], &'a [u8]), String>;

fn chunks(mut data: &[u8]) -> impl Iterator<Item = Chunk<'_>> {
    std::iter::from_fn(move || {
        if data.is_empty() {
            return None;
        }
        if data.len() < 8 {
            data = &[];
            return Some(Err("a chunk header is cut short".to_owned()));
        }
        let fourcc: [u8; 4] = data[..4].try_into().expect("four bytes");
        let size = u32::from_le_bytes(data[4..8].try_into().expect("four bytes")) as usize;
        let Some(payload) = data.get(8..8 + size) else {
            data = &[];
            return Some(Err(format!("the {} chunk is cut short", String::from_utf8_lossy(&fourcc))));
        };
        data = data.get(8 + size + (size & 1)..).unwrap_or(&[]);
        Some(Ok((fourcc, payload)))
    })
}

fn u24(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16
}

impl<'a> AnimatedWebp<'a> {
    /// The animation in `data`, or `None` when it is not an animated WebP.
    pub fn parse(data: &'a [u8]) -> Result<Option<Self>, String> {
        if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WEBP" {
            return Ok(None);
        }
        let riff_size = u32::from_le_bytes(data[4..8].try_into().expect("four bytes")) as usize;
        let body = &data[12..data.len().min(riff_size.saturating_add(8).max(12))];
        let mut canvas = None;
        let mut loops = 0;
        let mut frames = Vec::new();
        for (index, chunk) in chunks(body).enumerate() {
            // Until VP8X says the file is animated, a broken chunk is left to the still decoder,
            // which reads a simple WebP with junk after it, or a RIFF size past the end, well.
            let (fourcc, payload) = match chunk {
                Ok(chunk) => chunk,
                Err(_) if canvas.is_none() => return Ok(None),
                Err(error) => return Err(error),
            };
            if index == 0 && &fourcc != b"VP8X" {
                return Ok(None);
            }
            match &fourcc {
                b"VP8X" if payload.len() >= 10 => {
                    if payload[0] & 0b10 == 0 {
                        return Ok(None);
                    }
                    canvas = Some((u24(&payload[4..7]) + 1, u24(&payload[7..10]) + 1));
                }
                b"ANIM" if payload.len() >= 6 => loops = u32::from(u16::from_le_bytes([payload[4], payload[5]])),
                b"ANMF" if payload.len() >= 16 => {
                    let flags = payload[15];
                    frames.push(AnmfChunk {
                        x: u24(&payload[0..3]) * 2,
                        y: u24(&payload[3..6]) * 2,
                        width: u24(&payload[6..9]) + 1,
                        height: u24(&payload[9..12]) + 1,
                        duration_ms: u24(&payload[12..15]),
                        blend: flags & 0b10 == 0,
                        dispose: flags & 0b1 != 0,
                        bitstream: &payload[16..],
                    });
                }
                b"ANMF" => return Err("an ANMF chunk is cut short".to_owned()),
                _ => {}
            }
        }
        let Some((width, height)) = canvas else { return Ok(None) };
        if frames.is_empty() {
            return Err("an animated picture with no frame".to_owned());
        }
        for frame in &frames {
            if frame.x + frame.width > width || frame.y + frame.height > height {
                return Err(format!("a {}x{} frame at {},{} is outside the {width}x{height} canvas", frame.width, frame.height, frame.x, frame.y));
            }
        }
        Ok(Some(Self { width, height, loops, frames }))
    }

    /// The frames, each the whole canvas, one at a time; `limits` bound each frame's decode.
    pub fn frames(&self, limits: Limits) -> impl Iterator<Item = Result<CanvasFrame, String>> + '_ {
        let width = self.width as usize;
        // libwebp starts from, and disposes to, transparent; the background colour is only a hint.
        let mut canvas = vec![0u8; width * self.height as usize * 4];
        let mut disposed: Option<(usize, usize, usize, usize)> = None;
        self.frames.iter().map(move |frame| {
            if let Some((x, y, w, h)) = disposed.take() {
                for row in y..y + h {
                    let start = (row * width + x) * 4;
                    canvas[start..start + w * 4].fill(0);
                }
            }
            let pixels = decode_bitstream(frame, limits.clone())?;
            let (x, y, w, h) = (frame.x as usize, frame.y as usize, frame.width as usize, frame.height as usize);
            for (row, source) in pixels.chunks_exact(w * 4).enumerate().take(h) {
                let start = ((y + row) * width + x) * 4;
                let target = &mut canvas[start..start + w * 4];
                if frame.blend {
                    for (dst, src) in target.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
                        blend_non_premultiplied(dst, src);
                    }
                } else {
                    target.copy_from_slice(source);
                }
            }
            if frame.dispose {
                disposed = Some((x, y, w, h));
            }
            // The canvas goes on to the next frame, so the frame shown is a copy of it.
            Ok(CanvasFrame { rgba: canvas.clone(), duration_ms: frame.duration_ms })
        })
    }
}

/// The frame's bitstream as straight RGBA at the frame's size, decoded as a still WebP of its own.
fn decode_bitstream(frame: &AnmfChunk, limits: Limits) -> Result<Vec<u8>, String> {
    // A decoder reads alpha only when VP8X says so; a lossless bitstream carries its own alpha.
    let has_alpha = chunks(frame.bitstream).any(|chunk| matches!(chunk, Ok((fourcc, _)) if &fourcc == b"ALPH" || &fourcc == b"VP8L"));
    let mut vp8x = vec![if has_alpha { 0b1_0000 } else { 0 }, 0, 0, 0];
    vp8x.extend_from_slice(&(frame.width - 1).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(frame.height - 1).to_le_bytes()[..3]);
    let size = 4 + 8 + vp8x.len() + frame.bitstream.len();
    let mut still = Vec::with_capacity(size + 8);
    still.extend_from_slice(b"RIFF");
    still.extend_from_slice(&(size as u32).to_le_bytes());
    still.extend_from_slice(b"WEBPVP8X");
    still.extend_from_slice(&(vp8x.len() as u32).to_le_bytes());
    still.extend_from_slice(&vp8x);
    still.extend_from_slice(frame.bitstream);
    let mut decoder = WebPDecoder::new(Cursor::new(still)).map_err(|e| format!("a frame: {e}"))?;
    decoder.set_limits(limits).map_err(|e| format!("a frame: {e}"))?;
    if decoder.dimensions() != (frame.width, frame.height) {
        return Err(format!("a frame's picture is {:?}, not the {}x{} its ANMF says", decoder.dimensions(), frame.width, frame.height));
    }
    Ok(DynamicImage::from_decoder(decoder).map_err(|e| format!("a frame: {e}"))?.into_rgba8().into_raw())
}

/// `src` over `dst`, both straight alpha, with libwebp's integer arithmetic (`BlendPixelNonPremult`
/// in `anim_decode.c`), so the colours match what it shows.
fn blend_non_premultiplied(dst: &mut [u8], src: &[u8]) {
    let src_a = u32::from(src[3]);
    if src_a == 0 {
        return;
    }
    let dst_factor_a = (u32::from(dst[3]) * (256 - src_a)) >> 8;
    let blend_a = src_a + dst_factor_a;
    let scale = (1u32 << 24) / blend_a;
    for c in 0..3 {
        let blended = u32::from(src[c]) * src_a + u32::from(dst[c]) * dst_factor_a;
        dst[c] = ((blended * scale) >> 24) as u8;
    }
    dst[3] = blend_a as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blending_matches_libwebp() {
        let mut dst = [255, 0, 0, 255];
        blend_non_premultiplied(&mut dst, &[0, 0, 255, 128]);
        assert_eq!(dst, [126, 0, 127, 255], "what Pillow shows for the subframes fixture");
        let mut dst = [9, 9, 9, 0];
        blend_non_premultiplied(&mut dst, &[1, 2, 3, 0]);
        assert_eq!(dst, [9, 9, 9, 0], "a transparent pixel leaves what is under it");
    }

    #[test]
    fn a_file_that_is_not_an_animated_webp_is_none() {
        assert!(AnimatedWebp::parse(b"not a webp at all").unwrap().is_none());
    }
}
