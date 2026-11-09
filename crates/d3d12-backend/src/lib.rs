//! The Windows render backend: D3D12VA decoding, gpufx compositing on one shared D3D12 device, and
//! libav encoding. It implements the traits in `engine::render`; nothing here touches the project
//! model or the scheduler.
//!
//! Rendering is single threaded, but a window may present from another thread. So every
//! `process_d3d12` and `queue.submit` holds [`Gpu::lock_submits`]: none may overlap
//! `process_d3d12`, which is the contract of `GpuContext::from_device` for one queue.

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

pub use audio::{AudioFileDecoder, AudioFiles};
pub use compositor::{D3d12Compositor, SharedImage};
pub use decoder::{is_cancelled, DecodedFrame, PublishClock, SequentialDecoder};
pub use encoder::{encoder_exists, EncoderChoice, Mp4Encoder, AUTO_ENCODERS};
pub use export::{temporary_path, ExportControl, ExportError, ExportJob, ExportReport};
pub use gpu::Gpu;
pub use packages::{read_package_info, PackageInfo};
pub use paths::ProjectPaths;
