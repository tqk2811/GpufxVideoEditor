//! Draws the editor and forwards input. Decisions live in `editor`, `params`, `media`, `layout` and
//! `timeline`.

mod icons;
mod inspector;
mod left_panel;
mod numeric;
mod param_widgets;
mod player;
mod theme;
mod timeline_panel;
mod top_bar;

use crate::editor::{EditorState, Status};
use crate::i18n::{Language, Message};
use crate::layout::{panels, Panels};
use crate::presenter::PreviewTexture;
use egui::{CornerRadius, Event, Key, Modifiers, Rect, Stroke, StrokeKind, UiBuilder};

/// Work the window does after the frame, outside the UI pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiRequest {
    PickClipFile { track: usize },
    ImportMedia { audio_only: bool },
    NewProject,
    OpenProject,
    SaveProject,
    SaveProjectAs,
    Export,
    CancelExport,
    ToggleFullscreen,
    SetLanguage(Language),
}

pub fn show(root: &mut egui::Ui, state: &mut EditorState, requests: &mut Vec<UiRequest>, preview: Option<PreviewTexture>) {
    theme::apply(root.ctx());
    state.visuals.borrow_mut().begin_frame(root.ctx());
    {
        let mut visuals = state.visuals.borrow_mut();
        visuals.set_paused(state.is_playing() || state.is_exporting());
        visuals.sync_project(state.revision(), state.project());
    }
    handle_shortcuts(root.ctx(), state, requests);
    let screen = root.max_rect();
    let Panels { top_bar, left, center, right, timeline, status } = panels(screen.size());
    root.painter().rect_filled(screen, CornerRadius::ZERO, theme::BACKGROUND);
    let enabled = !state.is_exporting();
    in_rect(root, "top-bar", top_bar, |ui| top_bar::show(ui, state, requests));
    panel(root, "left-panel", left, |ui| {
        ui.add_enabled_ui(enabled, |ui| left_panel::show(ui, state, requests));
    });
    panel(root, "player-panel", center, |ui| player::show(ui, state, requests, preview));
    panel(root, "right-panel", right, |ui| {
        ui.add_enabled_ui(enabled, |ui| inspector::show(ui, state));
    });
    panel(root, "timeline-panel", timeline, |ui| {
        ui.add_enabled_ui(enabled, |ui| timeline_panel::show(ui, state, requests));
    });
    in_rect(root, "status-line", status, |ui| status_line(ui, state));
    // A release ends a held control wherever the pointer went, and a click or key press is
    // finished in the frame that made it.
    if !root.ctx().input(|input| input.pointer.any_down()) {
        state.pointer_released();
    }
    state.visuals.borrow_mut().end_frame();
}

/// The window lost the focus: a hold is cancelled, as Escape cancels it, because switching windows
/// is not a drop. The release may never reach this window, so egui is told to forget the buttons
/// that are down. Without a release event nothing can turn into a click, and the next real press
/// starts from a clean state; a release that does arrive later finds no button to end.
pub(crate) fn focus_lost(ctx: &egui::Context, state: &mut EditorState) {
    state.cancel_drag();
    if ctx.input(|input| input.pointer.any_down()) {
        ctx.input_mut(|input| input.pointer = egui::PointerState::default());
        ctx.stop_dragging();
    }
}

fn in_rect(root: &mut egui::Ui, id: &str, rect: Rect, add_contents: impl FnOnce(&mut egui::Ui)) {
    root.scope_builder(UiBuilder::new().id_salt(id).max_rect(rect), |ui| {
        ui.set_clip_rect(rect);
        add_contents(ui);
    });
}

/// A rounded card holding `add_contents`.
fn panel(root: &mut egui::Ui, id: &str, rect: Rect, add_contents: impl FnOnce(&mut egui::Ui)) {
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let radius = CornerRadius::same(theme::PANEL_RADIUS);
    root.painter().rect_filled(rect, radius, theme::PANEL);
    root.painter().rect_stroke(rect, radius, Stroke::new(1.0, theme::BORDER), StrokeKind::Inside);
    in_rect(root, id, rect.shrink(1.0), add_contents);
}

/// Consumes `key` only when exactly the `wanted` modifiers are down. egui's own matching ignores
/// an extra Shift or Alt, which would make Shift+S or Alt+Delete fire the plain shortcut.
fn consume_exact(input: &mut egui::InputState, wanted: Modifiers, key: Key) -> bool {
    input.modifiers.matches_exact(wanted) && input.consume_key(wanted, key)
}

/// Whether this frame's press of `key` is the auto-repeat of a key held down.
fn is_repeat(input: &egui::InputState, wanted: Key) -> bool {
    input.events.iter().any(|event| matches!(event, Event::Key { key, repeat: true, .. } if *key == wanted))
}

fn handle_shortcuts(ctx: &egui::Context, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    // Only a text edit needs the keys. A focused button or slider must not turn them off.
    if ctx.text_edit_focused() {
        return;
    }
    ctx.input_mut(|input| {
        let repeat = is_repeat(input, Key::Escape);
        if consume_exact(input, Modifiers::NONE, Key::Escape) && !repeat {
            if state.is_holding() {
                state.cancel_drag();
            } else if state.ui.fullscreen {
                requests.push(UiRequest::ToggleFullscreen);
            }
        }
        let repeat = is_repeat(input, Key::F11);
        if consume_exact(input, Modifiers::NONE, Key::F11) && !repeat && !state.is_holding() {
            requests.push(UiRequest::ToggleFullscreen);
        }
        if consume_exact(input, Modifiers::COMMAND, Key::Z) {
            state.undo();
        }
        if consume_exact(input, Modifiers::COMMAND, Key::Y) || consume_exact(input, Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) {
            state.redo();
        }
        if consume_exact(input, Modifiers::COMMAND, Key::S) {
            requests.push(UiRequest::SaveProject);
        }
        if consume_exact(input, Modifiers::COMMAND | Modifiers::SHIFT, Key::S) {
            requests.push(UiRequest::SaveProjectAs);
        }
        if consume_exact(input, Modifiers::COMMAND, Key::O) {
            requests.push(UiRequest::OpenProject);
        }
        if consume_exact(input, Modifiers::NONE, Key::S) {
            state.split_selected();
        }
        if consume_exact(input, Modifiers::NONE, Key::Delete) {
            state.delete_selected();
        }
        // Holding the key must not toggle again on each repeat.
        let repeat = is_repeat(input, Key::Space);
        if consume_exact(input, Modifiers::NONE, Key::Space) && !repeat {
            state.toggle_playback();
        }
    });
}

/// Everything the status line says, one item a line, for its tooltip.
fn status_summary(state: &EditorState) -> String {
    let language = state.language();
    let mut lines: Vec<String> = Vec::new();
    lines.extend(state.loading().map(|name| language.t(Message::Loading(name.to_owned()))));
    match state.status() {
        Status::Idle => {}
        Status::Info(message) | Status::Error(message) => lines.push(language.text(message)),
        Status::Refused(message) => lines.push(language.t(Message::RefusedLine(language.text(message)))),
    }
    lines.extend(state.preview_error().map(|message| language.t(Message::PreviewLine(message.to_owned()))));
    lines.extend(state.playback_message().map(|message| language.t(Message::AudioLine(message.to_owned()))));
    lines.extend(state.no_sound().map(|reason| language.t(Message::NoSoundLine(reason.to_owned()))));
    if state.silence_ms() > 0 {
        lines.push(language.t(Message::AudioGapLine(state.silence_ms())));
    }
    lines.join("\n")
}

fn status_line(ui: &mut egui::Ui, state: &EditorState) {
    let language = state.language();
    let summary = status_summary(state);
    if !summary.is_empty() {
        ui.interact(ui.max_rect(), ui.scope_id().with("status-tooltip"), egui::Sense::hover()).on_hover_text(summary);
    }
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        // The load is shown beside the status, which may say why a request meanwhile was not taken.
        let loading = state.loading();
        if let Some(name) = loading {
            ui.label(language.t(Message::Loading(name.to_owned())));
            if !matches!(state.status(), Status::Idle) {
                ui.separator();
            }
        }
        match state.status() {
            Status::Idle if loading.is_some() => {}
            Status::Idle => {
                ui.colored_label(theme::TEXT_DIM, language.t(Message::Ready));
            }
            Status::Info(message) => {
                ui.colored_label(theme::TEXT_DIM, language.text(message));
            }
            Status::Refused(message) => {
                ui.colored_label(theme::REFUSED, language.t(Message::RefusedLine(language.text(message))));
            }
            Status::Error(message) => {
                ui.colored_label(theme::REFUSED, language.text(message));
            }
        }
        if let Some(message) = state.preview_error() {
            ui.separator();
            ui.colored_label(theme::REFUSED, language.t(Message::PreviewLine(message.to_owned())));
        }
        if let Some(message) = state.playback_message() {
            ui.separator();
            ui.colored_label(theme::REFUSED, language.t(Message::AudioLine(message.to_owned())));
        }
        if let Some(reason) = state.no_sound() {
            ui.separator();
            ui.colored_label(theme::REFUSED, language.t(Message::NoSoundLine(reason.to_owned())));
        }
        if state.silence_ms() > 0 {
            ui.separator();
            ui.colored_label(theme::TEXT_DIM, language.t(Message::AudioGapLine(state.silence_ms())));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::drag::{ClipDrag, DragKind};
    use engine::{ClipKey, Time, TrackKind};
    use std::path::Path;

    fn seconds(value: i64) -> Time {
        Time::from_seconds(value).unwrap()
    }

    /// Two clips on one video track with the first selected and the playhead inside it.
    fn state() -> EditorState {
        let mut state = EditorState::default();
        state.add_track(TrackKind::Video);
        state.add_clip_from_file(0, Path::new("a.mp4"));
        state.set_playhead(seconds(5));
        state.select(Some(ClipKey { track: 0, clip: 0 }));
        state
    }

    /// One headless frame. The texture changes are dropped, because egui panics on unapplied ones.
    fn run_frame(ctx: &egui::Context, raw: egui::RawInput, add_contents: impl FnMut(&mut egui::Ui)) {
        let mut output = ctx.run_ui(raw, add_contents);
        output.textures_delta.clear();
    }

    fn press_in(ctx: &egui::Context, state: &mut EditorState, key: Key, modifiers: Modifiers, mut extra: impl FnMut(&egui::Context)) -> Vec<UiRequest> {
        let mut requests = Vec::new();
        let event = Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers };
        let raw = egui::RawInput { events: vec![Event::ModifiersChanged(modifiers), event], ..Default::default() };
        run_frame(ctx, raw, |ui| {
            handle_shortcuts(ui.ctx(), state, &mut requests);
            extra(ui.ctx());
        });
        requests
    }

    fn press(state: &mut EditorState, key: Key, modifiers: Modifiers) -> Vec<UiRequest> {
        press_in(&egui::Context::default(), state, key, modifiers, |_| {})
    }

    fn window_input(width: f32, height: f32) -> egui::RawInput {
        egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height))), ..Default::default() }
    }

    #[test]
    fn the_whole_ui_draws_at_every_window_size_with_and_without_a_selection() {
        let mut selected = state();
        let mut empty = EditorState::default();
        for (width, height) in [(1280.0, 720.0), (800.0, 500.0), (500.0, 300.0), (100.0, 60.0), (2560.0, 1440.0)] {
            for current in [&mut selected, &mut empty] {
                let ctx = egui::Context::default();
                let mut requests = Vec::new();
                run_frame(&ctx, window_input(width, height), |ui| show(ui, current, &mut requests, None));
                run_frame(&ctx, window_input(width, height), |ui| show(ui, current, &mut requests, None));
                assert!(requests.is_empty(), "drawing asks for nothing by itself");
            }
        }
    }

    /// Hovers, presses and releases at `at` in three frames, as a real click reaches egui.
    fn click(ctx: &egui::Context, state: &mut EditorState, at: egui::Pos2) -> Vec<UiRequest> {
        let mut requests = Vec::new();
        let button = |pressed| Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        for events in [vec![], vec![Event::PointerMoved(at)], vec![button(true)], vec![button(false)]] {
            let raw = egui::RawInput { events, ..window_input(1280.0, 720.0) };
            run_frame(ctx, raw, |ui| show(ui, state, &mut requests, None));
        }
        requests
    }

    #[test]
    fn clicking_a_tab_switches_the_left_panel() {
        let mut state = state();
        let left = panels(egui::vec2(1280.0, 720.0)).left;
        let at = egui::pos2(left.left() + left.width() * 5.0 / 6.0, left.top() + 20.0);
        click(&egui::Context::default(), &mut state, at);
        assert_eq!(state.ui.left_tab, crate::editor::LeftTab::Effects);
    }

    #[test]
    fn clicking_play_does_what_space_does() {
        let mut state = state();
        let center = panels(egui::vec2(1280.0, 720.0)).center;
        let at = egui::pos2(center.center().x, center.bottom() - 21.0);
        let ctx = egui::Context::default();
        click(&ctx, &mut state, at);
        assert!(state.is_playing());
        click(&ctx, &mut state, at);
        assert!(!state.is_playing());
    }

    #[test]
    fn clicking_the_fullscreen_button_asks_the_window() {
        let mut state = state();
        let center = panels(egui::vec2(1280.0, 720.0)).center;
        let requests = click(&egui::Context::default(), &mut state, egui::pos2(center.right() - 25.0, center.bottom() - 21.0));
        assert_eq!(requests, vec![UiRequest::ToggleFullscreen]);
    }

    #[test]
    fn a_release_commits_a_held_control() {
        let mut state = state();
        let ctx = egui::Context::default();
        state.live_gain(-6.0);
        let mut requests = Vec::new();
        run_frame(&ctx, window_input(1280.0, 720.0), |ui| show(ui, &mut state, &mut requests, None));
        assert!(!state.is_live(), "no button is down, so the edit is finished");
        assert_eq!(state.project().tracks[0].clips[0].gain_db, -6.0);
        state.undo();
        assert_eq!(state.project().tracks[0].clips[0].gain_db, 0.0);
    }

    #[test]
    fn file_shortcuts_become_requests_and_do_not_split() {
        let mut state = state();
        assert_eq!(press(&mut state, Key::S, Modifiers::COMMAND), vec![UiRequest::SaveProject]);
        assert_eq!(press(&mut state, Key::S, Modifiers::COMMAND | Modifiers::SHIFT), vec![UiRequest::SaveProjectAs]);
        assert_eq!(press(&mut state, Key::O, Modifiers::COMMAND), vec![UiRequest::OpenProject]);
        assert_eq!(state.project().tracks[0].clips.len(), 1, "Ctrl+S does not split");
    }

    #[test]
    fn the_plain_shortcuts_work() {
        let mut state = state();
        press(&mut state, Key::S, Modifiers::NONE);
        assert_eq!(state.project().tracks[0].clips.len(), 2, "S splits");
        press(&mut state, Key::Delete, Modifiers::NONE);
        assert_eq!(state.project().tracks[0].clips.len(), 1, "Delete deletes the selected half");
        press(&mut state, Key::Z, Modifiers::COMMAND);
        assert_eq!(state.project().tracks[0].clips.len(), 2, "Ctrl+Z undoes");
        press(&mut state, Key::Y, Modifiers::COMMAND);
        assert_eq!(state.project().tracks[0].clips.len(), 1, "Ctrl+Y redoes");
        press(&mut state, Key::Z, Modifiers::COMMAND);
        press(&mut state, Key::Z, Modifiers::COMMAND | Modifiers::SHIFT);
        assert_eq!(state.project().tracks[0].clips.len(), 1, "Ctrl+Shift+Z redoes");
    }

    #[test]
    fn a_shortcut_with_an_extra_modifier_does_nothing() {
        for modifiers in [Modifiers::SHIFT, Modifiers::ALT, Modifiers::COMMAND] {
            let mut state = state();
            let before = state.project().clone();
            press(&mut state, Key::S, modifiers);
            assert_eq!(state.project(), &before, "S with {modifiers:?}");
        }
        for modifiers in [Modifiers::SHIFT, Modifiers::ALT, Modifiers::COMMAND] {
            let mut state = state();
            let before = state.project().clone();
            press(&mut state, Key::Delete, modifiers);
            assert_eq!(state.project(), &before, "Delete with {modifiers:?}");
        }
    }

    #[test]
    fn undo_and_redo_need_exactly_their_modifiers() {
        let mut state = state();
        let with_clip = state.project().clone();
        press(&mut state, Key::Z, Modifiers::COMMAND | Modifiers::ALT);
        press(&mut state, Key::Y, Modifiers::COMMAND | Modifiers::SHIFT);
        press(&mut state, Key::Z, Modifiers::NONE);
        assert_eq!(state.project(), &with_clip, "nothing fired");
        press(&mut state, Key::Z, Modifiers::COMMAND);
        assert!(state.project().tracks[0].clips.is_empty());
        press(&mut state, Key::Y, Modifiers::COMMAND | Modifiers::ALT);
        assert!(state.project().tracks[0].clips.is_empty(), "Ctrl+Alt+Y does not redo");
    }

    #[test]
    fn space_toggles_and_a_held_key_does_not_toggle_again() {
        let mut state = state();
        let ctx = egui::Context::default();
        press_in(&ctx, &mut state, Key::Space, Modifiers::NONE, |_| {});
        assert!(state.is_playing());
        // Still down, so egui marks the second press as a repeat.
        press_in(&ctx, &mut state, Key::Space, Modifiers::NONE, |_| {});
        assert!(state.is_playing(), "a repeat is not a press");
        press(&mut state, Key::Space, Modifiers::NONE);
        assert!(!state.is_playing());
        press(&mut state, Key::Space, Modifiers::SHIFT);
        assert!(!state.is_playing(), "Shift+Space does nothing");
    }

    #[test]
    fn escape_cancels_a_drag_without_an_edit() {
        let mut state = state();
        let before = state.project().clone();
        state.drag = Some(ClipDrag { key: ClipKey { track: 0, clip: 0 }, kind: DragKind::Move, grab_offset: Time::ZERO });
        press(&mut state, Key::Escape, Modifiers::NONE);
        assert_eq!(state.drag, None);
        assert_eq!(state.project(), &before);
    }

    #[test]
    fn selecting_a_clip_with_an_out_of_range_gain_makes_no_edit() {
        let mut project = EditorState::default().project().clone();
        project.tracks.push(engine::Track { kind: TrackKind::Video, clips: vec![crate::editor::new_clip("a.mp4", Time::ZERO, seconds(5))], transitions: Vec::new() });
        project.tracks[0].clips[0].gain_db = -80.0;
        let mut state = EditorState::new(project);
        state.select(Some(ClipKey { track: 0, clip: 0 }));
        let ctx = egui::Context::default();
        let mut requests = Vec::new();
        for _ in 0..3 {
            run_frame(&ctx, window_input(1280.0, 720.0), |ui| show(ui, &mut state, &mut requests, None));
        }
        assert!(!state.is_dirty());
        assert!(!state.can_undo());
        assert_eq!(state.project().tracks[0].clips[0].gain_db, -80.0);
    }

    #[test]
    fn the_status_tooltip_holds_every_message_in_full() {
        let mut state = state();
        state.set_language(Language::Vietnamese);
        state.report_refused(Message::Technical("too long to fit".to_owned()));
        state.set_preview_error(Some("preview broke".to_owned()));
        state.set_no_sound(Some("no device".to_owned()));
        let summary = status_summary(&state);
        for part in ["Từ chối: too long to fit", "Xem trước: preview broke", "Không có tiếng: no device"] {
            assert!(summary.contains(part), "{summary}");
        }
        assert!(status_summary(&EditorState::default()).is_empty());
    }

    #[test]
    fn what_is_on_the_status_line_follows_a_language_change() {
        let mut state = state();
        state.report_refused(Message::Technical("too long".to_owned()));
        assert!(status_summary(&state).contains("Refused: too long"), "the default language");
        state.set_language(Language::Vietnamese);
        assert!(status_summary(&state).contains("Từ chối: too long"));
        state.set_language(Language::English);
        assert!(status_summary(&state).contains("Refused: too long"));
    }

    #[test]
    fn the_whole_ui_draws_in_every_language_at_the_narrow_size() {
        for language in Language::ALL {
            let mut state = state();
            state.set_language(language);
            let ctx = egui::Context::default();
            let mut requests = Vec::new();
            run_frame(&ctx, window_input(800.0, 500.0), |ui| show(ui, &mut state, &mut requests, None));
            run_frame(&ctx, window_input(800.0, 500.0), |ui| show(ui, &mut state, &mut requests, None));
            assert!(requests.is_empty());
        }
    }

    #[test]
    fn double_clicking_a_media_tile_adds_one_clip() {
        let mut state = state();
        state.library.add(crate::media::MediaItem { source: "b.mp4".to_owned(), name: "b.mp4".to_owned(), kind: crate::media::MediaKind::Video, duration: Some(seconds(2)), estimated: false });
        let left = panels(egui::vec2(1280.0, 720.0)).left;
        let at = egui::pos2(left.left() + 40.0, left.top() + 200.0);
        let ctx = egui::Context::default();
        click(&ctx, &mut state, at);
        assert_eq!(state.project().tracks.iter().map(|track| track.clips.len()).sum::<usize>(), 2, "the first click adds the file");
        let tracks = state.project().tracks.len();
        click(&ctx, &mut state, at);
        assert_eq!(state.project().tracks.iter().map(|track| track.clips.len()).sum::<usize>(), 2, "the second click of the double click adds nothing");
        assert_eq!(state.project().tracks.len(), tracks, "and no spare track");
    }

    #[test]
    fn escape_leaves_fullscreen_only_when_nothing_is_held() {
        let mut state = state();
        assert!(press(&mut state, Key::Escape, Modifiers::NONE).is_empty(), "not fullscreen");
        state.ui.fullscreen = true;
        assert_eq!(press(&mut state, Key::Escape, Modifiers::NONE), vec![UiRequest::ToggleFullscreen]);
        state.drag = Some(ClipDrag { key: ClipKey { track: 0, clip: 0 }, kind: DragKind::Move, grab_offset: Time::ZERO });
        assert!(press(&mut state, Key::Escape, Modifiers::NONE).is_empty(), "Escape cancels the drag first");
        assert_eq!(state.drag, None);
    }

    #[test]
    fn f11_toggles_fullscreen_unless_something_is_held() {
        let mut state = state();
        assert_eq!(press(&mut state, Key::F11, Modifiers::NONE), vec![UiRequest::ToggleFullscreen]);
        state.live_gain(-6.0);
        assert!(press(&mut state, Key::F11, Modifiers::NONE).is_empty());
    }

    /// A press held for two frames, so egui marks the second as a repeat.
    fn press_twice(state: &mut EditorState, key: Key) -> (Vec<UiRequest>, Vec<UiRequest>) {
        let ctx = egui::Context::default();
        let first = press_in(&ctx, state, key, Modifiers::NONE, |_| {});
        let second = press_in(&ctx, state, key, Modifiers::NONE, |_| {});
        (first, second)
    }

    #[test]
    fn holding_f11_toggles_fullscreen_once() {
        let mut state = state();
        let (first, second) = press_twice(&mut state, Key::F11);
        assert_eq!(first, vec![UiRequest::ToggleFullscreen]);
        assert!(second.is_empty(), "a repeat is not a press");
    }

    #[test]
    fn holding_escape_cancels_the_hold_and_stays_fullscreen() {
        let mut state = state();
        state.ui.fullscreen = true;
        state.live_gain(-6.0);
        let (first, second) = press_twice(&mut state, Key::Escape);
        assert!(state.hold_cancelled());
        assert!(first.is_empty() && second.is_empty(), "the repeat must not go on to leave fullscreen");
    }

    #[test]
    fn holding_escape_in_fullscreen_asks_to_leave_once() {
        let mut state = state();
        state.ui.fullscreen = true;
        let (first, second) = press_twice(&mut state, Key::Escape);
        assert_eq!(first, vec![UiRequest::ToggleFullscreen]);
        assert!(second.is_empty());
    }

    /// egui takes the focus from a text edit on Escape before the frame runs, so the shortcut is
    /// not hidden by the text edit that had it.
    #[test]
    fn escape_cancels_a_hold_while_a_text_edit_has_the_focus() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        let edit_id = egui::Id::unique("hold-edit");
        for _ in 0..2 {
            run_frame(&ctx, egui::RawInput::default(), |ui| {
                ui.add(egui::TextEdit::singleline(&mut text).id(edit_id)).request_focus();
            });
        }
        assert!(ctx.text_edit_focused(), "the test must really focus a text edit");
        let mut state = state();
        state.live_gain(-6.0);
        let escape = Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed: true, repeat: false, modifiers: Modifiers::NONE };
        let mut requests = Vec::new();
        run_frame(&ctx, egui::RawInput { events: vec![escape], ..Default::default() }, |ui| {
            handle_shortcuts(ui.ctx(), &mut state, &mut requests);
            ui.add(egui::TextEdit::singleline(&mut text).id(edit_id));
        });
        assert!(!state.is_live(), "Escape cancels the hold");
        assert!(state.hold_cancelled());
        assert_eq!(state.project().tracks[0].clips[0].gain_db, 0.0);
    }

    fn pointer_press(at: egui::Pos2) -> Vec<Event> {
        vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]
    }

    /// One whole-UI frame with `events`.
    fn input_frame(ctx: &egui::Context, state: &mut EditorState, events: Vec<Event>) {
        let mut requests = Vec::new();
        run_frame(ctx, egui::RawInput { events, ..window_input(1280.0, 720.0) }, |ui| show(ui, state, &mut requests, None));
    }

    fn pointer_release(at: egui::Pos2) -> Vec<Event> {
        vec![Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]
    }

    /// The screen position of `time` inside the only track.
    fn on_track(state: &EditorState, time: Time) -> egui::Pos2 {
        let origin = timeline_panel::tracks_origin(panels(egui::vec2(1280.0, 720.0)).timeline);
        origin + egui::vec2(state.view.time_to_x(time) as f32, state.view.track_top(0, 1) as f32 + 30.0)
    }

    /// Presses on the clip at 2 s and moves away, leaving a timeline drag in progress.
    fn start_clip_drag(ctx: &egui::Context, state: &mut EditorState) {
        let from = on_track(state, seconds(2));
        input_frame(ctx, state, vec![]);
        input_frame(ctx, state, vec![Event::PointerMoved(from)]);
        input_frame(ctx, state, pointer_press(from));
        input_frame(ctx, state, vec![Event::PointerMoved(from + egui::vec2(120.0, 0.0))]);
        input_frame(ctx, state, vec![Event::PointerMoved(from + egui::vec2(240.0, 0.0))]);
        assert!(state.drag.is_some(), "the test must really start a drag");
    }

    #[test]
    fn losing_the_focus_during_a_drag_moves_nothing_and_adds_no_undo_step() {
        let mut state = state();
        let ctx = egui::Context::default();
        let before = state.project().clone();
        start_clip_drag(&ctx, &mut state);
        focus_lost(&ctx, &mut state);
        input_frame(&ctx, &mut state, vec![]);
        assert_eq!(state.drag, None);
        assert_eq!(state.project(), &before, "the clip stays where it was");
        // The release can still arrive after the window lost the focus.
        let late = on_track(&state, seconds(4));
        input_frame(&ctx, &mut state, pointer_release(late));
        assert_eq!(state.project(), &before, "a late release drops nothing");
        state.undo();
        assert!(state.project().tracks[0].clips.is_empty(), "the next undo step is still adding the clip");
    }

    #[test]
    fn losing_the_focus_during_a_live_edit_restores_the_value() {
        let mut state = state();
        let ctx = egui::Context::default();
        input_frame(&ctx, &mut state, pointer_press(egui::pos2(5.0, 5.0)));
        state.live_gain(-6.0);
        input_frame(&ctx, &mut state, vec![]);
        assert!(state.is_live(), "the button is still down");
        focus_lost(&ctx, &mut state);
        input_frame(&ctx, &mut state, vec![]);
        assert!(!state.is_live());
        assert_eq!(state.project().tracks[0].clips[0].gain_db, 0.0);
        assert!(!state.hold_cancelled(), "egui no longer has a button down, so nothing stays cancelled");
    }

    #[test]
    fn a_press_without_a_move_and_a_lost_focus_fires_no_click() {
        let mut state = state();
        let ctx = egui::Context::default();
        let center = panels(egui::vec2(1280.0, 720.0)).center;
        let at = egui::pos2(center.center().x, center.bottom() - 21.0);
        input_frame(&ctx, &mut state, vec![]);
        input_frame(&ctx, &mut state, vec![Event::PointerMoved(at)]);
        input_frame(&ctx, &mut state, pointer_press(at));
        focus_lost(&ctx, &mut state);
        input_frame(&ctx, &mut state, vec![]);
        assert!(!state.is_playing(), "the play button was not clicked");
        input_frame(&ctx, &mut state, pointer_release(at));
        assert!(!state.is_playing(), "a release after the focus came back is no click either");
    }

    #[test]
    fn after_the_focus_returns_the_next_drag_works_and_edits_are_not_blocked() {
        let mut state = state();
        let ctx = egui::Context::default();
        start_clip_drag(&ctx, &mut state);
        focus_lost(&ctx, &mut state);
        input_frame(&ctx, &mut state, vec![]);
        assert!(!state.is_holding());
        press(&mut state, Key::S, Modifiers::NONE);
        assert_ne!(state.status(), &Status::Info(Message::FinishDragFirst), "edits are not blocked by the lost drag");
        state.undo();
        input_frame(&ctx, &mut state, vec![]);

        start_clip_drag(&ctx, &mut state);
        let to = on_track(&state, seconds(2)) + egui::vec2(240.0, 0.0);
        input_frame(&ctx, &mut state, pointer_release(to));
        assert_eq!(state.drag, None);
        assert!(state.project().tracks[0].clips[0].start > Time::ZERO, "the next drag moved the clip");
    }

    #[test]
    fn losing_the_focus_with_no_button_down_keeps_the_pointer_position() {
        let mut state = state();
        let ctx = egui::Context::default();
        input_frame(&ctx, &mut state, vec![Event::PointerMoved(egui::pos2(5.0, 5.0))]);
        focus_lost(&ctx, &mut state);
        assert_eq!(ctx.input(|input| input.pointer.latest_pos()), Some(egui::pos2(5.0, 5.0)));
    }

    #[test]
    fn a_release_ends_a_cancelled_hold() {
        let mut state = state();
        state.live_gain(-6.0);
        press(&mut state, Key::Escape, Modifiers::NONE);
        assert!(state.hold_cancelled());
        let mut requests = Vec::new();
        run_frame(&egui::Context::default(), window_input(1280.0, 720.0), |ui| show(ui, &mut state, &mut requests, None));
        assert!(!state.hold_cancelled());
    }

    #[test]
    fn a_focused_button_does_not_block_shortcuts_but_a_text_edit_does() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        let edit_id = egui::Id::unique("edit");
        let mut state = state();

        // Frame 1 focuses a button, frame 2 presses S.
        run_frame(&ctx, egui::RawInput::default(), |ui| {
            ui.button("Focus me").request_focus();
        });
        run_frame(&ctx, egui::RawInput::default(), |ui| {
            let _ = ui.button("Focus me");
        });
        assert!(ctx.egui_wants_keyboard_input() && !ctx.text_edit_focused(), "the test must really focus a button");
        press_in(&ctx, &mut state, Key::S, Modifiers::NONE, |_| {});
        assert_eq!(state.project().tracks[0].clips.len(), 2, "S still splits with a button focused");

        let mut state = self::state();
        run_frame(&ctx, egui::RawInput::default(), |ui| {
            let response = ui.add(egui::TextEdit::singleline(&mut text).id(edit_id));
            response.request_focus();
        });
        run_frame(&ctx, egui::RawInput::default(), |ui| {
            ui.add(egui::TextEdit::singleline(&mut text).id(edit_id));
        });
        assert!(ctx.text_edit_focused(), "the test must really focus a text edit");
        press_in(&ctx, &mut state, Key::S, Modifiers::NONE, |_| {});
        assert_eq!(state.project().tracks[0].clips.len(), 1, "S types into the text edit instead of splitting");
    }
}
