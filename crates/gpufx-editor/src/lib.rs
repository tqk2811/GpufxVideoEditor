//! The timeline editor. [`timeline`], [`editor`] and most of [`preview`] are pure and tested
//! without a window or GPU; [`ui`], [`presenter`] and [`window`] only draw them and forward input.

#![cfg(windows)]

pub mod editor;
pub mod presenter;
pub mod preview;
pub mod timeline;
pub mod ui;
pub mod window;
