//! Rendered previews of effect packages, kept as files so an effect is never rendered again in a
//! later run. This is the part both the editor and the helper process that renders use: how a
//! preview is named ([`key`]) and how it is stored and found again ([`disk`]). Nothing here
//! touches the GPU or a window.

// Only the later steps (the renderer and the effect list) use most of this.
#![allow(dead_code)]

pub mod disk;
pub mod key;

pub use disk::{PreviewImage, PreviewStore, SweepReport, DISK_LIMIT};
pub use key::{package_digest, package_prefix, preview_key, PreviewSpec, PREVIEW_RENDER_VERSION};

#[cfg(test)]
mod tests;
