//! The Windows render backend: D3D12VA decoding, gpufx compositing on one shared D3D12 device, and
//! libav encoding. It implements the traits in `engine::render`; nothing here touches the project
//! model or the scheduler.
//!
//! Rendering is single threaded, but a window may present from another thread. So every
//! `queue.submit`, and the part of `process_d3d12` from staging its fences to taking them back,
//! holds [`Gpu::lock_submits`]: no other submit may come between them, which is the contract of
//! `GpuContext::from_device` for one queue. So does computing output sizes, which submits. The rest
//! of `process_d3d12`, such as building pipelines, runs outside it.

#![cfg(windows)]

mod audio;
mod compositor;
mod decoder;
mod encoder;
mod export;
mod ffi;
mod gpu;
mod packages;
mod paths;
mod probe;
mod progress;
mod thumbnail;

pub use audio::{AudioFileDecoder, AudioFiles};
pub use compositor::{D3d12Compositor, SharedImage};
pub use decoder::{is_cancelled, DecodedFrame, PublishClock, SequentialDecoder};
pub use encoder::{encoder_exists, EncoderChoice, Mp4Encoder, AUTO_ENCODERS};
pub use export::{reserve_temporary, ExportControl, ExportError, ExportJob, ExportReport};
pub use gpu::Gpu;
pub use packages::{expression_error, read_package_info, PackageInfo, ParamInfo, ParamKind};
pub use paths::ProjectPaths;
pub use probe::{probe_media, MediaProbe};
pub use progress::{ProgressEvent, CANCEL_LINE, MAX_EVENT_LINE, PROGRESS_PROTOCOL};
pub use thumbnail::{FrameSampler, ThumbnailError, ThumbnailImage};
