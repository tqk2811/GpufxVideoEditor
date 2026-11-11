//! Colours and widget style of the editor.

use egui::{Color32, CornerRadius, Margin, Stroke, Visuals};
use std::cell::Cell;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Which of the two palettes is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Dark,
    Light,
}

/// What the user chose in the Theme menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    /// Follow the operating system.
    #[default]
    System,
    Dark,
    Light,
}

impl ThemeChoice {
    pub const ALL: [Self; 3] = [Self::System, Self::Dark, Self::Light];

    /// How the settings file names the choice.
    pub fn code(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// The choice with settings code `code`, if there is one.
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.code() == code)
    }

    /// The palette to paint with. `system` is what the operating system reports, if anything;
    /// when it is not known the editor stays dark.
    pub fn resolve(self, system: Option<egui::Theme>) -> Variant {
        match self {
            Self::Dark => Variant::Dark,
            Self::Light => Variant::Light,
            Self::System => match system {
                Some(egui::Theme::Light) => Variant::Light,
                Some(egui::Theme::Dark) | None => Variant::Dark,
            },
        }
    }
}

/// Every colour the editor paints with.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub background: Color32,
    pub panel: Color32,
    pub panel_raised: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub accent_text: Color32,
    pub refused: Color32,
    pub row_even: Color32,
    pub row_odd: Color32,
    pub ruler: Color32,
    pub tick: Color32,
    pub video_clip: Color32,
    pub video_clip_header: Color32,
    pub text_clip: Color32,
    pub text_clip_header: Color32,
    pub audio_clip: Color32,
    pub audio_clip_header: Color32,
    pub selected: Color32,
    pub playhead: Color32,
    pub transition: Color32,
    /// Text on a filled accent button.
    pub on_accent: Color32,
    /// Text on a clip, and on the dark pill over a thumbnail.
    pub on_clip: Color32,
    /// Accent text on a clip.
    pub on_clip_accent: Color32,
    /// The outline of the place a dragged clip would land.
    pub drop_outline: Color32,
    /// Behind a media thumbnail that has no picture yet.
    pub thumb_bg: Color32,
    /// Behind text fields.
    pub field_bg: Color32,
    /// A control under the pointer.
    pub hover: Color32,
    /// The level bars drawn on a clip, which is dark in both palettes.
    pub bars: Color32,
    pub bars_muted: Color32,
}

pub const DARK: Palette = Palette {
    background: Color32::from_rgb(12, 12, 17),
    panel: Color32::from_rgb(20, 20, 27),
    panel_raised: Color32::from_rgb(31, 31, 42),
    border: Color32::from_rgb(44, 44, 58),
    text: Color32::from_rgb(232, 232, 240),
    text_dim: Color32::from_rgb(150, 150, 168),
    accent: Color32::from_rgb(124, 92, 255),
    accent_soft: Color32::from_rgb(44, 36, 88),
    accent_text: Color32::from_rgb(190, 176, 255),
    refused: Color32::from_rgb(255, 120, 110),
    row_even: Color32::from_rgb(24, 24, 32),
    row_odd: Color32::from_rgb(28, 28, 37),
    ruler: Color32::from_rgb(20, 20, 27),
    tick: Color32::from_rgb(90, 90, 108),
    video_clip: Color32::from_rgb(25, 72, 96),
    video_clip_header: Color32::from_rgb(16, 48, 66),
    text_clip: Color32::from_rgb(104, 50, 86),
    text_clip_header: Color32::from_rgb(74, 32, 60),
    audio_clip: Color32::from_rgb(20, 92, 88),
    audio_clip_header: Color32::from_rgb(13, 62, 60),
    selected: Color32::from_rgb(190, 176, 255),
    playhead: Color32::WHITE,
    transition: Color32::from_rgba_premultiplied(84, 62, 170, 190),
    on_accent: Color32::WHITE,
    on_clip: Color32::from_rgb(232, 232, 240),
    on_clip_accent: Color32::from_rgb(190, 176, 255),
    drop_outline: Color32::WHITE,
    thumb_bg: Color32::from_rgb(16, 16, 24),
    field_bg: Color32::from_rgb(14, 14, 20),
    hover: Color32::from_rgb(42, 42, 58),
    bars: Color32::from_rgb(96, 208, 158),
    bars_muted: Color32::from_rgb(120, 120, 130),
};

pub const LIGHT: Palette = Palette {
    background: Color32::from_rgb(230, 231, 239),
    panel: Color32::from_rgb(247, 247, 251),
    panel_raised: Color32::from_rgb(255, 255, 255),
    border: Color32::from_rgb(200, 200, 214),
    text: Color32::from_rgb(28, 28, 40),
    text_dim: Color32::from_rgb(88, 88, 108),
    accent: Color32::from_rgb(108, 76, 230),
    accent_soft: Color32::from_rgb(222, 214, 255),
    accent_text: Color32::from_rgb(72, 44, 190),
    refused: Color32::from_rgb(190, 40, 40),
    row_even: Color32::from_rgb(240, 241, 247),
    row_odd: Color32::from_rgb(233, 234, 243),
    ruler: Color32::from_rgb(238, 238, 246),
    tick: Color32::from_rgb(100, 100, 118),
    video_clip: Color32::from_rgb(36, 110, 146),
    video_clip_header: Color32::from_rgb(24, 82, 112),
    text_clip: Color32::from_rgb(146, 60, 112),
    text_clip_header: Color32::from_rgb(112, 40, 84),
    audio_clip: Color32::from_rgb(24, 118, 112),
    audio_clip_header: Color32::from_rgb(16, 88, 84),
    selected: Color32::from_rgb(72, 44, 190),
    playhead: Color32::from_rgb(230, 110, 0),
    transition: Color32::from_rgba_premultiplied(112, 90, 200, 190),
    on_accent: Color32::WHITE,
    on_clip: Color32::from_rgb(244, 244, 250),
    on_clip_accent: Color32::from_rgb(214, 204, 255),
    drop_outline: Color32::from_rgb(40, 40, 60),
    thumb_bg: Color32::from_rgb(222, 222, 232),
    field_bg: Color32::from_rgb(255, 255, 255),
    hover: Color32::from_rgb(226, 226, 240),
    bars: Color32::from_rgb(96, 208, 158),
    bars_muted: Color32::from_rgb(120, 120, 130),
};

impl Variant {
    pub const fn palette(self) -> &'static Palette {
        match self {
            Self::Dark => &DARK,
            Self::Light => &LIGHT,
        }
    }
}

thread_local! {
    /// The palette of the thread that draws. The window draws on one thread; a thread of its own
    /// per test keeps tests that run in parallel from seeing each other's palette.
    static CURRENT: Cell<Variant> = const { Cell::new(Variant::Dark) };
}

fn palette() -> &'static Palette {
    CURRENT.with(Cell::get).palette()
}

macro_rules! colours {
    ($($name:ident),* $(,)?) => {
        $(
            #[inline]
            pub fn $name() -> Color32 {
                palette().$name
            }
        )*
    };
}

colours!(
    background, panel, panel_raised, border, text, text_dim, accent, accent_soft, accent_text, refused, row_even, row_odd, ruler, tick, video_clip,
    video_clip_header, text_clip, text_clip_header, audio_clip, audio_clip_header, selected, playhead, transition, on_accent, on_clip, on_clip_accent, drop_outline, thumb_bg,
    field_bg, hover, bars, bars_muted,
);

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

/// The look of one palette: egui's dark or light base with the editor's colours and corners.
fn visuals_for(variant: Variant) -> Visuals {
    let colours = variant.palette();
    let mut visuals = match variant {
        Variant::Dark => Visuals::dark(),
        Variant::Light => Visuals::light(),
    };
    let control = CornerRadius::same(CONTROL_RADIUS);
    visuals.panel_fill = colours.panel;
    visuals.window_fill = colours.panel_raised;
    visuals.window_stroke = Stroke::new(1.0, colours.border);
    visuals.extreme_bg_color = colours.field_bg;
    visuals.faint_bg_color = colours.panel_raised;
    visuals.selection.bg_fill = colours.accent_soft;
    visuals.selection.stroke = Stroke::new(1.0, colours.accent_text);
    visuals.hyperlink_color = colours.accent_text;
    visuals.override_text_color = Some(colours.text);
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, colours.border);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, colours.text_dim);
    widgets.noninteractive.corner_radius = control;
    for style in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active, &mut widgets.open] {
        style.corner_radius = control;
    }
    widgets.inactive.weak_bg_fill = colours.panel_raised;
    widgets.inactive.bg_fill = colours.panel_raised;
    widgets.inactive.bg_stroke = Stroke::new(1.0, colours.border);
    widgets.hovered.weak_bg_fill = colours.hover;
    widgets.hovered.bg_fill = colours.hover;
    widgets.hovered.bg_stroke = Stroke::new(1.0, colours.accent);
    widgets.active.weak_bg_fill = colours.accent_soft;
    widgets.active.bg_fill = colours.accent_soft;
    widgets.active.bg_stroke = Stroke::new(1.0, colours.accent);
    visuals
}

/// Sets the palette of this frame and, when it differs from the last frame, egui's visuals. The
/// OS theme is read every frame, so a change of it while running is followed; nothing is rebuilt
/// while the resolved palette stays the same.
pub fn apply(ctx: &egui::Context, choice: ThemeChoice) {
    let variant = choice.resolve(ctx.system_theme());
    CURRENT.with(|current| current.set(variant));
    let fonts_id = egui::Id::NULL.with("gpufx-fonts-installed");
    if !ctx.data(|data| data.get_temp::<bool>(fonts_id)).unwrap_or(false) {
        ctx.data_mut(|data| data.insert_temp(fonts_id, true));
        install_fonts(ctx);
    }
    let id = egui::Id::NULL.with("gpufx-theme-applied");
    if ctx.data(|data| data.get_temp::<Variant>(id)) == Some(variant) {
        return;
    }
    ctx.data_mut(|data| data.insert_temp(id, variant));
    // An explicit theme, so that egui's own choice between its dark and light style always
    // matches the palette, and the visuals set below are the ones in use.
    ctx.set_theme(match variant {
        Variant::Dark => egui::Theme::Dark,
        Variant::Light => egui::Theme::Light,
    });
    ctx.set_visuals(visuals_for(variant));
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

    fn channel(value: u8) -> f64 {
        let value = f64::from(value) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(color: Color32) -> f64 {
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }

    /// The WCAG contrast ratio of two opaque colours.
    fn contrast(a: Color32, b: Color32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn contrast_is_computed_as_wcag_does() {
        assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 1e-9);
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 1e-9);
    }

    /// Main text on its backgrounds. The dark palette is the long-standing look and must not change,
    /// so the accent pairs, which are below 4.5 there, are held to 4.0; the light palette has no exception.
    #[test]
    fn text_is_readable_on_its_background_in_both_palettes() {
        for (name, variant) in [("dark", Variant::Dark), ("light", Variant::Light)] {
            let c = variant.palette();
            let surfaces = [("background", c.background), ("panel", c.panel), ("raised", c.panel_raised), ("row_even", c.row_even), ("row_odd", c.row_odd), ("ruler", c.ruler), ("hover", c.hover), ("field", c.field_bg)];
            let mut pairs: Vec<(String, Color32, Color32, f64)> = Vec::new();
            for (surface, bg) in surfaces {
                pairs.push((format!("text on {surface}"), c.text, bg, 4.5));
                pairs.push((format!("text_dim on {surface}"), c.text_dim, bg, 4.5));
            }
            for (surface, bg) in [("panel", c.panel), ("raised", c.panel_raised), ("accent_soft", c.accent_soft)] {
                pairs.push((format!("accent_text on {surface}"), c.accent_text, bg, 4.5));
            }
            for (surface, bg) in [("panel", c.panel), ("background", c.background), ("raised", c.panel_raised)] {
                pairs.push((format!("refused on {surface}"), c.refused, bg, 4.5));
            }
            pairs.push(("text on accent_soft".into(), c.text, c.accent_soft, 4.5));
            for (clip, bg) in [("video", c.video_clip), ("video header", c.video_clip_header), ("text", c.text_clip), ("text header", c.text_clip_header), ("audio", c.audio_clip), ("audio header", c.audio_clip_header)] {
                pairs.push((format!("on_clip on {clip}"), c.on_clip, bg, 4.5));
            }
            pairs.push(("on_clip_accent on video header".into(), c.on_clip_accent, c.video_clip_header, 4.5));
            pairs.push(("on_clip_accent on audio header".into(), c.on_clip_accent, c.audio_clip_header, 4.5));
            pairs.push(("on_clip_accent on text header".into(), c.on_clip_accent, c.text_clip_header, 4.5));
            pairs.push(("on_clip on thumbnail pill".into(), c.on_clip, Color32::from_rgb(40, 40, 40), 4.5));
            let accent_minimum = if variant == Variant::Dark { 4.0 } else { 4.5 };
            pairs.push(("on_accent on accent".into(), c.on_accent, c.accent, accent_minimum));
            pairs.push(("accent on panel".into(), c.accent, c.panel, accent_minimum));
            if variant == Variant::Light {
                // Disabled icons and ticks. The dark palette has always dimmed these further.
                for (surface, bg) in [("panel", c.panel), ("raised", c.panel_raised), ("ruler", c.ruler)] {
                    pairs.push((format!("tick on {surface}"), c.tick, bg, 4.5));
                }
            }
            for (label, fg, bg, minimum) in pairs {
                let ratio = contrast(fg, bg);
                assert!(ratio >= minimum, "{name}: {label} is {ratio:.2}, below {minimum}");
            }
        }
    }

    #[test]
    fn the_dark_palette_is_the_look_the_editor_always_had() {
        assert_eq!(DARK.background, Color32::from_rgb(12, 12, 17));
        assert_eq!(DARK.panel, Color32::from_rgb(20, 20, 27));
        assert_eq!(DARK.text, Color32::from_rgb(232, 232, 240));
        assert_eq!(DARK.accent, Color32::from_rgb(124, 92, 255));
        assert_eq!(DARK.selected, DARK.accent_text);
        assert_eq!(DARK.playhead, Color32::WHITE);
        assert_eq!(DARK.transition, Color32::from_rgba_premultiplied(84, 62, 170, 190));
        assert_eq!(DARK.field_bg, Color32::from_rgb(14, 14, 20));
        assert_eq!(DARK.hover, Color32::from_rgb(42, 42, 58));
    }

    #[test]
    fn each_choice_resolves_to_a_palette() {
        use egui::Theme;
        assert_eq!(ThemeChoice::Dark.resolve(Some(Theme::Light)), Variant::Dark);
        assert_eq!(ThemeChoice::Dark.resolve(None), Variant::Dark);
        assert_eq!(ThemeChoice::Light.resolve(Some(Theme::Dark)), Variant::Light);
        assert_eq!(ThemeChoice::Light.resolve(None), Variant::Light);
        assert_eq!(ThemeChoice::System.resolve(Some(Theme::Dark)), Variant::Dark);
        assert_eq!(ThemeChoice::System.resolve(Some(Theme::Light)), Variant::Light);
        assert_eq!(ThemeChoice::System.resolve(None), Variant::Dark, "an unknown system theme stays dark");
    }

    #[test]
    fn a_choice_reads_back_from_its_code() {
        for choice in ThemeChoice::ALL {
            assert_eq!(ThemeChoice::from_code(choice.code()), Some(choice));
        }
        assert_eq!(ThemeChoice::from_code("sepia"), None);
        assert_eq!(ThemeChoice::default(), ThemeChoice::System);
    }

    /// The palette is per thread, and each test runs on a thread of its own.
    #[test]
    fn apply_follows_the_system_theme_and_rebuilds_only_when_it_changes() {
        use egui::Theme;
        let ctx = egui::Context::default();
        let frame = |system: Option<Theme>, choice: ThemeChoice| {
            let raw = egui::RawInput { system_theme: system, ..Default::default() };
            let mut output = ctx.run_ui(raw, |_| apply(&ctx, choice));
            output.textures_delta.clear();
            (palette().background, ctx.global_style().visuals.panel_fill)
        };
        assert_eq!(frame(None, ThemeChoice::System), (DARK.background, DARK.panel));
        assert_eq!(frame(Some(Theme::Light), ThemeChoice::System), (LIGHT.background, LIGHT.panel), "the OS changed to light");
        assert_eq!(frame(Some(Theme::Dark), ThemeChoice::System), (DARK.background, DARK.panel), "and back");
        assert_eq!(frame(Some(Theme::Dark), ThemeChoice::Light), (LIGHT.background, LIGHT.panel), "a fixed choice ignores the OS");
        // Nothing is rebuilt while the palette is the same: a visual changed by hand stays.
        ctx.global_style_mut(|style| style.visuals.panel_fill = Color32::from_rgb(1, 2, 3));
        assert_eq!(frame(Some(Theme::Light), ThemeChoice::System).1, Color32::from_rgb(1, 2, 3));
    }
}
