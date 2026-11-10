//! The timeline editor. [`timeline`], [`editor`] and most of [`preview`] are pure and tested
//! without a window or GPU; [`ui`], [`presenter`] and [`window`] only draw them and forward input.

#![cfg(windows)]

pub mod agent;
pub mod asset_install;
pub mod audio_library;
pub(crate) mod child_process;
pub mod editor;
pub mod effect_previews;
pub mod export;
pub mod i18n;
pub mod layout;
pub mod media;
pub mod package_import;
pub mod packages;
pub mod params;
pub mod playback;
pub mod presenter;
pub mod preview;
pub mod project_file;
pub mod project_lock;
pub mod settings;
pub mod sticker_install;
pub mod sticker_library;
pub mod tasks;
pub mod timecode;
pub mod timeline;
pub mod ui;
pub mod visuals;
pub mod window;
