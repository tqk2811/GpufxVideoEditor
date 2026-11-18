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
mod animated_gif;
mod animated_webp;
mod audio;
mod clip_animation_pack;
mod compositor;
mod decoder;
mod effect_preview;
mod effect_preview_protocol;
mod encoder;
mod export;
mod ffi;
mod gpu;
mod matte;
mod pack_presets;
mod packages;
mod orientation;
mod paths;
mod probe;
mod progress;
mod rotated_place;
mod preset_effects;
mod still;
mod thumbnail;

pub use audio::{AudioFileDecoder, AudioFiles};
pub use compositor::{D3d12Compositor, SharedImage, SourceDecoder, SourceFrame};
pub use decoder::{is_cancelled, DecodedFrame, PublishClock, ReverseDecoder, SequentialDecoder};
pub use effect_preview::{preview_image_files, PreviewError, PreviewLoop, PreviewRenderer, PREVIEW_FPS, PREVIEW_FRAMES, PREVIEW_SIZE};
pub use effect_preview_protocol::{InvalidJob, PreviewBlame, PreviewEvent, PreviewJob, PreviewJobKind, PREVIEW_PROTOCOL};
pub use encoder::{auto_select, encoder_available, encoder_exists, needs_gpl, probe_encoder, EncoderChoice, Mp4Encoder, VideoCodec};
pub use export::{overwritten_input, reserve_temporary, same_file, ExportControl, ExportError, ExportJob, ExportReport};
pub use gpu::Gpu;
pub use matte::{check_matting_package, find_matting_model, matte_radii, model_folders, model_in, user_models_dir, MatteCache, MATTE_CACHE_BYTES, MATTING_FOLDER, MATTING_MODELS_ENV};
pub use clip_animation_pack::{check_clip_animation_fields, is_clip_animation_type, parse_clip_animation_manifest, scan_clip_animation_folders, ClipAnimationManifest, ClipAnimationManifestItem, ClipAnimationScan, ItemEffect, ManifestProblem, PackLicense, PackReadProblem, CLIP_ANIMATION_FOLDER, CLIP_KINDS, DEFAULT_PROGRESS_PARAM, MAX_DEFAULT_DURATION, MAX_MANIFEST_ITEMS as MAX_CLIP_ANIMATION_ITEMS};
pub use pack_presets::read_pack_presets;
pub use preset_effects::{effect_mismatch, resolve_item_effects, ItemEffects};
pub use packages::{expression_error, read_package_info, read_zip_package_info, PackageInfo, ParamInfo, ParamKind, CLIP_DURATION, CLIP_TIME};
pub use paths::{asset_source, check_path_part, pack_key, pack_key_of, read_disabled_packs, DISABLED_PACKS_FILE, is_asset_source, is_safe_id, is_safe_pack_file, parse_asset_source, AssetProblem, AssetRef, PathPartProblem, ProjectPaths, ProxyFile, ProxyTiming, ASSET_AUDIO_EXTENSIONS, ASSET_FONT_EXTENSIONS, ASSET_IMAGE_EXTENSIONS, ASSET_PREFIX, ASSET_TEXTURE_EXTENSIONS, ASSET_TYPES, FONT_ASSET_TYPE, MAX_ASSET_ID_LENGTH, SUBTITLE_STYLE_ASSET_TYPE, TEXT_STYLE_ASSET_TYPE};
pub use probe::{frame_ticks_floor, probe_media, probe_video_format, MediaProbe, VideoFormat};
pub use progress::{ProgressEvent, CANCEL_LINE, MAX_EVENT_LINE, PROGRESS_PROTOCOL};
pub use still::{animation_timing, read_still, still_dimensions, still_thumbnail, AnimationTiming, StillPicture, MAX_STILL_SIDE};
pub use thumbnail::{FrameSampler, ThumbnailError, ThumbnailImage};
