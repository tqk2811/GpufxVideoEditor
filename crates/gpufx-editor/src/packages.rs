//! The list of effect and transition packages read from the package roots. No window here.

use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use d3d12_backend::{PackageInfo, ParamInfo};

/// How deep the groups of `packages/gpufx` may nest.
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PackageKind {
    #[default]
    Effect,
    Transition,
}

/// How a package can be used, as the render path uses it: an effect has one input, a transition has
/// two and a `progress` parameter to drive. Anything else needs inputs the timeline cannot give it.
pub fn classify(info: &PackageInfo) -> Option<PackageKind> {
    match (info.inputs, info.has_progress) {
        (1, _) => Some(PackageKind::Effect),
        (2, true) => Some(PackageKind::Transition),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackageEntry {
    /// The name stored in a project: the path relative to the package root.
    pub package: String,
    pub name: String,
    pub description: String,
    pub kind: PackageKind,
    pub params: Vec<ParamInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageProblem {
    pub package: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    pub loaded: bool,
    pub effects: Vec<PackageEntry>,
    pub transitions: Vec<PackageEntry>,
    pub problems: Vec<PackageProblem>,
}

impl Catalog {
    pub fn entries(&self, kind: PackageKind) -> &[PackageEntry] {
        match kind {
            PackageKind::Effect => &self.effects,
            PackageKind::Transition => &self.transitions,
        }
    }

    /// The parameters of the package stored in a project as `package`.
    pub fn params_of(&self, package: &str) -> Option<&[ParamInfo]> {
        self.effects.iter().chain(&self.transitions).find(|entry| entry.package == package).map(|entry| entry.params.as_slice())
    }
}

fn is_package(path: &Path) -> bool {
    path.join("manifest.json").is_file() || (path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
}

fn package_name(root: &Path, package: &Path) -> String {
    package.strip_prefix(root).unwrap_or(package).components().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

fn collect(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if is_package(&path) {
            found.push(path);
        } else if path.is_dir() && depth < MAX_DEPTH {
            collect(&path, depth + 1, found);
        }
    }
}

/// Reads every package under the `packages/gpufx` of each root. `read` opens one package; what it
/// refuses, or panics on, becomes a problem and the scan goes on.
pub fn scan(roots: &[PathBuf], read: impl Fn(&Path) -> Result<PackageInfo, String>) -> Catalog {
    let mut catalog = Catalog { loaded: true, ..Catalog::default() };
    // The first root with a name is the one a project resolves it to.
    let mut seen = HashSet::new();
    for root in roots {
        let mut found = Vec::new();
        collect(&root.join("packages").join("gpufx"), 0, &mut found);
        for path in found {
            let package = package_name(root, &path);
            if !seen.insert(package.clone()) {
                continue;
            }
            let info = catch_unwind(AssertUnwindSafe(|| read(&path))).unwrap_or_else(|_| Err("reading the package panicked".to_owned()));
            match info {
                Ok(info) => {
                    let Some(kind) = classify(&info) else { continue };
                    let entry = PackageEntry { package, name: info.name, description: info.description, kind, params: info.params };
                    match kind {
                        PackageKind::Effect => catalog.effects.push(entry),
                        PackageKind::Transition => catalog.transitions.push(entry),
                    }
                }
                Err(message) => catalog.problems.push(PackageProblem { package, message }),
            }
        }
    }
    for list in [&mut catalog.effects, &mut catalog.transitions] {
        list.sort_by_key(|entry| entry.name.to_lowercase());
    }
    catalog
}

/// The entries whose name or package contains every word of `filter`, ignoring case.
pub fn filter_entries<'a>(entries: &'a [PackageEntry], filter: &str) -> Vec<&'a PackageEntry> {
    let filter = filter.to_lowercase();
    let words: Vec<&str> = filter.split_whitespace().collect();
    entries.iter().filter(|entry| {
        let haystack = format!("{} {}", entry.name.to_lowercase(), entry.package.to_lowercase());
        words.iter().all(|word| haystack.contains(word))
    }).collect()
}

/// The last part of a package name, for showing it, without the `.zip` of a zipped package.
pub fn short_name(package: &str) -> &str {
    let last = package.rsplit('/').next().unwrap_or(package);
    let stem = last.len().checked_sub(".zip".len()).filter(|&end| last.is_char_boundary(end) && last[end..].eq_ignore_ascii_case(".zip"));
    stem.map_or(last, |end| &last[..end])
}

/// The group of a package: its folders under `packages/gpufx`, or the path it was stored under.
pub fn package_group(package: &str) -> &str {
    let inside = package.strip_prefix("packages/gpufx/").unwrap_or(package);
    inside.rsplit_once('/').map_or("", |(group, _)| group)
}

/// The names, lower-cased, that more than one of `entries` has.
pub fn duplicated_names(entries: &[PackageEntry]) -> HashSet<String> {
    let mut seen = HashSet::new();
    entries.iter().map(|entry| entry.name.to_lowercase()).filter(|name| !seen.insert(name.clone())).collect()
}

/// How the list shows `entry`: the name, followed by the group when another package has that name.
pub fn list_label(entry: &PackageEntry, duplicates: &HashSet<String>) -> String {
    if duplicates.contains(&entry.name.to_lowercase()) {
        format!("{} ({})", entry.name, package_group(&entry.package))
    } else {
        entry.name.clone()
    }
}

/// What the picker shows: which list, the filter text and the highlighted package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PickerState {
    pub kind: PackageKind,
    pub filter: String,
    pub selected: Option<String>,
}

impl PickerState {
    /// Shows `kind`'s list. A selection belongs to the list it was made in.
    pub fn set_kind(&mut self, kind: PackageKind) {
        if self.kind != kind {
            self.kind = kind;
            self.selected = None;
        }
    }

    /// The selection, if it is one of the entries now listed.
    pub fn addable(&self, catalog: &Catalog) -> Option<String> {
        let selected = self.selected.as_ref()?;
        filter_entries(catalog.entries(self.kind), &self.filter).into_iter().any(|entry| entry.package == *selected).then(|| selected.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(inputs: usize, has_progress: bool) -> PackageInfo {
        PackageInfo { name: "n".to_owned(), description: String::new(), inputs, has_progress, params: Vec::new() }
    }

    #[test]
    fn one_input_is_an_effect_and_two_with_progress_a_transition() {
        assert_eq!(classify(&info(1, false)), Some(PackageKind::Effect));
        assert_eq!(classify(&info(1, true)), Some(PackageKind::Effect));
        assert_eq!(classify(&info(2, true)), Some(PackageKind::Transition));
    }

    #[test]
    fn other_input_counts_are_not_offered() {
        assert_eq!(classify(&info(2, false)), None);
        assert_eq!(classify(&info(3, true)), None);
    }

    fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("gpufx-packages-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for package in ["packages/gpufx/group/blur", "packages/gpufx/group/nested/wipe", "packages/gpufx/group/broken"] {
            std::fs::create_dir_all(root.join(package)).unwrap();
            std::fs::write(root.join(package).join("manifest.json"), "{}").unwrap();
        }
        root
    }

    fn fake_read(path: &Path) -> Result<PackageInfo, String> {
        match path.file_name().unwrap().to_str().unwrap() {
            "blur" => Ok(PackageInfo { name: "Blur".into(), description: String::new(), inputs: 1, has_progress: false, params: Vec::new() }),
            "wipe" => Ok(PackageInfo { name: "Wipe".into(), description: String::new(), inputs: 2, has_progress: true, params: Vec::new() }),
            _ => Err("bad manifest".to_owned()),
        }
    }

    #[test]
    fn a_scan_sorts_packages_by_kind_and_keeps_relative_names() {
        let root = fixture("scan");
        let catalog = scan(std::slice::from_ref(&root), fake_read);
        assert_eq!(catalog.effects.iter().map(|e| e.package.as_str()).collect::<Vec<_>>(), vec!["packages/gpufx/group/blur"]);
        assert_eq!(catalog.transitions.iter().map(|e| e.package.as_str()).collect::<Vec<_>>(), vec!["packages/gpufx/group/nested/wipe"]);
        assert_eq!(catalog.problems, vec![PackageProblem { package: "packages/gpufx/group/broken".to_owned(), message: "bad manifest".to_owned() }]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_package_that_panics_is_a_problem_not_a_crash() {
        let root = fixture("panic");
        let catalog = scan(std::slice::from_ref(&root), |path| {
            assert!(path.file_name().unwrap() != "broken", "deliberate test panic");
            fake_read(path)
        });
        assert_eq!(catalog.problems.len(), 1);
        assert_eq!(catalog.effects.len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_filter_matches_every_word_in_name_or_package() {
        let entries = vec![
            PackageEntry { package: "packages/gpufx/capcut/snow".into(), name: "Snow Fall".into(), description: String::new(), kind: PackageKind::Effect, params: Vec::new() },
            PackageEntry { package: "packages/gpufx/ffmpeg/blur".into(), name: "Box Blur".into(), description: String::new(), kind: PackageKind::Effect, params: Vec::new() },
        ];
        assert_eq!(filter_entries(&entries, "").len(), 2);
        assert_eq!(filter_entries(&entries, "BLUR")[0].name, "Box Blur");
        assert_eq!(filter_entries(&entries, "fall capcut")[0].name, "Snow Fall");
        assert!(filter_entries(&entries, "snow blur").is_empty());
    }

    #[test]
    fn the_shipped_packages_are_read_and_sorted_into_both_lists() {
        let catalog = scan(&crate::project_file::package_roots(), d3d12_backend::read_package_info);
        assert!(catalog.effects.iter().any(|entry| short_name(&entry.package) == "grayscale"), "{} effects", catalog.effects.len());
        assert!(catalog.transitions.iter().any(|entry| short_name(&entry.package) == "fade-grays"), "{} transitions", catalog.transitions.len());
        assert!(catalog.effects.len() + catalog.transitions.len() > 100, "the list must need its scroll bar");
        assert!(catalog.effects.iter().chain(&catalog.transitions).all(|entry| entry.package.starts_with("packages/gpufx/")));
    }

    #[test]
    fn the_parameters_of_a_shipped_package_come_from_its_manifest() {
        let catalog = scan(&crate::project_file::package_roots(), d3d12_backend::read_package_info);
        let params = catalog.params_of("packages/gpufx/ffmpeg/box-blur").expect("box-blur is shipped");
        let radius = params.iter().find(|param| param.name == "radius").unwrap();
        assert_eq!((radius.kind, radius.min, radius.max), (d3d12_backend::ParamKind::Int, Some(0.0), Some(64.0)));
        assert_eq!(radius.default, Some(serde_json::json!(2)));
        assert_eq!(catalog.params_of("packages/gpufx/none"), None);
    }

    fn picker_catalog() -> Catalog {
        let entry = |package: &str, name: &str, kind| PackageEntry { package: package.into(), name: name.into(), description: String::new(), kind, params: Vec::new() };
        Catalog {
            loaded: true,
            effects: vec![entry("packages/gpufx/a/blur", "Blur", PackageKind::Effect)],
            transitions: vec![entry("packages/gpufx/a/mix", "Mix", PackageKind::Transition)],
            problems: Vec::new(),
        }
    }

    #[test]
    fn changing_the_tab_drops_the_selection() {
        let mut picker = PickerState { selected: Some("packages/gpufx/a/blur".to_owned()), ..PickerState::default() };
        picker.set_kind(PackageKind::Effect);
        assert_eq!(picker.selected.as_deref(), Some("packages/gpufx/a/blur"), "the tab did not change");
        picker.set_kind(PackageKind::Transition);
        assert_eq!(picker.selected, None);
    }

    #[test]
    fn only_a_listed_entry_of_the_current_kind_can_be_added() {
        let catalog = picker_catalog();
        let mut picker = PickerState { selected: Some("packages/gpufx/a/blur".to_owned()), ..PickerState::default() };
        assert_eq!(picker.addable(&catalog).as_deref(), Some("packages/gpufx/a/blur"));
        picker.kind = PackageKind::Transition;
        assert_eq!(picker.addable(&catalog), None, "an effect cannot be added as a transition");
        picker.kind = PackageKind::Effect;
        picker.filter = "mix".to_owned();
        assert_eq!(picker.addable(&catalog), None, "a package hidden by the filter cannot be added");
    }

    #[test]
    fn a_shared_name_shows_the_group_and_a_unique_one_does_not() {
        let entry = |package: &str, name: &str| PackageEntry { package: package.into(), name: name.into(), description: String::new(), kind: PackageKind::Effect, params: Vec::new() };
        let entries = vec![entry("packages/gpufx/capcut/blur", "Blur"), entry("packages/gpufx/ffmpeg/deep/blur", "blur"), entry("packages/gpufx/capcut/snow", "Snow")];
        let duplicates = duplicated_names(&entries);
        assert_eq!(list_label(&entries[0], &duplicates), "Blur (capcut)");
        assert_eq!(list_label(&entries[1], &duplicates), "blur (ffmpeg/deep)");
        assert_eq!(list_label(&entries[2], &duplicates), "Snow");
    }

    #[test]
    fn a_package_is_shown_by_its_last_part() {
        assert_eq!(short_name("packages/gpufx/ffmpeg/mix"), "mix");
        assert_eq!(short_name("mix"), "mix");
    }

    #[test]
    fn a_zip_is_shown_without_its_extension() {
        assert_eq!(short_name("packages/gpufx/user/io.example.iris.zip"), "io.example.iris");
        assert_eq!(short_name("iris.ZIP"), "iris");
        assert_eq!(short_name("a.zip/iris"), "iris", "only the last part counts");
        assert_eq!(short_name("packages/gpufx/ffmpeg/zip"), "zip", "a name that is only those letters");
    }

    #[test]
    fn a_name_found_in_two_roots_is_listed_once_from_the_first() {
        let (first, second) = (fixture("first-root"), fixture("second-root"));
        let catalog = scan(&[first.clone(), second.clone()], fake_read);
        assert_eq!(catalog.effects.len(), 1, "as a project resolves it");
        assert_eq!(catalog.problems.len(), 1);
        std::fs::remove_dir_all(&first).unwrap();
        std::fs::remove_dir_all(&second).unwrap();
    }
}
