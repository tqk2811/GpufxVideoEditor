//! The package manager: a window that stays open beside the editor, with a tab for each kind of
//! pack. It only draws and asks; the window session does the work (see [`UiRequest`]).

use std::collections::HashSet;

use super::{theme, UiRequest};
use crate::editor::EditorState;
use crate::i18n::pack::PackCollection;
use crate::i18n::{Language, Message};
use crate::package_manager::{filter_packs, find_pack, InstalledPack, PackSource, PackTab, UseSite};

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
    /// The packs (their keys and paths, which outlive a new read of `packs`) the user asked to
    /// remove, one or a whole collection, with where the open project uses them, until the removal
    /// is confirmed or cancelled.
    pub confirm_remove: Option<(Vec<(String, std::path::PathBuf)>, Vec<UseSite>)>,
    /// The collections shown open, by tab code and collection id; the others start closed.
    pub open_collections: HashSet<String>,
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
    // Packs do not change under a running export, which reads them; the window drops such
    // requests then too (see `handle`).
    let busy = state.is_exporting();
    let mut open = state.ui.packages.open;
    egui::Window::new(language.t(Message::PackManagerTitle)).id(egui::Id::unique("package-manager")).open(&mut open).default_size(DEFAULT_SIZE).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).collapsible(false).show(ctx, |ui| {
        let ui_state = &mut state.ui.packages;
        restore_banner(ui, language, ui_state, busy, requests);
        ui.horizontal_wrapped(|ui| {
            for tab in PackTab::ALL {
                let count = ui_state.packs.iter().filter(|pack| pack.tab == tab).count();
                ui.selectable_value(&mut ui_state.tab, tab, format!("{} ({count})", language.t(Message::PackTabName(tab))));
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            let install = ui.add_enabled(!busy, egui::Button::new(language.t(Message::PackInstallZip)));
            if install.clicked() {
                requests.push(UiRequest::ImportPackage);
            }
            install.on_disabled_hover_text(language.t(Message::PackBusyExporting));
            let folder = ui.add_enabled(!busy, egui::Button::new(language.t(Message::PackInstallFolder)));
            if folder.clicked() {
                requests.push(UiRequest::ImportPackageFolder);
            }
            folder.on_disabled_hover_text(language.t(Message::PackBusyExporting));
            if ui.add_enabled(!ui_state.reading, egui::Button::new(language.t(Message::PackRefresh))).clicked() {
                requests.push(UiRequest::RefreshPacks);
            }
            if ui_state.reading {
                ui.spinner();
            }
            ui.add(egui::TextEdit::singleline(&mut ui_state.filter).hint_text(language.t(Message::PackFilter)).desired_width(f32::INFINITY));
        });
        if let Some((targets, uses)) = ui_state.confirm_remove.clone() {
            confirm_removal(ui, language, ui_state, &targets, &uses, busy, requests);
        }
        if busy {
            ui.colored_label(theme::refused(), language.t(Message::PackBusyExporting));
        }
        ui.separator();
        pack_table(ui, language, ui_state, busy, requests);
    });
    if !open {
        state.ui.packages.open = false;
        state.ui.packages.confirm_remove = None;
    }
}

fn restore_banner(ui: &mut egui::Ui, language: Language, ui_state: &mut PackageManagerUi, busy: bool, requests: &mut Vec<UiRequest>) {
    if ui_state.restore_offer.is_empty() {
        return;
    }
    ui.colored_label(theme::accent(), language.t(Message::PackRestoreOffer(ui_state.restore_offer.len())));
    ui.label(egui::RichText::new(ui_state.restore_offer.join(", ")).small().color(theme::text_dim()));
    ui.horizontal(|ui| {
        let install = ui.add_enabled(!busy, egui::Button::new(language.t(Message::PackRestoreInstall)));
        if install.clicked() {
            requests.push(UiRequest::RestorePacks);
        }
        install.on_disabled_hover_text(language.t(Message::PackBusyExporting));
        if ui.button(language.t(Message::PackRestoreDismiss)).clicked() {
            ui_state.restore_offer.clear();
        }
    });
    ui.separator();
}

fn confirm_removal(ui: &mut egui::Ui, language: Language, ui_state: &mut PackageManagerUi, targets: &[(String, std::path::PathBuf)], uses: &[UseSite], busy: bool, requests: &mut Vec<UiRequest>) {
    let found: Vec<&InstalledPack> = targets.iter().filter_map(|(key, path)| find_pack(&ui_state.packs, key, path)).collect();
    let question = match found.as_slice() {
        [] => {
            ui_state.confirm_remove = None;
            return;
        }
        [pack] if targets.len() == 1 => Message::PackRemoveQuestion(pack.display_name(language.code()).to_owned()),
        [pack, ..] => Message::PackRemoveCollectionQuestion { name: pack.collection.display_name(language.code()).to_owned(), count: found.len() },
    };
    ui.separator();
    ui.strong(language.t(question));
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
        let remove = ui.add_enabled(!busy, egui::Button::new(language.t(Message::PackRemove)));
        if remove.clicked() {
            // The window takes the pack from `confirm_remove` and clears it.
            requests.push(UiRequest::RemovePackConfirmed);
        }
        remove.on_disabled_hover_text(language.t(Message::PackBusyExporting));
        if ui.button(language.t(Message::Cancel)).clicked() {
            ui_state.confirm_remove = None;
        }
    });
}

/// The height of a row of the pack table: every row is one line, so only the rows in view are drawn.
const ROW_HEIGHT: f32 = 24.0;
/// The widths of the columns after the name: enabled, version, source, items, size, actions.
const COLUMN_WIDTHS: [f32; 6] = [24.0, 80.0, 90.0, 50.0, 70.0, 170.0];
/// The narrowest the name column gets; the table scrolls sideways below that.
const MIN_NAME_WIDTH: f32 = 180.0;

/// One line of the pack table.
#[derive(Debug, Clone, PartialEq)]
pub enum TableRow<'a> {
    /// A collection of more than one pack, with its packs; open or not.
    Collection { collection: &'a PackCollection, packs: Vec<&'a InstalledPack>, open: bool },
    /// A pack: alone in its collection, or inside an open one.
    Pack { pack: &'a InstalledPack, nested: bool },
}

/// The rows of `shown` (packs of one tab, in the list's order): packs gathered by collection, the
/// packs of a collection listed only while it is open. Every collection is open while `filtering`.
/// Only packs in use make a collection: a shadowed copy is listed right after the pack that hides
/// it, so the two copies of one pack never look like a collection of two.
pub fn table_rows<'a>(shown: &[&'a InstalledPack], open: &HashSet<String>, tab: PackTab, filtering: bool, code: &str) -> Vec<TableRow<'a>> {
    let groups = crate::i18n::pack::group_by_collection(shown.iter().filter(|pack| !pack.shadowed).map(|pack| (&pack.collection, *pack)), code);
    let shadowed: Vec<&'a InstalledPack> = shown.iter().copied().filter(|pack| pack.shadowed).collect();
    let mut placed = vec![false; shadowed.len()];
    let mut with_copies = |rows: &mut Vec<TableRow<'a>>, pack: &'a InstalledPack, nested: bool| {
        rows.push(TableRow::Pack { pack, nested });
        for (copy, done) in shadowed.iter().zip(placed.iter_mut()) {
            if copy.key == pack.key {
                *done = true;
                rows.push(TableRow::Pack { pack: copy, nested });
            }
        }
    };
    let mut rows = Vec::new();
    for (collection, packs) in groups {
        if let [pack] = packs.as_slice() {
            with_copies(&mut rows, pack, false);
            continue;
        }
        let is_open = filtering || open.contains(&collection_key(tab, &collection.id));
        rows.push(TableRow::Collection { collection, packs: packs.clone(), open: is_open });
        if is_open {
            for pack in packs {
                with_copies(&mut rows, pack, true);
            }
        }
    }
    // A shadowed copy whose pack in use is filtered out still shows, on its own.
    let orphans: Vec<&'a InstalledPack> = shadowed.iter().zip(&placed).filter(|(_, done)| !**done).map(|(pack, _)| *pack).collect();
    rows.extend(orphans.into_iter().map(|pack| TableRow::Pack { pack, nested: false }));
    rows
}

/// The packs a collection header acts on: those listed under it, in the tab and matching the
/// filter of `ui_state`, not shadowed; the ones in the collection of pack `index` of the list.
pub fn collection_members(ui_state: &PackageManagerUi, index: usize) -> Vec<&InstalledPack> {
    let Some(pack) = ui_state.packs.get(index) else { return Vec::new() };
    filter_packs(&ui_state.packs, pack.tab, &ui_state.filter).into_iter().filter(|other| !other.shadowed && other.collection.id == pack.collection.id).collect()
}

/// What [`PackageManagerUi::open_collections`] holds for collection `id` of `tab`.
pub fn collection_key(tab: PackTab, id: &str) -> String {
    format!("{}/{id}", tab.code())
}

/// Where pack `pack` is in the list, for a request.
fn index_of(packs: &[InstalledPack], pack: &InstalledPack) -> usize {
    packs.iter().position(|candidate| std::ptr::eq(candidate, pack)).unwrap_or(usize::MAX)
}

/// A cell `width` wide of the row being drawn.
fn cell<R>(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.allocate_ui_with_layout(egui::vec2(width, ROW_HEIGHT), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_width(width);
        ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
        add(ui)
    })
    .inner
}

fn source_label(language: Language, source: PackSource) -> String {
    language.t(match source {
        PackSource::Shipped => Message::PackSourceShipped,
        PackSource::User => Message::PackSourceUser,
    })
}

/// The size of `packs` together, or an ellipsis while one of them is not measured yet.
fn sizes_label<'a>(packs: impl IntoIterator<Item = &'a InstalledPack>) -> String {
    packs.into_iter().map(|pack| pack.bytes).sum::<Option<u64>>().map_or_else(|| "\u{2026}".to_owned(), size_label)
}

fn pack_table(ui: &mut egui::Ui, language: Language, ui_state: &mut PackageManagerUi, busy: bool, requests: &mut Vec<UiRequest>) {
    let shown = filter_packs(&ui_state.packs, ui_state.tab, &ui_state.filter);
    if shown.is_empty() {
        ui.colored_label(theme::text_dim(), language.t(Message::PackNone));
        return;
    }
    let code = language.code();
    let rows = table_rows(&shown, &ui_state.open_collections, ui_state.tab, !ui_state.filter.trim().is_empty(), code);
    let fixed: f32 = COLUMN_WIDTHS.iter().sum::<f32>() + ui.spacing().item_spacing.x * 7.0;
    let name_width = (ui.available_width() - fixed).max(MIN_NAME_WIDTH);
    let [w_enabled, w_version, w_source, w_items, w_size, w_actions] = COLUMN_WIDTHS;
    ui.horizontal(|ui| {
        cell(ui, w_enabled, |ui| ui.strong(language.t(Message::PackColumnEnabled)));
        cell(ui, name_width, |ui| ui.strong(language.t(Message::PackColumnName)));
        cell(ui, w_version, |ui| ui.strong(language.t(Message::PackColumnVersion)));
        cell(ui, w_source, |ui| ui.strong(language.t(Message::PackColumnSource)));
        cell(ui, w_items, |ui| ui.strong(language.t(Message::PackColumnItems)));
        cell(ui, w_size, |ui| ui.strong(language.t(Message::PackColumnSize)));
    });
    let mut toggled = None;
    let tab = ui_state.tab;
    let packs = &ui_state.packs;
    egui::ScrollArea::both().id_salt("pack-table").auto_shrink([false, false]).show_rows(ui, ROW_HEIGHT, rows.len(), |ui, range| {
        for row in &rows[range] {
            ui.horizontal(|ui| match row {
                TableRow::Collection { collection, packs: members, open } => {
                    let index = index_of(packs, members[0]);
                    let mut enabled = members.iter().all(|pack| pack.enabled);
                    let toggle = cell(ui, w_enabled, |ui| ui.add_enabled(!busy, egui::Checkbox::without_text(&mut enabled)));
                    if toggle.changed() {
                        requests.push(UiRequest::SetPackEnabled { index, enabled, collection: true });
                    }
                    toggle.on_disabled_hover_text(language.t(Message::PackBusyExporting));
                    cell(ui, name_width, |ui| {
                        let arrow = if *open { "\u{25be}" } else { "\u{25b8}" };
                        let label = egui::RichText::new(format!("{arrow} {} ({})", collection.display_name(code), members.len())).strong();
                        if ui.add(egui::Label::new(label).sense(egui::Sense::click())).clicked() {
                            toggled = Some(collection_key(tab, &collection.id));
                        }
                    });
                    cell(ui, w_version, |_| ());
                    let first = members[0].source;
                    cell(ui, w_source, |ui| members.iter().all(|pack| pack.source == first).then(|| ui.label(source_label(language, first))));
                    cell(ui, w_items, |ui| ui.label(members.iter().map(|pack| pack.items).sum::<usize>().to_string()));
                    cell(ui, w_size, |ui| ui.label(sizes_label(members.iter().copied())));
                    cell(ui, w_actions, |ui| {
                        let none = !members.iter().any(|pack| pack.removable());
                        let remove = ui.add_enabled(!none && !busy, egui::Button::new(language.t(Message::PackRemove)));
                        if remove.clicked() {
                            requests.push(UiRequest::RemovePack { index, collection: true });
                        }
                        if none {
                            remove.on_disabled_hover_text(language.t(Message::PackReadOnly));
                        } else if busy {
                            remove.on_disabled_hover_text(language.t(Message::PackBusyExporting));
                        }
                    });
                }
                TableRow::Pack { pack, nested } => pack_row(ui, language, packs, pack, *nested, name_width, busy, requests),
            });
        }
    });
    if let Some(key) = toggled {
        if !ui_state.open_collections.remove(&key) {
            ui_state.open_collections.insert(key);
        }
    }
}

/// One pack's line: the name, its labels; why a broken pack cannot be used shows on hover.
#[allow(clippy::too_many_arguments)]
fn pack_row(ui: &mut egui::Ui, language: Language, packs: &[InstalledPack], pack: &InstalledPack, nested: bool, name_width: f32, busy: bool, requests: &mut Vec<UiRequest>) {
    let [w_enabled, w_version, w_source, w_items, w_size, w_actions] = COLUMN_WIDTHS;
    let index = index_of(packs, pack);
    let mut enabled = pack.enabled;
    let toggle = cell(ui, w_enabled, |ui| ui.add_enabled(!busy, egui::Checkbox::without_text(&mut enabled)));
    if toggle.changed() {
        requests.push(UiRequest::SetPackEnabled { index, enabled, collection: false });
    }
    toggle.on_disabled_hover_text(language.t(Message::PackBusyExporting));
    cell(ui, name_width, |ui| {
        if nested {
            ui.add_space(16.0);
        }
        let code = language.code();
        let shown = pack.display_name(code);
        let name = if pack.problem.is_some() { egui::RichText::new(shown).color(theme::refused()) } else { egui::RichText::new(shown) };
        let description = pack.i18n.get(code).and_then(|texts| texts.description.as_deref());
        let mut hover = description.map_or_else(|| pack.path.display().to_string(), |description| format!("{description}\n{}", pack.path.display()));
        if let Some(problem) = &pack.problem {
            hover = format!("{problem}\n{hover}");
        }
        ui.label(name).on_hover_text(hover);
        if pack.hidden {
            ui.label(egui::RichText::new(language.t(Message::PackInternal)).small().color(theme::text_dim()));
        }
        if pack.shadowed {
            ui.label(egui::RichText::new(language.t(Message::PackShadowed)).small().color(theme::text_dim()));
        }
    });
    cell(ui, w_version, |ui| ui.label(&pack.version));
    cell(ui, w_source, |ui| ui.label(source_label(language, pack.source)));
    cell(ui, w_items, |ui| ui.label(pack.items.to_string()));
    cell(ui, w_size, |ui| ui.label(sizes_label([pack])));
    cell(ui, w_actions, |ui| {
        if ui.button(language.t(Message::PackOpenFolder)).clicked() {
            requests.push(UiRequest::OpenPackFolder { index });
        }
        let remove = ui.add_enabled(pack.removable() && !busy, egui::Button::new(language.t(Message::PackRemove)));
        if remove.clicked() {
            requests.push(UiRequest::RemovePack { index, collection: false });
        }
        if !pack.removable() {
            remove.on_disabled_hover_text(language.t(Message::PackReadOnly));
        } else if busy {
            remove.on_disabled_hover_text(language.t(Message::PackBusyExporting));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(key: &str, collection: &str, source: PackSource, shadowed: bool) -> InstalledPack {
        InstalledPack {
            tab: PackTab::Sticker,
            key: key.to_owned(),
            id: key.to_owned(),
            name: key.to_owned(),
            version: String::new(),
            source,
            path: std::path::PathBuf::from(key),
            items: 1,
            bytes: Some(10),
            enabled: true,
            hidden: false,
            problem: None,
            shadowed,
            tags: Vec::new(),
            i18n: Default::default(),
            collection: PackCollection { id: collection.to_owned(), name: collection.to_owned(), i18n: Default::default() },
        }
    }

    #[test]
    fn a_shadowed_copy_is_not_a_second_pack_of_its_collection() {
        // No `collection` in the manifest: the pack's own id is its collection, for both copies.
        let packs = [pack("a", "a", PackSource::User, false), pack("a", "a", PackSource::Shipped, true)];
        let shown: Vec<&InstalledPack> = packs.iter().collect();
        let rows = table_rows(&shown, &HashSet::new(), PackTab::Sticker, false, "en");
        assert_eq!(rows, [TableRow::Pack { pack: &packs[0], nested: false }, TableRow::Pack { pack: &packs[1], nested: false }]);
    }

    #[test]
    fn a_header_acts_on_the_packs_it_lists() {
        let mut ui_state = PackageManagerUi::default();
        ui_state.packs = vec![pack("fun-a", "fun", PackSource::User, false), pack("fun-b", "fun", PackSource::User, false), pack("fun-a", "fun", PackSource::Shipped, true), pack("other", "o", PackSource::User, false)];
        let keys = |ui_state: &PackageManagerUi| collection_members(ui_state, 0).iter().map(|pack| pack.key.clone()).collect::<Vec<_>>();
        assert_eq!(keys(&ui_state), ["fun-a", "fun-b"], "the shadowed copy is not listed under the header");
        ui_state.filter = "fun-b".to_owned();
        assert_eq!(keys(&ui_state), ["fun-b"], "nor a pack the filter hides");
        ui_state.filter.clear();
        let shown = filter_packs(&ui_state.packs, PackTab::Sticker, "");
        let rows = table_rows(&shown, &HashSet::new(), PackTab::Sticker, false, "en");
        let TableRow::Collection { packs, .. } = &rows[0] else { panic!("{rows:?}") };
        assert_eq!(packs.len(), collection_members(&ui_state, 0).len(), "the header count is what it acts on");
    }

    #[test]
    fn sizes_read_in_the_unit_that_fits() {
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(2048), "2.0 KB");
        assert_eq!(size_label(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
    }
}
