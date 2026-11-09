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
//!
//! Floating point is never used on timeline paths.

pub mod project;
pub mod render;
pub mod schedule;
pub mod time;
pub mod validate;

pub use project::{Clip, Effect, LoadError, Output, Project, Track, Transition, SUPPORTED_VERSION};
pub use schedule::{
    frame_count, frame_time, layers_at_frame, Layer, LayerContent, Progress, SourceSample,
    PROGRESS_PPM_SCALE,
};
pub use time::{Time, TICKS_PER_SECOND};
pub use validate::{validate, AcceptAll, Resolver, ValidationError};
