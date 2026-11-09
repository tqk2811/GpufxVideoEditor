//! Colours and widget style of the editor.

use egui::{Color32, CornerRadius, Margin, Stroke, Visuals};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub const BACKGROUND: Color32 = Color32::from_rgb(12, 12, 17);
pub const PANEL: Color32 = Color32::from_rgb(20, 20, 27);
pub const PANEL_RAISED: Color32 = Color32::from_rgb(31, 31, 42);
pub const BORDER: Color32 = Color32::from_rgb(44, 44, 58);
pub const TEXT: Color32 = Color32::from_rgb(232, 232, 240);
pub const TEXT_DIM: Color32 = Color32::from_rgb(150, 150, 168);
pub const ACCENT: Color32 = Color32::from_rgb(124, 92, 255);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(44, 36, 88);
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(190, 176, 255);
pub const REFUSED: Color32 = Color32::from_rgb(255, 120, 110);

pub const ROW_EVEN: Color32 = Color32::from_rgb(24, 24, 32);
pub const ROW_ODD: Color32 = Color32::from_rgb(28, 28, 37);
pub const RULER: Color32 = Color32::from_rgb(20, 20, 27);
pub const TICK: Color32 = Color32::from_rgb(90, 90, 108);
pub const VIDEO_CLIP: Color32 = Color32::from_rgb(25, 72, 96);
pub const VIDEO_CLIP_HEADER: Color32 = Color32::from_rgb(16, 48, 66);
pub const AUDIO_CLIP: Color32 = Color32::from_rgb(20, 92, 88);
pub const AUDIO_CLIP_HEADER: Color32 = Color32::from_rgb(13, 62, 60);
pub const SELECTED: Color32 = ACCENT_TEXT;
pub const PLAYHEAD: Color32 = Color32::WHITE;
pub const TRANSITION: Color32 = Color32::from_rgba_premultiplied(84, 62, 170, 190);

pub const PANEL_RADIUS: u8 = 10;
pub const CONTROL_RADIUS: u8 = 6;

/// Fonts of Linux and macOS with the Vietnamese letters egui's bundled fonts lack, best first.
const UNIX_FONT_FILES: [&str; 9] = [
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "/Library/Fonts/Arial.ttf",
];

/// Where a font with Vietnamese letters may be, best first: the Windows fonts when `windows_dir`
/// is known, then the fonts of other systems.
fn font_candidates(windows_dir: Option<&OsStr>) -> Vec<PathBuf> {
    let windows = windows_dir.into_iter().flat_map(|dir| ["segoeui.ttf", "arial.ttf"].map(|file| Path::new(dir).join("Fonts").join(file)));
    windows.chain(UNIX_FONT_FILES.iter().map(PathBuf::from)).collect()
}

/// The contents of the first candidate that can be read.
fn first_readable(candidates: &[PathBuf], read: impl Fn(&Path) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
    candidates.iter().find_map(|path| read(path))
}

/// Without a font the bundled ones stay, and Vietnamese letters show as boxes. Apply runs before
/// any text is laid out, so egui takes the font in this very pass and no repaint is needed.
fn install_fonts(ctx: &egui::Context) {
    let candidates = font_candidates(std::env::var_os("WINDIR").as_deref());
    let Some(bytes) = first_readable(&candidates, |path| std::fs::read(path).ok()) else {
        eprintln!("gpufx-editor: no font with Vietnamese letters was found; tried {candidates:?}");
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("ui".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        family.insert(0, "ui".to_owned());
    }
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        family.push("ui".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// Applies the dark, rounded look once.
pub fn apply(ctx: &egui::Context) {
    let id = egui::Id::NULL.with("gpufx-theme-applied");
    if ctx.data(|data| data.get_temp::<bool>(id)).unwrap_or(false) {
        return;
    }
    ctx.data_mut(|data| data.insert_temp(id, true));
    install_fonts(ctx);
    let mut visuals = Visuals::dark();
    let control = CornerRadius::same(CONTROL_RADIUS);
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL_RAISED;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.extreme_bg_color = Color32::from_rgb(14, 14, 20);
    visuals.faint_bg_color = PANEL_RAISED;
    visuals.selection.bg_fill = ACCENT_SOFT;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT_TEXT);
    visuals.hyperlink_color = ACCENT_TEXT;
    visuals.override_text_color = Some(TEXT);
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    widgets.noninteractive.corner_radius = control;
    for style in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active, &mut widgets.open] {
        style.corner_radius = control;
    }
    widgets.inactive.weak_bg_fill = PANEL_RAISED;
    widgets.inactive.bg_fill = PANEL_RAISED;
    widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.hovered.weak_bg_fill = Color32::from_rgb(42, 42, 58);
    widgets.hovered.bg_fill = Color32::from_rgb(42, 42, 58);
    widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    widgets.active.weak_bg_fill = ACCENT_SOFT;
    widgets.active.bg_fill = ACCENT_SOFT;
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 6.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.window_margin = Margin::same(8);
        style.spacing.interact_size.y = 24.0;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_fonts_come_first_from_the_system_folder() {
        let candidates = font_candidates(Some(OsStr::new("D:\\Win")));
        assert_eq!(candidates[0], Path::new("D:\\Win").join("Fonts").join("segoeui.ttf"));
        assert!(candidates.iter().any(|path| path.to_string_lossy().contains("NotoSans")));
        assert!(!font_candidates(None).iter().any(|path| path.to_string_lossy().contains("segoeui")));
    }

    #[test]
    fn the_first_font_that_can_be_read_is_taken() {
        let candidates = [PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        let found = first_readable(&candidates, |path| (path != Path::new("a")).then(|| path.to_string_lossy().into_owned().into_bytes()));
        assert_eq!(found, Some(b"b".to_vec()));
        assert_eq!(first_readable(&candidates, |_| None), None);
    }
}
