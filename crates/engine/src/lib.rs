//! Editing engine: project model, validation and frame scheduling.
//!
//! This crate is pure Rust. It has no GPU, FFmpeg or decoder dependency, so
//! everything here can be tested without media files or a graphics device.
//!
//! * [`Time`] is the timeline unit: an `i64` count of 1/90000 s ticks.
//! * [`Project`] is the JSON project model ([`Project::from_json`] rejects unknown versions).
//! * [`validate`] checks a project and reports every problem with its location.
//! * [`schedule`] turns a project and a frame index into the layers to composite.
//! * [`render`] drives a project through decoder, compositor and sink traits a backend implements.
//! * [`audio_schedule`] turns a project and a sample range into the source segments to mix.
//! * [`audio_mix`] sums those segments, reading sources through a trait a backend implements.
//! * [`audio_clock`] is the device-independent audio clock and pull API.
//! * [`edit`] holds the pure timeline edit commands and the undo history.
//!
//! Floating point is never used on timeline paths. Audio uses it only for gains and sample values.

pub mod audio_clock;
pub mod audio_mix;
pub mod audio_schedule;
pub mod edit;
pub mod karaoke;
pub mod keyframe;
pub mod project;
pub mod render;
pub mod schedule;
pub mod srt;
pub mod stretch;
pub mod clip_animation;
pub mod per_unit;
pub mod time;
pub mod validate;

pub use audio_clock::{
    audio_channel, sample_time, AudioClock, AudioConsumer, AudioProducer, PrepareReport, PullReport,
};
pub use audio_mix::{mix_audio, mix_audio_with, AudioSource, MixReport};
pub use stretch::StretchStates;
pub use audio_schedule::{
    audio_sample_count, audio_segments, sample_position, AudioSegment, GainEnvelope, MIX_CHANNELS,
    MIX_SAMPLE_RATE,
};
pub use edit::{
    add_clip, add_effect, add_subtitle_track, add_text_clip, restyle_track, text_clip, ClipRestyle, set_text, set_animation, add_track, remove_track, add_transition, clip_at, delete_clip, move_clip,
    nearest_free_start, remove_effect, remove_keyframe, remove_transition, set_effect_params, set_effect_params_at, set_fades, set_gain,
    set_frame_duration, set_keyframe, set_motion_value, set_muted, set_opacity, set_output_size, set_placement, set_placement_at, set_rotation, set_speed, set_track_hidden, set_track_locked, set_transition_params, split_clip, trim_end, trim_source_in, trim_start, EditError, EditHistory,
};
pub use karaoke::{clip_words, karaoke_problem, KaraokeLook, KaraokeMode, KaraokeWord, TextKaraoke, WordState, WordStyle, MAX_KARAOKE_SCALE};
pub use keyframe::{
    effects_at, keyframe_neighbors, keyframe_tolerance, nearest_keyframe, nearest_keyframe_inside, sample_keyframes, ClipMotion, ClipProperty, ClipTransform, Easing, Keyframe, KeyframeTarget, MAX_KEYFRAMES, MAX_KEYFRAME_TIME, MAX_ROTATION,
};
pub use project::{
    ClipPlacement, PlacedRect, MAX_PLACEMENT_OFFSET, MAX_PLACEMENT_WIDTH, MIN_PLACEMENT_WIDTH,
    AudioOutput, Clip, Effect, LoadError, Output, Project, SubtitleTrack, Track, TrackKind, Transition,
    FontRef, Rgba, TextAlign, ClipAnimation, AnimationRef, TextBackground, TextContent, TextOutline, TextShadow, TextGlow, MAX_GLOW_INTENSITY, GLOW_FALLOFF_RANGE, TextTransform, NORMAL_WEIGHT, TEXT_WEIGHT_RANGE, MAX_LETTER_SPACING, MAX_GLOW_RADIUS, TEXT_SOURCE, text_raster_scale, text_step_up, MAX_TEXT_SCALE, MIN_TEXT_SCALE,
    is_image_source, speed_in_range, speed_milli, IMAGE_EXTENSIONS, MAX_SPEED, MIN_FRAME_DURATION, MIN_SPEED, MIN_SUPPORTED_VERSION, SPEED_SCALE, SUPPORTED_VERSION,
};
pub use render::{ClipKey, DecoderSpec, RenderError};
pub use schedule::{
    frame_count, frame_time, layers_at_frame, ClipClock, Layer, LayerContent, Progress, SourceSample,
    PROGRESS_PPM_SCALE,
};
pub use clip_animation::{animation_effects, animation_has_per_unit, animation_max_scale, animation_unit_poses, preset_is_per_unit, check_curve, animation_pose, effects_with_animation, PresetEffect, animation_rotates, find_preset, is_pack_preset, pack_presets, preset_pose, set_pack_presets, PackPreset, PackPresets, PACK_PRESET_SEPARATOR, AnimationCurve, AnimationGroup, CurveKey, CurveProperty, Ease, AnimationPose, AnimationPreset, ANIMATION_PRESETS};
pub use per_unit::{check_per_unit, line_ranges, unit_ranges, PerUnit, RangeSelector, SelectorShape, TextUnit, UnitOrder};
pub use srt::{format_timestamp, parse_srt, strip_tags, write_srt, SrtCue, SrtDocument, SrtWarning, SrtWarningKind};
pub use time::{Time, TICKS_PER_SECOND};
pub use validate::{text_style_problems, validate, AcceptAll, Resolver, TextStyleProblem, ValidationError};
