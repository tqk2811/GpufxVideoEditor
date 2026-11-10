//! Where previews live on disk and how they are read and written so that two editors, or an
//! editor and its helper, never see a half-written file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::key::PreviewSpec;

const FOLDER: &str = "gpufx-editor";
const SUBFOLDER: &str = "effect-previews";
const VERSION_FOLDER: &str = "v1";

/// Past this the oldest files go at the start of the next run.
pub const DISK_LIMIT: u64 = 1024 * 1024 * 1024;

/// A write that crashed leaves a `.tmp` file; it is only swept once it is this old, so one that
/// another process is still writing is left alone.
const STALE_TMP: Duration = Duration::from_secs(60 * 60);

const STILL_SUFFIX: &str = "still.png";
const LOOP_SUFFIX: &str = "loop.png";
const ATTEMPT_SUFFIX: &str = "attempt";
const FAILED_SUFFIX: &str = "failed";
const TMP_EXTENSION: &str = "tmp";

/// A decoded preview: RGBA8 pixels, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// What a sweep removed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepReport {
    pub files: usize,
    pub bytes: u64,
}

pub struct PreviewStore {
    pub root: PathBuf,
}

fn io_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

/// Decodes a PNG that must be RGBA8, with every checksum verified.
fn decode(bytes: &[u8]) -> Result<PreviewImage, png::DecodingError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.ignore_checksums(false);
    let mut reader = decoder.read_info()?;
    let info = reader.info();
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(png::DecodingError::LimitsExceeded);
    }
    let mut rgba = vec![0u8; reader.output_buffer_size().ok_or(png::DecodingError::LimitsExceeded)?];
    let frame = reader.next_frame(&mut rgba)?;
    // Reading to the end checks the checksum of the last chunk too.
    reader.finish()?;
    rgba.truncate(frame.buffer_size());
    Ok(PreviewImage { width: frame.width, height: frame.height, rgba })
}

fn encode(image: &PreviewImage) -> std::io::Result<Vec<u8>> {
    if image.rgba.len() as u64 != image.width as u64 * image.height as u64 * 4 {
        return Err(io_error("the pixels do not match the size"));
    }
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| io_error(e.to_string()))?;
    writer.write_image_data(&image.rgba).map_err(|e| io_error(e.to_string()))?;
    writer.finish().map_err(|e| io_error(e.to_string()))?;
    Ok(bytes)
}

fn still_size_ok(spec: PreviewSpec, image: &PreviewImage) -> bool {
    image.width == spec.width && image.height == spec.height
}

/// A strip is as wide as a still and a whole number of stills tall, at most `spec.frames`.
fn loop_size_ok(spec: PreviewSpec, image: &PreviewImage) -> bool {
    image.width == spec.width && image.height >= spec.height && image.height % spec.height == 0 && image.height / spec.height <= spec.frames
}

impl PreviewStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `%LOCALAPPDATA%\gpufx-editor\effect-previews\v1`: local, not roaming, since previews can be
    /// rendered again.
    pub fn default_root() -> Option<PathBuf> {
        let folder = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty())?;
        Some(PathBuf::from(folder).join(FOLDER).join(SUBFOLDER).join(VERSION_FOLDER))
    }

    fn file(&self, package: &str, key: &str, suffix: &str) -> PathBuf {
        self.root.join(format!("{package}-{key}.{suffix}"))
    }

    pub fn still_path(&self, package: &str, key: &str) -> PathBuf {
        self.file(package, key, STILL_SUFFIX)
    }

    pub fn loop_path(&self, package: &str, key: &str) -> PathBuf {
        self.file(package, key, LOOP_SUFFIX)
    }

    /// Reads and checks one file. A missing file is `None`; one that does not decode or has an
    /// unexpected size is deleted and is `None` too.
    fn read_checked(path: &Path, size_ok: impl Fn(&PreviewImage) -> bool) -> std::io::Result<Option<PreviewImage>> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        match decode(&bytes) {
            Ok(image) if size_ok(&image) => Ok(Some(image)),
            _ => {
                let _ = std::fs::remove_file(path);
                Ok(None)
            }
        }
    }

    pub fn read_still(&self, package: &str, key: &str, spec: PreviewSpec) -> std::io::Result<Option<PreviewImage>> {
        Self::read_checked(&self.still_path(package, key), |image| still_size_ok(spec, image))
    }

    pub fn read_loop(&self, package: &str, key: &str, spec: PreviewSpec) -> std::io::Result<Option<PreviewImage>> {
        Self::read_checked(&self.loop_path(package, key), |image| loop_size_ok(spec, image))
    }

    /// Writes `bytes` to `target` through a `.tmp` file in the same folder. When the rename fails
    /// but the target is already there and valid, another process won and that counts as done.
    fn write_atomic(&self, target: &Path, bytes: &[u8], size_ok: impl Fn(&PreviewImage) -> bool) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let mut name = target.file_name().expect("a file name").to_os_string();
        name.push(format!(".{}.{TMP_EXTENSION}", std::process::id()));
        let temporary = target.with_file_name(name);
        let result = (|| {
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, target)
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&temporary);
            return match Self::read_checked(target, size_ok) {
                Ok(Some(_)) => Ok(()),
                _ => Err(e),
            };
        }
        Ok(())
    }

    fn write_image(&self, package: &str, key: &str, suffix: &str, image: &PreviewImage, size_ok: impl Fn(&PreviewImage) -> bool) -> std::io::Result<()> {
        let bytes = encode(image)?;
        if !size_ok(image) {
            return Err(io_error("the preview has an unexpected size"));
        }
        self.write_atomic(&self.file(package, key, suffix), &bytes, size_ok)?;
        self.remove_other_keys(package, key);
        Ok(())
    }

    pub fn write_still(&self, package: &str, key: &str, spec: PreviewSpec, image: &PreviewImage) -> std::io::Result<()> {
        self.write_image(package, key, STILL_SUFFIX, image, |image| still_size_ok(spec, image))
    }

    pub fn write_loop(&self, package: &str, key: &str, spec: PreviewSpec, image: &PreviewImage) -> std::io::Result<()> {
        self.write_image(package, key, LOOP_SUFFIX, image, |image| loop_size_ok(spec, image))
    }

    /// Deletes what an older version of the package left: files of this package with another key.
    /// Files of the same key and of other packages stay, and so do `.tmp` files, which belong to
    /// a write in progress.
    fn remove_other_keys(&self, package: &str, key: &str) {
        let Ok(entries) = std::fs::read_dir(&self.root) else { return };
        let prefix = format!("{package}-");
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix(&prefix) else { continue };
            if name.ends_with(&format!(".{TMP_EXTENSION}")) {
                continue;
            }
            let file_key = rest.split('.').next().unwrap_or("");
            if file_key != key {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn read_text(path: &Path) -> Option<String> {
        std::fs::read(path).ok().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Records one more attempt to render this preview and returns how many were recorded before
    /// it, so the caller can tell a first try from a second one after a crash.
    pub fn mark_attempt(&self, package: &str, key: &str) -> std::io::Result<u32> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.file(package, key, ATTEMPT_SUFFIX);
        let before = Self::read_text(&path).and_then(|text| text.trim().parse::<u32>().ok()).unwrap_or(0);
        std::fs::write(&path, (before + 1).to_string())?;
        Ok(before)
    }

    pub fn clear_attempt(&self, package: &str, key: &str) {
        let _ = std::fs::remove_file(self.file(package, key, ATTEMPT_SUFFIX));
    }

    pub fn mark_failed(&self, package: &str, key: &str, reason: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(self.file(package, key, FAILED_SUFFIX), reason.as_bytes())
    }

    /// Why rendering this preview failed, when it did.
    pub fn failure(&self, package: &str, key: &str) -> Option<String> {
        Self::read_text(&self.file(package, key, FAILED_SUFFIX))
    }

    /// Run once at start: removes stale `.tmp` files, then the oldest files while the folder is
    /// over `limit_bytes`.
    pub fn sweep(&self, limit_bytes: u64, now: SystemTime) -> std::io::Result<SweepReport> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SweepReport::default()),
            Err(e) => return Err(e),
        };
        let mut report = SweepReport::default();
        let mut kept: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else { continue };
            if !metadata.is_file() {
                continue;
            }
            let modified = metadata.modified().unwrap_or(now);
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == TMP_EXTENSION) {
                let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
                if age >= STALE_TMP && std::fs::remove_file(&path).is_ok() {
                    report.files += 1;
                    report.bytes += metadata.len();
                    continue;
                }
            }
            kept.push((modified, metadata.len(), path));
        }
        let mut total: u64 = kept.iter().map(|(_, size, _)| size).sum();
        kept.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, size, path) in kept {
            if total <= limit_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= size;
                report.files += 1;
                report.bytes += size;
            }
        }
        Ok(report)
    }
}
