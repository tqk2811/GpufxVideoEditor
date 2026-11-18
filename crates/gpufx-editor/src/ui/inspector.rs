//! The panel on the right: the project figures, or everything about the selected clip.

use super::icons::{self, Icon};
use super::numeric::{self, drag_field, speed_field};
use super::param_widgets;
use super::{theme, UiRequest};
use crate::editor::{clip_name, speed_limits, tempo_text, transition_duration_limits, ClipField, ClipTab, EditorState, PictureTab};
use crate::i18n::{Language, Message};
use crate::packages::short_name;
use crate::timecode::{aspect_label, format_fps, format_timecode, from_seconds, to_seconds};
use crate::timeline::hit::TransitionKey;
use crate::timeline::view::content_end;
use egui::{Rect, Stroke, UiBuilder, Vec2};
use engine::{ClipKey, ClipProperty, Easing, Keyframe, KeyframeTarget, Time, Transition, MAX_PLACEMENT_OFFSET, MAX_PLACEMENT_WIDTH, MAX_ROTATION, MIN_PLACEMENT_WIDTH};
use serde_json::{Map, Value};

const HEADER_HEIGHT: f32 = 34.0;
const RESET_BUTTON_SIDE: f32 = 20.0;
/// As far up as a project may go; validation sets no floor.
const GAIN_RANGE: std::ops::RangeInclusive<f64> = -60.0..=60.0;
const SPEED_RANGE: std::ops::RangeInclusive<f64> = engine::MIN_SPEED..=engine::MAX_SPEED;
/// The speed slider takes 1x for anything within this ratio of it, so going back to exactly 1x
/// does not need a steady hand: one pixel of the slider is about 2.9%.
const SPEED_SNAP_RATIO: f64 = 1.04;
/// How far the pointer must go from where it was pressed before the slider counts as dragged.
const SLIDER_MOVE_POINTS: f32 = 2.0;

pub fn show(ui: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let rect = ui.max_rect();
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), HEADER_HEIGHT.min(rect.height())));
    let key = state.selection().filter(|key| clip_of(state, *key).is_some());
    follow_clip(ui.ctx(), state, key);
    let transition = if key.is_none() { state.selected_transition() } else { None };
    let title = state.language().t(if let Some(key) = key {
        clip_title(state.project().tracks[key.track].kind, clip_of(state, key))
    } else if transition.is_some() {
        Message::TransitionTitle
    } else {
        Message::ProjectTitle
    });
    ui.painter_at(rect).text(header.left_center() + Vec2::new(12.0, 0.0), egui::Align2::LEFT_CENTER, title, egui::FontId::proportional(14.0), theme::accent_text());
    ui.painter_at(rect).hline(rect.x_range(), header.bottom(), Stroke::new(1.0, theme::border()));
    let body = Rect::from_min_max(egui::pos2(rect.left(), header.bottom()), rect.max).shrink2(Vec2::new(10.0, 8.0));
    if body.width() < 1.0 || body.height() < 1.0 {
        return;
    }
    let effects_tab = key.filter(|key| shows_effects_tab(state, *key));
    if effects_tab.is_none() {
        state.ui.effect_tile_hover.clear();
    }
    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
        // The Effects tab scrolls its own lists: its grid fills the height that is left.
        if let Some(key) = effects_tab {
            clip_header(ui, state, key);
            effects_tab_contents(ui, state, key, requests);
            return;
        }
        egui::ScrollArea::vertical().id_salt("inspector").auto_shrink([false, false]).show(ui, |ui| match (key, transition) {
            (Some(key), _) => clip_inspector(ui, state, key),
            (None, Some(transition)) => transition_inspector(ui, state, transition),
            (None, None) => project_summary(ui, state),
        });
    });
}

/// The inspector title for a clip on a track of `kind`: what the clip is, not just "Clip".
fn clip_title(kind: engine::TrackKind, clip: Option<&engine::Clip>) -> Message {
    let source = clip.map_or("", |clip| clip.source.as_str());
    if clip.is_some_and(engine::Clip::is_censor) {
        Message::ClipTitleCensor
    } else if clip.is_some_and(engine::Clip::is_text) || kind == engine::TrackKind::Text {
        Message::ClipTitleText
    } else if kind == engine::TrackKind::Audio {
        Message::ClipTitleAudio
    } else if engine::is_sticker_source(source) {
        Message::ClipTitleSticker
    } else if engine::is_image_source(source) {
        Message::ClipTitleImage
    } else {
        Message::ClipTitleVideo
    }
}

/// Notices that the inspector shows another clip, or the same one elsewhere. The expression
/// drafts are about the clip they were typed for, so they go, and the status says so when that
/// loses text.
fn follow_clip(ctx: &egui::Context, state: &mut EditorState, key: Option<ClipKey>) {
    let shown = key.and_then(|key| clip_of(state, key).map(|clip| clip_identity(key, clip)));
    if state.ui.inspector_clip != shown {
        state.ui.inspector_clip = shown;
        if param_widgets::drop_expression_drafts(ctx) {
            state.report_info(Message::ExpressionDropped);
        }
    }
}

/// A slider that leaves an existing value outside its range alone until the user moves it.
fn slider<'a>(value: &'a mut f64, range: std::ops::RangeInclusive<f64>) -> egui::Slider<'a> {
    egui::Slider::new(value, range).show_value(false).clamping(egui::SliderClamping::Edits)
}

/// What tells clips apart in the id of a widget. The clip index alone would hand the state of the
/// widget, such as text being typed, to whichever clip gets that index or selection next.
fn clip_identity(key: ClipKey, clip: &engine::Clip) -> (ClipKey, String, i64) {
    (key, clip.source.clone(), clip.start.ticks())
}

fn clip_of(state: &EditorState, key: ClipKey) -> Option<&engine::Clip> {
    state.project().tracks.get(key.track)?.clips.get(key.clip)
}

fn field(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.monospace(value));
    });
}

fn project_summary(ui: &mut egui::Ui, state: &EditorState) {
    let language = state.language();
    let output = &state.project().output;
    field(ui, &language.t(Message::FieldSize), &format!("{}\u{d7}{}", output.width, output.height));
    field(ui, &language.t(Message::FieldFrameRate), &format_fps(output.frame_rate));
    field(ui, &language.t(Message::FieldAspect), &aspect_label(output.width, output.height));
    field(ui, &language.t(Message::FieldDuration), &format_timecode(content_end(state.project()), output.frame_rate));
    ui.add_space(8.0);
    ui.separator();
    ui.add(egui::Label::new(egui::RichText::new(language.t(Message::SelectClipHint)).color(theme::text_dim())).wrap());
}

/// The tabs `clip` on a track of `kind` has: the general one, the picture one when it shows a
/// picture and the sound one when it has a sound.
fn clip_tabs(kind: engine::TrackKind, clip: &engine::Clip) -> Vec<ClipTab> {
    let mut tabs = vec![ClipTab::General];
    if super::animation_inspector::shows_animation(kind) {
        tabs.push(ClipTab::Picture);
    }
    // A still, an image or text, has no sound.
    if !clip.is_still() {
        tabs.push(ClipTab::Sound);
    }
    tabs
}

/// The tab shown for the chosen `tab`: the general one when the clip has no such tab.
fn shown_clip_tab(tab: ClipTab, tabs: &[ClipTab]) -> ClipTab {
    if tabs.contains(&tab) { tab } else { ClipTab::General }
}

/// The label of a clip tab. The general tab is named after what the clip is, as the title is,
/// except on a sound clip, whose sound tab already has that name.
fn clip_tab_label(tab: ClipTab, kind: engine::TrackKind, clip: &engine::Clip) -> Message {
    match tab {
        ClipTab::General if kind == engine::TrackKind::Audio => Message::ClipTabGeneral,
        ClipTab::General => clip_title(kind, Some(clip)),
        ClipTab::Picture => Message::ClipTabPicture,
        ClipTab::Sound => Message::TabAudio,
    }
}

/// The clip's name, the locked note and the tabs. Returns the tab shown.
fn clip_header(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey) -> ClipTab {
    let language = state.language();
    let Some(clip) = clip_of(state, key).cloned() else { return ClipTab::General };
    let kind = state.project().tracks[key.track].kind;
    ui.add(egui::Label::new(egui::RichText::new(clip_name(&clip)).strong()).truncate());
    if state.is_track_locked(key.track) {
        ui.add(egui::Label::new(egui::RichText::new(language.t(Message::TrackLockedNote)).color(theme::text_dim())).wrap());
    }
    let tabs = clip_tabs(kind, &clip);
    let mut shown = shown_clip_tab(state.ui.clip_tab, &tabs);
    ui.horizontal_wrapped(|ui| {
        for tab in tabs {
            if ui.selectable_value(&mut shown, tab, language.t(clip_tab_label(tab, kind, &clip))).clicked() {
                state.ui.clip_tab = tab;
            }
        }
    });
    if shown == ClipTab::Picture {
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut state.ui.picture_tab, PictureTab::Properties, language.t(Message::ClipTabProperties));
            ui.selectable_value(&mut state.ui.picture_tab, PictureTab::Animation, language.t(Message::TabAnimation));
            ui.selectable_value(&mut state.ui.picture_tab, PictureTab::Effects, language.t(Message::TabEffects));
        });
    }
    ui.separator();
    shown
}

/// Whether the clip at `key` shows its Effects tab now, which scrolls its own lists.
fn shows_effects_tab(state: &EditorState, key: ClipKey) -> bool {
    let Some(clip) = clip_of(state, key) else { return false };
    let tabs = clip_tabs(state.project().tracks[key.track].kind, clip);
    shown_clip_tab(state.ui.clip_tab, &tabs) == ClipTab::Picture && state.ui.picture_tab == PictureTab::Effects
}

/// The Effects tab: the effects on the clip, then the list to add more from, which fills the rest.
fn effects_tab_contents(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, requests: &mut Vec<UiRequest>) {
    let editable = !state.is_track_locked(key.track);
    // The effects on the clip take at most this share of the height, so the list stays usable.
    let attached_height = (ui.available_height() * 0.45).max(60.0);
    egui::ScrollArea::vertical().id_salt("clip-effects").max_height(attached_height).auto_shrink([false, true]).show(ui, |ui| effects(ui, state, key, editable));
    ui.separator();
    super::left_panel::picker(ui, state, requests, super::left_panel::PickerFor::Effects, editable);
}

fn clip_inspector(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey) {
    let Some(clip) = clip_of(state, key).cloned() else { return };
    let editable = !state.is_track_locked(key.track);
    match clip_header(ui, state, key) {
        ClipTab::General => {
            ui.add_enabled_ui(editable, |ui| general_rows(ui, state, key, &clip));
        }
        ClipTab::Sound => {
            ui.add_enabled_ui(editable, |ui| sound_rows(ui, state, key, &clip));
        }
        ClipTab::Picture => match state.ui.picture_tab {
            PictureTab::Properties => picture_rows(ui, state, key, &clip, editable),
            PictureTab::Animation => {
                let id = ui.scope_id().with(("animation-inspector", clip_identity(key, &clip)));
                ui.add_enabled_ui(editable, |ui| super::animation_inspector::show(ui, state, id, &clip));
            }
            // Drawn by `show`, outside the scrolling card.
            PictureTab::Effects => {}
        },
    }
}

/// The general tab: where the clip sits and how much of its source it plays, how fast.
fn general_rows(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, clip: &engine::Clip) {
    let language = state.language();
    // A still has no source to walk through.
    if clip.is_still() {
        time_field(ui, state, &language.t(Message::FieldVideoDuration), to_seconds(clip.duration), ClipField::Duration, None);
        time_field(ui, state, &language.t(Message::FieldStart), to_seconds(clip.start), ClipField::Start, None);
        return;
    }
    let full_source = state.source_remaining(key).filter(|left| clip.timeline_ticks(*left) != clip.media_duration());
    time_field(ui, state, &language.t(Message::FieldSourceIn), to_seconds(clip.source_in), ClipField::SourceIn, Some((clip.source_in != Time::ZERO).then_some(ClipField::SourceIn(0.0))));
    time_field(ui, state, &language.t(Message::FieldVideoDuration), to_seconds(clip.source_offset(clip.media_duration())), ClipField::Duration, Some(full_source.map(|source| ClipField::Duration(to_seconds(source)))));
    ui.separator();

    time_field(ui, state, &language.t(Message::FieldStart), to_seconds(clip.start), ClipField::Start, None);
    speed_row(ui, state, key, &language.t(Message::FieldSpeed), clip.speed);
    let mut reverse = clip.reverse;
    if ui.checkbox(&mut reverse, language.t(Message::Reverse)).changed() {
        state.set_reverse(reverse);
    }
}

/// The sound tab: volume, fades, noise, voice and beats.
fn sound_rows(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, clip: &engine::Clip) {
    let language = state.language();
    let mut gain = clip.gain_db;
    ui.colored_label(theme::text_dim(), language.t(Message::FieldVolume));
    let changed = ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
        let mut changed = numeric::add_slider(ui, slider(&mut gain, GAIN_RANGE)).changed();
        changed |= drag_field(ui, &mut gain, GAIN_RANGE, 0.1, 1, " dB", None).1;
        changed
    });
    if changed.inner {
        state.live_gain(gain);
    }
    let mut muted = clip.muted;
    if ui.checkbox(&mut muted, language.t(Message::Mute)).changed() {
        state.set_muted(muted);
    }

    let (has_before, has_after) = transitions_beside(state, key);
    // The sound, and so its fades, is only the moving part between the holds.
    fade_row(ui, state, &language.t(Message::FadeIn), clip.fade_in, clip.media_duration().saturating_sub(clip.fade_out), !has_before, |state, value| state.live_fades(value, clip.fade_out));
    fade_row(ui, state, &language.t(Message::FadeOut), clip.fade_out, clip.media_duration().saturating_sub(clip.fade_in), !has_after, |state, value| state.live_fades(clip.fade_in, value));
    ui.separator();
    denoise_rows(ui, state, &clip.denoise);
    ui.separator();
    voice_rows(ui, state, &clip.voice);
    ui.separator();
    beats_rows(ui, state, key, clip);
}

/// The picture's properties: the text and font of a text clip, then place, motion and the
/// background removal.
fn picture_rows(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, clip: &engine::Clip, editable: bool) {
    let language = state.language();
    if let Some(text) = &clip.text {
        let id = ui.scope_id().with(("text-inspector", clip_identity(key, clip)));
        ui.add_enabled_ui(editable, |ui| super::text_inspector::show(ui, state, key, id, text));
        ui.separator();
    }
    if let Some(censor) = &clip.censor {
        ui.add_enabled_ui(editable, |ui| censor_rows(ui, state, censor));
        ui.separator();
    }
    ui.add_enabled_ui(editable, |ui| {
        placement_row(ui, state, &language.t(Message::FieldPlacement), clip.placement.is_some() || ClipProperty::ALL.into_iter().any(|property| property.is_placement() && clip.motion.has_keyframes(property)));
        motion_rows(ui, state, key, clip);
    });
    if clip.text.is_none() && clip.censor.is_none() {
        ui.separator();
        ui.add_enabled_ui(editable, |ui| matte_rows(ui, state, &clip.matte));
    }
}

/// How a censor region hides: its shape, the softness of its edge, and blur, blocks or a colour
/// with its strength.
fn censor_rows(ui: &mut egui::Ui, state: &mut EditorState, censor: &engine::CensorContent) {
    let language = state.language();
    ui.colored_label(theme::text_dim(), language.t(Message::CensorShape));
    ui.horizontal(|ui| {
        for (shape, label) in [(engine::RegionShape::Rect, Message::CensorShapeRect), (engine::RegionShape::Ellipse, Message::CensorShapeEllipse)] {
            if ui.selectable_label(censor.shape == shape, language.t(label)).clicked() && censor.shape != shape {
                state.set_censor(engine::CensorContent { shape, ..*censor });
            }
        }
    });
    let mut feather = censor.feather * 100.0;
    let changed = ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), language.t(Message::CensorFeather));
        ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
        let mut changed = numeric::add_slider(ui, slider(&mut feather, 0.0..=100.0)).changed();
        changed |= drag_field(ui, &mut feather, 0.0..=100.0, 1.0, 0, " %", None).1;
        changed
    });
    if changed.inner {
        state.live_censor(engine::CensorContent { feather: (feather / 100.0).clamp(0.0, 1.0), ..*censor });
    }
    ui.colored_label(theme::text_dim(), language.t(Message::CensorMode));
    ui.horizontal(|ui| {
        let modes = [
            (engine::CensorMode::Blur { radius: 30.0 }, Message::CensorBlur),
            (engine::CensorMode::Pixelate { block: 24.0 }, Message::CensorPixelate),
            (engine::CensorMode::Solid { color: [0, 0, 0, 255] }, Message::CensorSolid),
        ];
        for (mode, label) in modes {
            let same = std::mem::discriminant(&mode) == std::mem::discriminant(&censor.mode);
            if ui.selectable_label(same, language.t(label)).clicked() && !same {
                state.set_censor(engine::CensorContent { mode, ..*censor });
            }
        }
    });
    match censor.mode {
        engine::CensorMode::Blur { radius } => {
            let mut value = radius;
            if strength_row(ui, &language.t(Message::CensorStrength), &mut value, 0.0..=engine::MAX_CENSOR_BLUR) {
                state.live_censor(engine::CensorContent { mode: engine::CensorMode::Blur { radius: value }, ..*censor });
            }
        }
        engine::CensorMode::Pixelate { block } => {
            let mut value = block;
            if strength_row(ui, &language.t(Message::CensorBlock), &mut value, 1.0..=engine::MAX_CENSOR_BLOCK) {
                state.live_censor(engine::CensorContent { mode: engine::CensorMode::Pixelate { block: value }, ..*censor });
            }
        }
        engine::CensorMode::Solid { color } => {
            let mut picked = egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
            ui.horizontal(|ui| {
                ui.colored_label(theme::text_dim(), language.t(Message::CensorColor));
                if egui::color_picker::color_edit_button_srgba(ui, &mut picked, egui::color_picker::Alpha::OnlyBlend).changed() {
                    state.live_censor(engine::CensorContent { mode: engine::CensorMode::Solid { color: picked.to_srgba_unmultiplied() }, ..*censor });
                }
            });
        }
    }
}

/// A labelled slider with a number field, in pixels of a 1080-line frame. Whether it changed.
fn strength_row(ui: &mut egui::Ui, label: &str, value: &mut f64, range: std::ops::RangeInclusive<f64>) -> bool {
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), label);
        ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
        let mut changed = numeric::add_slider(ui, slider(value, range.clone())).changed();
        changed |= drag_field(ui, value, range, 1.0, 0, " px", None).1;
        changed
    })
    .inner
}

/// The AI background removal of the clip: the model, whether it is installed, and the shaping.
fn matte_rows(ui: &mut egui::Ui, state: &mut EditorState, matte: &engine::ClipMatte) {
    let language = state.language();
    ui.colored_label(theme::text_dim(), language.t(Message::FieldMatte));
    ui.horizontal(|ui| {
        for (model, label, hint) in [
            (engine::MatteModel::Off, Message::MatteOff, None),
            (engine::MatteModel::Quality, Message::MatteQuality, Some(Message::MatteQualityHint)),
            (engine::MatteModel::Fast, Message::MatteFast, Some(Message::MatteFastHint)),
        ] {
            let mut response = ui.selectable_label(matte.model == model, language.t(label));
            if let Some(hint) = hint {
                response = response.on_hover_text(language.t(hint));
            }
            if response.clicked() && matte.model != model {
                state.set_matte_model(model);
            }
        }
    });
    if matte.is_off() {
        return;
    }
    if crate::matting_models::installed(matte.model, &crate::project_file::package_roots()).is_none() {
        ui.colored_label(theme::text_dim(), language.t(Message::MatteModelMissing));
        let downloads = crate::matting_models::downloads();
        match downloads.status(matte.model) {
            Some(crate::matting_models::DownloadStatus::Running(progress)) => {
                let (received, total) = progress.snapshot();
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("{} {}", language.t(Message::MatteFetching), crate::matting_models::format_progress(received, total)));
                });
                ui.horizontal(|ui| {
                    let fraction = total.map_or(0.0, |total| received.min(total) as f32 / total.max(1) as f32);
                    ui.add(egui::ProgressBar::new(fraction).desired_width((ui.available_width() - 80.0).max(40.0)));
                    if ui.button(language.t(Message::MatteFetchCancel)).clicked() {
                        downloads.cancel(matte.model);
                    }
                });
                // The download thread does not wake the window: look again soon.
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            }
            status => {
                let failed = match status {
                    Some(crate::matting_models::DownloadStatus::Failed(problem)) => {
                        ui.colored_label(theme::refused(), language.t(Message::MatteImportFailed(problem)));
                        true
                    }
                    _ => false,
                };
                ui.horizontal(|ui| {
                    let source = crate::matting_models::ModelSource::of(matte.model);
                    let label = if failed { Message::MatteFetchRetry } else { Message::MatteFetch };
                    if ui.add_enabled(source.enabled(), egui::Button::new(language.t(label))).on_disabled_hover_text(language.t(Message::MatteFetchUnavailable)).clicked() {
                        state.ui.matte_fetch_request = Some(matte.model);
                    }
                    if ui.button(language.t(Message::MatteImportZip)).clicked() {
                        state.ui.matte_import_request = Some(matte.model);
                    }
                });
            }
        }
    }
    let max = engine::MAX_MATTE_PIXELS;
    let mut next = *matte;
    let mut changed = false;
    for (label, value, range, step, decimals, suffix) in [
        (Message::MatteThreshold, &mut next.threshold, 0.0..=1.0, 0.01, 2, ""),
        (Message::MatteFeather, &mut next.feather, 0.0..=max, 0.1, 1, " px"),
        (Message::MatteEdge, &mut next.edge, -max..=max, 0.1, 1, " px"),
    ] {
        ui.colored_label(theme::text_dim(), language.t(label));
        changed |= ui
            .horizontal(|ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
                let mut changed = numeric::add_slider(ui, slider(value, range.clone())).changed();
                changed |= drag_field(ui, value, range, step, decimals, suffix, None).1;
                changed
            })
            .inner;
    }
    let mut temporal = f64::from(next.temporal);
    ui.colored_label(theme::text_dim(), language.t(Message::MatteTemporal)).on_hover_text(language.t(Message::MatteTemporalHint));
    changed |= ui
        .horizontal(|ui| {
            let range = 0.0..=f64::from(engine::MAX_MATTE_TEMPORAL);
            ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
            let mut changed = numeric::add_slider(ui, slider(&mut temporal, range.clone()).step_by(1.0)).changed();
            changed |= drag_field(ui, &mut temporal, range, 1.0, 0, "", None).1;
            changed
        })
        .inner;
    next.temporal = temporal.round().clamp(0.0, f64::from(engine::MAX_MATTE_TEMPORAL)) as u32;
    if changed {
        state.live_matte(next);
    }
    let mut invert = matte.invert;
    if ui.checkbox(&mut invert, language.t(Message::MatteInvert)).changed() {
        state.set_matte(engine::ClipMatte { invert, ..*matte });
    }
}

/// The noise reduction of the clip: a method and, when one is chosen, its strength in percent.
fn denoise_rows(ui: &mut egui::Ui, state: &mut EditorState, denoise: &engine::ClipDenoise) {
    let language = state.language();
    ui.colored_label(theme::text_dim(), language.t(Message::FieldDenoise));
    ui.horizontal(|ui| {
        for (mode, label, hint) in [
            (engine::DenoiseMode::Off, Message::DenoiseOff, None),
            (engine::DenoiseMode::Voice, Message::DenoiseVoice, Some(Message::DenoiseVoiceHint)),
            (engine::DenoiseMode::General, Message::DenoiseGeneral, Some(Message::DenoiseGeneralHint)),
        ] {
            let mut response = ui.selectable_label(denoise.mode == mode, language.t(label));
            if let Some(hint) = hint {
                response = response.on_hover_text(language.t(hint));
            }
            if response.clicked() && denoise.mode != mode {
                state.set_denoise_mode(mode);
            }
        }
    });
    if denoise.mode == engine::DenoiseMode::Off {
        return;
    }
    let mut percent = denoise.strength * 100.0;
    let changed = ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), language.t(Message::DenoiseStrength));
        ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
        let mut changed = numeric::add_slider(ui, slider(&mut percent, 0.0..=100.0)).changed();
        changed |= drag_field(ui, &mut percent, 0.0..=100.0, 1.0, 0, " %", None).1;
        changed
    });
    if changed.inner {
        state.live_denoise_strength((percent / 100.0).clamp(0.0, 1.0));
    }
}

/// The voice change of the clip: presets, the pitch in semitones and the robot effect.
fn voice_rows(ui: &mut egui::Ui, state: &mut EditorState, voice: &engine::ClipVoice) {
    let language = state.language();
    ui.colored_label(theme::text_dim(), language.t(Message::FieldVoiceChange));
    let current = engine::VoicePreset::of(voice);
    ui.horizontal_wrapped(|ui| {
        for preset in engine::VoicePreset::ALL {
            let label = match preset {
                engine::VoicePreset::Normal => Message::VoiceNormal,
                engine::VoicePreset::Chipmunk => Message::VoiceChipmunk,
                engine::VoicePreset::Deep => Message::VoiceDeep,
                engine::VoicePreset::Robot => Message::VoiceRobot,
            };
            if ui.selectable_label(current == Some(preset), language.t(label)).clicked() && current != Some(preset) {
                state.apply_voice_preset(preset);
            }
        }
    });
    let range = -engine::MAX_PITCH_SEMITONES..=engine::MAX_PITCH_SEMITONES;
    let mut semitones = voice.semitones;
    ui.colored_label(theme::text_dim(), language.t(Message::FieldPitch));
    let changed = ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
        let mut changed = numeric::add_slider(ui, slider(&mut semitones, range.clone()).step_by(0.1)).changed();
        changed |= drag_field(ui, &mut semitones, range, 0.05, 1, "", None).1;
        changed
    });
    if changed.inner {
        state.live_pitch(semitones);
    }
    let mut robot = voice.effect == engine::VoiceEffect::Robot;
    if ui.checkbox(&mut robot, language.t(Message::VoiceRobot)).changed() {
        state.set_voice_effect(if robot { engine::VoiceEffect::Robot } else { engine::VoiceEffect::None });
    }
}

/// The beats of the clip: how many it plays and their tempo, a button to detect them (a progress
/// bar and Cancel while that runs) and one to clear them.
fn beats_rows(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, clip: &engine::Clip) {
    let language = state.language();
    ui.colored_label(theme::text_dim(), language.t(Message::FieldBeats));
    if let Some(progress) = state.beat_progress(key) {
        ui.horizontal(|ui| {
            let text = language.t(Message::BeatsDetecting((progress * 100.0).round() as u32));
            ui.add(egui::ProgressBar::new(progress).desired_width((ui.available_width() - 70.0).max(40.0)).text(text));
            if ui.button(language.t(Message::CancelBeats)).clicked() {
                state.cancel_beats(key);
            }
        });
        return;
    }
    let found = state.project().clip_beats(clip).map(|beats| (clip.beat_times(&beats.beats).count(), beats.tempo));
    let summary = match found {
        None => Message::BeatsNone,
        Some((count, tempo)) => Message::BeatsSummary { count, tempo: tempo_text(tempo) },
    };
    ui.label(language.t(summary));
    ui.horizontal(|ui| {
        if ui.button(language.t(Message::DetectBeats)).on_hover_text(language.t(Message::DetectBeatsHint)).clicked() {
            state.detect_beats(key);
        }
        if found.is_some() && ui.button(language.t(Message::ClearBeats)).clicked() {
            state.clear_beats(key);
        }
    });
}

fn transitions_beside(state: &EditorState, key: ClipKey) -> (bool, bool) {
    let transitions = &state.project().tracks[key.track].transitions;
    (key.clip > 0 && transitions.iter().any(|tr| tr.after_clip + 1 == key.clip), transitions.iter().any(|tr| tr.after_clip == key.clip))
}

/// A time of the clip. `reset` is `None` for a field with no reset button; otherwise what the
/// button applies, or `None` inside when it has nothing to do: the value is the default already,
/// or the default is not known.
fn time_field(ui: &mut egui::Ui, state: &mut EditorState, label: &str, actual: f64, make: fn(f64) -> ClipField, reset: Option<Option<ClipField>>) {
    let pending = state.ui.clip_field.filter(|field| std::mem::discriminant(field) == std::mem::discriminant(&make(0.0)));
    let mut value = match pending {
        Some(ClipField::Start(value) | ClipField::SourceIn(value) | ClipField::Duration(value)) => value,
        None => actual,
    };
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (response, changed) = drag_field(ui, &mut value, 0.0..=f64::MAX, 0.02, 3, " s", None);
            if changed && !state.hold_cancelled() {
                state.ui.clip_field = Some(make(value));
            }
            if response.drag_stopped() || (changed && !response.dragged()) {
                if let Some(field) = state.ui.clip_field.take() {
                    state.apply_clip_field(field);
                }
            }
            if let Some(reset) = reset {
                let tooltip = state.language().t(Message::Reset);
                let clicked = ui.add_enabled_ui(reset.is_some() && !state.is_holding(), |ui| icons::button_of_size(ui, Icon::Undo, &tooltip, false, RESET_BUTTON_SIDE).clicked()).inner;
                if let Some(field) = reset.filter(|_| clicked) {
                    state.apply_clip_field(field);
                }
            }
        });
    });
}

/// The place of a video clip in the frame, which the red frame on the player changes. The button
/// puts the clip back where it fits the frame, and is off when it already does.
fn placement_row(ui: &mut egui::Ui, state: &mut EditorState, label: &str, placed: bool) {
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let tooltip = state.language().t(Message::Reset);
            let clicked = ui.add_enabled_ui(placed && !state.is_holding(), |ui| icons::button_of_size(ui, Icon::Undo, &tooltip, false, RESET_BUTTON_SIDE).clicked()).inner;
            if clicked {
                state.reset_placement();
            }
        });
    });
}

/// One row of the place, opacity or rotation of a clip.
struct MotionRow {
    label: Message,
    range: std::ops::RangeInclusive<f64>,
    speed: f64,
    suffix: &'static str,
}

fn motion_row_of(property: ClipProperty) -> MotionRow {
    match property {
        ClipProperty::X => MotionRow { label: Message::FieldPositionX, range: -MAX_PLACEMENT_OFFSET..=MAX_PLACEMENT_OFFSET, speed: 0.005, suffix: "" },
        ClipProperty::Y => MotionRow { label: Message::FieldPositionY, range: -MAX_PLACEMENT_OFFSET..=MAX_PLACEMENT_OFFSET, speed: 0.005, suffix: "" },
        ClipProperty::Width => MotionRow { label: Message::FieldWidth, range: MIN_PLACEMENT_WIDTH..=MAX_PLACEMENT_WIDTH, speed: 0.005, suffix: "" },
        ClipProperty::Opacity => MotionRow { label: Message::FieldOpacity, range: 0.0..=1.0, speed: 0.005, suffix: "" },
        ClipProperty::Rotation => MotionRow { label: Message::FieldRotation, range: -MAX_ROTATION..=MAX_ROTATION, speed: 0.5, suffix: "\u{b0}" },
    }
}

/// Each animatable number of the clip: its value at the playhead, and a diamond for its keyframe.
fn motion_rows(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, clip: &engine::Clip) {
    let offset = state.playhead_offset(key);
    for property in ClipProperty::ALL {
        let row = motion_row_of(property);
        let Some(mut value) = state.property_shown(key, property) else { return };
        let target = KeyframeTarget::Property(property);
        let keys = clip.keyframes_of(&target).unwrap_or_default();
        let keyed = !keys.is_empty();
        let on_keyframe = state.keyframe_at_playhead(key, &target);
        let (before, after) = state.keyframe_neighbors_of(key, &target);
        // Until the probe says what shape the picture is, the place of the clip would be made from
        // the shape of the output.
        let blocked = property.is_placement() && !state.placement_ready(key);
        let language = state.language();
        let mut toggled = false;
        let mut jump = None;
        ui.horizontal(|ui| {
            ui.colored_label(theme::text_dim(), language.t(row.label.clone()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (previous_tooltip, next_tooltip) = (language.t(Message::KeyframePrevious), language.t(Message::KeyframeNext));
                if ui.add_enabled_ui(after.is_some(), |ui| icons::keyframe_step_button(ui, true, &next_tooltip).clicked()).inner {
                    jump = after;
                }
                let tooltip = language.t(if on_keyframe.is_some() { Message::KeyframeRemove } else { Message::KeyframeAdd });
                toggled = ui.add_enabled_ui(offset.is_some() && !blocked, |ui| icons::keyframe_button(ui, on_keyframe.is_some(), keyed, &tooltip).clicked()).inner;
                if ui.add_enabled_ui(before.is_some(), |ui| icons::keyframe_step_button(ui, false, &previous_tooltip).clicked()).inner {
                    jump = before;
                }
                // A value with keyframes can only be set where the playhead gives it a time.
                ui.add_enabled_ui((offset.is_some() || !keyed) && !blocked, |ui| {
                    if drag_field(ui, &mut value, row.range.clone(), row.speed, 3, row.suffix, None).1 {
                        state.live_motion_value(key, property, value);
                    }
                });
            });
        });
        if toggled {
            state.toggle_keyframe(key, target.clone(), value);
        }
        if let Some(at) = jump {
            state.jump_to_keyframe(key, at);
        }
        if let Some(keyframe) = on_keyframe {
            easing_row(ui, state, language, key, target, keyframe);
        }
    }
}

/// How the keyframe under the playhead moves on to the next one.
fn easing_row(ui: &mut egui::Ui, state: &mut EditorState, language: Language, key: ClipKey, target: KeyframeTarget, keyframe: Keyframe) {
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), language.t(Message::FieldEasing));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut chosen = None;
            egui::ComboBox::from_id_salt(("easing", &target)).selected_text(language.t(easing_label(keyframe.easing))).show_ui(ui, |ui| {
                for easing in Easing::ALL {
                    if ui.selectable_label(easing == keyframe.easing, language.t(easing_label(easing))).clicked() {
                        chosen = Some(easing);
                        ui.close();
                    }
                }
            });
            if let Some(easing) = chosen {
                state.set_keyframe_easing(key, target.clone(), easing);
            }
        });
    });
}

fn easing_label(easing: Easing) -> Message {
    match easing {
        Easing::Linear => Message::EasingLinear,
        Easing::EaseIn => Message::EasingEaseIn,
        Easing::EaseOut => Message::EasingEaseOut,
        Easing::EaseInOut => Message::EasingEaseInOut,
    }
}

/// The speed of the clip: a logarithmic slider and a number shown as the factor. Goes through the
/// live edit path, so a drag is one undo step and Esc restores the clip.
fn speed_row(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, label: &str, speed: f64) {
    let mut value = speed;
    ui.colored_label(theme::text_dim(), label);
    let changeable = speed_limits(state.project(), key).is_some();
    ui.add_enabled_ui(changeable, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
            let response = numeric::add_slider(ui, slider(&mut value, SPEED_RANGE).logarithmic(true));
            // The frame of the release sets a value too, so it counts as part of the press.
            let pressed = response.interact_pointer_pos().is_some();
            let mut applied = slider_speed(response.changed(), pressed, pointer_moved(ui, response.id, pressed), value);
            // A press alone must not show the value under the pointer either.
            value = applied.unwrap_or(speed);
            if speed_field(ui, &mut value, SPEED_RANGE).1 {
                applied = Some(value);
            }
            if let Some(next) = applied {
                state.live_speed(next);
            }
        });
    });
    if !changeable {
        ui.add(egui::Label::new(egui::RichText::new(state.language().t(Message::SpeedBlockedByEndTransition)).color(theme::text_dim())).wrap());
    }
}

/// What the speed slider sets this frame, if anything. A press alone is not a change: pressing
/// moves the slider under the pointer, and on a slider of this scale one pixel is 2.9% of the
/// length of the clip. The keyboard changes it without a press.
fn slider_speed(changed: bool, pressed: bool, moved: bool, value: f64) -> Option<f64> {
    (changed && (!pressed || moved)).then(|| if pressed { snap_speed(value) } else { value })
}

/// `value`, or exactly 1x when it is close to it.
fn snap_speed(value: f64) -> f64 {
    if (value / SPEED_SNAP_RATIO..=value * SPEED_SNAP_RATIO).contains(&1.0) {
        1.0
    } else {
        value
    }
}

/// Whether the pointer pressed on widget `id` has gone anywhere since the press, and stays true
/// until the press ends, however the pointer comes back.
fn pointer_moved(ui: &egui::Ui, id: egui::Id, pressed: bool) -> bool {
    let now = ui.input(|input| input.pointer.press_origin().zip(input.pointer.latest_pos()).is_some_and(|(origin, latest)| origin.distance(latest) >= SLIDER_MOVE_POINTS));
    ui.data_mut(|data| {
        let moved = data.get_temp_mut_or(id, false);
        *moved = (*moved || now) && pressed;
        *moved
    })
}

fn fade_row(ui: &mut egui::Ui, state: &mut EditorState, label: &str, value: Time, longest: Time, enabled: bool, mut apply: impl FnMut(&mut EditorState, Time)) {
    ui.colored_label(theme::text_dim(), label);
    let mut seconds = to_seconds(value);
    let max = to_seconds(longest).max(0.0);
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
            let mut changed = numeric::add_slider(ui, slider(&mut seconds, 0.0..=max)).changed();
            changed |= drag_field(ui, &mut seconds, 0.0..=max, 0.02, 2, " s", None).1;
            if changed {
                apply(state, from_seconds(seconds));
            }
        });
    });
}

fn effects(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, editable: bool) {
    let Some(clip) = clip_of(state, key).cloned() else { return };
    let language = state.language();
    ui.strong(language.t(Message::TabEffects));
    if clip.effects.is_empty() {
        ui.add(egui::Label::new(egui::RichText::new(language.t(Message::NoEffectsYet)).color(theme::text_dim())).wrap());
    }
    let mut remove = None;
    for (index, effect) in clip.effects.iter().enumerate() {
        ui.push_id(("effect", clip_identity(key, &clip), index, &effect.package), |ui| {
            // The header stays live: a locked clip can still be looked through.
            egui::CollapsingHeader::new(short_name(&effect.package)).default_open(true).show(ui, |ui| {
                ui.add_enabled_ui(editable, |ui| {
                    ui.horizontal(|ui| {
                        if ui.small_button(language.t(Message::Remove)).clicked() {
                            remove = Some(index);
                        }
                        if ui.small_button(language.t(Message::Reset)).clicked() {
                            state.reset_effect(index);
                        }
                    });
                    match state.catalog.params_of(&effect.package).map(<[d3d12_backend::ParamInfo]>::to_vec) {
                        Some(infos) => {
                            let offset = state.playhead_offset(key);
                            let shown = state.effect_params_shown(key, index).unwrap_or_default();
                            let mut keyframes = param_widgets::RowKeyframes {
                                keyframes: &effect.keyframes,
                                offset,
                                position: state.playhead_position(key).unwrap_or(Time::ZERO),
                                duration: clip.duration,
                                tolerance: state.keyframe_tolerance(),
                                toggled: None,
                                jump: None,
                            };
                            if let Some(next) = param_widgets::rows(ui, ui.scope_id(), language, &infos, &shown, Some(&mut keyframes)) {
                                state.live_effect_params_shown(index, next);
                            }
                            if let Some((param, value)) = keyframes.toggled.take() {
                                state.toggle_keyframe(key, KeyframeTarget::EffectParam { effect: index, param }, value);
                            }
                            if let Some(at) = keyframes.jump.take() {
                                state.jump_to_keyframe(key, at);
                            }
                        }
                        None => {
                            ui.colored_label(theme::text_dim(), language.t(Message::PackageParamsUnreadable));
                        }
                    }
                });
            });
        });
    }
    if let Some(index) = remove {
        state.remove_effect_at(index);
    }
}

/// The card of the selected transition: its package, duration and parameters, and a button that removes it.
fn transition_inspector(ui: &mut egui::Ui, state: &mut EditorState, key: TransitionKey) {
    let language = state.language();
    let Some(transition) = state.project().tracks[key.track].transitions.iter().find(|tr| tr.after_clip == key.after_clip).cloned() else { return };
    let editable = !state.is_track_locked(key.track);
    ui.add(egui::Label::new(egui::RichText::new(short_name(&transition.package)).strong()).truncate());
    if !editable {
        ui.add(egui::Label::new(egui::RichText::new(language.t(Message::TrackLockedNote)).color(theme::text_dim())).wrap());
    }
    let first = state.project().tracks[key.track].clips.get(key.after_clip).map(|clip| clip_identity(ClipKey { track: key.track, clip: key.after_clip }, clip));
    let mut remove = None;
    ui.push_id(("transition", first, key.after_clip, &transition.package), |ui| {
        ui.add_enabled_ui(editable, |ui| transition_controls(ui, state, key.track, &transition, &mut remove, language));
    });
    if let Some(after_clip) = remove {
        state.remove_transition_after(key.track, after_clip);
    }
}

fn transition_controls(ui: &mut egui::Ui, state: &mut EditorState, track: usize, transition: &Transition, remove: &mut Option<usize>, language: crate::i18n::Language) {
    if ui.small_button(language.t(Message::Remove)).clicked() {
        *remove = Some(transition.after_clip);
    }
    if let Some((shortest, longest)) = transition_duration_limits(state.project(), track, transition.after_clip) {
        let mut seconds = to_seconds(transition.duration);
        let (low, high) = (to_seconds(shortest), to_seconds(longest));
        ui.colored_label(theme::text_dim(), language.t(Message::FieldDuration));
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
            let mut changed = numeric::add_slider(ui, slider(&mut seconds, low..=high)).changed();
            changed |= drag_field(ui, &mut seconds, low..=high, 0.02, 2, " s", None).1;
            if changed {
                state.live_transition(track, transition.after_clip, transition.params.clone(), from_seconds(seconds));
            }
        });
    }
    let infos: Option<Vec<d3d12_backend::ParamInfo>> = state.catalog.params_of(&transition.package).map(<[_]>::to_vec);
    let params: Map<String, Value> = transition.params.clone();
    if let Some(infos) = infos {
        if let Some(next) = param_widgets::rows(ui, ui.scope_id(), language, &infos, &params, None) {
            state.live_transition(track, transition.after_clip, next, transition.duration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{new_clip, Status};
    use std::path::Path;

    fn frame(ctx: &egui::Context, state: &mut EditorState) {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| show(ui, state, &mut Vec::new()));
        output.textures_delta.clear();
    }

    fn selected_clip() -> EditorState {
        let mut state = EditorState::default();
        state.add_track(engine::TrackKind::Video);
        state.add_clip_from_file(0, Path::new("a.mp4"));
        state.select(Some(ClipKey { track: 0, clip: 0 }));
        state
    }

    /// Every text one pass of the inspector draws.
    fn drawn_texts(ctx: &egui::Context, state: &mut EditorState) -> Vec<String> {
        let output = ctx.run_ui(egui::RawInput::default(), |ui| show(ui, state, &mut Vec::new()));
        fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect(shape, texts)),
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut texts);
        }
        texts
    }

    #[test]
    fn a_video_clip_splits_its_card_into_general_picture_and_sound_tabs() {
        let ctx = egui::Context::default();
        let mut state = selected_clip();
        let language = state.language();
        let (enter, volume, speed, search) = (language.t(Message::AnimationEnter), language.t(Message::FieldVolume), language.t(Message::FieldSpeed), language.t(Message::SearchEffects));
        frame(&ctx, &mut state);
        let general = drawn_texts(&ctx, &mut state);
        for tab in [Message::ClipTitleVideo, Message::ClipTabPicture, Message::TabAudio] {
            assert!(general.contains(&language.t(tab)), "{general:?}");
        }
        assert!(general.contains(&speed) && !general.contains(&volume) && !general.contains(&language.t(Message::TabAnimation)), "{general:?}");
        state.ui.clip_tab = ClipTab::Sound;
        frame(&ctx, &mut state);
        let sound = drawn_texts(&ctx, &mut state);
        assert!(sound.contains(&volume) && !sound.contains(&speed), "{sound:?}");
        state.ui.clip_tab = ClipTab::Picture;
        frame(&ctx, &mut state);
        let picture = drawn_texts(&ctx, &mut state);
        for tab in [Message::ClipTabProperties, Message::TabAnimation, Message::TabEffects] {
            assert!(picture.contains(&language.t(tab)), "{picture:?}");
        }
        state.ui.picture_tab = PictureTab::Animation;
        frame(&ctx, &mut state);
        let animation = drawn_texts(&ctx, &mut state);
        assert!(animation.contains(&enter) && !animation.contains(&volume), "{animation:?}");
        state.ui.picture_tab = PictureTab::Effects;
        frame(&ctx, &mut state);
        let effects = drawn_texts(&ctx, &mut state);
        assert!(effects.contains(&search) && effects.contains(&language.t(Message::ImportPackage)) && !effects.contains(&enter), "{effects:?}");
    }

    #[test]
    fn a_sound_clip_has_no_picture_tab_and_a_still_no_sound_tab() {
        let ctx = egui::Context::default();
        let mut state = EditorState::default();
        state.add_track(engine::TrackKind::Audio);
        state.add_clip_from_file(0, Path::new("a.mp3"));
        state.select(Some(ClipKey { track: 0, clip: 0 }));
        frame(&ctx, &mut state);
        let texts = drawn_texts(&ctx, &mut state);
        let language = state.language();
        assert!(!texts.contains(&language.t(Message::ClipTabPicture)) && texts.contains(&language.t(Message::ClipTabGeneral)), "{texts:?}");
        let mut state = EditorState::default();
        state.add_track(engine::TrackKind::Video);
        state.add_clip_from_file(0, Path::new("a.png"));
        state.select(Some(ClipKey { track: 0, clip: 0 }));
        frame(&ctx, &mut state);
        let texts = drawn_texts(&ctx, &mut state);
        assert!(texts.contains(&language.t(Message::ClipTabPicture)) && !texts.contains(&language.t(Message::TabAudio)), "{texts:?}");
    }

    #[test]
    fn a_chosen_tab_the_clip_lacks_shows_the_general_tab_and_stays_chosen() {
        let tabs = [ClipTab::General, ClipTab::Picture];
        assert_eq!(shown_clip_tab(ClipTab::Sound, &tabs), ClipTab::General);
        assert_eq!(shown_clip_tab(ClipTab::Picture, &tabs), ClipTab::Picture);
        let ctx = egui::Context::default();
        let mut state = selected_clip();
        state.ui.clip_tab = ClipTab::Sound;
        state.add_track(engine::TrackKind::Video);
        state.add_clip_from_file(1, Path::new("b.png"));
        state.select(Some(ClipKey { track: 1, clip: 0 }));
        frame(&ctx, &mut state);
        assert_eq!(state.ui.clip_tab, ClipTab::Sound, "an image keeps the choice for the next video");
    }

    fn typed_draft(ctx: &egui::Context) {
        param_widgets::store_test_draft(ctx, "1+x", "x");
    }

    #[test]
    fn a_speed_slider_change_waits_for_the_pointer_to_move() {
        assert_eq!(slider_speed(true, true, false, 2.0), None, "a press alone");
        assert_eq!(slider_speed(true, true, true, 2.0), Some(2.0), "a real drag");
        assert_eq!(slider_speed(false, true, true, 2.0), None, "nothing changed");
        assert_eq!(slider_speed(true, false, false, 2.0), Some(2.0), "the keyboard");
        assert_eq!(slider_speed(true, false, false, 1.029), Some(1.029), "an arrow key leaves 1x");
        assert_eq!(slider_speed(true, true, true, 1.01), Some(1.0), "a drag snaps to 1x");
    }

    #[test]
    fn the_speed_slider_snaps_back_to_exactly_one() {
        for near in [1.0, 1.02, 0.97, 1.039] {
            assert_eq!(snap_speed(near), 1.0, "{near}");
        }
        for far in [1.05, 0.95, 2.0, 0.1] {
            assert_eq!(snap_speed(far), far, "{far}");
        }
    }

    /// One pass of the speed row alone, so a point on the row is a point on its slider.
    fn speed_pass(ctx: &egui::Context, state: &mut EditorState, key: ClipKey, events: Vec<egui::Event>) {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 120.0));
        let speed = state.project().tracks[key.track].clips[key.clip].speed;
        let raw = egui::RawInput { events, screen_rect: Some(screen), ..Default::default() };
        let mut output = ctx.run_ui(raw, |ui| speed_row(ui, state, key, "Speed", speed));
        output.textures_delta.clear();
    }

    fn button(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    #[test]
    fn pressing_the_speed_slider_without_moving_changes_nothing_and_a_drag_does() {
        let ctx = egui::Context::default();
        let mut state = selected_clip();
        let key = ClipKey { track: 0, clip: 0 };
        let before = state.project().clone();
        speed_pass(&ctx, &mut state, key, vec![]);
        speed_pass(&ctx, &mut state, key, vec![]);
        for y in (2..60).step_by(4) {
            let at = egui::pos2(30.0, y as f32);
            speed_pass(&ctx, &mut state, key, vec![egui::Event::PointerMoved(at)]);
            speed_pass(&ctx, &mut state, key, vec![button(at, true)]);
            speed_pass(&ctx, &mut state, key, vec![button(at, false)]);
            speed_pass(&ctx, &mut state, key, vec![]);
        }
        assert_eq!(state.project(), &before, "the slider only jumped under a press");

        let mut moved = false;
        for y in (2..60).step_by(4) {
            let at = egui::pos2(30.0, y as f32);
            speed_pass(&ctx, &mut state, key, vec![egui::Event::PointerMoved(at)]);
            speed_pass(&ctx, &mut state, key, vec![button(at, true)]);
            speed_pass(&ctx, &mut state, key, vec![egui::Event::PointerMoved(egui::pos2(160.0, y as f32))]);
            moved |= state.project().tracks[0].clips[0].speed != 1.0;
            speed_pass(&ctx, &mut state, key, vec![button(egui::pos2(160.0, y as f32), false)]);
            speed_pass(&ctx, &mut state, key, vec![]);
            state.cancel_live();
            if moved {
                break;
            }
        }
        assert!(moved, "dragging the slider does set a speed");
    }

    #[test]
    fn moving_the_clip_drops_a_draft_and_says_so() {
        let ctx = egui::Context::default();
        let mut state = selected_clip();
        frame(&ctx, &mut state);
        typed_draft(&ctx);
        frame(&ctx, &mut state);
        assert!(param_widgets::has_test_draft(&ctx), "nothing about the clip changed");

        let key = ClipKey { track: 0, clip: 0 };
        state.finish_drag(crate::timeline::drag::EditCommand::Move { key, track: 0, desired_start: Time::from_ticks(90_000) });
        typed_draft(&ctx);
        frame(&ctx, &mut state);
        assert!(!param_widgets::has_test_draft(&ctx), "no stale draft stays in memory");
        assert_eq!(state.status(), &Status::Info(Message::ExpressionDropped));
    }

    #[test]
    fn a_clip_on_a_locked_track_can_be_inspected_but_a_click_changes_nothing() {
        let ctx = egui::Context::default();
        let mut state = selected_clip();
        state.toggle_track_locked(0);
        let before = state.project().clone();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 500.0));
        let mut pass = |events: Vec<egui::Event>| {
            let raw = egui::RawInput { events, screen_rect: Some(screen), ..Default::default() };
            let mut output = ctx.run_ui(raw, |ui| show(ui, &mut state, &mut Vec::new()));
            output.textures_delta.clear();
        };
        let button = |at, pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        pass(vec![]);
        // Down the left of the panel, over the time fields, the sliders and the mute box.
        for y in (40..440).step_by(6) {
            let at = egui::pos2(24.0, y as f32);
            pass(vec![egui::Event::PointerMoved(at)]);
            pass(vec![button(at, true)]);
            pass(vec![button(at, false)]);
        }
        assert_eq!(state.selection(), Some(ClipKey { track: 0, clip: 0 }), "it stays selected");
        assert!(!matches!(state.status(), Status::Refused(_)), "the controls are off, not refused: {:?}", state.status());
        assert_eq!(state.project(), &before);
    }

    #[test]
    fn a_locked_clip_still_imports_packages_from_the_effects_tab() {
        let ctx = egui::Context::default();
        let mut state = selected_clip();
        state.toggle_track_locked(0);
        state.ui.clip_tab = ClipTab::Picture;
        state.ui.picture_tab = PictureTab::Effects;
        let label = state.language().t(Message::ImportPackage);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 500.0));
        let mut pass = |events: Vec<egui::Event>| {
            let mut requests = Vec::new();
            let raw = egui::RawInput { events, screen_rect: Some(screen), ..Default::default() };
            let mut output = ctx.run_ui(raw, |ui| show(ui, &mut state, &mut requests));
            output.textures_delta.clear();
            (output, requests)
        };
        fn find(shape: &egui::Shape, label: &str, found: &mut Option<egui::Pos2>) {
            match shape {
                egui::Shape::Text(text) if text.galley.text() == label => *found = Some(text.visual_bounding_rect().center()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| find(shape, label, found)),
                _ => {}
            }
        }
        let (output, _) = pass(vec![]);
        let mut at = None;
        output.shapes.iter().for_each(|clipped| find(&clipped.shape, &label, &mut at));
        let at = at.expect("the import button is drawn");
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        pass(vec![egui::Event::PointerMoved(at)]);
        pass(vec![button(true)]);
        let (_, requests) = pass(vec![button(false)]);
        assert!(requests.iter().any(|request| matches!(request, UiRequest::ImportPackage)), "{requests:?}");
    }

    #[test]
    fn selecting_nothing_drops_a_draft() {
        let ctx = egui::Context::default();
        let mut state = selected_clip();
        frame(&ctx, &mut state);
        typed_draft(&ctx);
        state.select(None);
        frame(&ctx, &mut state);
        assert!(!param_widgets::has_test_draft(&ctx));
    }

    #[test]
    fn another_clip_at_the_same_index_has_another_identity() {
        let key = ClipKey { track: 0, clip: 1 };
        let a = new_clip("a.mp4", Time::ZERO, Time::from_ticks(9_000));
        let b = new_clip("b.mp4", Time::ZERO, Time::from_ticks(9_000));
        assert_ne!(clip_identity(key, &a), clip_identity(key, &b));
        let moved = new_clip("a.mp4", Time::from_ticks(3_000), Time::from_ticks(9_000));
        assert_ne!(clip_identity(key, &a), clip_identity(key, &moved));
    }
}
