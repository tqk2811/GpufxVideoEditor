//! The panel on the left: imported media, audio and the effect picker.

use super::icons::{self, Icon};
use super::{theme, UiRequest};
use crate::audio_library::LibrarySound;
use crate::editor::{EditorState, LeftTab, LibraryTarget, SoundPreviewWish};
use crate::effect_previews::play::{loop_step, static_step, Hover, Shown, Step};
use crate::effect_previews::{AnimationState, LoopFrames, PreviewManager, TileState};
use crate::i18n::{Language, Message};
use crate::layout::grid_columns;
use crate::media::{MediaItem, MediaKind};
use crate::sticker_library::LibrarySticker;
use crate::packages::{duplicated_names, filter_entries, list_label, short_name, PackageEntry, PackageKind};
use crate::timecode::format_duration;
use std::cell::RefCell;
use std::ops::Range;
use std::time::Duration;

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, UiBuilder, Vec2};

const RAIL_WIDTH: f32 = 68.0;
const RAIL_CELL_HEIGHT: f32 = 56.0;
const TAB_FONT: f32 = 12.0;
const TAB_FONT_MIN: f32 = 9.5;
const TAB_PADDING: f32 = 6.0;
const CELL_MIN: f32 = 118.0;
const CELL_GAP: f32 = 8.0;
const CELL_CAPTION: f32 = 24.0;
const THUMB_RATIO: f32 = 0.6;
const PICKER_FOOTER_HEIGHT: f32 = 44.0;
const SOUND_ROW_HEIGHT: f32 = 34.0;
const SOUND_BUTTON: f32 = 22.0;
/// How often the Audio tab looks whether a preview has ended.
const PREVIEW_REPAINT: Duration = Duration::from_millis(250);
const TILE_MIN: f32 = 96.0;
const TILE_GAP: f32 = 8.0;
const TILE_CAPTION: f32 = 22.0;
/// The height of the activity bar on the bottom edge of a tile whose loop is being rendered.
const BUSY_BAR_HEIGHT: f32 = 3.0;
/// How often the activity bar moves, in seconds.
const BUSY_REPAINT: f64 = 1.0 / 30.0;
/// How long the bar takes to cross the tile, in seconds.
const BUSY_SWEEP: f64 = 1.2;

pub fn show(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let rect = ui.max_rect();
    let rail = Rect::from_min_size(rect.min, Vec2::new(RAIL_WIDTH.min(rect.width()), rect.height()));
    rail_tabs(ui, state, rail);
    ui.painter_at(rect).vline(rail.right(), rect.y_range(), Stroke::new(1.0, theme::border()));
    let content = Rect::from_min_max(egui::pos2(rail.right(), rect.top()), rect.max).shrink(10.0);
    if content.width() < 1.0 || content.height() < 1.0 {
        return;
    }
    ui.scope_builder(UiBuilder::new().max_rect(content), |ui| match state.ui.left_tab {
        LeftTab::Media => {
            state.ui.tile_hover.clear();
            library(ui, state, requests, MediaKind::Video)
        }
        LeftTab::Audio => {
            state.ui.tile_hover.clear();
            audio_tab(ui, state, requests);
        }
        LeftTab::Effects | LeftTab::Transitions => picker(ui, state, requests),
        LeftTab::Stickers => {
            state.ui.tile_hover.clear();
            sticker_tab(ui, state, requests);
        }
        LeftTab::Text => {
            state.ui.tile_hover.clear();
            text_tab(ui, state);
        }
    });
}

/// The height of one entry of the rail: all `count` entries stay reachable in a panel too short
/// for the usual height.
fn rail_cell_height(available: f32, count: usize) -> f32 {
    if RAIL_CELL_HEIGHT * count as f32 <= available {
        RAIL_CELL_HEIGHT
    } else {
        available / count.max(1) as f32
    }
}

/// The centre of the rail entry `index` in a left panel at `panel`, for tests that click it.
#[cfg(test)]
pub(super) fn rail_cell_center(panel: Rect, index: usize) -> egui::Pos2 {
    egui::pos2(panel.left() + 1.0 + RAIL_WIDTH / 2.0, panel.top() + 1.0 + RAIL_CELL_HEIGHT * (index as f32 + 0.5))
}

/// The icons on the left edge, one per tab.
fn rail_tabs(ui: &mut egui::Ui, state: &mut EditorState, rail: Rect) {
    let language = state.language();
    let entries = [
        (LeftTab::Media, Icon::Film, Message::TabMedia),
        (LeftTab::Audio, Icon::Music, Message::TabAudio),
        (LeftTab::Effects, Icon::Sparkle, Message::TabEffects),
        (LeftTab::Transitions, Icon::Transition, Message::Transitions),
        (LeftTab::Stickers, Icon::Sticker, Message::TabStickers),
        (LeftTab::Text, Icon::Text, Message::TabText),
    ];
    let height = rail_cell_height(rail.height(), entries.len());
    for (index, (tab, icon, label)) in entries.into_iter().enumerate() {
        let cell = Rect::from_min_size(rail.min + Vec2::new(0.0, height * index as f32), Vec2::new(rail.width(), height));
        let response = ui.interact(cell, ui.scope_id().with(("left-tab", index)), Sense::click());
        let selected = state.ui.left_tab == tab;
        let painter = ui.painter_at(cell);
        if selected {
            painter.rect_filled(cell.shrink(3.0), CornerRadius::same(theme::CONTROL_RADIUS), theme::accent_soft());
            painter.vline(cell.left() + 1.0, cell.y_range(), Stroke::new(2.0, theme::accent()));
        } else if response.hovered() {
            painter.rect_filled(cell.shrink(3.0), CornerRadius::same(theme::CONTROL_RADIUS), theme::panel_raised());
        }
        let color = if selected { theme::accent_text() } else { theme::text_dim() };
        let icon_rect = Rect::from_center_size(egui::pos2(cell.center().x, cell.center().y - 8.0), Vec2::splat(16.0));
        icons::paint(&painter, icon_rect, icon, color);
        let label = language.t(label);
        let natural = painter.layout_no_wrap(label.clone(), FontId::proportional(TAB_FONT), color).size().x;
        painter.text(egui::pos2(cell.center().x, cell.center().y + 12.0), Align2::CENTER_CENTER, label, FontId::proportional(tab_font_size(natural, cell.width())), color);
        if response.clicked() {
            state.ui.left_tab = tab;
            match tab {
                LeftTab::Effects => state.picker.set_kind(PackageKind::Effect),
                LeftTab::Transitions => state.picker.set_kind(PackageKind::Transition),
                LeftTab::Media | LeftTab::Audio | LeftTab::Stickers | LeftTab::Text => {}
            }
        }
    }
}

/// The size that fits a tab label `natural` wide in a cell: the longest labels of some languages
/// would not fit at a narrow window, so they shrink, down to what is still readable.
fn tab_font_size(natural: f32, cell_width: f32) -> f32 {
    let room = (cell_width - TAB_PADDING).max(1.0);
    (TAB_FONT * (room / natural.max(1.0))).clamp(TAB_FONT_MIN, TAB_FONT)
}

fn search_box(ui: &mut egui::Ui, text: &mut String, hint: &str) {
    ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(f32::INFINITY));
}

fn library(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, kind: MediaKind) {
    let language = state.language();
    let audio_only = kind == MediaKind::Audio;
    if icons::label_button(ui, Icon::Upload, &language.t(Message::ImportFromComputer), theme::accent_soft(), theme::accent_text(), Some(ui.available_width())).clicked() {
        requests.push(UiRequest::ImportMedia { audio_only });
    }
    let hint = language.t(if audio_only { Message::SearchAudio } else { Message::SearchMedia });
    search_box(ui, if audio_only { &mut state.ui.audio_search } else { &mut state.ui.media_search }, &hint);
    let search = if audio_only { state.ui.audio_search.clone() } else { state.ui.media_search.clone() };
    let items: Vec<&MediaItem> = state.library.visible(kind, &search);
    if items.is_empty() {
        ui.add(egui::Label::new(egui::RichText::new(language.t(empty_message(&search))).color(theme::text_dim())).wrap());
        return;
    }
    ui.add(egui::Label::new(egui::RichText::new(language.t(Message::AddHint)).color(theme::text_dim())).wrap());
    let mut chosen = None;
    egui::ScrollArea::vertical().id_salt("media-grid").auto_shrink([false, false]).show(ui, |ui| chosen = media_grid(ui, state, &items, kind));
    if let Some(item) = chosen {
        state.add_media_at_playhead(&item);
    }
}

/// The Sticker tab: the import button, a search, a category filter and the grid.
fn sticker_tab(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let language = state.language();
    if icons::label_button(ui, Icon::Upload, &language.t(Message::ImportPackage), theme::accent_soft(), theme::accent_text(), Some(ui.available_width())).clicked() {
        requests.push(UiRequest::ImportPackage);
    }
    search_box(ui, &mut state.ui.sticker_search, &language.t(Message::SearchStickers));
    category_filter(ui, state);
    let (search, category) = (state.ui.sticker_search.clone(), state.ui.sticker_category.clone());
    let found: Vec<LibrarySticker> = state.sticker_library.matching(&search, category.as_deref()).into_iter().cloned().collect();
    if found.is_empty() {
        let message = empty_sticker_message(state.sticker_library.loaded, search.trim().is_empty() && category.is_none());
        ui.add(egui::Label::new(egui::RichText::new(language.t(message)).color(theme::text_dim())).wrap());
        sticker_problems(ui, state);
        return;
    }
    ui.add(egui::Label::new(egui::RichText::new(language.t(Message::StickerAddHint)).color(theme::text_dim())).wrap());
    let width = (ui.available_width() - ui.spacing().scroll.allocated_width()).max(1.0);
    let (columns, side) = picker_grid(width);
    let mut chosen = None;
    egui::ScrollArea::vertical().id_salt("sticker-grid").auto_shrink([false, false]).show_rows(ui, side + TILE_CAPTION, found.len().div_ceil(columns), |ui, rows| {
        for row in rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = TILE_GAP;
                for sticker in &found[row_tiles(row, columns, found.len())] {
                    if sticker_tile(ui, state, sticker, side) {
                        chosen = Some(sticker.media_item());
                    }
                }
            });
        }
    });
    sticker_problems(ui, state);
    if let Some(item) = chosen {
        state.add_media_at_playhead(&item);
    }
}

/// The Text tab: a button that adds a default text at the playhead.
fn text_tab(ui: &mut egui::Ui, state: &mut EditorState) {
    let language = state.language();
    if icons::label_button(ui, Icon::Plus, &language.t(Message::AddText), theme::accent_soft(), theme::accent_text(), Some(ui.available_width())).clicked() {
        state.add_text_at_playhead();
    }
    ui.add_space(6.0);
    ui.add(egui::Label::new(egui::RichText::new(language.t(Message::TextAddHint)).color(theme::text_dim())).wrap());
}

/// What the Sticker tab says when it has nothing to show.
fn empty_sticker_message(loaded: bool, unfiltered: bool) -> Message {
    if !loaded {
        Message::ReadingStickerPacks
    } else if unfiltered {
        Message::StickerLibraryEmpty
    } else {
        Message::NoSearchMatch
    }
}

/// The combo box of the categories; "all" clears the filter.
fn category_filter(ui: &mut egui::Ui, state: &mut EditorState) {
    let language = state.language();
    let categories: Vec<String> = state.sticker_library.categories().into_iter().map(str::to_owned).collect();
    let all = language.t(Message::AllCategories);
    let shown = state.ui.sticker_category.clone().unwrap_or_else(|| all.clone());
    egui::ComboBox::from_id_salt("sticker-category").selected_text(shown).width(ui.available_width()).show_ui(ui, |ui| {
        ui.selectable_value(&mut state.ui.sticker_category, None, all);
        for category in categories {
            ui.selectable_value(&mut state.ui.sticker_category, Some(category.clone()), category);
        }
    });
}

fn sticker_problems(ui: &mut egui::Ui, state: &EditorState) {
    if state.sticker_library.problems.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(state.language().t(Message::StickerPackProblems(state.sticker_library.problems.len()))).id_salt("sticker-pack-problems").show(ui, |ui| {
        for problem in &state.sticker_library.problems {
            ui.colored_label(theme::refused(), format!("{}: {}", problem.pack, problem.message));
        }
    });
}

/// One square tile of the Sticker tab: the picture on a checker-free dark background, its name below.
/// Can be dragged onto a track like any media. True on a first click.
fn sticker_tile(ui: &mut egui::Ui, state: &EditorState, sticker: &LibrarySticker, side: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(side, side + TILE_CAPTION), Sense::click_and_drag());
    response.dnd_set_drag_payload(sticker.media_item());
    let painter = ui.painter_at(rect);
    let radius = CornerRadius::same(theme::CONTROL_RADIUS);
    let picture = Rect::from_min_size(rect.min, Vec2::splat(side));
    painter.rect_filled(picture, radius, theme::thumb_bg());
    // Only a tile scrolled into view asks for its picture, which a worker makes off the UI thread.
    let poster = ui.is_rect_visible(picture).then(|| state.visuals.borrow_mut().poster(&sticker.source, None)).flatten();
    match poster {
        Some((texture, aspect)) => {
            let fitted = Rect::from_center_size(picture.center(), fit_size(picture.shrink(6.0).size(), aspect));
            painter.image(texture, fitted, Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => icons::paint(&painter, Rect::from_center_size(picture.center(), Vec2::splat(24.0)), Icon::Sticker, theme::text_dim()),
    }
    let stroke = if response.hovered() { Stroke::new(1.0, theme::accent()) } else { Stroke::new(1.0, theme::border()) };
    painter.rect_stroke(picture, radius, stroke, StrokeKind::Inside);
    let mut job = egui::text::LayoutJob::simple_singleline(sticker.name.clone(), FontId::proportional(12.0), theme::text());
    job.wrap = egui::text::TextWrapping::truncate_at_width((side - 4.0).max(1.0));
    let galley = painter.layout_job(job);
    painter.galley(egui::pos2(rect.left() + 2.0, picture.bottom() + (TILE_CAPTION - galley.size().y) / 2.0), galley, theme::text());
    let response = response.on_hover_text(sticker_tooltip(sticker));
    response.clicked() && !response.double_clicked() && !response.triple_clicked()
}

fn sticker_tooltip(sticker: &LibrarySticker) -> String {
    let tags = if sticker.tags.is_empty() { sticker.category.clone() } else { format!("{} [{}]", sticker.category, sticker.tags.join(", ")) };
    format!("{}\n{tags}", sticker.name)
}

/// The grid of `items`, and the one chosen this frame.
fn media_grid(ui: &mut egui::Ui, state: &EditorState, items: &[&MediaItem], kind: MediaKind) -> Option<MediaItem> {
    let mut chosen = None;
    let width = ui.available_width();
    let columns = grid_columns(width, CELL_MIN, CELL_GAP);
    let cell_width = (width - CELL_GAP * (columns as f32 - 1.0)) / columns as f32;
    let cell_height = cell_width * THUMB_RATIO + CELL_CAPTION;
    for row in items.chunks(columns) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = CELL_GAP;
            for item in row {
                if media_cell(ui, state, item, Vec2::new(cell_width, cell_height), kind) {
                    chosen = Some((*item).clone());
                }
            }
        });
    }
    chosen
}

/// What a row of a sound was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SoundAction {
    None,
    Listen,
    Stop,
    Add,
}

/// The Audio tab: what was imported in this session, then the installed packs by name.
fn audio_tab(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let language = state.language();
    if icons::label_button(ui, Icon::Upload, &language.t(Message::ImportFromComputer), theme::accent_soft(), theme::accent_text(), Some(ui.available_width())).clicked() {
        requests.push(UiRequest::ImportMedia { audio_only: true });
    }
    if icons::label_button(ui, Icon::Upload, &language.t(Message::ImportPackage), theme::accent_soft(), theme::accent_text(), Some(ui.available_width())).clicked() {
        requests.push(UiRequest::ImportPackage);
    }
    search_box(ui, &mut state.ui.audio_search, &language.t(Message::SearchAudio));
    if state.ui.previewing.is_some() {
        ui.ctx().request_repaint_after(PREVIEW_REPAINT);
    }
    let search = state.ui.audio_search.clone();
    // Borrowed from the state, not copied: a pack may list thousands of sounds, drawn every frame.
    let session: Vec<&MediaItem> = state.library.visible(MediaKind::Audio, &search);
    let found = state.audio_library.matching(&search);
    if session.is_empty() && found.is_empty() {
        let message = if state.audio_library.loaded { language.t(if search.trim().is_empty() { Message::AudioLibraryEmpty } else { Message::NoSearchMatch }) } else { language.t(Message::ReadingAudioPacks) };
        ui.add(egui::Label::new(egui::RichText::new(message).color(theme::text_dim())).wrap());
        library_problems(ui, state);
        return;
    }
    ui.add(egui::Label::new(egui::RichText::new(language.t(Message::AudioAddHint)).color(theme::text_dim())).wrap());
    let mut chosen = None;
    let mut acted = None;
    egui::ScrollArea::vertical().id_salt("audio-library").auto_shrink([false, false]).show(ui, |ui| {
        if !session.is_empty() {
            ui.label(egui::RichText::new(language.t(Message::SessionAudio)).strong());
            chosen = media_grid(ui, state, &session, MediaKind::Audio);
        }
        if !found.is_empty() {
            ui.label(egui::RichText::new(language.t(Message::InstalledPacks)).strong());
        }
        for (pack, sounds) in &found {
            egui::CollapsingHeader::new(format!("{} ({})", pack.name, sounds.len())).id_salt(("audio-pack", pack.kind.name(), &pack.id)).default_open(true).show(ui, |ui| {
                if let Some(done) = sound_rows(ui, state, sounds) {
                    acted = Some(done);
                }
            });
        }
        library_problems(ui, state);
    });
    if let Some(item) = chosen {
        state.add_media_at_playhead(&item);
    }
    match acted {
        Some((sound, SoundAction::Listen)) => state.ui.preview_wish = Some(SoundPreviewWish::Play { source: sound.source, duration: sound.duration }),
        Some((_, SoundAction::Stop)) => state.ui.preview_wish = Some(SoundPreviewWish::Stop),
        Some((sound, SoundAction::Add)) => state.place_library_audio(&sound.media_item(), LibraryTarget::Playhead),
        Some((_, SoundAction::None)) | None => {}
    }
}

/// The rows of `sounds`, drawing only those in view: the rows are all one height, so the ones above
/// and below are skipped with blank space. The first one asked for something, if any did.
fn sound_rows(ui: &mut egui::Ui, state: &EditorState, sounds: &[&LibrarySound]) -> Option<(LibrarySound, SoundAction)> {
    ui.spacing_mut().item_spacing.y = 0.0;
    let visible = visible_rows(ui.cursor().top(), ui.clip_rect().top(), ui.clip_rect().bottom(), SOUND_ROW_HEIGHT, sounds.len());
    ui.add_space(visible.start as f32 * SOUND_ROW_HEIGHT);
    let mut acted = None;
    for sound in &sounds[visible.clone()] {
        match sound_row(ui, state, sound) {
            SoundAction::None => {}
            action => acted = Some(((*sound).clone(), action)),
        }
    }
    ui.add_space((sounds.len() - visible.end) as f32 * SOUND_ROW_HEIGHT);
    acted
}

/// The rows of `count`, each `height` tall from `top` down, that meet the span `clip_top..clip_bottom`.
fn visible_rows(top: f32, clip_top: f32, clip_bottom: f32, height: f32, count: usize) -> Range<usize> {
    let first = (((clip_top - top) / height).floor().max(0.0) as usize).min(count);
    let last = (((clip_bottom - top) / height).ceil().max(0.0) as usize).min(count).max(first);
    first..last
}

fn library_problems(ui: &mut egui::Ui, state: &EditorState) {
    if state.audio_library.problems.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(state.language().t(Message::AudioPackProblems(state.audio_library.problems.len()))).id_salt("audio-pack-problems").show(ui, |ui| {
        for problem in &state.audio_library.problems {
            ui.colored_label(theme::refused(), format!("{}: {}", problem.pack, problem.message));
        }
    });
}

/// One sound: a button to listen, its name and manifest length, and a button to add it. The row
/// itself can be dragged onto a track like any media, and a double click adds it.
fn sound_row(ui: &mut egui::Ui, state: &EditorState, sound: &LibrarySound) -> SoundAction {
    let language = state.language();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), SOUND_ROW_HEIGHT), Sense::click_and_drag());
    response.dnd_set_drag_payload(sound.media_item());
    let painter = ui.painter_at(rect);
    if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(theme::CONTROL_RADIUS), theme::panel_raised());
    }
    let playing = state.ui.previewing.as_deref() == Some(&sound.source);
    let listen_rect = Rect::from_center_size(egui::pos2(rect.left() + 4.0 + SOUND_BUTTON / 2.0, rect.center().y), Vec2::splat(SOUND_BUTTON));
    let add_rect = Rect::from_center_size(egui::pos2(rect.right() - 4.0 - SOUND_BUTTON / 2.0, rect.center().y), Vec2::splat(SOUND_BUTTON));
    let listen = ui.interact(listen_rect, ui.scope_id().with(("sound-listen", &sound.source)), Sense::click()).on_hover_text(language.t(if playing { Message::StopPreview } else { Message::PreviewSound }));
    let add = ui.interact(add_rect, ui.scope_id().with(("sound-add", &sound.source)), Sense::click()).on_hover_text(language.t(Message::AddSound));
    icons::paint(&painter, listen_rect.shrink(4.0), if playing { Icon::Stop } else { Icon::Play }, if listen.hovered() { theme::accent() } else { theme::text() });
    painter.rect_filled(add_rect, CornerRadius::same(4), theme::accent());
    icons::paint(&painter, add_rect.shrink(5.0), Icon::Plus, theme::on_accent());
    let length = sound.duration.map_or_else(|| "--:--".to_owned(), format_duration);
    let length_text = painter.layout_no_wrap(length, FontId::monospace(11.0), theme::text_dim());
    let length_at = egui::pos2(add_rect.left() - 6.0 - length_text.size().x, rect.center().y - length_text.size().y / 2.0);
    let name_rect = Rect::from_min_max(egui::pos2(listen_rect.right() + 6.0, rect.top()), egui::pos2(length_at.x - 4.0, rect.bottom()));
    painter.galley(length_at, length_text, theme::text_dim());
    painter.with_clip_rect(name_rect).text(name_rect.left_center(), Align2::LEFT_CENTER, &sound.name, FontId::proportional(12.0), theme::text());
    let response = response.on_hover_text(sound_tooltip(sound));
    if listen.clicked() {
        if playing {
            SoundAction::Stop
        } else {
            SoundAction::Listen
        }
    } else if add.clicked() || response.double_clicked() {
        SoundAction::Add
    } else {
        SoundAction::None
    }
}

/// The name of a sound, what it is filed under, and the source a clip would store.
fn sound_tooltip(sound: &LibrarySound) -> String {
    let tags = if sound.tags.is_empty() { sound.category.clone() } else { format!("{} [{}]", sound.category, sound.tags.join(", ")) };
    format!("{}\n{tags}\n{}", sound.name, sound.source)
}

/// What an empty grid says: a filter that matches nothing is not an empty library.
fn empty_message(search: &str) -> Message {
    if search.trim().is_empty() {
        Message::LibraryEmpty
    } else {
        Message::NoSearchMatch
    }
}

/// One tile of the grid, with a thumbnail of the file once it is made. True on a first click: the
/// later clicks of a double click do not add the file again.
fn media_cell(ui: &mut egui::Ui, state: &EditorState, item: &MediaItem, size: Vec2, kind: MediaKind) -> bool {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    response.dnd_set_drag_payload(item.clone());
    let painter = ui.painter_at(rect);
    let radius = CornerRadius::same(theme::CONTROL_RADIUS);
    painter.rect_filled(rect, radius, theme::panel_raised());
    let stroke = if response.hovered() { Stroke::new(1.0, theme::accent()) } else { Stroke::new(1.0, theme::border()) };
    painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
    let thumb = Rect::from_min_size(rect.min, Vec2::new(rect.width(), rect.height() - CELL_CAPTION)).shrink(4.0);
    painter.rect_filled(thumb, CornerRadius::same(4), theme::thumb_bg());
    // Only a tile scrolled into view asks for its picture.
    let poster = (kind == MediaKind::Video && ui.is_rect_visible(thumb)).then(|| state.visuals.borrow_mut().poster(&item.source, item.duration)).flatten();
    match poster {
        Some((texture, aspect)) => {
            let picture = Rect::from_center_size(thumb.center(), fit_size(thumb.size(), aspect));
            painter.image(texture, picture, Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => icons::paint(&painter, Rect::from_center_size(thumb.center(), Vec2::splat(22.0)), if kind == MediaKind::Audio { Icon::Music } else { Icon::Film }, theme::text_dim()),
    }
    let duration = match (item.duration, item.estimated) {
        (Some(duration), false) => format_duration(duration),
        (Some(duration), true) => format!("~{}", format_duration(duration)),
        (None, _) => "--:--".to_owned(),
    };
    let pill = painter.layout_no_wrap(duration, FontId::monospace(11.0), theme::on_clip());
    let pill_rect = Rect::from_min_size(egui::pos2(thumb.right() - pill.size().x - 8.0, thumb.top() + 3.0), pill.size() + Vec2::new(5.0, 2.0));
    painter.rect_filled(pill_rect, CornerRadius::same(3), Color32::from_black_alpha(180));
    painter.galley(pill_rect.min + Vec2::new(2.5, 1.0), pill, theme::on_clip());
    let plus = Rect::from_min_size(thumb.left_top() + Vec2::splat(3.0), Vec2::splat(18.0));
    painter.rect_filled(plus, CornerRadius::same(4), theme::accent());
    icons::paint(&painter, plus.shrink(4.0), Icon::Plus, theme::on_accent());
    let caption = rect.with_min_y(rect.bottom() - CELL_CAPTION).shrink2(Vec2::new(8.0, 0.0));
    painter.with_clip_rect(caption).text(caption.left_center(), Align2::LEFT_CENTER, &item.name, FontId::proportional(12.0), theme::text());
    let response = response.on_hover_text(&item.source);
    response.clicked() && !response.double_clicked() && !response.triple_clicked()
}

/// The largest size of shape `aspect` inside `room`.
fn fit_size(room: Vec2, aspect: f32) -> Vec2 {
    let width = room.x.min(room.y * aspect);
    Vec2::new(width, width / aspect.max(0.01))
}

/// The effect and transition lists with their filter.
fn picker(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let language: Language = state.language();
    if icons::label_button(ui, Icon::Upload, &language.t(Message::ImportPackage), theme::accent_soft(), theme::accent_text(), Some(ui.available_width())).clicked() {
        requests.push(UiRequest::ImportPackage);
    }
    search_box(ui, &mut state.picker.filter, &language.t(Message::SearchEffects));
    if !state.catalog.loaded {
        ui.colored_label(theme::text_dim(), language.t(Message::ReadingPackages));
    } else if state.catalog.effects.is_empty() && state.catalog.transitions.is_empty() {
        ui.colored_label(theme::refused(), language.t(Message::NoPackages(crate::project_file::PACKAGE_ROOT_VARIABLE.to_owned())));
    }
    picker_list(ui, state);
    let selected = state.picker.addable(&state.catalog);
    if ui.add_enabled(selected.is_some(), egui::Button::new(language.t(Message::AddToClip)).min_size(Vec2::new(ui.available_width(), 28.0))).clicked() {
        if let Some(package) = &selected {
            add_package(state, package);
        }
    }
    picker_problems(ui, state);
}

fn add_package(state: &mut EditorState, package: &str) {
    if state.picker.kind == PackageKind::Effect {
        state.add_effect_package(package);
    } else {
        state.add_transition_package(package);
    }
}

/// The layout of the picker grid for a content `width`: the number of columns and the side of a
/// square tile, which is at least [`TILE_MIN`] unless the width itself is smaller.
fn picker_grid(width: f32) -> (usize, f32) {
    let columns = grid_columns(width, TILE_MIN, TILE_GAP);
    let side = ((width - TILE_GAP * (columns as f32 - 1.0)) / columns as f32).max(1.0);
    (columns, side)
}

/// The tiles of grid row `row`, as indices into `total` tiles.
fn row_tiles(row: usize, columns: usize, total: usize) -> Range<usize> {
    let start = row.saturating_mul(columns).min(total);
    start..start.saturating_add(columns).min(total)
}

/// The tiles in the grid rows `rows`: only these are built, and only these ask for a preview.
fn visible_tiles(rows: Range<usize>, columns: usize, total: usize) -> Range<usize> {
    let start = rows.start.saturating_mul(columns).min(total);
    start..rows.end.saturating_mul(columns).min(total).max(start)
}

/// How a tile shows its preview state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileLook {
    Picture,
    Placeholder,
    /// The placeholder with a warning on it.
    Broken,
}

fn tile_look(tile: &TileState) -> TileLook {
    match tile {
        TileState::Still(_) => TileLook::Picture,
        TileState::Pending | TileState::Loading | TileState::Unavailable(_) => TileLook::Placeholder,
        TileState::Failed(_) => TileLook::Broken,
    }
}

/// The tooltip of a tile: the full name, what the package says about itself (its path when it says
/// nothing), and the reason a preview is missing, if there is one.
fn tile_tooltip(label: &str, entry: &PackageEntry, language: Language, tile: &TileState) -> String {
    let about = if entry.description.is_empty() { entry.package.as_str() } else { entry.description.as_str() };
    let mut text = format!("{label}\n{about}");
    match tile {
        TileState::Failed(reason) => text.push_str(&format!("\n{}", language.t(Message::PreviewFailed(reason.clone())))),
        TileState::Unavailable(reason) => text.push_str(&format!("\n{}", language.t(Message::PreviewUnavailable(reason.clone())))),
        TileState::Pending | TileState::Loading | TileState::Still(_) => {}
    }
    text
}

/// The picture a tile shows.
#[derive(Clone)]
enum TileImage {
    /// The still.
    Still,
    /// Picture A without the effect.
    Original(egui::TextureHandle),
    Frame(LoopFrames, u32),
}

/// What a tile shows beyond its placeholder: the picture, and while a loop is being rendered, how far
/// the activity bar has moved, from 0 to 1.
#[derive(Clone)]
struct TileView {
    image: TileImage,
    busy: Option<f32>,
}

impl TileView {
    fn still() -> Self {
        Self { image: TileImage::Still, busy: None }
    }
}

/// Where the activity bar is `now`, from 0 to 1.
fn busy_phase(now: f64) -> f32 {
    ((now % BUSY_SWEEP) / BUSY_SWEEP) as f32
}

/// The activity bar along the bottom edge of `picture`: a short stretch that crosses the tile.
fn paint_busy_bar(painter: &egui::Painter, picture: Rect, phase: f32) {
    let track = Rect::from_min_max(egui::pos2(picture.left(), picture.bottom() - BUSY_BAR_HEIGHT), picture.right_bottom());
    painter.rect_filled(track, 0.0, Color32::from_black_alpha(110));
    let stretch = track.width() * 0.35;
    let start = track.left() - stretch + (track.width() + stretch) * phase;
    let bar = Rect::from_min_max(egui::pos2(start.max(track.left()), track.top()), egui::pos2((start + stretch).min(track.right()), track.bottom()));
    if bar.width() > 0.0 {
        painter.rect_filled(bar, 0.0, theme::accent());
    }
}

/// One square tile of the picker: the preview on an opaque background, since a preview with
/// transparent pixels must not show the panel through it, and a caption of one line below.
/// `view` is told whether the pointer is on the tile and says what the picture is.
fn package_tile(ui: &mut egui::Ui, side: f32, label: &str, selected: bool, tile: &TileState, view: impl FnOnce(bool) -> TileView) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(side, side + TILE_CAPTION), Sense::click());
    let view = view(response.hovered());
    let painter = ui.painter_at(rect);
    let radius = CornerRadius::same(theme::CONTROL_RADIUS);
    let picture = Rect::from_min_size(rect.min, Vec2::splat(side));
    painter.rect_filled(picture, radius, theme::thumb_bg());
    match (tile_look(tile), tile) {
        (TileLook::Picture, TileState::Still(texture)) => {
            let whole = Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
            let (id, uv) = match &view.image {
                TileImage::Still => (texture.id(), whole),
                TileImage::Original(original) => (original.id(), whole),
                TileImage::Frame(frames, index) => (frames.texture.id(), frames.uv(*index)),
            };
            painter.image(id, picture.shrink(1.0), uv, Color32::WHITE);
            if let Some(phase) = view.busy {
                paint_busy_bar(&painter, picture.shrink(1.0), phase);
            }
        }
        (look, _) => {
            icons::paint(&painter, Rect::from_center_size(picture.center(), Vec2::splat(28.0)), Icon::Sparkle, theme::text_dim());
            if look == TileLook::Broken {
                icons::paint(&painter, Rect::from_min_size(picture.right_top() + Vec2::new(-20.0, 4.0), Vec2::splat(16.0)), Icon::Warning, theme::refused());
            }
        }
    }
    let stroke = if selected {
        Stroke::new(2.0, theme::accent())
    } else if response.hovered() {
        Stroke::new(1.0, theme::accent())
    } else {
        Stroke::new(1.0, theme::border())
    };
    painter.rect_stroke(picture, radius, stroke, StrokeKind::Inside);
    let color = if selected { theme::accent_text() } else { theme::text() };
    let mut job = egui::text::LayoutJob::simple_singleline(label.to_owned(), FontId::proportional(12.0), color);
    job.wrap = egui::text::TextWrapping::truncate_at_width((side - 4.0).max(1.0));
    let galley = painter.layout_job(job);
    painter.galley(egui::pos2(rect.left() + 2.0, picture.bottom() + (TILE_CAPTION - galley.size().y) / 2.0), galley, color);
    response
}

/// What `tile` shows given how the pointer stands on it, and the repaint the picture needs: only a
/// hovered tile that animates asks for one, `next_in` seconds ahead.
fn tile_view(ctx: &egui::Context, now: f64, kind: PackageKind, package: &str, tile: &TileState, hover: Hover, previews: &RefCell<PreviewManager>) -> TileView {
    let elapsed = match hover {
        Hover::Idle => return TileView::still(),
        Hover::Waiting { remaining } => {
            ctx.request_repaint_after(Duration::from_secs_f64(remaining));
            return TileView::still();
        }
        Hover::Playing { elapsed } => elapsed,
    };
    if !matches!(tile, TileState::Still(_)) {
        return TileView::still();
    }
    // Each borrow ends with its statement.
    let animation = previews.borrow_mut().animation(package);
    let (Step { shown, next_in }, frames) = match &animation {
        AnimationState::Frames(frames) => (loop_step(kind, frames.frames, elapsed), Some(frames)),
        AnimationState::Static => (static_step(elapsed), None),
        AnimationState::Rendering => {
            ctx.request_repaint_after(Duration::from_secs_f64(BUSY_REPAINT));
            return TileView { image: TileImage::Still, busy: Some(busy_phase(now)) };
        }
        AnimationState::Unavailable => return TileView::still(),
    };
    ctx.request_repaint_after(Duration::from_secs_f64(next_in));
    let image = match (shown, frames) {
        (Shown::Frame(index), Some(frames)) => TileImage::Frame(frames.clone(), index),
        (Shown::Original, _) => previews.borrow().original().map_or(TileImage::Still, TileImage::Original),
        _ => TileImage::Still,
    };
    TileView { image, busy: None }
}

fn picker_list(ui: &mut egui::Ui, state: &mut EditorState) {
    let language = state.language();
    let duplicates = duplicated_names(state.catalog.entries(state.picker.kind));
    let entries: Vec<_> = filter_entries(state.catalog.entries(state.picker.kind), &state.picker.filter).into_iter().cloned().collect();
    let width = (ui.available_width() - ui.spacing().scroll.allocated_width()).max(1.0);
    let (columns, side) = picker_grid(width);
    let list_height = (ui.available_height() - PICKER_FOOTER_HEIGHT).max(TILE_MIN);
    let mut double_clicked = None;
    let mut visible: Vec<&str> = Vec::new();
    let kind = state.picker.kind;
    let ctx = ui.ctx().clone();
    let now = ctx.input(|input| input.time);
    state.ui.tile_hover.begin_frame();
    egui::ScrollArea::vertical().id_salt("package-list").max_height(list_height).auto_shrink([false, false]).show_rows(ui, side + TILE_CAPTION, entries.len().div_ceil(columns), |ui, rows| {
        let shown = visible_tiles(rows.clone(), columns, entries.len());
        visible.extend(entries[shown].iter().map(|entry| entry.package.as_str()));
        for row in rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = TILE_GAP;
                for entry in &entries[row_tiles(row, columns, entries.len())] {
                    let selected = state.picker.selected.as_deref() == Some(&entry.package);
                    // The borrow ends here: nothing below holds it across the drawing.
                    let tile = state.previews.borrow_mut().tile(&entry.package);
                    let label = list_label(entry, &duplicates);
                    let (previews, tile_hover) = (&state.previews, &mut state.ui.tile_hover);
                    let view = |hovered: bool| tile_view(&ctx, now, kind, &entry.package, &tile, tile_hover.observe(&entry.package, hovered, now), previews);
                    let response = package_tile(ui, side, &label, selected, &tile, view).on_hover_text(tile_tooltip(&label, entry, language, &tile));
                    if response.clicked() {
                        state.picker.selected = Some(entry.package.clone());
                    }
                    if response.double_clicked() {
                        double_clicked = Some(entry.package.clone());
                    }
                }
            });
        }
    });
    state.ui.tile_hover.end_frame();
    // Only a tile that has been hovered long enough asks for its loop.
    let playing = state.ui.tile_hover.playing().map(str::to_owned);
    state.previews.borrow_mut().set_wanted(playing.as_deref(), visible.iter().copied());
    if let Some(package) = double_clicked {
        add_package(state, &package);
    }
}

fn picker_problems(ui: &mut egui::Ui, state: &EditorState) {
    if state.catalog.problems.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(state.language().t(Message::PackageProblems(state.catalog.problems.len()))).id_salt("package-problems").show(ui, |ui| {
        for problem in &state.catalog.problems {
            ui.colored_label(theme::refused(), format!("{}: {}", short_name(&problem.package), problem.message));
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_sticker_tab_uses_its_own_words_and_not_the_audio_ones() {
        assert_eq!(super::empty_sticker_message(false, true), super::Message::ReadingStickerPacks);
        assert_eq!(super::empty_sticker_message(true, true), super::Message::StickerLibraryEmpty);
        assert_eq!(super::empty_sticker_message(true, false), super::Message::NoSearchMatch);
    }

    use super::*;

    #[test]
    fn only_the_rows_in_view_are_drawn() {
        // 100 rows of 10 starting at y = 50; the view shows y = 200..300.
        assert_eq!(visible_rows(50.0, 200.0, 300.0, 10.0, 100), 15..25);
        assert_eq!(visible_rows(50.0, 205.0, 295.0, 10.0, 100), 15..25, "a row cut by the edge is drawn");
        assert_eq!(visible_rows(50.0, -500.0, 100.0, 10.0, 100), 0..5, "above the list");
        assert_eq!(visible_rows(50.0, 900.0, 2000.0, 10.0, 100), 85..100, "the end of the list");
        assert_eq!(visible_rows(50.0, 5000.0, 6000.0, 10.0, 100), 100..100, "scrolled past the end");
        assert_eq!(visible_rows(50.0, 0.0, 30.0, 10.0, 100), 0..0, "all of the list is below the view");
        assert_eq!(visible_rows(50.0, 0.0, 1000.0, 10.0, 3), 0..3);
    }

    #[test]
    fn a_sound_tooltip_names_its_tags_and_source() {
        let sound = LibrarySound { source: "asset:sfx/p/a.flac".to_owned(), name: "Boom".to_owned(), duration: None, tags: vec!["hit".to_owned(), "low".to_owned()], category: "impact".to_owned(), looped: false };
        let text = sound_tooltip(&sound);
        assert!(text.contains("Boom") && text.contains("hit, low") && text.contains("impact") && text.ends_with("asset:sfx/p/a.flac"), "{text}");
    }

    #[test]
    fn a_poster_fits_its_tile_keeping_its_shape() {
        assert_eq!(fit_size(Vec2::new(160.0, 90.0), 16.0 / 9.0), Vec2::new(160.0, 90.0));
        assert_eq!(fit_size(Vec2::new(160.0, 90.0), 1.0), Vec2::new(90.0, 90.0));
        assert_eq!(fit_size(Vec2::new(90.0, 90.0), 2.0), Vec2::new(90.0, 45.0));
    }

    #[test]
    fn a_tab_label_shrinks_only_when_it_does_not_fit() {
        assert_eq!(tab_font_size(40.0, 100.0), TAB_FONT);
        let fitted = tab_font_size(70.0, 60.0);
        assert!(fitted < TAB_FONT && fitted >= TAB_FONT_MIN, "{fitted}");
        assert_eq!(tab_font_size(500.0, 60.0), TAB_FONT_MIN);
    }

    #[test]
    fn the_rail_cells_keep_their_height_unless_the_panel_is_too_short() {
        assert_eq!(rail_cell_height(600.0, 4), RAIL_CELL_HEIGHT);
        assert_eq!(rail_cell_height(4.0 * RAIL_CELL_HEIGHT, 4), RAIL_CELL_HEIGHT);
        let short = rail_cell_height(120.0, 4);
        assert_eq!(short, 30.0);
        assert!(short * 4.0 <= 120.0);
    }

    #[test]
    fn an_empty_grid_says_whether_a_filter_hides_the_files() {
        assert_eq!(empty_message("  "), Message::LibraryEmpty);
        assert_eq!(empty_message("beach"), Message::NoSearchMatch);
    }

    fn entry(package: &str, name: &str, description: &str) -> PackageEntry {
        PackageEntry { package: package.to_owned(), name: name.to_owned(), description: description.to_owned(), kind: PackageKind::Effect, params: Vec::new() }
    }

    #[test]
    fn the_picker_grid_fills_the_width_with_tiles_of_at_least_the_minimum_side() {
        let (columns, side) = picker_grid(400.0);
        assert_eq!(columns, 3);
        assert!((side * 3.0 + TILE_GAP * 2.0 - 400.0).abs() < 0.01);
        assert!(side >= TILE_MIN);
        assert_eq!(picker_grid(TILE_MIN).0, 1);
        let (columns, side) = picker_grid(40.0);
        assert_eq!((columns, side), (1, 40.0), "a narrow panel still has one tile");
        assert!(picker_grid(0.0).1 >= 1.0);
    }

    #[test]
    fn a_grid_row_holds_the_next_tiles_and_the_last_row_may_be_short() {
        assert_eq!(row_tiles(0, 3, 7), 0..3);
        assert_eq!(row_tiles(2, 3, 7), 6..7);
        assert_eq!(row_tiles(3, 3, 7), 7..7, "past the last row there is nothing");
        assert_eq!(row_tiles(0, 3, 0), 0..0);
    }

    #[test]
    fn only_the_tiles_of_the_visible_rows_are_visible() {
        assert_eq!(visible_tiles(1..3, 3, 20), 3..9);
        assert_eq!(visible_tiles(5..7, 3, 20), 15..20);
        assert_eq!(visible_tiles(7..9, 3, 20), 20..20);
        assert_eq!(visible_tiles(0..0, 3, 20), 0..0);
        assert_eq!(visible_tiles(0..4, 3, 0), 0..0);
    }

    #[test]
    fn a_tile_shows_a_picture_only_for_a_still_and_a_warning_only_for_a_failure() {
        assert_eq!(tile_look(&TileState::Pending), TileLook::Placeholder);
        assert_eq!(tile_look(&TileState::Loading), TileLook::Placeholder);
        assert_eq!(tile_look(&TileState::Unavailable("no".to_owned())), TileLook::Placeholder);
        assert_eq!(tile_look(&TileState::Failed("no".to_owned())), TileLook::Broken);
    }

    #[test]
    fn the_tooltip_has_the_full_name_the_description_and_the_reason_of_a_missing_preview() {
        let described = entry("packages/gpufx/ffmpeg/blur", "Blur", "Softens the picture");
        assert_eq!(tile_tooltip("Blur", &described, Language::English, &TileState::Pending), "Blur
Softens the picture");
        let bare = entry("packages/gpufx/ffmpeg/blur", "Blur", "");
        assert_eq!(tile_tooltip("Blur", &bare, Language::English, &TileState::Loading), "Blur
packages/gpufx/ffmpeg/blur");
        let failed = tile_tooltip("Blur (ffmpeg)", &described, Language::English, &TileState::Failed("the shader did not compile".to_owned()));
        assert!(failed.starts_with("Blur (ffmpeg)
Softens the picture
") && failed.ends_with("the shader did not compile"), "{failed}");
        let unavailable = tile_tooltip("Blur", &described, Language::Vietnamese, &TileState::Unavailable("no helper".to_owned()));
        assert!(unavailable.ends_with("no helper") && unavailable.lines().count() == 3, "{unavailable}");
    }
}
