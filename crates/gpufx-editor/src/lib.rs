//! The timeline editor. [`timeline`] and [`editor`] are pure and tested without a window; [`ui`],
//! [`presenter`] and [`window`] only draw them and forward input.

pub mod editor;
pub mod presenter;
pub mod timeline;
pub mod ui;
pub mod window;
