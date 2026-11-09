//! What the user chose for this machine, in a file of their own and never in a project.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::i18n::Language;

const FOLDER: &str = "gpufx-editor";
const FILE: &str = "settings.json";
const LANGUAGE_KEY: &str = "language";

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

/// Records `language` in `file`, through a temporary file and keeping any other setting.
fn write_language(file: &Path, language: Language) -> Result<(), String> {
    let mut settings = read_settings(file);
    settings.insert(LANGUAGE_KEY.to_owned(), json!(language.code()));
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

/// Saves the choice in the settings file of this user.
pub fn save_language(language: Language) -> Result<(), String> {
    let file = settings_file().ok_or("APPDATA is not set")?;
    write_language(&file, language)
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
