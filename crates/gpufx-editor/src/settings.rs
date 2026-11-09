//! What the user chose for this machine, in a file of their own and never in a project.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::i18n::Language;
use crate::layout::PanelSizes;

const FOLDER: &str = "gpufx-editor";
const FILE: &str = "settings.json";
const LANGUAGE_KEY: &str = "language";
const PANELS_KEY: &str = "panels";

/// `%APPDATA%\gpufx-editor\settings.json`, when the variable is set.
pub fn settings_file() -> Option<PathBuf> {
    let folder = std::env::var_os("APPDATA").filter(|value| !value.is_empty())?;
    Some(PathBuf::from(folder).join(FOLDER).join(FILE))
}

fn read_settings(file: &Path) -> Map<String, Value> {
    std::fs::read_to_string(file).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok()).and_then(|value| value.as_object().cloned()).unwrap_or_default()
}

/// The language saved in `file`, if it names one the editor has.
fn saved_language(file: &Path) -> Option<Language> {
    read_settings(file).get(LANGUAGE_KEY).and_then(Value::as_str).and_then(Language::from_code)
}

/// The language saved in `file`, or English when the user has not chosen one.
fn language_or_default(file: Option<&Path>) -> Language {
    file.and_then(saved_language).unwrap_or_default()
}

/// The language to start in: the saved choice, else English.
pub fn startup_language() -> Language {
    language_or_default(settings_file().as_deref())
}

/// The temporary file of this process, so that another editor saving at the same moment has its
/// own and a failure here cannot delete it.
fn temporary_file(file: &Path) -> PathBuf {
    file.with_extension(format!("json.{}.tmp", std::process::id()))
}

/// Records `value` under `key` in `file`, through a temporary file and keeping any other setting.
fn write_setting(file: &Path, key: &str, value: Value) -> Result<(), String> {
    let mut settings = read_settings(file);
    settings.insert(key.to_owned(), value);
    let text = serde_json::to_string_pretty(&Value::Object(settings)).map_err(|e| e.to_string())?;
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    }
    let temporary = temporary_file(file);
    std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, file)).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        e.to_string()
    })
}

fn write_language(file: &Path, language: Language) -> Result<(), String> {
    write_setting(file, LANGUAGE_KEY, json!(language.code()))
}

/// Saves the choice in the settings file of this user.
pub fn save_language(language: Language) -> Result<(), String> {
    let file = settings_file().ok_or("APPDATA is not set")?;
    write_language(&file, language)
}

fn panel_size(panels: &Value, name: &str) -> Option<f32> {
    panels.get(name).and_then(Value::as_f64).map(|size| size as f32).filter(|size| size.is_finite() && *size > 0.0)
}

/// The dragged panel sizes in `file`. Anything unusable counts as not dragged.
fn saved_panel_sizes(file: &Path) -> PanelSizes {
    let settings = read_settings(file);
    let Some(panels) = settings.get(PANELS_KEY) else { return PanelSizes::default() };
    PanelSizes { left: panel_size(panels, "left"), right: panel_size(panels, "right"), timeline: panel_size(panels, "timeline") }
}

fn write_panel_sizes(file: &Path, sizes: &PanelSizes) -> Result<(), String> {
    let mut panels = Map::new();
    for (name, size) in [("left", sizes.left), ("right", sizes.right), ("timeline", sizes.timeline)] {
        if let Some(size) = size {
            panels.insert(name.to_owned(), json!(size));
        }
    }
    write_setting(file, PANELS_KEY, Value::Object(panels))
}

/// The panel sizes to start with.
pub fn startup_panel_sizes() -> PanelSizes {
    settings_file().map_or_else(PanelSizes::default, |file| saved_panel_sizes(&file))
}

/// Saves the dragged sizes in the settings file of this user.
pub fn save_panel_sizes(sizes: &PanelSizes) -> Result<(), String> {
    let file = settings_file().ok_or("APPDATA is not set")?;
    write_panel_sizes(&file, sizes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("gpufx-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        folder.join(FILE)
    }

    #[test]
    fn the_first_run_is_english_and_a_saved_choice_wins() {
        let file = temp_file("first-run");
        assert_eq!(language_or_default(Some(&file)), Language::English);
        assert_eq!(language_or_default(None), Language::English);
        write_language(&file, Language::Vietnamese).unwrap();
        assert_eq!(language_or_default(Some(&file)), Language::Vietnamese);
    }

    #[test]
    fn panel_sizes_are_read_back_and_a_reset_panel_is_left_out() {
        let file = temp_file("panels");
        assert_eq!(saved_panel_sizes(&file), PanelSizes::default());
        let sizes = PanelSizes { left: Some(300.0), right: None, timeline: Some(250.5) };
        write_panel_sizes(&file, &sizes).unwrap();
        assert_eq!(saved_panel_sizes(&file), sizes);
        write_language(&file, Language::Vietnamese).unwrap();
        assert_eq!(saved_panel_sizes(&file), sizes, "saving the language keeps the sizes");
        write_panel_sizes(&file, &PanelSizes::default()).unwrap();
        assert_eq!(saved_panel_sizes(&file), PanelSizes::default());
        assert_eq!(saved_language(&file), Some(Language::Vietnamese), "saving the sizes keeps the language");
    }

    #[test]
    fn unusable_panel_sizes_count_as_not_dragged() {
        let file = temp_file("panels-bad");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"panels":{"left":"wide","right":-5,"timeline":1e300}}"#).unwrap();
        assert_eq!(saved_panel_sizes(&file), PanelSizes::default());
        std::fs::write(&file, r#"{"panels":3}"#).unwrap();
        assert_eq!(saved_panel_sizes(&file), PanelSizes::default());
    }

    #[test]
    fn a_saved_language_is_read_back() {
        let file = temp_file("round-trip");
        assert_eq!(saved_language(&file), None);
        write_language(&file, Language::Vietnamese).unwrap();
        assert_eq!(saved_language(&file), Some(Language::Vietnamese));
        write_language(&file, Language::English).unwrap();
        assert_eq!(saved_language(&file), Some(Language::English));
    }

    #[test]
    fn a_broken_or_unknown_file_counts_as_no_choice() {
        let file = temp_file("broken");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(saved_language(&file), None);
        std::fs::write(&file, r#"{"language":"xx"}"#).unwrap();
        assert_eq!(saved_language(&file), None);
    }

    #[test]
    fn a_failed_save_removes_only_the_temporary_file_of_this_process() {
        let file = temp_file("foreign");
        // A folder at the path of the file makes the final rename fail.
        std::fs::create_dir_all(&file).unwrap();
        let foreign = file.with_extension("json.tmp");
        std::fs::write(&foreign, "another editor").unwrap();
        assert!(write_language(&file, Language::English).is_err());
        assert_eq!(std::fs::read_to_string(&foreign).unwrap(), "another editor");
        assert!(!temporary_file(&file).exists());
    }

    #[test]
    fn saving_keeps_other_settings() {
        let file = temp_file("other");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"other":1}"#).unwrap();
        write_language(&file, Language::Vietnamese).unwrap();
        assert_eq!(read_settings(&file).get("other"), Some(&json!(1)));
    }
}
