//! What the user chose for this machine, in a file of their own and never in a project.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::i18n::Language;

const FOLDER: &str = "gpufx-editor";
const FILE: &str = "settings.json";
const LANGUAGE_KEY: &str = "language";
/// The primary language id of Vietnamese in a Windows LANGID.
const LANG_VIETNAMESE: u16 = 0x2A;
const PRIMARY_LANGUAGE_MASK: u16 = 0x3FF;

#[link(name = "kernel32")]
extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
}

/// The editor language for a Windows LANGID; anything it does not have falls back to English.
fn language_of_langid(langid: u16) -> Language {
    match langid & PRIMARY_LANGUAGE_MASK {
        LANG_VIETNAMESE => Language::Vietnamese,
        _ => Language::English,
    }
}

fn system_language() -> Language {
    // SAFETY: the function takes nothing and only returns a value.
    language_of_langid(unsafe { GetUserDefaultUILanguage() })
}

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

/// The language to start in: the saved choice, else the system's, else English.
pub fn startup_language() -> Language {
    settings_file().and_then(|file| saved_language(&file)).unwrap_or_else(system_language)
}

/// Records `language` in `file`, through a temporary file and keeping any other setting.
fn write_language(file: &Path, language: Language) -> Result<(), String> {
    let mut settings = read_settings(file);
    settings.insert(LANGUAGE_KEY.to_owned(), json!(language.code()));
    let text = serde_json::to_string_pretty(&Value::Object(settings)).map_err(|e| e.to_string())?;
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    }
    let temporary = file.with_extension("json.tmp");
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
    fn a_system_language_the_editor_lacks_falls_back_to_english() {
        assert_eq!(language_of_langid(0x042A), Language::Vietnamese, "vi-VN");
        assert_eq!(language_of_langid(0x0409), Language::English, "en-US");
        assert_eq!(language_of_langid(0x040C), Language::English, "fr-FR");
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
    fn saving_keeps_other_settings() {
        let file = temp_file("other");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"other":1}"#).unwrap();
        write_language(&file, Language::Vietnamese).unwrap();
        assert_eq!(read_settings(&file).get("other"), Some(&json!(1)));
    }
}
