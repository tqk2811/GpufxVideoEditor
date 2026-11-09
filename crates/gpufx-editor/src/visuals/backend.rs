//! The file work behind thumbnails and levels, on the libav software decoders: no GPU, so it can
//! run beside the preview and playback.

use std::path::{Path, PathBuf};
use std::time::Instant;

use d3d12_backend::{probe_media, AudioFileDecoder, FrameSampler, ThumbnailImage};
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
    type Local = Option<(PathBuf, FrameSampler)>;

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

    fn frame(&self, local: &mut Self::Local, path: &Path, at: Time, cancelled: &dyn Fn() -> bool) -> Result<ThumbnailImage, String> {
        if local.as_ref().is_none_or(|(open, _)| open != path) {
            *local = Some((path.to_path_buf(), FrameSampler::open(path, THUMBNAIL_HEIGHT)?));
        }
        let (_, sampler) = local.as_mut().expect("opened above");
        sampler.picture_at(at, cancelled)
    }

    fn peaks(&self, path: &Path, emit: &mut dyn FnMut(&[f32]) -> bool) -> Result<(), String> {
        let mut decoder = AudioFileDecoder::open(path)?;
        let frames_per_peak = i64::from(MIX_SAMPLE_RATE) / PEAKS_PER_SECOND;
        let mut block = vec![0.0_f32; frames_per_peak as usize * MIX_CHANNELS];
        let mut pending = Vec::new();
        let mut position = 0;
        let mut total = 0;
        let mut reported = Instant::now();
        while total < MAX_PEAKS && !decoder.finished() {
            block.fill(0.0);
            decoder.read(position, &mut block)?;
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
}
