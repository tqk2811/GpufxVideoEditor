//! The editor's wrapper around a gpufx effect package. A gpufx package holds nothing of the editor;
//! the editor ships each one inside a folder `<name>/` holding:
//!
//! - `manifest.json`: `{"format":1,"type":"gpufx","id":"<inner id>","version":"...","package":"<name>.zip","tags":[...]}`,
//!   strict: an unknown field is an error. The name and description are the inner manifest's;
//!   `tags` are English search tags.
//! - `i18n/<code>.json`: the translations, the only ones the editor reads for the package.
//! - `<name>.zip`: the gpufx package, untouched, opened by `gpufx_core::Package::open`.
//!
//! It is distributed as `<name>.zip` holding those three, and installed unpacked one level. A bare
//! gpufx package (a folder whose `manifest.json` has `used_for`, or a gpufx `.zip`) still loads,
//! without translations: a wrapper's manifest has `format` and `type`, a gpufx one has neither.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// The `format` of a wrapper manifest this editor reads.
pub const WRAPPER_FORMAT: u32 = 1;
/// The `type` of a wrapper manifest.
pub const WRAPPER_TYPE: &str = "gpufx";
/// A wrapper manifest is a few hundred bytes.
const MAX_WRAPPER_MANIFEST_BYTES: u64 = 1 << 20;

/// The manifest of a wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrapperManifest {
    pub format: u32,
    #[serde(rename = "type")]
    pub kind: String,
    /// The id of the inner gpufx package.
    pub id: String,
    pub version: String,
    /// The file name of the inner gpufx zip, beside the manifest.
    pub package: String,
    /// English search tags.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl WrapperManifest {
    /// The folder name of the wrapper: [`Self::package`] without `.zip`.
    pub fn name(&self) -> &str {
        &self.package[..self.package.len() - ".zip".len()]
    }
}

/// Whether the manifest `bytes` declare a wrapper: a JSON object with `format` and `"type":"gpufx"`.
/// A wrapper that is invalid otherwise is still one, so its problem is reported.
pub fn is_wrapper_manifest(bytes: &[u8]) -> bool {
    let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(strip_bom(bytes)) else { return false };
    object.contains_key("format") && object.get("type").and_then(Value::as_str) == Some(WRAPPER_TYPE)
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
}

/// Parses and checks a wrapper manifest.
pub fn parse_wrapper_manifest(bytes: &[u8]) -> Result<WrapperManifest, String> {
    let manifest: WrapperManifest = serde_json::from_slice(strip_bom(bytes)).map_err(|e| e.to_string())?;
    if manifest.format != WRAPPER_FORMAT {
        return Err(format!("format {} is not {WRAPPER_FORMAT}", manifest.format));
    }
    if manifest.kind != WRAPPER_TYPE {
        return Err(format!("type \"{}\" is not \"{WRAPPER_TYPE}\"", manifest.kind));
    }
    if manifest.id.trim().is_empty() {
        return Err("the id is empty".to_owned());
    }
    let lower = manifest.package.to_ascii_lowercase();
    let stem = lower.strip_suffix(".zip").unwrap_or_default();
    if !lower.ends_with(".zip") || !crate::is_safe_id(stem) || manifest.package.len() != stem.len() + 4 {
        return Err(format!("\"{}\" is not the file name of a .zip beside the manifest", manifest.package));
    }
    Ok(manifest)
}

/// The wrapper manifest of the folder `folder`: `None` when it is not a wrapper (no manifest, or a
/// gpufx one), else the manifest or why it is invalid.
pub fn read_wrapper(folder: &Path) -> Option<Result<WrapperManifest, String>> {
    if !folder.is_dir() {
        return None;
    }
    let file = std::fs::File::open(folder.join("manifest.json")).ok()?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, MAX_WRAPPER_MANIFEST_BYTES + 1), &mut bytes).ok()?;
    if !is_wrapper_manifest(&bytes) {
        return None;
    }
    if bytes.len() as u64 > MAX_WRAPPER_MANIFEST_BYTES {
        return Some(Err("the manifest is too large".to_owned()));
    }
    Some(parse_wrapper_manifest(&bytes))
}

/// The path gpufx opens for the package at `path`: the inner zip of a wrapper folder, else `path`
/// itself (a bare gpufx folder or zip). Fails for a wrapper that is invalid or lacks its zip.
pub fn effect_package_path(path: &Path) -> Result<PathBuf, String> {
    match read_wrapper(path) {
        None => Ok(path.to_path_buf()),
        Some(Err(message)) => Err(format!("its wrapper manifest is invalid: {message}")),
        Some(Ok(manifest)) => {
            let inner = path.join(&manifest.package);
            if inner.is_file() {
                Ok(inner)
            } else {
                Err(format!("the wrapper has no {}", manifest.package))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"format":1,"type":"gpufx","id":"io.example.iris","version":"1.0.0","package":"iris.zip","tags":["circle"]}"#;

    #[test]
    fn a_wrapper_is_told_from_a_gpufx_manifest() {
        assert!(is_wrapper_manifest(GOOD.as_bytes()));
        assert!(is_wrapper_manifest(br#"{"format":2,"type":"gpufx"}"#), "an invalid wrapper is still one");
        assert!(!is_wrapper_manifest(br#"{"id":"a","used_for":["gpufx"]}"#));
        assert!(!is_wrapper_manifest(br#"{"type":"gpufx"}"#), "no format");
        assert!(!is_wrapper_manifest(br#"{"format":1,"type":"sticker"}"#));
        assert!(!is_wrapper_manifest(b"nope"));
    }

    #[test]
    fn the_wrapper_manifest_is_strict() {
        let manifest = parse_wrapper_manifest(GOOD.as_bytes()).unwrap();
        assert_eq!((manifest.id.as_str(), manifest.name(), manifest.tags.as_slice()), ("io.example.iris", "iris", ["circle".to_owned()].as_slice()));
        let bad = [
            r#"{"format":1,"type":"gpufx","id":"a","version":"1","package":"a.zip","name":"A"}"#,
            r#"{"format":2,"type":"gpufx","id":"a","version":"1","package":"a.zip"}"#,
            r#"{"format":1,"type":"gpufx","id":"","version":"1","package":"a.zip"}"#,
            r#"{"format":1,"type":"gpufx","id":"a","version":"1","package":"a.json"}"#,
            r#"{"format":1,"type":"gpufx","id":"a","version":"1","package":"../a.zip"}"#,
            r#"{"format":1,"type":"gpufx","id":"a","version":"1","package":"sub/a.zip"}"#,
            r#"{"format":1,"type":"gpufx","id":"a","package":"a.zip"}"#,
        ];
        for text in bad {
            assert!(parse_wrapper_manifest(text.as_bytes()).is_err(), "{text}");
        }
    }

    #[test]
    fn a_wrapper_folder_opens_its_inner_zip_and_a_bare_package_opens_itself() {
        let dir = std::env::temp_dir().join(format!("gpufx-wrapper-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let wrapper = dir.join("iris");
        std::fs::create_dir_all(&wrapper).unwrap();
        std::fs::write(wrapper.join("manifest.json"), GOOD).unwrap();
        assert!(effect_package_path(&wrapper).is_err(), "the inner zip is missing");
        std::fs::write(wrapper.join("iris.zip"), b"zip").unwrap();
        assert_eq!(effect_package_path(&wrapper).unwrap(), wrapper.join("iris.zip"));
        let bare = dir.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::write(bare.join("manifest.json"), br#"{"id":"a","used_for":["gpufx"]}"#).unwrap();
        assert_eq!(effect_package_path(&bare).unwrap(), bare);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
