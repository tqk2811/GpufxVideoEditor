//! Installing a subtitle style pack from a `.zip`. No window here.
//!
//! A zip with `"type": "subtitle-style"` is unpacked once to
//! `<user root>/packages/subtitle-style/<id lower-cased>/`. An item is a text style (the schema of
//! [`crate::text_style_install`]) with a `layout`, where the subtitle sits, and an optional
//! `karaoke`, how the spoken word is marked. The karaoke part is read and checked but not used yet.
//!
//! A shared text-pack module for text and subtitle styles was considered; the user chose one module
//! per pack type.

use std::fs::{self, File};
use std::io::BufReader;
use std::path::Path;

use engine::Rgba;
use serde::Deserialize;
use zip::ZipArchive;

use crate::asset_install::{check_entry_extensions, check_item_ids, check_named_files, check_pack_header, clean_leftovers, lock_installs, rename_with_retries, unpack_into_place, AudioLicense, Unpacked, MANIFEST};
use crate::package_import::{entries_of, read_manifest, validate_entries, ImportOutcome, ImportProblem};
use crate::text_style_install::{check_style_item_parts, no_media, PackStyleAnimation, PackTextStyle, STYLE_PACK_EXTRACT, STYLE_PACK_LIMITS};

/// What the `type` of a subtitle style pack says, and the folder it goes to.
pub const SUBTITLE_STYLE_TYPE: &str = "subtitle-style";
/// The most lines a subtitle layout may ask for.
pub const MAX_SUBTITLE_LINES: u32 = 10;
/// The most a karaoke look may scale a word.
pub const MAX_KARAOKE_SCALE: f32 = 4.0;

/// A subtitle style pack that was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledSubtitleStylePack {
    pub id: String,
    pub name: String,
    pub replaced: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubtitleStyleManifest {
    pub format: u32,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: AudioLicense,
    pub items: Vec<SubtitleStyleManifestItem>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubtitleStyleManifestItem {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub preview_text: Option<String>,
    pub style: PackTextStyle,
    #[serde(default)]
    pub animation: Option<PackStyleAnimation>,
    pub layout: SubtitleLayout,
    #[serde(default)]
    pub karaoke: Option<SubtitleKaraoke>,
}

/// Where a subtitle sits in the frame.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubtitleLayout {
    /// The middle of the text, in parts of the frame height from the top, 0 to 1.
    pub anchor_y: f64,
    /// The widest the text may be, in parts of the frame width, above 0 and at most 1.
    pub max_width: f64,
    /// The most lines a cue is broken into, 1 to [`MAX_SUBTITLE_LINES`]. Kept, not used yet.
    pub max_lines: u32,
}

/// How a karaoke style marks the words. Read and kept; nothing draws it yet.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubtitleKaraoke {
    pub mode: KaraokeMode,
    #[serde(default)]
    pub active: Option<KaraokeLook>,
    #[serde(default)]
    pub spoken: Option<KaraokeLook>,
    #[serde(default)]
    pub upcoming: Option<KaraokeLook>,
    #[serde(default)]
    pub emphasis: Option<KaraokeLook>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KaraokeMode {
    None,
    Color,
    Box,
    Scale,
}

/// The look of the words in one state.
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KaraokeLook {
    #[serde(default)]
    pub color: Option<Rgba>,
    #[serde(default)]
    pub background: Option<Rgba>,
    /// Above 0 and at most [`MAX_KARAOKE_SCALE`].
    #[serde(default)]
    pub scale: Option<f32>,
    /// 0 to 1.
    #[serde(default)]
    pub opacity: Option<f32>,
}

fn check_layout(layout: &SubtitleLayout) -> Result<(), String> {
    if !(layout.anchor_y.is_finite() && (0.0..=1.0).contains(&layout.anchor_y)) {
        return Err(format!("layout.anchor_y must be from 0 to 1, not {}", layout.anchor_y));
    }
    if !(layout.max_width.is_finite() && layout.max_width > 0.0 && layout.max_width <= 1.0) {
        return Err(format!("layout.max_width must be above 0 and at most 1, not {}", layout.max_width));
    }
    if !(1..=MAX_SUBTITLE_LINES).contains(&layout.max_lines) {
        return Err(format!("layout.max_lines must be from 1 to {MAX_SUBTITLE_LINES}, not {}", layout.max_lines));
    }
    Ok(())
}

fn check_karaoke(karaoke: &SubtitleKaraoke) -> Result<(), String> {
    for (state, look) in [("active", karaoke.active), ("spoken", karaoke.spoken), ("upcoming", karaoke.upcoming), ("emphasis", karaoke.emphasis)] {
        let Some(look) = look else { continue };
        if let Some(scale) = look.scale {
            if !(scale.is_finite() && scale > 0.0 && scale <= MAX_KARAOKE_SCALE) {
                return Err(format!("karaoke.{state}.scale must be above 0 and at most {MAX_KARAOKE_SCALE}, not {scale}"));
            }
        }
        if let Some(opacity) = look.opacity {
            if !(opacity.is_finite() && (0.0..=1.0).contains(&opacity)) {
                return Err(format!("karaoke.{state}.opacity must be from 0 to 1, not {opacity}"));
            }
        }
    }
    Ok(())
}

/// Parses a manifest. The fields are exactly those of the format.
pub fn parse_subtitle_style_manifest(bytes: &[u8]) -> Result<SubtitleStyleManifest, ImportProblem> {
    serde_json::from_slice(bytes).map_err(|e| ImportProblem::BadManifest(e.to_string()))
}

/// Checks what a manifest says, apart from the files. The reader of installed packs makes the same
/// check, so a pack that installs reads.
pub fn check_subtitle_style_fields(manifest: &SubtitleStyleManifest) -> Result<(), ImportProblem> {
    if manifest.kind != SUBTITLE_STYLE_TYPE {
        return Err(ImportProblem::UnknownType(manifest.kind.clone()));
    }
    check_pack_header(manifest.format, &manifest.id, &manifest.name, &manifest.version, &manifest.license, manifest.items.len(), "styles")?;
    check_item_ids(manifest.items.iter().map(|item| (item.id.as_str(), item.name.as_str())))?;
    for item in &manifest.items {
        check_style_item_parts(&item.id, &item.name, item.category.as_deref(), item.preview_text.as_deref(), &item.style, item.animation.as_ref())?;
        let in_item = |message: String| ImportProblem::BadManifest(format!("the item \"{}\": {message}", item.id));
        check_layout(&item.layout).map_err(in_item)?;
        item.karaoke.as_ref().map_or(Ok(()), check_karaoke).map_err(in_item)?;
    }
    Ok(())
}

/// Checks the zip at `source` and installs it under `user_root`. An existing pack of the same id is
/// replaced only when `overwrite` allows it.
pub fn install_subtitle_style_pack(source: &Path, user_root: &Path, overwrite: bool) -> Result<ImportOutcome, ImportProblem> {
    let packages = user_root.join("packages");
    fs::create_dir_all(&packages).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?;
    // Held for the whole install, as for the other packs: the working folders are shared.
    let _lock = lock_installs(&packages).map_err(|e| ImportProblem::WriteFailed(format!("another install is running ({e})")))?;
    clean_leftovers(&packages);

    let file = File::open(source).map_err(|e| ImportProblem::Unreadable(e.to_string()))?;
    let mut zip = ZipArchive::new(BufReader::new(file)).map_err(|e| ImportProblem::NotAZip(e.to_string()))?;
    let entries = entries_of(&mut zip, STYLE_PACK_LIMITS)?;
    validate_entries(&entries, STYLE_PACK_LIMITS)?;
    check_entry_extensions(&entries, no_media)?;
    let manifest = parse_subtitle_style_manifest(&read_manifest(&mut zip)?)?;
    check_subtitle_style_fields(&manifest)?;
    check_named_files(&manifest.license.files, std::iter::empty(), &entries, no_media, "media")?;

    let id = manifest.id.to_ascii_lowercase();
    let check_unpacked = |temporary: &Path| {
        let unpacked = parse_subtitle_style_manifest(&fs::read(temporary.join(MANIFEST)).map_err(|e| ImportProblem::WriteFailed(e.to_string()))?)?;
        if unpacked == manifest {
            Ok(())
        } else {
            Err(ImportProblem::BadManifest("it changed while the zip was being read".to_owned()))
        }
    };
    let unpacked = unpack_into_place(&mut zip, &entries, &packages, SUBTITLE_STYLE_TYPE, &id, &manifest.name, overwrite, STYLE_PACK_EXTRACT, &mut rename_with_retries, check_unpacked)?;
    Ok(match unpacked {
        Unpacked::AlreadyThere { name } => ImportOutcome::AlreadyInstalled { name },
        Unpacked::Done { replaced } => ImportOutcome::SubtitleStyleInstalled(InstalledSubtitleStylePack { id, name: manifest.name, replaced }),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::package_import::install_zip;
    use crate::text_style_install::tests::{pack, scratch, written};

    pub(crate) const ITEM_JSON: &str = r#"{"id":"tiktok","name":"TikTok","style":{"size":0.06,"color":[255,255,255,255],"outline":{"color":[0,0,0,255],"width":0.08},"weight":800},"animation":{"in":{"preset":"pop","duration":0.3}},"layout":{"anchor_y":0.8,"max_width":0.8,"max_lines":2},"karaoke":{"mode":"color","active":{"color":[255,220,0,255],"scale":1.1},"upcoming":{"opacity":0.6}}}"#;

    pub(crate) fn manifest_json() -> String {
        format!(r#"{{"format":1,"type":"subtitle-style","id":"Avs-Subs","name":"Subtitles","version":"1.0.0","license":{{"spdx":"MIT","files":["LICENSE.txt"]}},"items":[{ITEM_JSON}]}}"#)
    }

    #[test]
    fn a_pack_goes_through_install_zip_to_its_folder_and_keeps_its_karaoke() {
        let dir = scratch("subtitle-ok");
        let root = dir.join("root");
        let outcome = install_zip(&written(&dir, &pack(&manifest_json())), &root, false).unwrap();
        assert_eq!(outcome, ImportOutcome::SubtitleStyleInstalled(InstalledSubtitleStylePack { id: "avs-subs".to_owned(), name: "Subtitles".to_owned(), replaced: false }));
        let manifest = parse_subtitle_style_manifest(&fs::read(root.join("packages/subtitle-style/avs-subs/manifest.json")).unwrap()).unwrap();
        let item = &manifest.items[0];
        assert_eq!(item.layout, SubtitleLayout { anchor_y: 0.8, max_width: 0.8, max_lines: 2 });
        assert_eq!(item.karaoke.as_ref().map(|karaoke| karaoke.mode), Some(KaraokeMode::Color));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_that_is_not_exactly_the_format_is_refused_and_installs_nothing() {
        let full = manifest_json();
        for (name, text, wanted) in [
            ("no layout", full.replace(r#","layout":{"anchor_y":0.8,"max_width":0.8,"max_lines":2}"#, ""), "BadManifest"),
            ("an unknown layout field", full.replace(r#""max_lines":2"#, r#""max_lines":2,"margin":1"#), "BadManifest"),
            ("an anchor below the frame", full.replace(r#""anchor_y":0.8"#, r#""anchor_y":1.5"#), "BadManifest"),
            ("a zero width", full.replace(r#""max_width":0.8"#, r#""max_width":0"#), "BadManifest"),
            ("no lines", full.replace(r#""max_lines":2"#, r#""max_lines":0"#), "BadManifest"),
            ("an unknown karaoke mode", full.replace(r#""mode":"color""#, r#""mode":"bounce""#), "BadManifest"),
            ("an unknown karaoke state", full.replace(r#""upcoming":"#, r#""later":"#), "BadManifest"),
            ("a karaoke opacity above 1", full.replace(r#""opacity":0.6"#, r#""opacity":2"#), "BadManifest"),
            ("a karaoke scale of 0", full.replace(r#""scale":1.1"#, r#""scale":0"#), "BadManifest"),
            ("a bad style value", full.replace(r#""weight":800"#, r#""weight":1000"#), "BadManifest"),
            ("a text style type", full.replace(r#""type":"subtitle-style""#, r#""type":"text-style""#), "BadManifest"),
        ] {
            let dir = scratch("subtitle-manifest");
            let error = install_zip(&written(&dir, &pack(&text)), &dir.join("root"), false).unwrap_err();
            assert!(format!("{error:?}").starts_with(wanted), "{name}: {error:?}");
            assert!(!dir.join("root/packages/subtitle-style").exists(), "{name}: nothing installed");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn an_existing_pack_is_replaced_only_when_allowed() {
        let dir = scratch("subtitle-overwrite");
        let root = dir.join("root");
        install_zip(&written(&dir, &pack(&manifest_json())), &root, false).unwrap();
        let newer = written(&dir, &pack(&manifest_json().replace(r#""name":"Subtitles""#, r#""name":"New Name""#)));
        assert_eq!(install_zip(&newer, &root, false), Ok(ImportOutcome::AlreadyInstalled { name: "Subtitles".to_owned() }));
        assert!(matches!(install_zip(&newer, &root, true), Ok(ImportOutcome::SubtitleStyleInstalled(InstalledSubtitleStylePack { replaced: true, .. }))));
        let _ = fs::remove_dir_all(&dir);
    }
}
