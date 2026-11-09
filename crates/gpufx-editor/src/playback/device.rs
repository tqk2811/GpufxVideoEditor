//! The cpal output stream. Only opens the device and forwards the callback.

use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, OutputCallbackInfo, SampleFormat, Stream};

use d3d12_backend::{AudioFiles, ProjectPaths};
use engine::{Project, MIX_CHANNELS, MIX_SAMPLE_RATE};

use super::plan::{choose_format, FormatRange};
use super::thread::{fill_device_buffer, PreparingThread};

/// The device stream and the thread feeding it. Field order matters: the stream stops first.
pub struct Playback {
    _stream: Stream,
    thread: PreparingThread,
}

fn latency_of(info: &OutputCallbackInfo) -> Option<Duration> {
    let timestamp = info.timestamp();
    Some(timestamp.playback.duration_since(timestamp.callback)).filter(|latency| !latency.is_zero())
}

impl Playback {
    /// Opens the default output device and starts a stream that stays silent until `play`.
    pub fn open(project: &Project, paths: Arc<ProjectPaths>) -> Result<Self, String> {
        let device = cpal::default_host().default_output_device().ok_or("there is no audio output device")?;
        let ranges: Vec<_> = device.supported_output_configs().map_err(|e| format!("reading the audio formats failed: {e}"))?.collect();
        let offers: Vec<FormatRange> = ranges
            .iter()
            .map(|range| FormatRange { channels: range.channels(), min_rate: range.min_sample_rate(), max_rate: range.max_sample_rate(), is_f32: range.sample_format() == SampleFormat::F32 })
            .collect();
        let chosen = choose_format(&offers)?;
        let mut config = ranges[chosen].clone().with_sample_rate(MIX_SAMPLE_RATE).config();
        config.buffer_size = BufferSize::Default;
        debug_assert_eq!(usize::from(config.channels), MIX_CHANNELS);

        let (thread, mut consumer) = PreparingThread::spawn(project, move || AudioFiles::new(paths))?;
        let shared = thread.shared().clone();
        let error_shared = thread.error_reporter();
        let stream = device
            .build_output_stream(
                config,
                move |out: &mut [f32], info: &OutputCallbackInfo| fill_device_buffer(&mut consumer, &shared, out, latency_of(info)),
                move |e| error_shared(format!("the audio device failed: {e}")),
                None,
            )
            .map_err(|e| format!("opening the audio stream failed: {e}"))?;
        stream.play().map_err(|e| format!("starting the audio stream failed: {e}"))?;
        Ok(Self { _stream: stream, thread })
    }

    pub fn thread(&self) -> &PreparingThread {
        &self.thread
    }
}
