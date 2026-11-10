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

mod alpha_merge;
mod audio;
mod compositor;
mod decoder;
mod effect_preview;
mod effect_preview_protocol;
mod encoder;
mod export;
mod ffi;
mod gpu;
mod packages;
mod paths;
mod probe;
mod progress;
mod rotated_place;
mod still;
mod thumbnail;

pub use audio::{AudioFileDecoder, AudioFiles};
pub use compositor::{D3d12Compositor, SharedImage, SourceDecoder, SourceFrame};
pub use decoder::{is_cancelled, DecodedFrame, PublishClock, SequentialDecoder};
pub use effect_preview::{preview_image_files, PreviewError, PreviewLoop, PreviewRenderer, PREVIEW_FPS, PREVIEW_FRAMES, PREVIEW_SIZE};
pub use effect_preview_protocol::{InvalidJob, PreviewBlame, PreviewEvent, PreviewJob, PreviewJobKind, PREVIEW_PROTOCOL};
pub use encoder::{encoder_exists, EncoderChoice, Mp4Encoder, AUTO_ENCODERS};
pub use export::{overwritten_input, reserve_temporary, same_file, ExportControl, ExportError, ExportJob, ExportReport};
pub use gpu::Gpu;
pub use packages::{expression_error, read_package_info, read_zip_package_info, PackageInfo, ParamInfo, ParamKind, CLIP_DURATION, CLIP_TIME};
pub use paths::{asset_source, check_path_part, is_asset_source, is_safe_id, parse_asset_source, AssetProblem, AssetRef, PathPartProblem, ProjectPaths, ASSET_AUDIO_EXTENSIONS, ASSET_IMAGE_EXTENSIONS, ASSET_PREFIX, ASSET_TYPES, MAX_ASSET_ID_LENGTH};
pub use probe::{probe_media, MediaProbe};
pub use progress::{ProgressEvent, CANCEL_LINE, MAX_EVENT_LINE, PROGRESS_PROTOCOL};
pub use still::{read_still, still_dimensions, still_thumbnail, StillPicture, MAX_STILL_SIDE};
pub use thumbnail::{FrameSampler, ThumbnailError, ThumbnailImage};
