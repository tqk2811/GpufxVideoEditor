//! The tracking parts of the inspector: the Tracking section of a video clip (pick an object, run
//! the tracker, choose, smooth or delete tracks, add a censor that follows, lock the frame to the object), the follow section
//! of an overlay, and the region of a blur or pixelate effect.

use super::numeric::{add_slider, drag_field};
use super::theme;
use crate::editor::EditorState;
use crate::i18n::{Language, Message};
use crate::tracking::Direction;
use engine::{ClipKey, EffectRegion, MotionTrack, RegionShape, MAX_FOLLOW_SMOOTH, MAX_LOCK_ZOOM, MIN_LOCK_ZOOM};

/// The region a blur or pixelate effect takes when it is first limited: a third of the picture
/// in the middle, soft edged.
const DEFAULT_REGION: EffectRegion = EffectRegion { shape: RegionShape::Rect, cx: 0.5, cy: 0.5, w: 0.3, h: 0.3, angle: 0.0, feather: 0.2, link: None };

/// A labelled slider with a number field. Whether it changed.
fn number_row(ui: &mut egui::Ui, label: &str, value: &mut f64, range: std::ops::RangeInclusive<f64>, decimals: usize, suffix: &str) -> bool {
    ui.horizontal(|ui| {
        ui.colored_label(theme::text_dim(), label);
        ui.spacing_mut().slider_width = (ui.available_width() - 66.0).max(40.0);
        let mut changed = add_slider(ui, egui::Slider::new(value, range.clone()).show_value(false).clamping(egui::SliderClamping::Edits)).changed();
        let speed = (range.end() - range.start()) / 200.0;
        changed |= drag_field(ui, value, range, speed, decimals, suffix, None).1;
        changed
    })
    .inner
}

fn track_label(language: Language, track: &MotionTrack) -> String {
    language.t(Message::TrackName { id: track.id, count: track.samples.len() })
}

/// The Tracking section of video clip `key`.
pub(super) fn tracking_rows(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, source: &str) {
    let language = state.language();
    ui.colored_label(theme::text_dim(), language.t(Message::FieldTracking));
    let picking = state.track_pick() == Some(key);
    let label = language.t(if picking { Message::TrackPicking } else { Message::TrackPick });
    if ui.selectable_label(picking, label).on_hover_text(language.t(Message::TrackPickHint)).clicked() {
        state.toggle_track_pick(key);
    }
    let tracks: Vec<MotionTrack> = state.motion_tracks_of(source).into_iter().cloned().collect();
    let Some(current) = state.motion_track_for(key).cloned() else {
        ui.colored_label(theme::text_dim(), language.t(Message::TrackNone));
        return;
    };
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("motion-track", key)).selected_text(track_label(language, &current)).show_ui(ui, |ui| {
            for track in &tracks {
                if ui.selectable_label(track.id == current.id, track_label(language, track)).clicked() {
                    state.choose_motion_track(Some(track.id));
                }
            }
        });
        if ui.small_button(language.t(Message::TrackDelete)).clicked() {
            state.delete_motion_track(current.id);
        }
    });
    match state.track_progress(current.id) {
        Some(progress) => {
            ui.horizontal(|ui| {
                let text = language.t(Message::TrackRunning((progress * 100.0).round() as u32));
                ui.add(egui::ProgressBar::new(progress).desired_width((ui.available_width() - 70.0).max(40.0)).text(text));
                if ui.button(language.t(Message::TrackStop)).clicked() {
                    state.stop_track_run(current.id);
                }
            });
        }
        None => {
            ui.horizontal_wrapped(|ui| {
                if ui.button(language.t(Message::TrackBackward)).clicked() {
                    state.ask_track_run(key, current.id, Direction::Backward, None);
                }
                if ui.button(language.t(Message::TrackOneFrame)).clicked() {
                    state.ask_track_run(key, current.id, Direction::Forward, Some(1));
                }
                if ui.button(language.t(Message::TrackForward)).clicked() {
                    state.ask_track_run(key, current.id, Direction::Forward, None);
                }
            });
        }
    }
    let mut smooth = crate::track_edit::track_smooth(state.project(), current.id);
    if number_row(ui, &language.t(Message::TrackSmooth), &mut smooth, 0.0..=MAX_FOLLOW_SMOOTH, 2, " s") {
        state.live_track_smooth(current.id, smooth.clamp(0.0, MAX_FOLLOW_SMOOTH));
    }
    if ui.button(language.t(Message::AddCensorFollowing)).clicked() {
        state.add_censor_following(current.id);
    }
    lock_rows(ui, state, key, current.id);

    ui.add(egui::Label::new(egui::RichText::new(language.t(Message::TrackHintFix)).color(theme::text_dim()).small()).wrap());
}

/// Locking the frame of video clip `key` to the object: a button that locks it to motion track
/// `track_id` at the playhead, or the options of the lock it has.
fn lock_rows(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, track_id: u32) {
    let language = state.language();
    let Some(clip) = state.project().tracks.get(key.track).and_then(|track| track.clips.get(key.clip)) else { return };
    let Some(lock) = clip.lock else {
        if ui.button(language.t(Message::LockToObject)).clicked() {
            state.lock_to_track(key, track_id);
        }
        return;
    };
    ui.colored_label(theme::text_dim(), language.t(Message::LockedToTrack(lock.link.track_id)));
    let (mut scale, mut rotation) = (lock.link.follow_scale, lock.link.follow_rotation);
    let changed = ui.checkbox(&mut scale, language.t(Message::LockScale)).changed() | ui.checkbox(&mut rotation, language.t(Message::LockRotation)).changed();
    if changed {
        state.set_lock_options(key, |lock| {
            lock.link.follow_scale = scale;
            lock.link.follow_rotation = rotation;
        });
    }
    let mut zoom = lock.zoom;
    if number_row(ui, &language.t(Message::LockZoom), &mut zoom, MIN_LOCK_ZOOM..=MAX_LOCK_ZOOM, 2, "\u{d7}") {
        state.live_lock_zoom(key, zoom);
    }
    ui.horizontal(|ui| {
        if ui.button(language.t(Message::FollowBake)).clicked() {
            state.bake_lock(key);
        }
        if ui.button(language.t(Message::LockClear)).clicked() {
            state.clear_lock(key);
        }
    });
}

/// The follow section of overlay `key`: which track it follows and how, or a picker to start.

pub(super) fn follow_rows(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, clip: &engine::Clip) {
    let language = state.language();
    let tracks: Vec<MotionTrack> = state.project().motion_tracks.clone();
    match clip.follow {
        None => {
            ui.add_enabled_ui(!tracks.is_empty(), |ui| {
                ui.menu_button(language.t(Message::FollowTrack), |ui| {
                    for track in &tracks {
                        let name = std::path::Path::new(&track.media_path).file_name().map_or_else(|| track.media_path.clone(), |name| name.to_string_lossy().into_owned());
                        if ui.button(format!("{} \u{2014} {name}", track_label(language, track))).clicked() {
                            state.follow_motion_track(key, track.id);
                            ui.close();
                        }
                    }
                })
                .response
                .on_disabled_hover_text(language.t(Message::TrackNone));
            });
        }
        Some(follow) => {
            ui.colored_label(theme::text_dim(), language.t(Message::FollowingTrack(follow.link.track_id)));
            let (mut scale, mut rotation) = (follow.link.follow_scale, follow.link.follow_rotation);
            let changed = ui.checkbox(&mut scale, language.t(Message::FollowScale)).changed() | ui.checkbox(&mut rotation, language.t(Message::FollowRotation)).changed();
            if changed {
                state.set_follow_options(key, scale, rotation);
            }
            ui.horizontal(|ui| {
                if ui.button(language.t(Message::FollowBake)).clicked() {
                    state.bake_follow(key);
                }
                if ui.button(language.t(Message::FollowClear)).clicked() {
                    state.clear_follow(key);
                }
            });
        }
    }
}

/// The region of effect `index` of video clip `key`, a blur or pixelate: off, or its shape, box,
/// soft edge and the track it moves with.
pub(super) fn region_rows(ui: &mut egui::Ui, state: &mut EditorState, key: ClipKey, index: usize, region: Option<EffectRegion>, source: &str) {
    let language = state.language();
    let mut limited = region.is_some();
    if ui.checkbox(&mut limited, language.t(Message::EffectRegion)).changed() {
        state.set_effect_region(index, limited.then_some(DEFAULT_REGION));
    }
    let Some(region) = region else { return };
    ui.horizontal(|ui| {
        for (shape, label) in [(RegionShape::Rect, Message::CensorShapeRect), (RegionShape::Ellipse, Message::CensorShapeEllipse)] {
            if ui.selectable_label(region.shape == shape, language.t(label)).clicked() && region.shape != shape {
                state.set_effect_region(index, Some(EffectRegion { shape, ..region }));
            }
        }
    });
    let mut next = region;
    let mut changed = number_row(ui, &language.t(Message::RegionCenterX), &mut next.cx, 0.0..=1.0, 3, "");
    changed |= number_row(ui, &language.t(Message::RegionCenterY), &mut next.cy, 0.0..=1.0, 3, "");
    changed |= number_row(ui, &language.t(Message::RegionWidth), &mut next.w, 0.01..=1.0, 3, "");
    changed |= number_row(ui, &language.t(Message::RegionHeight), &mut next.h, 0.01..=1.0, 3, "");
    let mut feather = next.feather * 100.0;
    if number_row(ui, &language.t(Message::CensorFeather), &mut feather, 0.0..=100.0, 0, "%") {
        next.feather = (feather / 100.0).clamp(0.0, 1.0);
        changed = true;
    }
    if changed {
        state.live_effect_region(index, next);
    }
    let tracks: Vec<MotionTrack> = state.motion_tracks_of(source).into_iter().cloned().collect();
    let linked = region.link.map(|link| link.track_id);
    let shown = tracks.iter().find(|track| Some(track.id) == linked).map_or_else(|| language.t(Message::RegionLinkNone), |track| track_label(language, track));
    egui::ComboBox::from_id_salt(("region-link", key, index)).selected_text(shown).show_ui(ui, |ui| {
        if ui.selectable_label(linked.is_none(), language.t(Message::RegionLinkNone)).clicked() && linked.is_some() {
            state.link_effect_region(index, None);
        }
        for track in &tracks {
            if ui.selectable_label(linked == Some(track.id), track_label(language, track)).clicked() && linked != Some(track.id) {
                state.link_effect_region(index, Some(track.id));
            }
        }
    });
}
