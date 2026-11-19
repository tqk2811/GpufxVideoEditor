//! Language files and the order they are looked up in.
//!
//! A language file is `<code>.json`: an object of string values under flat or dotted keys (nested
//! objects are read as dotted keys), and an optional `"_meta": {"name": "..."}` with the name of
//! the language in itself. Files come in layers: the English built into the binary, then each folder
//! in order (the folder beside the app, then the user's), a later folder overriding an earlier one
//! key by key. A key is looked up in the layers of the chosen language, most specific first, then
//! in the built-in English. A file that cannot be read is reported and skipped.
//!
//! Nothing here is about the editor's own messages, so the same loader can read the translations
//! a package brings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::text::{format, plural_forms, Arg, Text};

const META_KEY: &str = "_meta";

/// The strings of one language file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Strings {
    map: HashMap<String, String>,
    /// The name of the language in itself, from `_meta.name`.
    name: Option<String>,
}

impl Strings {
    /// Reads a language file. Keys whose value is not a string are left out and listed in the
    /// second value; a file that is not a JSON object is an error.
    pub fn parse(text: &str) -> Result<(Self, Vec<String>), String> {
        let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        let Value::Object(root) = value else { return Err("the file is not a JSON object".to_owned()) };
        let mut strings = Self::default();
        let mut skipped = Vec::new();
        for (key, value) in root {
            if key == META_KEY {
                strings.name = value.get("name").and_then(Value::as_str).map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned);
            } else {
                strings.flatten(key, value, &mut skipped);
            }
        }
        Ok((strings, skipped))
    }

    fn flatten(&mut self, key: String, value: Value, skipped: &mut Vec<String>) {
        match value {
            Value::String(text) => {
                self.map.insert(key, text);
            }
            Value::Object(children) => {
                for (child, value) in children {
                    self.flatten(format!("{key}.{child}"), value, skipped);
                }
            }
            _ => skipped.push(key),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// One language: its code, its name and its files, the most specific first.
#[derive(Debug, Clone)]
struct Entry {
    code: String,
    name: String,
    layers: Vec<Strings>,
}

/// Every language found, and how to put a [`Text`] into words in each.
#[derive(Debug, Clone)]
pub struct Catalog {
    english: Strings,
    /// English first, then by code.
    languages: Vec<Entry>,
    problems: Vec<String>,
}

pub const ENGLISH_CODE: &str = "en";

/// Whether `code` can name a language file: letters, digits, `-` and `_`.
fn usable_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 32 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Catalog {
    /// The built-in English and the files of `folders`, a later folder overriding an earlier one.
    /// A folder that does not exist is skipped without a word.
    pub fn load(embedded_english: &str, folders: &[PathBuf]) -> Self {
        let mut problems = Vec::new();
        let english = match Strings::parse(embedded_english) {
            Ok((strings, _)) => strings,
            Err(e) => {
                problems.push(format!("the built-in English is broken: {e}"));
                Strings::default()
            }
        };
        let mut files = Vec::new();
        for folder in folders {
            files.extend(read_folder(folder, &mut problems));
        }
        let mut catalog = Self::from_layers(english, files);
        catalog.problems.splice(0..0, problems);
        catalog
    }

    /// The built-in English and `(code, strings)` files, a later file overriding an earlier one.
    pub fn from_layers(english: Strings, files: Vec<(String, Strings)>) -> Self {
        let english_name = english.name().unwrap_or("English").to_owned();
        let mut languages = vec![Entry { code: ENGLISH_CODE.to_owned(), name: english_name, layers: Vec::new() }];
        for (code, strings) in files {
            let index = match languages.iter().position(|entry| entry.code == code) {
                Some(index) => index,
                None => {
                    languages.push(Entry { name: code.clone(), code, layers: Vec::new() });
                    languages.len() - 1
                }
            };
            let entry = &mut languages[index];
            if let Some(name) = strings.name() {
                entry.name = name.to_owned();
            }
            entry.layers.insert(0, strings);
        }
        languages[1..].sort_by(|a, b| a.code.cmp(&b.code));
        Self { english, languages, problems: Vec::new() }
    }

    /// The files that could not be read, and the values that were not text.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    pub fn len(&self) -> usize {
        self.languages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.languages.is_empty()
    }

    pub fn code(&self, language: usize) -> &str {
        &self.languages[language].code
    }

    pub fn name(&self, language: usize) -> &str {
        &self.languages[language].name
    }

    pub fn find(&self, code: &str) -> Option<usize> {
        self.languages.iter().position(|entry| entry.code == code)
    }

    /// The built-in English, whose keys are the full set.
    pub fn english(&self) -> &Strings {
        &self.english
    }

    /// The template of `key` in `language`, falling back to the built-in English. With a count, the
    /// form the count asks is looked for in every file of the language first, then the other form,
    /// then `key` itself, so a file holding only the other form does not hide the right one in a
    /// file below it. The files of English and the built-in English are one language here; for any
    /// other language the built-in English is only looked in when the language has no form at all.
    pub fn template(&self, language: usize, key: &str, count: Option<i64>) -> Option<&str> {
        let layers = self.languages.get(language).map(|entry| entry.layers.as_slice()).unwrap_or_default();
        let english = [&self.english];
        let groups: Vec<Vec<&Strings>> = if language == 0 { vec![layers.iter().chain(english).collect()] } else { vec![layers.iter().collect(), english.to_vec()] };
        groups.iter().find_map(|group| {
            count
                .into_iter()
                .flat_map(plural_forms)
                .find_map(|form| {
                    let plural = format!("{key}.{form}");
                    group.iter().find_map(|strings| strings.get(&plural))
                })
                .or_else(|| group.iter().find_map(|strings| strings.get(key)))
        })
    }

    /// `text` in `language`.
    pub fn resolve(&self, language: usize, text: &Text) -> String {
        match text {
            Text::Raw(text) => text.clone(),
            Text::Key { key, args, fallback, .. } => match self.template(language, key, text.plural_count()) {
                Some(template) => format(template, |name| {
                    args.iter().find(|(arg, _)| *arg == name).map(|(_, value)| match value {
                        Arg::Plain(text) => text.clone(),
                        Arg::Count(count) => count.to_string(),
                        Arg::Text(text) => self.resolve(language, text),
                    })
                }),
                None => fallback.clone().unwrap_or_else(|| key.to_string()),
            },
        }
    }
}

/// The language files of `folder`, by name. Files that cannot be read go to `problems`.
fn read_folder(folder: &Path, problems: &mut Vec<String>) -> Vec<(String, Strings)> {
    let Ok(entries) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("json")) && path.is_file()).collect();
    paths.sort();
    let mut files = Vec::new();
    for path in paths {
        let Some(code) = path.file_stem().and_then(|stem| stem.to_str()) else { continue };
        if !usable_code(code) {
            problems.push(format!("{}: the file name is not a language code", path.display()));
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                problems.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        match Strings::parse(&text) {
            Ok((strings, skipped)) => {
                if !skipped.is_empty() {
                    problems.push(format!("{}: these keys are not text and were left out: {}", path.display(), skipped.join(", ")));
                }
                files.push((code.to_owned(), strings));
            }
            Err(e) => problems.push(format!("{}: {e}", path.display())),
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(json: &str) -> Strings {
        Strings::parse(json).unwrap().0
    }

    fn english() -> Strings {
        strings(r#"{"_meta": {"name": "English"}, "hello": "Hello {name}", "bye": "Bye", "files.one": "{count} file", "files.other": "{count} files", "nested": "In {inner}", "inner": "the box"}"#)
    }

    fn catalog(files: Vec<(&str, &str)>) -> Catalog {
        Catalog::from_layers(english(), files.into_iter().map(|(code, json)| (code.to_owned(), strings(json))).collect())
    }

    fn say(catalog: &Catalog, code: &str, text: &Text) -> String {
        catalog.resolve(catalog.find(code).unwrap(), text)
    }

    #[test]
    fn nested_objects_read_as_dotted_keys_and_meta_is_not_a_key() {
        let strings = strings(r#"{"_meta": {"name": " Tiếng Việt "}, "a": {"b": "x", "c": {"d": "y"}}, "e.f": "z"}"#);
        assert_eq!(strings.get("a.b"), Some("x"));
        assert_eq!(strings.get("a.c.d"), Some("y"));
        assert_eq!(strings.get("e.f"), Some("z"));
        assert_eq!(strings.get("_meta.name"), None);
        assert_eq!(strings.name(), Some("Tiếng Việt"));
    }

    #[test]
    fn values_that_are_not_text_are_left_out_and_listed() {
        let (strings, skipped) = Strings::parse(r#"{"a": "x", "b": 3, "c": ["y"]}"#).unwrap();
        assert_eq!(strings.get("a"), Some("x"));
        assert_eq!(strings.get("b"), None);
        assert_eq!(skipped, vec!["b".to_owned(), "c".to_owned()]);
        assert!(Strings::parse("[]").is_err());
        assert!(Strings::parse("{").is_err());
    }

    #[test]
    fn plural_forms_and_nested_texts_resolve() {
        let catalog = catalog(vec![("vi", r#"{"files.other": "{count} tệp", "inner": "hộp"}"#)]);
        let files = |count| Text::key("files").count("count", count);
        assert_eq!(say(&catalog, "en", &files(1)), "1 file");
        assert_eq!(say(&catalog, "en", &files(0)), "0 files");
        assert_eq!(say(&catalog, "vi", &files(1)), "1 tệp", "a language with only `other` uses it for one too");
        assert_eq!(say(&catalog, "vi", &files(5)), "5 tệp");
        let nested = Text::key("nested").arg("inner", Text::key("inner"));
        assert_eq!(say(&catalog, "vi", &nested), "In hộp", "the nested text is in the chosen language, the missing outer key in English");
    }

    #[test]
    fn only_one_form_given_serves_every_count() {
        let catalog = catalog(vec![("xx", r#"{"files.one": "{count} only-one"}"#)]);
        assert_eq!(say(&catalog, "xx", &Text::key("files").count("count", 7)), "7 only-one");
    }

    #[test]
    fn a_missing_key_falls_back_to_english_then_to_the_key() {
        let catalog = catalog(vec![("vi", r#"{"bye": "Tạm biệt"}"#)]);
        assert_eq!(say(&catalog, "vi", &Text::key("bye")), "Tạm biệt");
        assert_eq!(say(&catalog, "vi", &Text::key("hello").arg("name", "An")), "Hello An");
        assert_eq!(say(&catalog, "vi", &Text::key("nowhere")), "nowhere");
        assert_eq!(say(&catalog, "vi", &Text::key("nowhere").or_raw("raw id")), "raw id");
        assert_eq!(say(&catalog, "vi", &Text::raw("{as is}")), "{as is}");
    }

    #[test]
    fn a_later_file_of_a_code_overrides_key_by_key() {
        let catalog = catalog(vec![
            ("vi", r#"{"_meta": {"name": "Tiếng Việt"}, "bye": "Tạm biệt", "hello": "Chào {name}"}"#),
            ("vi", r#"{"bye": "Bai bai"}"#),
            ("en", r#"{"bye": "See you"}"#),
        ]);
        assert_eq!(say(&catalog, "vi", &Text::key("bye")), "Bai bai", "the user's file wins");
        assert_eq!(say(&catalog, "vi", &Text::key("hello").arg("name", "An")), "Chào An", "the shipped file keeps the keys the user's lacks");
        assert_eq!(catalog.name(catalog.find("vi").unwrap()), "Tiếng Việt", "a file without a name keeps the name already found");
        assert_eq!(say(&catalog, "en", &Text::key("bye")), "See you", "English can be overridden too");
    }

    #[test]
    fn a_new_code_adds_a_language_named_by_its_meta_or_its_code() {
        let catalog = catalog(vec![("fr", r#"{"_meta": {"name": "Français"}, "bye": "Au revoir"}"#), ("de", r#"{"bye": "Tschüss"}"#)]);
        let codes: Vec<&str> = (0..catalog.len()).map(|language| catalog.code(language)).collect();
        assert_eq!(codes, ["en", "de", "fr"], "English first, then by code");
        assert_eq!(catalog.name(catalog.find("fr").unwrap()), "Français");
        assert_eq!(catalog.name(catalog.find("de").unwrap()), "de");
        assert_eq!(catalog.name(0), "English");
    }

    #[test]
    fn folders_load_in_order_and_a_broken_file_is_reported_and_skipped() {
        let root = std::env::temp_dir().join(format!("gpufx-i18n-test-{}", std::process::id()));
        let shipped = root.join("shipped");
        let user = root.join("user");
        std::fs::create_dir_all(&shipped).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(shipped.join("vi.json"), "\u{feff}{\"_meta\": {\"name\": \"Tiếng Việt\"}, \"bye\": \"Tạm biệt\", \"hello\": \"Chào {name}\"}").unwrap();
        std::fs::write(user.join("vi.json"), r#"{"bye": "Bai bai", "count": 3}"#).unwrap();
        std::fs::write(user.join("fr.json"), r#"{"bye": "Au revoir""#).unwrap();
        std::fs::write(user.join("bad name.json"), r#"{}"#).unwrap();
        std::fs::write(user.join("notes.txt"), "not a language").unwrap();
        let english = r#"{"hello": "Hello {name}", "bye": "Bye"}"#;
        let catalog = Catalog::load(english, &[shipped, user, root.join("missing")]);
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(catalog.len(), 2, "fr is broken, so only en and vi: {:?}", catalog.problems());
        assert_eq!(say(&catalog, "vi", &Text::key("bye")), "Bai bai");
        assert_eq!(say(&catalog, "vi", &Text::key("hello").arg("name", "An")), "Chào An");
        assert_eq!(catalog.problems().len(), 3, "{:?}", catalog.problems());
        assert!(catalog.problems().iter().any(|problem| problem.contains("fr.json")));
        assert!(catalog.problems().iter().any(|problem| problem.contains("bad name.json")));
        assert!(catalog.problems().iter().any(|problem| problem.contains("count")));
    }

    #[test]
    fn an_upper_layer_with_only_the_other_plural_form_does_not_hide_the_right_form_below() {
        let catalog = catalog(vec![("en", r#"{"files.one": "{count} single"}"#)]);
        assert_eq!(say(&catalog, "en", &Text::key("files").count("count", 5)), "5 files", "the built-in other form wins over the user's one form");
        assert_eq!(say(&catalog, "en", &Text::key("files").count("count", 1)), "1 single");
    }

    #[test]
    fn a_broken_built_in_english_is_reported_not_a_crash() {
        let catalog = Catalog::load("{", &[]);
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog.problems().len(), 1);
        assert_eq!(catalog.resolve(0, &Text::key("bye")), "bye");
    }
}
