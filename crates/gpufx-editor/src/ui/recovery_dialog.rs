//! The dialog that offers the autosaves of editors that did not close cleanly.

use super::{theme, UiRequest};
use crate::editor::{EditorState, RecoveryChoice};
use crate::i18n::Message;

const WIDTH: f32 = 520.0;

pub fn show(ctx: &egui::Context, state: &EditorState, requests: &mut Vec<UiRequest>) {
    let language = state.language();
    let now = crate::recovery::unix_now();
    let response = egui::Modal::new(egui::Id::unique("recovery-dialog")).show(ctx, |ui| {
        ui.set_width(WIDTH);
        ui.heading(language.t(Message::RecoveryTitle));
        ui.label(language.t(Message::RecoveryIntro));
        ui.small(language.t(Message::RecoveryKeptDays(crate::recovery::KEEP_SECS / 86_400)));
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for (index, offer) in state.ui.recovery_offers.iter().enumerate() {
                ui.separator();
                let name = offer.original.as_deref().and_then(std::path::Path::file_name).map_or_else(|| language.t(Message::Untitled), |name| name.to_string_lossy().into_owned());
                ui.strong(name);
                if let Some(original) = &offer.original {
                    ui.add(egui::Label::new(egui::RichText::new(original.display().to_string()).small().color(theme::text_dim())).truncate()).on_hover_text(original.display().to_string());
                }
                ui.label(language.t(Message::RecoveryAgo(now.saturating_sub(offer.saved_at))));
                if offer.original_newer {
                    ui.colored_label(theme::refused(), language.t(Message::RecoveryOriginalNewer));
                }
                ui.horizontal(|ui| {
                    for (label, choice) in [(Message::RecoveryRestore, RecoveryChoice::Restore), (Message::RecoveryDiscard, RecoveryChoice::Discard), (Message::RecoveryLater, RecoveryChoice::Later)] {
                        if ui.button(language.t(label)).clicked() {
                            requests.push(UiRequest::Recovery { index, choice });
                        }
                    }
                });
            }
        });
    });
    // Escape or a click outside puts every one off to the next start.
    if response.should_close() {
        for index in (0..state.ui.recovery_offers.len()).rev() {
            requests.push(UiRequest::Recovery { index, choice: RecoveryChoice::Later });
        }
    }
}
