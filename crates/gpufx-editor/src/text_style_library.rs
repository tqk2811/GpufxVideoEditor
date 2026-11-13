//! The text and subtitle style packs installed on this machine, and applying a style to a text
//! clip. No window here.
//!
//! A style is named `"<pack id>/<item id>"`, the pack id lower-cased. Applying it copies its values
//! into the clip's [`TextContent`]: a project never names a style, so it opens the same without the
//! pack. Only the animations a style sets name presets, as any clip animation does.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use engine::{AnimationRef, ClipAnimation, ClipPlacement, FontRef, TextContent, Time, MAX_PLACEMENT_WIDTH, MIN_PLACEMENT_WIDTH};

use crate::audio_library::LibraryProblem;
use crate::clip_animation_library::pack_reference;
use crate::package_import::ImportProblem;
use crate::subtitle_style_install::{check_subtitle_style_fields, parse_subtitle_style_manifest, SubtitleKaraoke, SubtitleLayout, SUBTITLE_STYLE_TYPE};
use crate::text_clip::default_text;
use crate::text_style_install::{check_text_style_fields, parse_text_style_manifest, PackStyleAnimation, PackTextStyle, TEXT_STYLE_TYPE};
use crate::timecode::from_seconds;

/// Which kind of pack a style comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StyleKind {
    Text,
    Subtitle,
}

impl StyleKind {
    /// The name an agent filters by.
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Subtitle => "subtitle",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "text" => Some(Self::Text),
            "subtitle" => Some(Self::Subtitle),
            _ => None,
        }
    }
}

/// One style of a pack.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryTextStyle {
    /// `"<pack id>/<item id>"`.
    pub reference: String,
    pub name: String,
    pub category: Option<String>,
    /// What the library shows the style on.
    pub preview_text: String,
    pub kind: StyleKind,
    pub style: PackTextStyle,
    /// The animations the style sets; `None` leaves a clip's as they are.
    pub animation: Option<ClipAnimation>,
    /// Where a subtitle style puts its text.
    pub layout: Option<SubtitleLayout>,
    /// Read and kept; not drawn yet.
    pub karaoke: Option<SubtitleKaraoke>,
}

impl LibraryTextStyle {
    /// `current` in this style: its content stays, the size too when the style has none, and the
    /// font when no family of the style's `font_hint` is among `families`.
    pub fn apply(&self, current: &TextContent, families: &[String]) -> TextContent {
        let mut next = self.style.text(&current.content, current.size);
        next.font = pick_font(&self.style.font_hint, families).unwrap_or_else(|| current.font.clone());
        next
    }

    /// A new text of `content` in this style.
    pub fn new_text(&self, content: impl Into<String>, families: &[String]) -> TextContent {
        self.apply(&default_text(content), families)
    }

    /// The animations of a clip once the style is applied: all three groups as the style sets them
    /// when it sets any, else `current`.
    pub fn animation_for(&self, current: &ClipAnimation) -> ClipAnimation {
        self.animation.clone().unwrap_or_else(|| current.clone())
    }
}

/// The first family of `hints` installed, as the font to use, matched without regard to case.
/// `None` when none is, or the installed fonts are not known yet.
pub fn pick_font(hints: &[String], families: &[String]) -> Option<FontRef> {
    hints.iter().find_map(|hint| families.iter().find(|family| family.eq_ignore_ascii_case(hint.trim()))).map(|family| FontRef::System { family: family.clone() })
}

/// The place of a subtitle whose picture is `pixels` in an `output` frame: at its natural size but
/// no wider than `layout.max_width`, centred across, its middle at `layout.anchor_y`.
pub fn subtitle_placement(pixels: (u32, u32), output: (u32, u32), layout: &SubtitleLayout) -> Option<ClipPlacement> {
    if pixels.0 == 0 || pixels.1 == 0 || output.0 == 0 || output.1 == 0 {
        return None;
    }
    let natural = f64::from(pixels.0) / f64::from(output.0);
    let height_per_width = (f64::from(pixels.1) / f64::from(output.1)) / natural;
    let width = natural.min(layout.max_width).clamp(MIN_PLACEMENT_WIDTH, MAX_PLACEMENT_WIDTH);
    let height = width * height_per_width;
    Some(ClipPlacement { x: (1.0 - width) / 2.0, y: layout.anchor_y - height / 2.0, width })
}

fn clip_animation(animation: &PackStyleAnimation) -> ClipAnimation {
    let reference = |value: Option<&crate::text_style_install::PackAnimationRef>| value.map(|value| AnimationRef { preset: value.preset.clone(), duration: from_seconds(value.duration).max(Time::from_ticks(1)) });
    ClipAnimation { enter: reference(animation.enter.as_ref()), exit: reference(animation.exit.as_ref()), repeat: reference(animation.repeat.as_ref()) }
}

/// One installed pack.
#[derive(Debug, Clone, PartialEq)]
pub struct TextStylePack {
    pub id: String,
    pub name: String,
    pub kind: StyleKind,
    pub styles: Vec<LibraryTextStyle>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextStyleLibrary {
    pub packs: Vec<TextStylePack>,
    pub problems: Vec<LibraryProblem>,
    /// The first scan has finished.
    pub loaded: bool,
}

impl TextStyleLibrary {
    /// Every style of `kind`, or of both kinds for `None`, with the name of its pack.
    pub fn styles(&self, kind: Option<StyleKind>) -> impl Iterator<Item = (&TextStylePack, &LibraryTextStyle)> {
        self.packs.iter().filter(move |pack| kind.is_none_or(|kind| pack.kind == kind)).flat_map(|pack| pack.styles.iter().map(move |style| (pack, style)))
    }

    /// The style named `reference`; a text style first when a subtitle pack has the same id.
    pub fn find(&self, reference: &str) -> Option<&LibraryTextStyle> {
        let reference = reference.to_ascii_lowercase();
        let found = |kind| self.styles(Some(kind)).map(|(_, style)| style).find(|style| style.reference.eq_ignore_ascii_case(&reference));
        found(StyleKind::Text).or_else(|| found(StyleKind::Subtitle))
    }
}

/// The pack read from `folder` of `kind`, whose name is the pack id.
fn read_pack(folder: &Path, label: &str, kind: StyleKind) -> Result<TextStylePack, String> {
    let bytes = std::fs::read(folder.join("manifest.json")).map_err(|e| e.to_string())?;
    let describe = |problem: ImportProblem| format!("{problem:?}");
    let (id, name, styles) = match kind {
        StyleKind::Text => {
            let manifest = parse_text_style_manifest(&bytes).map_err(describe)?;
            check_text_style_fields(&manifest).map_err(describe)?;
            let id = manifest.id.to_ascii_lowercase();
            let styles = manifest
                .items
                .into_iter()
                .map(|item| LibraryTextStyle { reference: pack_reference(&id, &item.id), preview_text: item.preview_text.unwrap_or_else(|| item.name.clone()), name: item.name, category: item.category, kind, animation: item.animation.as_ref().map(clip_animation), style: item.style, layout: None, karaoke: None })
                .collect();
            (manifest.id, manifest.name, styles)
        }
        StyleKind::Subtitle => {
            let manifest = parse_subtitle_style_manifest(&bytes).map_err(describe)?;
            check_subtitle_style_fields(&manifest).map_err(describe)?;
            let id = manifest.id.to_ascii_lowercase();
            let styles = manifest
                .items
                .into_iter()
                .map(|item| LibraryTextStyle { reference: pack_reference(&id, &item.id), preview_text: item.preview_text.unwrap_or_else(|| item.name.clone()), name: item.name, category: item.category, kind, animation: item.animation.as_ref().map(clip_animation), style: item.style, layout: Some(item.layout), karaoke: item.karaoke })
                .collect();
            (manifest.id, manifest.name, styles)
        }
    };
    if !id.eq_ignore_ascii_case(label) {
        return Err(format!("its manifest does not match the folder ({id})"));
    }
    Ok(TextStylePack { id: id.to_ascii_lowercase(), name, kind, styles })
}

/// Reads the text and subtitle style packs under every root. A pack is accepted whole or reported,
/// and a pack in an earlier root hides one of the same id and kind in a later one only when it is
/// accepted.
pub fn scan_text_style_packs(roots: &[PathBuf]) -> TextStyleLibrary {
    let mut library = TextStyleLibrary { loaded: true, ..TextStyleLibrary::default() };
    for (kind, folder_name) in [(StyleKind::Text, TEXT_STYLE_TYPE), (StyleKind::Subtitle, SUBTITLE_STYLE_TYPE)] {
        let mut taken: HashSet<String> = HashSet::new();
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root.join("packages").join(folder_name)) else { continue };
            let mut folders: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
            folders.sort();
            for folder in folders {
                let label = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                // A folder left by an interrupted install is not a pack.
                if label.starts_with('.') || taken.contains(&label.to_ascii_lowercase()) {
                    continue;
                }
                match read_pack(&folder, &label, kind) {
                    Ok(pack) => {
                        taken.insert(pack.id.clone());
                        library.packs.push(pack);
                    }
                    Err(message) => library.problems.push(LibraryProblem { pack: format!("{folder_name}/{label}"), message }),
                }
            }
        }
    }
    library.packs.sort_by_key(|pack| (pack.kind == StyleKind::Subtitle, pack.name.to_lowercase()));
    library
}

/// A library with one subtitle style, `s/low`, that names `font_hint`; for tests of other modules.
#[cfg(test)]
pub(crate) fn subtitle_library_for_test(font_hint: &[&str]) -> TextStyleLibrary {
    let style = LibraryTextStyle {
        reference: "s/low".to_owned(),
        name: "Low".to_owned(),
        category: None,
        preview_text: "Low!".to_owned(),
        kind: StyleKind::Subtitle,
        style: PackTextStyle { weight: Some(800), font_hint: font_hint.iter().map(|family| (*family).to_owned()).collect(), ..PackTextStyle::default() },
        animation: None,
        layout: Some(SubtitleLayout { anchor_y: 0.85, max_width: 0.9, max_lines: 2 }),
        karaoke: None,
    };
    TextStyleLibrary { packs: vec![TextStylePack { id: "s".to_owned(), name: "S".to_owned(), kind: StyleKind::Subtitle, styles: vec![style] }], problems: Vec::new(), loaded: true }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{AnimationGroup, TextAlign};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-text-style-library-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, kind: &str, folder: &str, manifest: &str) {
        let path = root.join("packages").join(kind).join(folder);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("manifest.json"), manifest).unwrap();
    }

    fn text_pack(id: &str, name: &str) -> String {
        crate::text_style_install::tests::manifest_json().replace("Avs-Text", id).replace(r#""name":"Text Styles""#, &format!(r#""name":"{name}""#))
    }

    fn library_with_both(dir: &Path) -> TextStyleLibrary {
        write(dir, "text-style", "avs-text", &text_pack("Avs-Text", "Text Styles"));
        write(dir, "subtitle-style", "avs-subs", &crate::subtitle_style_install::tests::manifest_json());
        scan_text_style_packs(&[dir.to_path_buf()])
    }

    #[test]
    fn a_scan_reads_both_kinds_and_reports_what_is_broken() {
        let dir = scratch("scan");
        let (shipped, user) = (dir.join("shipped"), dir.join("user"));
        write(&shipped, "text-style", "shared", &text_pack("Shared", "From Shipped"));
        write(&user, "text-style", "shared", &text_pack("shared", "From User"));
        write(&user, "subtitle-style", "avs-subs", &crate::subtitle_style_install::tests::manifest_json());
        write(&user, "text-style", "broken", "not json");
        write(&user, "text-style", "other", &text_pack("Elsewhere", "Mismatch"));

        let library = scan_text_style_packs(&[shipped, user]);

        assert!(library.loaded);
        let packs: Vec<(&str, StyleKind)> = library.packs.iter().map(|pack| (pack.name.as_str(), pack.kind)).collect();
        assert_eq!(packs, vec![("From Shipped", StyleKind::Text), ("Subtitles", StyleKind::Subtitle)], "the first root wins");
        assert_eq!(library.problems.len(), 2, "{:?}", library.problems);
        assert_eq!(library.styles(Some(StyleKind::Subtitle)).count(), 1);
        assert_eq!(library.styles(None).count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn applying_a_style_copies_its_values_and_keeps_the_content() {
        let dir = scratch("apply");
        let library = library_with_both(&dir);
        let neon = library.find("AVS-TEXT/neon").expect("found whatever the case");
        assert_eq!(neon.preview_text, "Hello");
        let mut current = default_text("Keep me");
        current.background = Some(crate::text_clip::default_background());
        current.font = FontRef::System { family: "Arial".to_owned() };

        let families = vec!["Arial".to_owned(), "segoe ui".to_owned()];
        let styled = neon.apply(&current, &families);
        assert_eq!(styled.content, "Keep me");
        assert_eq!((styled.size, styled.weight, styled.italic, styled.align), (0.1, 700, true, TextAlign::Left));
        assert!(styled.background.is_none(), "a style is whole: what it has not is taken off");
        assert_eq!(styled.font, FontRef::System { family: "segoe ui".to_owned() }, "the first hint installed");
        assert_eq!(neon.apply(&current, &[]).font, current.font, "no hint installed keeps the font");

        let animation = neon.animation_for(&ClipAnimation::default());
        assert_eq!(animation.enter.as_ref().map(|value| (value.preset.as_str(), value.duration)), Some(("fade", from_seconds(0.5))));
        assert_eq!(animation.repeat.as_ref().map(|value| value.preset.as_str()), Some("avs-moves/wave"));
        assert!(animation.exit.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_style_without_animation_or_size_keeps_the_clips() {
        let style = LibraryTextStyle { reference: "p/s".to_owned(), name: "S".to_owned(), category: None, preview_text: "S".to_owned(), kind: StyleKind::Text, style: PackTextStyle::default(), animation: None, layout: None, karaoke: None };
        let mut current = default_text("x");
        current.size = 0.3;
        assert_eq!(style.apply(&current, &[]).size, 0.3);
        let animation = crate::text_clip::choose_preset(&ClipAnimation::default(), AnimationGroup::Enter, Some("fade"));
        assert_eq!(style.animation_for(&animation), animation);
    }

    #[test]
    fn a_subtitle_sits_at_its_anchor_no_wider_than_its_layout() {
        let layout = SubtitleLayout { anchor_y: 0.8, max_width: 0.5, max_lines: 2 };
        let wide = subtitle_placement((1600, 100), (1920, 1080), &layout).unwrap();
        assert!((wide.width - 0.5).abs() < 1e-9 && (wide.x - 0.25).abs() < 1e-9);
        let height = 0.5 * (100.0 / 1080.0) / (1600.0 / 1920.0);
        assert!((wide.y + height / 2.0 - 0.8).abs() < 1e-9, "its middle is at the anchor");
        let narrow = subtitle_placement((192, 108), (1920, 1080), &layout).unwrap();
        assert!((narrow.width - 0.1).abs() < 1e-9, "a narrow text keeps its natural size");
        assert!(subtitle_placement((0, 10), (1920, 1080), &layout).is_none());
    }

    #[test]
    fn a_subtitle_style_has_its_layout_and_karaoke() {
        let dir = scratch("subtitle");
        let library = library_with_both(&dir);
        let style = library.find("avs-subs/tiktok").unwrap();
        assert_eq!(style.kind, StyleKind::Subtitle);
        assert_eq!(style.layout.map(|layout| layout.anchor_y), Some(0.8));
        assert!(style.karaoke.is_some());
        assert_eq!(style.preview_text, "TikTok", "the name when there is no preview text");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
