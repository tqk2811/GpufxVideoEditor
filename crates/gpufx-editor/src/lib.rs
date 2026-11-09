//! The timeline editor. [`timeline`], [`editor`] and most of [`preview`] are pure and tested
//! without a window or GPU; [`ui`], [`presenter`] and [`window`] only draw them and forward input.

#![cfg(windows)]

pub mod editor;
pub mod export;
pub mod i18n;
pub mod layout;
pub mod media;
pub mod packages;
pub mod params;
pub mod playback;
pub mod presenter;
pub mod preview;
pub mod project_file;
pub mod settings;
pub mod tasks;
pub mod timecode;
pub mod timeline;
pub mod ui;
pub mod window;
