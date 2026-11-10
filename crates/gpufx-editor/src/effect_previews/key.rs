//! What decides that a stored preview is still the right one: the package, the images it was
//! rendered with, the size and length, and the version of the renderer.

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Bump when a change to the renderer makes old previews look different.
pub const PREVIEW_RENDER_VERSION: u32 = 1;

/// How big and how long a preview is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewSpec {
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub fps: u32,
}

impl PreviewSpec {
    pub const STANDARD: PreviewSpec = PreviewSpec { width: 144, height: 144, frames: 24, fps: 20 };
}

/// Tags that keep a directory and a zip file with the same bytes from sharing a digest.
const DIRECTORY_TAG: u8 = b'D';
const ARCHIVE_TAG: u8 = b'Z';

fn update_with_length(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn collect_files(root: &Path, folder: &Path, files: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(folder)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(root, &path, files)?;
        } else {
            let relative = path.strip_prefix(root).expect("a file under the root").components().map(|part| part.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            files.push((relative, path));
        }
    }
    Ok(())
}

/// SHA-256 of an effect package: a directory (every file by its relative path and bytes, in
/// path order) or a zip file (its bytes).
pub fn package_digest(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    if path.is_dir() {
        hasher.update([DIRECTORY_TAG]);
        let mut files = Vec::new();
        collect_files(path, path, &mut files)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));
        for (relative, file) in &files {
            update_with_length(&mut hasher, relative.as_bytes());
            let bytes = std::fs::read(file)?;
            update_with_length(&mut hasher, &bytes);
        }
    } else {
        hasher.update([ARCHIVE_TAG]);
        let mut file = std::fs::File::open(path)?;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    Ok(hasher.finalize().into())
}

fn hex(bytes: &[u8], characters: usize) -> String {
    let mut text: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    text.truncate(characters);
    text
}

/// The name of one preview: 16 hex characters over the package, the images and the spec.
pub fn preview_key(package: &[u8; 32], images: &[&[u8]], spec: PreviewSpec) -> String {
    let mut hasher = Sha256::new();
    hasher.update(PREVIEW_RENDER_VERSION.to_le_bytes());
    hasher.update(package);
    hasher.update((images.len() as u64).to_le_bytes());
    for image in images {
        update_with_length(&mut hasher, image);
    }
    for value in [spec.width, spec.height, spec.frames, spec.fps] {
        hasher.update(value.to_le_bytes());
    }
    hex(&hasher.finalize(), 16)
}

/// Eight hex characters that group every file of one package by its name.
pub fn package_prefix(name: &str) -> String {
    hex(&Sha256::digest(name.as_bytes()), 8)
}
