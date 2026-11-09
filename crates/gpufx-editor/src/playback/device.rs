//! The cpal output stream. Only opens the device and forwards the callback.

use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, OutputCallbackInfo, SampleFormat, Stream};

use d3d12_backend::{AudioFiles, ProjectPaths};
use engine::{Project, MIX_CHANNELS, MIX_SAMPLE_RATE};

use super::plan::{choose_format, FormatRange};
use super::thread::{fill_device_channels, PreparingThread, SilentDriver};

/// Frames of stereo a surround callback pulls at a time; larger callbacks are pulled in pieces.
const SCRATCH_FRAMES: usize = 8_192;

/// The output and the thread feeding it: either a device stream or, without a device, a timer.
/// Field order matters: the output stops first.
pub struct Playback {
    _stream: Option<Stream>,
    _silent: Option<SilentDriver>,
    thread: PreparingThread,
    no_sound: Option<String>,
}

fn latency_of(info: &OutputCallbackInfo) -> Option<Duration> {
    let timestamp = info.timestamp();
    Some(timestamp.playback.duration_since(timestamp.callback)).filter(|latency| !latency.is_zero())
}

impl Playback {
    /// Opens the default output device and starts a stream that stays silent until `play`. Without
    /// a usable device the clock runs on a timer, so the picture still plays, and
    /// [`Playback::no_sound`] says why nothing is heard.
    pub fn open(project: &Project, paths: Arc<ProjectPaths>) -> Result<Self, String> {
        match Self::open_device(project, paths.clone()) {
            Ok(playback) => Ok(playback),
            Err(reason) => {
                let (thread, consumer) = PreparingThread::spawn(project, move || AudioFiles::new(paths))?;
                let driver = SilentDriver::spawn(consumer, thread.shared().clone())?;
                Ok(Self { _stream: None, _silent: Some(driver), thread, no_sound: Some(reason) })
            }
        }
    }

    fn open_device(project: &Project, paths: Arc<ProjectPaths>) -> Result<Self, String> {
        let device = cpal::default_host().default_output_device().ok_or("there is no audio output device")?;
        let ranges: Vec<_> = device.supported_output_configs().map_err(|e| format!("reading the audio formats failed: {e}"))?.collect();
        let offers: Vec<FormatRange> = ranges
            .iter()
            .map(|range| FormatRange { channels: range.channels(), min_rate: range.min_sample_rate(), max_rate: range.max_sample_rate(), is_f32: range.sample_format() == SampleFormat::F32 })
            .collect();
        let chosen = choose_format(&offers)?;
        let mut config = ranges[chosen].clone().with_sample_rate(MIX_SAMPLE_RATE).config();
        config.buffer_size = BufferSize::Default;
        let channels = usize::from(config.channels);
        let mut scratch = vec![0.0f32; SCRATCH_FRAMES * MIX_CHANNELS];

        let (thread, mut consumer) = PreparingThread::spawn(project, move || AudioFiles::new(paths))?;
        let shared = thread.shared().clone();
        let error_shared = thread.error_reporter();
        let stream = device
            .build_output_stream(
                config,
                move |out: &mut [f32], info: &OutputCallbackInfo| fill_device_channels(&mut consumer, &shared, out, channels, &mut scratch, latency_of(info)),
                move |e| error_shared(format!("the audio device failed: {e}")),
                None,
            )
            .map_err(|e| format!("opening the audio stream failed: {e}"))?;
        stream.play().map_err(|e| format!("starting the audio stream failed: {e}"))?;
        Ok(Self { _stream: Some(stream), _silent: None, thread, no_sound: None })
    }

    pub fn thread(&self) -> &PreparingThread {
        &self.thread
    }

    /// Why nothing is heard, when the clock runs without a device.
    pub fn no_sound(&self) -> Option<&str> {
        self.no_sound.as_deref()
    }
}
