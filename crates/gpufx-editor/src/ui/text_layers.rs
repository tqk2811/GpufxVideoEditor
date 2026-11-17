//! The layers of a text in the inspector: its fill, outlines, shadows (each with outlines of its
//! own), inner shadows and bloom. They sit in one section of their own that folds away, and each
//! list keeps every layer folded until opened, so the narrow inspector stays short. The rows only
//! change the text they are given; [`super::text_inspector`] hands the result to
//! [`crate::editor::EditorState::live_text`], which makes a drag one undo step.

use super::numeric::drag_field;
use super::text_inspector::{color_row, percent_row, slider_row, toggle};
use super::theme;
use crate::i18n::{Language, Message};
use crate::text_clip::{add_gradient_stop, apply_layer_action, convert_paint, default_bloom, default_inner_shadow, default_outline, default_shadow, LayerAction, PaintKind, PaintSlot};
use crate::text_style_library::TextStyleLibrary;
use engine::{GradientStop, TextContent, TextImageRef, TextOutline, TextPaint, TextTexture, TextureFit, MAX_GRADIENT_STOPS, MAX_TEXTURE_CELLS, MAX_TEXT_OUTLINES, MAX_TEXT_SHADOWS};
use std::cell::OnceCell;
use std::ops::RangeInclusive;

/// Outline width, in percent of the font size ([`engine::MAX_TEXT_OUTLINE_WIDTH`]).
const OUTLINE_WIDTH_RANGE: RangeInclusive<f64> = 0.0..=50.0;
/// Shadow distance and blur, in percent of the font size ([`engine::MAX_TEXT_SHADOW_DISTANCE`],
/// [`engine::MAX_TEXT_SHADOW_BLUR`]).
const SHADOW_DISTANCE_RANGE: RangeInclusive<f64> = 0.0..=400.0;
const SHADOW_BLUR_RANGE: RangeInclusive<f64> = 0.0..=200.0;
/// Angles in degrees.
const SHADOW_ANGLE_RANGE: RangeInclusive<f64> = -180.0..=180.0;
const GRADIENT_ANGLE_RANGE: RangeInclusive<f64> = 0.0..=360.0;
/// Texture scale in percent ([`engine::TEXT_TEXTURE_SCALE_RANGE`], kept to what is useful here).
const TEXTURE_SCALE_RANGE: RangeInclusive<f64> = 1.0..=1000.0;
const UNIT_PERCENT_RANGE: RangeInclusive<f64> = 0.0..=100.0;
const BLOOM_DIR_RANGE: RangeInclusive<f64> = -100.0..=100.0;
const PERCENT: f64 = 100.0;
const DEGREE: &str = "\u{b0}";

/// What the layer rows need besides the text: the language, the pictures of the installed style
/// packs (read once, when a texture row first asks), and the paint whose picture file the person
/// asked to choose.
pub(super) struct Layers<'a> {
    pub language: Language,
    library: &'a TextStyleLibrary,
    images: OnceCell<Vec<TextImageRef>>,
    pub file_request: Option<PaintSlot>,
}

impl<'a> Layers<'a> {
    pub fn new(language: Language, library: &'a TextStyleLibrary) -> Self {
        Layers { language, library, images: OnceCell::new(), file_request: None }
    }

    fn images(&self) -> &[TextImageRef] {
        self.images.get_or_init(|| pack_images(self.library))
    }
}

/// The pictures the styles of the installed packs paint with, each once, in the order found.
pub(super) fn pack_images(library: &TextStyleLibrary) -> Vec<TextImageRef> {
    let mut images: Vec<TextImageRef> = Vec::new();
    for (_, style) in library.styles(None) {
        let text = style.new_text("x", &[], &[]);
        for image in gpufx_text::text_images(&text) {
            if matches!(image, TextImageRef::Pack { .. }) && !images.contains(image) {
                images.push(image.clone());
            }
        }
    }
    images
}

/// The section of the layers.
pub(super) fn show(ui: &mut egui::Ui, id: egui::Id, text: &mut TextContent, layers: &mut Layers) {
    let language = layers.language;
    egui::CollapsingHeader::new(egui::RichText::new(language.t(Message::TextLayers)).strong()).id_salt(id.with("text-layers")).default_open(true).show(ui, |ui| {
        egui::CollapsingHeader::new(language.t(Message::TextFill)).id_salt(id.with("fill")).default_open(true).show(ui, |ui| {
            paint_editor(ui, id.with("fill-paint"), &mut text.fill, PaintSlot::Fill, layers);
        });
        egui::CollapsingHeader::new(counted(&language.t(Message::TextOutlines), text.outlines.len())).id_salt(id.with("outlines")).show(ui, |ui| {
            outline_list(ui, id.with("outline-list"), &mut text.outlines, PaintSlot::Outline, layers);
        });
        egui::CollapsingHeader::new(counted(&language.t(Message::TextShadows), text.shadows.len())).id_salt(id.with("shadows")).show(ui, |ui| {
            let title = language.t(Message::TextShadow);
            layer_list(ui, id.with("shadow-list"), &mut text.shadows, MAX_TEXT_SHADOWS, default_shadow, &title, language, |ui, index, shadow| {
                percent_row(ui, &language.t(Message::FieldDistance), &mut shadow.distance, SHADOW_DISTANCE_RANGE);
                degree_row(ui, &language.t(Message::FieldAngle), &mut shadow.angle, SHADOW_ANGLE_RANGE);
                percent_row(ui, &language.t(Message::FieldBlur), &mut shadow.blur, SHADOW_BLUR_RANGE);
                percent_row(ui, &language.t(Message::FieldSpread), &mut shadow.spread, OUTLINE_WIDTH_RANGE);
                paint_editor(ui, ui.scope_id().with("paint"), &mut shadow.paint, PaintSlot::Shadow(index), layers);
                egui::CollapsingHeader::new(counted(&language.t(Message::ShadowOutlines), shadow.outlines.len())).id_salt(ui.scope_id().with("outlines")).show(ui, |ui| {
                    outline_list(ui, ui.scope_id().with("outline-list"), &mut shadow.outlines, |outline| PaintSlot::ShadowOutline(index, outline), layers);
                });
            });
        });
        egui::CollapsingHeader::new(counted(&language.t(Message::TextInnerShadows), text.inner_shadows.len())).id_salt(id.with("inner-shadows")).show(ui, |ui| {
            let title = language.t(Message::TextInnerShadow);
            layer_list(ui, id.with("inner-list"), &mut text.inner_shadows, MAX_TEXT_SHADOWS, default_inner_shadow, &title, language, |ui, index, shadow| {
                percent_row(ui, &language.t(Message::FieldDistance), &mut shadow.distance, SHADOW_DISTANCE_RANGE);
                degree_row(ui, &language.t(Message::FieldAngle), &mut shadow.angle, SHADOW_ANGLE_RANGE);
                percent_row(ui, &language.t(Message::FieldBlur), &mut shadow.blur, SHADOW_BLUR_RANGE);
                paint_editor(ui, ui.scope_id().with("paint"), &mut shadow.paint, PaintSlot::InnerShadow(index), layers);
            });
        });
        egui::CollapsingHeader::new(language.t(Message::TextBloom)).id_salt(id.with("bloom")).show(ui, |ui| {
            toggle(ui, &language.t(Message::TextBloom), &mut text.bloom, default_bloom);
            if let Some(bloom) = text.bloom.as_mut() {
                color_row(ui, &language.t(Message::FieldColor), &mut bloom.color);
                percent_row(ui, &language.t(Message::FieldStrength), &mut bloom.strength, UNIT_PERCENT_RANGE);
                percent_row(ui, &language.t(Message::FieldSpread), &mut bloom.range, UNIT_PERCENT_RANGE);
                percent_row(ui, &language.t(Message::FieldShiftX), &mut bloom.dir_x, BLOOM_DIR_RANGE);
                percent_row(ui, &language.t(Message::FieldShiftY), &mut bloom.dir_y, BLOOM_DIR_RANGE);
            }
        });
    });
}

/// A title with how many layers it holds, when it holds any.
fn counted(title: &str, count: usize) -> String {
    if count == 0 {
        title.to_owned()
    } else {
        format!("{title} ({count})")
    }
}

fn degree_row(ui: &mut egui::Ui, label: &str, value: &mut f32, range: RangeInclusive<f64>) {
    if let Some(new) = slider_row(ui, label, f64::from(*value), range, 0, DEGREE) {
        *value = new as f32;
    }
}

/// A list of outlines, each with its width and paint; `slot` names the paint of outline `index`.
fn outline_list(ui: &mut egui::Ui, id: egui::Id, outlines: &mut Vec<TextOutline>, slot: impl Fn(usize) -> PaintSlot, layers: &mut Layers) {
    let language = layers.language;
    let title = language.t(Message::TextOutline);
    layer_list(ui, id, outlines, MAX_TEXT_OUTLINES, default_outline, &title, language, |ui, index, outline| {
        percent_row(ui, &language.t(Message::FieldThickness), &mut outline.width, OUTLINE_WIDTH_RANGE);
        paint_editor(ui, ui.scope_id().with("paint"), &mut outline.paint, slot(index), layers);
    });
}

/// Names the layers of one list, one stable key each, in the order of the layers. The open or folded
/// state of a layer and the widgets inside it hang on its key, not on its index, so moving or
/// removing a layer takes its state along instead of handing it to the layer that took its place.
/// Kept in the egui memory, since the rows are drawn again every frame; a list that changed behind
/// its back (an undo, a style applied) is fitted to its length, which only loses the state of
/// layers whose identity is unknown anyway.
#[derive(Clone, Default)]
struct LayerKeys {
    keys: Vec<u64>,
    next: u64,
}

impl LayerKeys {
    fn fit(&mut self, count: usize) {
        self.keys.truncate(count);
        while self.keys.len() < count {
            self.keys.push(self.next);
            self.next += 1;
        }
    }

    /// The same action as on the layers, so the two lists stay in step.
    fn apply(&mut self, action: LayerAction, max: usize) {
        let next = &mut self.next;
        apply_layer_action(&mut self.keys, action, max, || {
            *next += 1;
            *next - 1
        });
    }
}

/// A list of layers, each folded under a header with buttons to move it up or down and to remove
/// it, and a button that adds one, off once the list holds `max`.
#[allow(clippy::too_many_arguments)]
fn layer_list<T>(ui: &mut egui::Ui, id: egui::Id, items: &mut Vec<T>, max: usize, default: fn() -> T, title: &str, language: Language, mut body: impl FnMut(&mut egui::Ui, usize, &mut T)) {
    let mut action = None;
    let count = items.len();
    let keys_id = id.with("keys");
    let mut keys: LayerKeys = ui.ctx().data_mut(|data| data.get_temp(keys_id)).unwrap_or_default();
    keys.fit(count);
    for (index, item) in items.iter_mut().enumerate() {
        let item_id = id.with(keys.keys[index]);
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), item_id, false)
            .show_header(ui, |ui| {
                ui.label(format!("{title} {}", index + 1));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("\u{d7}").on_hover_text(language.t(Message::Remove)).clicked() {
                        action = Some(LayerAction::Remove(index));
                    }
                    if ui.add_enabled(index + 1 < count, egui::Button::new("\u{2193}").small()).on_hover_text(language.t(Message::MoveLayerDown)).clicked() {
                        action = Some(LayerAction::MoveDown(index));
                    }
                    if ui.add_enabled(index > 0, egui::Button::new("\u{2191}").small()).on_hover_text(language.t(Message::MoveLayerUp)).clicked() {
                        action = Some(LayerAction::MoveUp(index));
                    }
                });
            })
            .body(|ui| ui.push_id(item_id, |ui| body(ui, index, item)));
    }
    let add = ui.add_enabled(count < max, egui::Button::new(format!("+ {}", language.t(Message::AddLayer)))).on_disabled_hover_text(language.t(Message::LayerLimit(max)));
    if add.clicked() {
        action = Some(LayerAction::Add);
    }
    if let Some(action) = action {
        apply_layer_action(items, action, max, default);
        keys.apply(action, max);
    }
    ui.ctx().data_mut(|data| data.insert_temp(keys_id, keys));
}

/// The kind of a paint and its values: a colour, the stops and angle of a gradient, or the picture
/// of a texture and how it covers the text.
fn paint_editor(ui: &mut egui::Ui, id: egui::Id, paint: &mut TextPaint, slot: PaintSlot, layers: &mut Layers) {
    let language = layers.language;
    let current = PaintKind::of(paint);
    let mut kind = current;
    ui.horizontal(|ui| {
        for (value, label) in [(PaintKind::Solid, Message::PaintSolid), (PaintKind::Gradient, Message::PaintGradient), (PaintKind::Texture, Message::PaintTexture)] {
            ui.selectable_value(&mut kind, value, language.t(label));
        }
    });
    if kind != current {
        // The paint of each kind this one was last turned from, kept while the editor runs, so
        // switching to a solid colour and back gives the gradient or texture back whole.
        let memory_id = id.with("remembered");
        let mut remembered: RememberedPaints = ui.ctx().data_mut(|data| data.get_temp(memory_id)).unwrap_or_default();
        match convert_paint(paint, kind, layers.images().first().cloned(), remembered.get(kind)) {
            Some(next) => {
                remembered.keep(std::mem::replace(paint, next));
                ui.ctx().data_mut(|data| data.insert_temp(memory_id, remembered));
            }
            // No picture installed to start a texture with: the file dialog gives one.
            None => layers.file_request = Some(slot),
        }
    }
    match paint {
        TextPaint::Solid { color } => color_row(ui, &language.t(Message::FieldColor), color),
        TextPaint::Gradient { stops, angle, per_glyph } => gradient_rows(ui, stops, angle, per_glyph, language),
        TextPaint::Texture(texture) => texture_rows(ui, id, texture, slot, layers),
    }
}

/// The gradient and texture a paint editor last turned away from.
#[derive(Clone, Default)]
struct RememberedPaints {
    gradient: Option<TextPaint>,
    texture: Option<TextPaint>,
}

impl RememberedPaints {
    fn get(&self, kind: PaintKind) -> Option<&TextPaint> {
        match kind {
            PaintKind::Solid => None,
            PaintKind::Gradient => self.gradient.as_ref(),
            PaintKind::Texture => self.texture.as_ref(),
        }
    }

    fn keep(&mut self, paint: TextPaint) {
        match PaintKind::of(&paint) {
            PaintKind::Solid => {}
            PaintKind::Gradient => self.gradient = Some(paint),
            PaintKind::Texture => self.texture = Some(paint),
        }
    }
}

fn gradient_rows(ui: &mut egui::Ui, stops: &mut Vec<GradientStop>, angle: &mut f32, per_glyph: &mut bool, language: Language) {
    degree_row(ui, &language.t(Message::FieldAngle), angle, GRADIENT_ANGLE_RANGE);
    ui.checkbox(per_glyph, language.t(Message::GradientPerGlyph));
    let mut remove = None;
    let can_remove = stops.len() > 2;
    let positions: Vec<f32> = stops.iter().map(|stop| stop.position).collect();
    for (index, stop) in stops.iter_mut().enumerate() {
        // A stop moves only between its neighbours, so the stops stay in order while dragged.
        let low = if index == 0 { 0.0 } else { positions[index - 1] };
        let high = positions.get(index + 1).copied().unwrap_or(1.0);
        ui.push_id(("stop", index), |ui| {
            ui.horizontal(|ui| {
                ui.color_edit_button_srgba_unmultiplied(&mut stop.color);
                let mut position = f64::from(stop.position) * PERCENT;
                let range = f64::from(low) * PERCENT..=f64::from(high) * PERCENT;
                if drag_field(ui, &mut position, range, 0.5, 0, " %", None).1 {
                    stop.position = (position / PERCENT) as f32;
                }
                if ui.add_enabled(can_remove, egui::Button::new("\u{d7}").small()).on_hover_text(language.t(Message::Remove)).clicked() {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        stops.remove(index);
    }
    let add = ui.add_enabled(stops.len() < MAX_GRADIENT_STOPS, egui::Button::new(format!("+ {}", language.t(Message::AddGradientStop)))).on_disabled_hover_text(language.t(Message::LayerLimit(MAX_GRADIENT_STOPS)));
    if add.clicked() {
        add_gradient_stop(stops);
    }
}

/// A picture as the combo shows it: the file name of a file, `<pack>/<file>` of a pack.
fn image_label(image: &TextImageRef) -> String {
    match image {
        TextImageRef::File { path } => std::path::Path::new(path).file_name().map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned()),
        TextImageRef::Pack { .. } => image.label(),
    }
}

fn texture_rows(ui: &mut egui::Ui, id: egui::Id, texture: &mut TextTexture, slot: PaintSlot, layers: &mut Layers) {
    let language = layers.language;
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), language.t(Message::TexturePicture));
        egui::ComboBox::from_id_salt(id.with("picture")).selected_text(image_label(&texture.image)).width(ui.available_width()).show_ui(ui, |ui| {
            if let TextImageRef::File { .. } = &texture.image {
                let _ = ui.selectable_label(true, image_label(&texture.image));
            }
            for image in layers.images() {
                if ui.selectable_label(texture.image == *image, image_label(image)).clicked() {
                    texture.image = image.clone();
                }
            }
            ui.separator();
            if ui.selectable_label(false, language.t(Message::ChooseTextureFile)).clicked() {
                layers.file_request = Some(slot);
            }
        });
    });
    ui.horizontal_wrapped(|ui| {
        for (value, label) in [(TextureFit::Stretch, Message::FitStretch), (TextureFit::Tile, Message::FitTile), (TextureFit::Mirror, Message::FitMirror), (TextureFit::Glyph, Message::FitGlyph)] {
            ui.selectable_value(&mut texture.fit, value, language.t(label));
        }
    });
    if texture.fit == TextureFit::Glyph {
        // The picture is cut into this many cells side by side; the glyphs take them in turn.
        if let Some(cells) = slider_row(ui, &language.t(Message::FieldCells), f64::from(texture.cells), 1.0..=f64::from(MAX_TEXTURE_CELLS), 0, "") {
            texture.cells = (cells.round() as u32).clamp(1, MAX_TEXTURE_CELLS);
        }
    }
    percent_row(ui, &language.t(Message::FieldScale), &mut texture.scale, TEXTURE_SCALE_RANGE);
    degree_row(ui, &language.t(Message::FieldAngle), &mut texture.angle, SHADOW_ANGLE_RANGE);
    percent_row(ui, &language.t(Message::FieldOpacity), &mut texture.opacity, UNIT_PERCENT_RANGE);
    if texture.base.is_some() {
        // How much of the picture covers its base; without a base it is no more than the opacity.
        percent_row(ui, &language.t(Message::FieldTextureMix), &mut texture.mix, UNIT_PERCENT_RANGE);
    }
    ui.horizontal(|ui| {
        ui.checkbox(&mut texture.flip_x, language.t(Message::FlipX));
        ui.checkbox(&mut texture.flip_y, language.t(Message::FlipY));
    });
    base_rows(ui, id.with("base"), texture, language);
}

/// The paint under a texture's picture, shown where the picture is see-through: none, a colour or
/// a gradient (a texture under a texture is not allowed).
fn base_rows(ui: &mut egui::Ui, id: egui::Id, texture: &mut TextTexture, language: Language) {
    let current = texture.base.as_deref().map(PaintKind::of);
    let mut kind = current;
    ui.colored_label(theme::text_dim(), language.t(Message::TextureBase));
    ui.horizontal(|ui| {
        for (value, label) in [(None, Message::BaseNone), (Some(PaintKind::Solid), Message::BaseSolid), (Some(PaintKind::Gradient), Message::BaseGradient)] {
            ui.selectable_value(&mut kind, value, language.t(label));
        }
    });
    if kind != current {
        let memory_id = id.with("remembered");
        let mut remembered: RememberedPaints = ui.ctx().data_mut(|data| data.get_temp(memory_id)).unwrap_or_default();
        let from = texture.base.take().map(|base| *base);
        // A new base starts from the main colour of the base it replaces, else white.
        let start = from.clone().unwrap_or_else(|| TextPaint::solid([255, 255, 255, 255]));
        texture.base = kind.and_then(|kind| convert_paint(&start, kind, None, remembered.get(kind))).map(Box::new);
        if let Some(from) = from {
            remembered.keep(from);
            ui.ctx().data_mut(|data| data.insert_temp(memory_id, remembered));
        }
    }
    ui.push_id(id, |ui| match texture.base.as_deref_mut() {
        Some(TextPaint::Solid { color }) => color_row(ui, &language.t(Message::FieldColor), color),
        Some(TextPaint::Gradient { stops, angle, per_glyph }) => gradient_rows(ui, stops, angle, per_glyph, language),
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A headless inspector showing the layer section of one text, clicked by the labels of its
    /// widgets as the accessibility tree reports them.
    struct Panel {
        ctx: egui::Context,
        library: TextStyleLibrary,
        text: TextContent,
        request: Option<PaintSlot>,
        last: Option<egui::FullOutput>,
        id: egui::Id,
        time: f64,
    }

    impl Panel {
        fn new(text: TextContent) -> Panel {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            // Folding opens at once, so a click lands where the last frame drew.
            ctx.global_style_mut(|style| style.animation_time = 0.0);
            let mut panel = Panel { ctx, library: TextStyleLibrary::default(), text, request: None, last: None, id: egui::Id::unique("layers-test"), time: 0.0 };
            panel.frame(Vec::new());
            panel.frame(Vec::new());
            panel
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 3000.0));
            // Press and release make a click, and two clicks never make a double click.
            self.time += 0.2;
            let raw = egui::RawInput { events, screen_rect: Some(screen), time: Some(self.time), ..Default::default() };
            let (library, text, request, id) = (&self.library, &mut self.text, &mut self.request, self.id);
            let mut output = self.ctx.run_ui(raw, |ui| {
                let mut layers = Layers::new(Language::English, library);
                show(ui, id, text, &mut layers);
                *request = request.or(layers.file_request);
            });
            output.textures_delta.clear();
            self.last = Some(output);
        }

        /// The centre of the first widget labelled `label`.
        fn find(&self, label: &str) -> Option<egui::Pos2> {
            let update = self.last.as_ref()?.platform_output.accesskit_update.as_ref()?;
            let (_, node) = update.nodes.iter().find(|(_, node)| node.label() == Some(label) || node.value() == Some(label))?;
            let bounds = node.bounds()?;
            Some(egui::pos2(((bounds.x0 + bounds.x1) / 2.0) as f32, ((bounds.y0 + bounds.y1) / 2.0) as f32))
        }

        fn click(&mut self, label: &str) {
            let at = self.find(label).unwrap_or_else(|| panic!("no widget labelled {label:?}"));
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
            self.frame(vec![button(false)]);
            self.frame(Vec::new());
        }
    }

    fn full_text() -> TextContent {
        let mut text = TextContent::new("Hi", 0.1);
        text.fill = TextPaint::Gradient { stops: vec![GradientStop { position: 0.0, color: [255, 0, 0, 255] }, GradientStop { position: 1.0, color: [0, 0, 255, 255] }], angle: 90.0, per_glyph: false };
        text.outlines = vec![TextOutline { width: 0.05, paint: crate::text_clip::texture_paint(TextImageRef::File { path: "C:/a.png".to_owned() }) }; MAX_TEXT_OUTLINES];
        let mut shadow = default_shadow();
        shadow.outlines = vec![default_outline()];
        text.shadows = vec![shadow.clone(), shadow];
        text.inner_shadows = vec![default_inner_shadow()];
        text.bloom = Some(default_bloom());
        text
    }

    #[test]
    fn drawing_every_layer_without_input_changes_nothing() {
        let text = full_text();
        let panel = Panel::new(text.clone());
        assert_eq!((panel.text, panel.request), (text, None));
    }

    #[test]
    fn a_texture_is_cut_per_glyph_flipped_and_given_a_base() {
        let mut text = TextContent::new("Hi", 0.1);
        text.fill = crate::text_clip::texture_paint(TextImageRef::File { path: "C:/a.png".to_owned() });
        let mut panel = Panel::new(text);
        assert!(panel.find("Cells").is_none(), "cells show only when cut per glyph");
        panel.click("Per glyph");
        assert!(panel.find("Cells").is_some());
        panel.click("Flip horizontally");
        panel.click("Flip vertically");
        panel.click("Gradient base");
        panel.click("Across each glyph");
        let TextPaint::Texture(texture) = &panel.text.fill else { panic!("{:?}", panel.text.fill) };
        assert_eq!((texture.fit, texture.flip_x, texture.flip_y), (TextureFit::Glyph, true, true));
        let Some(TextPaint::Gradient { per_glyph: true, .. }) = texture.base.as_deref() else { panic!("{:?}", texture.base) };
        panel.click("Colour base");
        let TextPaint::Texture(texture) = &panel.text.fill else { panic!() };
        assert!(matches!(texture.base.as_deref(), Some(TextPaint::Solid { .. })), "{:?}", texture.base);
        // The gradient base comes back whole.
        panel.click("Gradient base");
        let TextPaint::Texture(texture) = &panel.text.fill else { panic!() };
        assert!(matches!(texture.base.as_deref(), Some(TextPaint::Gradient { per_glyph: true, .. })), "{:?}", texture.base);
        panel.click("No base");
        let TextPaint::Texture(texture) = &panel.text.fill else { panic!() };
        assert_eq!(texture.base, None);
        assert!(engine::text_style_problems(&panel.text).is_empty());
    }

    #[test]
    fn switching_the_paint_kind_and_back_keeps_the_gradient_and_the_texture() {
        let mut text = TextContent::new("Hi", 0.1);
        let gradient = TextPaint::Gradient { stops: vec![GradientStop { position: 0.0, color: [255, 0, 0, 255] }, GradientStop { position: 1.0, color: [0, 0, 255, 255] }], angle: 30.0, per_glyph: true };
        text.fill = gradient.clone();
        let mut panel = Panel::new(text);
        panel.click("Solid");
        assert!(matches!(panel.text.fill, TextPaint::Solid { .. }));
        panel.click("Gradient");
        assert_eq!(panel.text.fill, gradient);
        let texture = TextPaint::Texture(TextTexture { fit: TextureFit::Glyph, cells: 3, flip_x: true, base: Some(Box::new(TextPaint::solid([0, 255, 0, 255]))), ..TextTexture::new(TextImageRef::File { path: "C:/a.png".to_owned() }) });
        panel.text.fill = texture.clone();
        panel.frame(Vec::new());
        panel.click("Solid");
        assert_eq!(panel.text.fill, TextPaint::solid([0, 255, 0, 255]), "a texture's solid base becomes the colour");
        // No picture is installed, yet the texture comes back without asking for a file.
        panel.click("Texture");
        assert_eq!((&panel.text.fill, panel.request), (&texture, None));
    }

    #[test]
    fn the_fill_turns_into_a_gradient_of_its_colour_and_a_stop_is_added() {
        let mut panel = Panel::new(TextContent::new("Hi", 0.1));
        panel.click("Gradient");
        let TextPaint::Gradient { stops, .. } = &panel.text.fill else { panic!("{:?}", panel.text.fill) };
        assert_eq!(stops.len(), 2);
        assert_eq!(stops[0].color, [255, 255, 255, 255], "it starts from the colour it had");
        panel.click("+ Add a stop");
        let TextPaint::Gradient { stops, .. } = &panel.text.fill else { panic!() };
        assert_eq!(stops.iter().map(|stop| stop.position).collect::<Vec<_>>(), vec![0.0, 0.5, 1.0]);
        assert!(engine::text_style_problems(&panel.text).is_empty());
    }

    #[test]
    fn a_texture_with_no_picture_installed_asks_for_a_file_and_keeps_the_paint() {
        let mut panel = Panel::new(TextContent::new("Hi", 0.1));
        panel.click("Texture");
        assert_eq!(panel.request, Some(PaintSlot::Fill));
        assert_eq!(panel.text.fill, TextPaint::default());
    }

    #[test]
    fn outlines_are_added_up_to_the_limit_moved_and_removed() {
        let mut panel = Panel::new(TextContent::new("Hi", 0.1));
        panel.click("Outlines");
        for _ in 0..MAX_TEXT_OUTLINES + 1 {
            panel.click("+ Add");
        }
        assert_eq!(panel.text.outlines.len(), MAX_TEXT_OUTLINES, "the add button is off once full");
        panel.text.outlines[0].width = 0.01;
        panel.click("\u{2193}");
        assert_eq!(panel.text.outlines[1].width, 0.01, "the first outline moved down");
        panel.click("\u{d7}");
        assert_eq!(panel.text.outlines.len(), MAX_TEXT_OUTLINES - 1);
        assert_eq!(panel.text.outlines[0].width, 0.01, "the first outline was removed, the moved one is first again");
    }

    fn outline_item_open(panel: &Panel, key: u64) -> bool {
        let item_id = panel.id.with("outline-list").with(key);
        egui::collapsing_header::CollapsingState::load(&panel.ctx, item_id).is_some_and(|state| state.is_open())
    }

    fn open_outline_item(panel: &mut Panel, key: u64) {
        let item_id = panel.id.with("outline-list").with(key);
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(&panel.ctx, item_id, false);
        state.set_open(true);
        state.store(&panel.ctx);
        panel.frame(Vec::new());
    }

    #[test]
    fn a_layer_keeps_its_open_state_when_moved_and_the_next_one_does_not_inherit_it_when_removed() {
        let mut text = TextContent::new("Hi", 0.1);
        text.outlines = vec![default_outline(), default_outline(), default_outline()];
        let mut panel = Panel::new(text);
        panel.click("Outlines (3)");
        // The layers are keyed 0, 1, 2 as first drawn; open the first.
        open_outline_item(&mut panel, 0);
        assert!(outline_item_open(&panel, 0) && !outline_item_open(&panel, 1));
        let open_row = panel.find("Thickness").expect("the open layer shows its rows").y;
        panel.text.outlines[0].width = 0.01;
        panel.click("\u{2193}");
        assert_eq!(panel.text.outlines[1].width, 0.01, "the first outline moved down");
        // The moved layer is now second: its state moved with it, the one before it is shut.
        let keys: LayerKeys = panel.ctx.data_mut(|data| data.get_temp(panel.id.with("outline-list").with("keys"))).unwrap();
        assert_eq!(keys.keys, vec![1, 0, 2]);
        assert!(outline_item_open(&panel, 0), "the open layer is still open, at its new place");
        assert!(panel.find("Thickness").expect("its rows are still shown").y > open_row, "the rows of the open layer moved down with it");
        panel.click("\u{d7}");
        let keys: LayerKeys = panel.ctx.data_mut(|data| data.get_temp(panel.id.with("outline-list").with("keys"))).unwrap();
        assert_eq!(keys.keys, vec![0, 2], "the first row (key 1) was removed with its state");
        assert!(outline_item_open(&panel, 0));
        assert!(!outline_item_open(&panel, 2), "the layer that took the place of a removed one is not open");
    }

    #[test]
    fn a_layer_added_after_a_removal_gets_a_key_never_used_before() {
        let mut keys = LayerKeys::default();
        keys.fit(2);
        keys.apply(LayerAction::Remove(1), 4);
        keys.apply(LayerAction::Add, 4);
        assert_eq!(keys.keys, vec![0, 2]);
    }

    #[test]
    fn an_inner_shadow_layer_is_titled_in_the_singular() {
        let mut text = TextContent::new("Hi", 0.1);
        text.inner_shadows = vec![default_inner_shadow()];
        let mut panel = Panel::new(text);
        panel.click("Inner shadows (1)");
        assert!(panel.find("Inner shadow 1").is_some());
        assert!(panel.find("Inner shadows 1").is_none());
    }

    #[test]
    fn the_pictures_of_subtitle_style_packs_are_offered_too() {
        let mut library = crate::text_style_library::subtitle_library_for_test(&[]);
        let picture = TextImageRef::Pack { pack: "s".to_owned(), file: "glow.png".to_owned(), pack_type: engine::TexturePackType::SubtitleStyle };
        library.packs[0].styles[0].style.fill = Some(crate::text_clip::texture_paint(picture.clone()));
        let text_picture = TextImageRef::text_style_pack("t", "gold.png");
        let mut text_style = library.packs[0].styles[0].clone();
        text_style.kind = crate::text_style_library::StyleKind::Text;
        text_style.style.fill = Some(crate::text_clip::texture_paint(text_picture.clone()));
        library.packs.push(crate::text_style_library::TextStylePack { id: "t".to_owned(), name: "T".to_owned(), kind: crate::text_style_library::StyleKind::Text, styles: vec![text_style] });
        assert_eq!(pack_images(&library), vec![picture, text_picture]);
    }
}
