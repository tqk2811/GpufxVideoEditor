//! The package manager: a window that stays open beside the editor, with a tab for each kind of
//! pack. It only draws and asks; the window session does the work (see [`UiRequest`]).

use super::{theme, UiRequest};
use crate::editor::EditorState;
use crate::i18n::{Language, Message};
use crate::package_manager::{filter_packs, InstalledPack, PackSource, PackTab, UseSite};

const DEFAULT_SIZE: [f32; 2] = [760.0, 460.0];

/// What the package manager remembers between frames.
#[derive(Debug, Clone, Default)]
pub struct PackageManagerUi {
    pub open: bool,
    pub tab: PackTab,
    pub filter: String,
    /// The installed packs, as last read; empty until the first read finishes.
    pub packs: Vec<InstalledPack>,
    /// Whether a read of the packs is running.
    pub reading: bool,
    /// The pack (an index into `packs`) the user asked to remove, with where the open project
    /// uses it, until the removal is confirmed or cancelled.
    pub confirm_remove: Option<(usize, Vec<UseSite>)>,
    /// The packs the open project needs that the copy beside it can install.
    pub restore_offer: Vec<String>,
}

/// `bytes` in the unit that reads best.
pub fn size_label(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn show(ctx: &egui::Context, state: &mut EditorState, requests: &mut Vec<UiRequest>) {
    let language = state.language();
    let mut open = state.ui.packages.open;
    egui::Window::new(language.t(Message::PackManagerTitle)).id(egui::Id::unique("package-manager")).open(&mut open).default_size(DEFAULT_SIZE).collapsible(false).show(ctx, |ui| {
        let ui_state = &mut state.ui.packages;
        restore_banner(ui, language, ui_state, requests);
        ui.horizontal_wrapped(|ui| {
            for tab in PackTab::ALL {
                let count = ui_state.packs.iter().filter(|pack| pack.tab == tab).count();
                ui.selectable_value(&mut ui_state.tab, tab, format!("{} ({count})", language.t(Message::PackTabName(tab))));
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(language.t(Message::PackInstallZip)).clicked() {
                requests.push(UiRequest::ImportPackage);
            }
            if ui.add_enabled(!ui_state.reading, egui::Button::new(language.t(Message::PackRefresh))).clicked() {
                requests.push(UiRequest::RefreshPacks);
            }
            if ui_state.reading {
                ui.spinner();
            }
            ui.add(egui::TextEdit::singleline(&mut ui_state.filter).hint_text(language.t(Message::PackFilter)).desired_width(f32::INFINITY));
        });
        if let Some((index, uses)) = ui_state.confirm_remove.clone() {
            confirm_removal(ui, language, ui_state, index, &uses, requests);
        }
        ui.separator();
        pack_table(ui, language, ui_state, requests);
    });
    if !open {
        state.ui.packages.open = false;
        state.ui.packages.confirm_remove = None;
    }
}

fn restore_banner(ui: &mut egui::Ui, language: Language, ui_state: &mut PackageManagerUi, requests: &mut Vec<UiRequest>) {
    if ui_state.restore_offer.is_empty() {
        return;
    }
    ui.colored_label(theme::accent(), language.t(Message::PackRestoreOffer(ui_state.restore_offer.len())));
    ui.label(egui::RichText::new(ui_state.restore_offer.join(", ")).small().color(theme::text_dim()));
    ui.horizontal(|ui| {
        if ui.button(language.t(Message::PackRestoreInstall)).clicked() {
            requests.push(UiRequest::RestorePacks);
        }
        if ui.button(language.t(Message::PackRestoreDismiss)).clicked() {
            ui_state.restore_offer.clear();
        }
    });
    ui.separator();
}

fn confirm_removal(ui: &mut egui::Ui, language: Language, ui_state: &mut PackageManagerUi, index: usize, uses: &[UseSite], requests: &mut Vec<UiRequest>) {
    let Some(pack) = ui_state.packs.get(index) else {
        ui_state.confirm_remove = None;
        return;
    };
    ui.separator();
    ui.strong(language.t(Message::PackRemoveQuestion(pack.name.clone())));
    if !uses.is_empty() {
        ui.colored_label(theme::refused(), language.t(Message::PackUsedBy(uses.len())));
        egui::ScrollArea::vertical().id_salt("pack-uses").max_height(90.0).show(ui, |ui| {
            for site in uses {
                ui.label(language.t(match *site {
                    UseSite::Clip { track, clip } => Message::PackUseClip { track, clip },
                    UseSite::Transition { track, after_clip } => Message::PackUseTransition { track, after_clip },
                }));
            }
        });
    }
    ui.horizontal(|ui| {
        if ui.button(language.t(Message::PackRemove)).clicked() {
            requests.push(UiRequest::RemovePackConfirmed { index });
            ui_state.confirm_remove = None;
        }
        if ui.button(language.t(Message::Cancel)).clicked() {
            ui_state.confirm_remove = None;
        }
    });
}

fn pack_table(ui: &mut egui::Ui, language: Language, ui_state: &PackageManagerUi, requests: &mut Vec<UiRequest>) {
    let shown = filter_packs(&ui_state.packs, ui_state.tab, &ui_state.filter);
    if shown.is_empty() {
        ui.colored_label(theme::text_dim(), language.t(Message::PackNone));
        return;
    }
    egui::ScrollArea::both().id_salt("pack-table").auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new("pack-grid").striped(true).num_columns(7).spacing([12.0, 4.0]).show(ui, |ui| {
            for heading in [Message::PackColumnEnabled, Message::PackColumnName, Message::PackColumnVersion, Message::PackColumnSource, Message::PackColumnItems, Message::PackColumnSize] {
                ui.strong(language.t(heading));
            }
            ui.label("");
            ui.end_row();
            for pack in shown {
                let index = ui_state.packs.iter().position(|candidate| std::ptr::eq(candidate, pack)).unwrap_or(usize::MAX);
                let mut enabled = pack.enabled;
                if ui.checkbox(&mut enabled, "").changed() {
                    requests.push(UiRequest::SetPackEnabled { index, enabled });
                }
                name_cell(ui, language, pack);
                ui.label(&pack.version);
                ui.label(language.t(match pack.source {
                    PackSource::Shipped => Message::PackSourceShipped,
                    PackSource::User => Message::PackSourceUser,
                }));
                ui.label(pack.items.to_string());
                ui.label(size_label(pack.bytes));
                ui.horizontal(|ui| {
                    if ui.button(language.t(Message::PackOpenFolder)).clicked() {
                        requests.push(UiRequest::OpenPackFolder { index });
                    }
                    let remove = ui.add_enabled(pack.removable(), egui::Button::new(language.t(Message::PackRemove)));
                    if remove.clicked() {
                        requests.push(UiRequest::RemovePack { index });
                    }
                    if !pack.removable() {
                        remove.on_disabled_hover_text(language.t(Message::PackReadOnly));
                    }
                });
                ui.end_row();
            }
        });
    });
}

/// The name, its labels, and in red why a broken pack cannot be used.
fn name_cell(ui: &mut egui::Ui, language: Language, pack: &InstalledPack) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            let name = if pack.problem.is_some() { egui::RichText::new(&pack.name).color(theme::refused()) } else { egui::RichText::new(&pack.name) };
            ui.label(name).on_hover_text(pack.path.display().to_string());
            if pack.hidden {
                ui.label(egui::RichText::new(language.t(Message::PackInternal)).small().color(theme::text_dim()));
            }
        });
        if let Some(problem) = &pack.problem {
            ui.label(egui::RichText::new(problem).small().color(theme::refused()));
        }
        if pack.shadowed {
            ui.label(egui::RichText::new(language.t(Message::PackShadowed)).small().color(theme::text_dim()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_in_the_unit_that_fits() {
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(2048), "2.0 KB");
        assert_eq!(size_label(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
    }
}
