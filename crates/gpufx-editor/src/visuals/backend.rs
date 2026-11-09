//! The file work behind thumbnails and levels, on the libav software decoders: no GPU, so it can
//! run beside the preview and playback.

use std::path::Path;
use std::time::Instant;

use d3d12_backend::{probe_media, AudioFileDecoder, FrameSampler, ThumbnailError, ThumbnailImage};
use engine::{Time, MIX_CHANNELS, MIX_SAMPLE_RATE};

use super::levels::PEAKS_PER_SECOND;
use super::plan::THUMBNAIL_HEIGHT;
use super::worker::{FileStamp, Generator, SourceFacts};

/// Peaks are handed over at least this often, so a long file fills in as it is read.
const PEAK_REPORT_INTERVAL_MS: u128 = 400;
/// The most peaks a source may yield: a day of audio, against a corrupt length.
const MAX_PEAKS: usize = (PEAKS_PER_SECOND * 3600 * 24) as usize;

/// Decodes in software with FFmpeg. One decoder is kept open per worker.
#[derive(Debug, Default)]
pub struct FfmpegGenerator;

impl Generator for FfmpegGenerator {
    /// The open decoder and the source identity it was opened for: the same path with a new
    /// identity is another file, and an open decoder would go on reading the old one.
    type Local = Option<(u32, FrameSampler)>;

    fn stamp(&self, path: &Path) -> Option<FileStamp> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(FileStamp { size: metadata.len(), modified: metadata.modified().ok() })
    }

    fn inspect(&self, path: &Path) -> Result<SourceFacts, String> {
        let probe = probe_media(path)?;
        let sampler = if probe.has_video { FrameSampler::open(path, THUMBNAIL_HEIGHT).map(Some) } else { Ok(None) };
        let (has_video, aspect) = match sampler {
            Ok(Some(sampler)) => {
                let (width, height) = sampler.picture_size();
                (true, width as f32 / height.max(1) as f32)
            }
            Ok(None) => (false, 16.0 / 9.0),
            Err(reason) if probe.has_audio => {
                let _ = reason;
                (false, 16.0 / 9.0)
            }
            Err(reason) => return Err(reason),
        };
        Ok(SourceFacts { has_video, has_audio: probe.has_audio, aspect, length: probe.video_duration.or(probe.audio_duration) })
    }

    fn frame(&self, local: &mut Self::Local, identity: u32, path: &Path, at: Time, cancelled: &dyn Fn() -> bool) -> Result<ThumbnailImage, ThumbnailError> {
        if local.as_ref().is_none_or(|(open, _)| *open != identity) {
            // Closed first: a sampler still open on a replaced file must not outlive this call.
            *local = None;
            *local = Some((identity, FrameSampler::open(path, THUMBNAIL_HEIGHT)?));
        }
        let (_, sampler) = local.as_mut().expect("opened above");
        let result = sampler.picture_at(at, cancelled);
        if matches!(result, Err(ThumbnailError::Failed(_))) {
            // A decoder that lost its file stays lost; the next request opens it again.
            *local = None;
        }
        result
    }

    fn peaks(&self, path: &Path, emit: &mut dyn FnMut(&[f32]) -> bool) -> Result<(), String> {
        let mut decoder = AudioFileDecoder::open(path)?;
        read_peaks(&mut |position, block| if decoder.finished() { None } else { Some(decoder.read(position, block)) }, emit)
    }
}

/// Turns blocks from `read` into peaks. `read` returns `None` at the end of the audio. What was
/// read before an error is still handed over.
pub(super) fn read_peaks(read: &mut dyn FnMut(i64, &mut [f32]) -> Option<Result<(), String>>, emit: &mut dyn FnMut(&[f32]) -> bool) -> Result<(), String> {
    let frames_per_peak = i64::from(MIX_SAMPLE_RATE) / PEAKS_PER_SECOND;
    let mut block = vec![0.0_f32; frames_per_peak as usize * MIX_CHANNELS];
    let mut pending = Vec::new();
    let mut position = 0;
    let mut total = 0;
    let mut reported = Instant::now();
    while total < MAX_PEAKS {
        block.fill(0.0);
        match read(position, &mut block) {
            None => break,
            Some(Ok(())) => {}
            Some(Err(reason)) => {
                if !pending.is_empty() {
                    emit(&pending);
                }
                return Err(reason);
            }
        }
        position += frames_per_peak;
        pending.push(block.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs())));
        total += 1;
        if reported.elapsed().as_millis() >= PEAK_REPORT_INTERVAL_MS {
            if !emit(&pending) {
                return Ok(());
            }
            pending.clear();
            reported = Instant::now();
        }
    }
    if !pending.is_empty() {
        emit(&pending);
    }
    Ok(())
}
