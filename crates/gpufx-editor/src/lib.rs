//! The timeline editor. [`timeline`], [`editor`] and most of [`preview`] are pure and tested
//! without a window or GPU; [`ui`], [`presenter`] and [`window`] only draw them and forward input.

#![cfg(windows)]

pub mod agent;
pub mod asset_install;
pub mod audio_library;
pub mod beats;
pub(crate) mod child_process;
pub mod dpi_window;
pub mod editor;
pub mod effect_previews;
pub mod export;
pub mod face_censor;
pub mod font_install;
pub mod font_library;
pub mod i18n;
pub mod layout;
pub mod media;
pub mod matting_models;
pub mod model_download;
pub mod package_import;
pub mod package_manager;
pub mod packages;
pub mod params;
pub mod playback;
pub mod presenter;
pub mod preview;
pub mod project_bundle;
pub mod project_file;
pub mod project_lock;
pub mod project_zip;
pub mod proxy;
pub mod recovery;
pub mod save_dialog;
pub mod search;
pub mod settings;
pub mod clip_animation_install;
pub mod clip_animation_library;
pub mod sticker_install;
pub mod sticker_library;
pub mod sticker_motion;
pub mod subtitle_style_install;
pub mod subtitle_track;
pub mod tasks;
pub mod tracking;
pub mod tracking_models;
pub mod track_edit;
pub mod timecode;
pub mod timeline;
pub mod ui;
pub mod visuals;
pub mod text_clip;
pub mod text_shapes;
pub mod text_style_install;
pub mod text_style_library;
pub mod window;
