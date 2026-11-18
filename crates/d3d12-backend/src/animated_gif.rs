//! The frames of an animated GIF, laid over each other as browsers lay them.
//!
//! Each GIF frame covers a rectangle of the canvas. Before the next frame is drawn, the rectangle is
//! disposed as the frame says: kept, cleared to transparent ("background": browsers ignore the
//! background colour the file names), or put back to what was under it ("previous"). A pixel of the
//! transparent index leaves the canvas under it alone. The NETSCAPE2.0 loop count is how many times
//! the loop repeats after the first play; with no NETSCAPE block the loop plays once.

use std::io::Read;

use gif::{ColorOutput, DecodeOptions, DisposalMethod, MemoryLimit, Repeat};

use crate::animated_webp::CanvasFrame;

/// How many times a GIF plays, as [`crate::still::AnimatedPicture::loops`] counts: 0 is forever.
fn loops_of(repeat: Repeat) -> u32 {
    match repeat {
        Repeat::Infinite => 0,
        Repeat::Finite(count) => u32::from(count) + 1,
    }
}

fn options(skip_pixels: bool, memory: u64) -> DecodeOptions {
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    options.skip_frame_decoding(skip_pixels);
    options.set_memory_limit(MemoryLimit::Bytes(std::num::NonZeroU64::new(memory.max(1)).unwrap()));
    options
}

/// The canvas size, each frame's delay in milliseconds and the loop count, decoding no pixel;
/// `None` for a GIF of one frame, which is a still.
pub(crate) fn timing(reader: impl Read) -> Result<Option<(u32, u32, Vec<u32>, u32)>, String> {
    let mut decoder = options(true, 64 * 1024 * 1024).read_info(reader).map_err(|e| e.to_string())?;
    let mut delays = Vec::new();
    while let Some(frame) = decoder.read_next_frame().map_err(|e| e.to_string())? {
        delays.push(u32::from(frame.delay) * 10);
    }
    if delays.len() < 2 {
        return Ok(None);
    }
    Ok(Some((u32::from(decoder.width()), u32::from(decoder.height()), delays, loops_of(decoder.repeat()))))
}

/// Calls `each` with every frame laid on the whole canvas, straight RGBA. `memory` bounds what the
/// decoder may allocate for one frame.
pub(crate) fn frames(reader: impl Read, memory: u64, mut each: impl FnMut(CanvasFrame) -> Result<(), String>) -> Result<(), String> {
    let mut decoder = options(false, memory).read_info(reader).map_err(|e| e.to_string())?;
    let (width, height) = (usize::from(decoder.width()), usize::from(decoder.height()));
    let mut canvas = vec![0u8; width * height * 4];
    // The disposal of the frame drawn last, its rectangle and, for "previous", what it covered.
    let mut pending: Option<(DisposalMethod, (usize, usize, usize, usize), Option<Vec<u8>>)> = None;
    while let Some(frame) = decoder.read_next_frame().map_err(|e| e.to_string())? {
        if let Some((dispose, (x0, y0, x1, y1), saved)) = pending.take() {
            match dispose {
                DisposalMethod::Background => {
                    for y in y0..y1 {
                        canvas[(y * width + x0) * 4..(y * width + x1) * 4].fill(0);
                    }
                }
                DisposalMethod::Previous => {
                    if let Some(saved) = saved {
                        canvas = saved;
                    }
                }
                DisposalMethod::Any | DisposalMethod::Keep => {}
            }
        }
        let (fw, fh) = (usize::from(frame.width), usize::from(frame.height));
        let (fx, fy) = (usize::from(frame.left), usize::from(frame.top));
        // Only what lies on the canvas is drawn; a frame may reach past it.
        let rect = (fx.min(width), fy.min(height), (fx + fw).min(width), (fy + fh).min(height));
        let saved = (frame.dispose == DisposalMethod::Previous).then(|| canvas.clone());
        let source = &frame.buffer;
        if source.len() < fw * fh * 4 {
            return Err("a GIF frame with fewer pixels than its size".to_owned());
        }
        for y in rect.1..rect.3 {
            for x in rect.0..rect.2 {
                let from = ((y - fy) * fw + (x - fx)) * 4;
                if source[from + 3] != 0 {
                    let to = (y * width + x) * 4;
                    canvas[to..to + 4].copy_from_slice(&source[from..from + 4]);
                }
            }
        }
        pending = Some((frame.dispose, rect, saved));
        each(CanvasFrame { rgba: canvas.clone(), duration_ms: u32::from(frame.delay) * 10 })?;
    }
    Ok(())
}
