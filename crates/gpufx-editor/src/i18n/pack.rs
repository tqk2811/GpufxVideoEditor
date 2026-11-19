//! The translations a package brings: `i18n/<code>.json` at the root of its folder or zip.
//!
//! ```json
//! {"name": "...", "description": "...", "tags": ["..."],
//!  "params": {"<param name>": {"label": "...", "description": "..."}},
//!  "items": {"<item id>": {"name": "...", "description": "...", "category": "...", "tags": ["..."]}}}
//! ```
//!
//! Every field is optional. What a file lacks, or a language with no file, shows the English text of
//! the manifest, field by field. Item ids are those of the manifest (an animation's id is also the
//! stem of `items/<id>.json`); an effect package has only `params`, keyed by the manifest's
//! parameter name. Searches match every language a pack brings, so a pack is found by its English
//! name and by any translation of it. A file that cannot be read is skipped: a translation never
//! stops a pack from loading.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use serde_json::Value;

use crate::search::SearchFields;

/// The folder of a package that holds its language files.
pub const PACK_LANGUAGE_FOLDER: &str = "i18n";
/// A language file of a pack is a few KiB; anything larger is not read.
const MAX_FILE_BYTES: u64 = 1 << 20;
/// More language files than this in one pack are not read.
const MAX_FILES: usize = 64;

/// The translated texts of a pack or of one item, in one language. A missing field is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextSet {
    pub name: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub tags: Vec<String>,
}

/// The translated label and description of one parameter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParamText {
    pub label: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PackFile {
    pack: TextSet,
    params: HashMap<String, ParamText>,
    items: HashMap<String, TextSet>,
}

/// Every language file of one pack, by code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackTranslations {
    files: Vec<(String, PackFile)>,
}

/// The translations of one thing (a pack or an item), by language code, sorted by code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Localized {
    texts: Vec<(String, TextSet)>,
}

/// The translated parameters of an effect package, by language code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParamTranslations {
    files: Vec<(String, HashMap<String, ParamText>)>,
}

fn text_of(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned)
}

fn tags_of(value: Option<&Value>) -> Vec<String> {
    value.and_then(Value::as_array).map(|tags| tags.iter().filter_map(|tag| text_of(Some(tag))).collect()).unwrap_or_default()
}

fn text_set(value: &Value) -> TextSet {
    TextSet { name: text_of(value.get("name")), description: text_of(value.get("description")), category: text_of(value.get("category")), tags: tags_of(value.get("tags")) }
}

fn usable_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 32 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn parse_file(text: &str) -> Option<PackFile> {
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let root = value.as_object()?;
    let params = root
        .get("params")
        .and_then(Value::as_object)
        .map(|params| params.iter().map(|(name, value)| (name.clone(), ParamText { label: text_of(value.get("label")), description: text_of(value.get("description")) })).collect())
        .unwrap_or_default();
    let items = root.get("items").and_then(Value::as_object).map(|items| items.iter().map(|(id, value)| (id.clone(), text_set(value))).collect()).unwrap_or_default();
    Some(PackFile { pack: text_set(&value), params, items })
}

/// The language code a path inside a pack names, when it is `i18n/<code>.json`.
pub fn language_file_code(path: &str) -> Option<&str> {
    let path = path.strip_prefix("./").unwrap_or(path);
    let (folder, file) = path.split_once(['/', '\\'])?;
    if !folder.eq_ignore_ascii_case(PACK_LANGUAGE_FOLDER) {
        return None;
    }
    let dot = file.len().checked_sub(".json".len()).filter(|&at| file.is_char_boundary(at) && file[at..].eq_ignore_ascii_case(".json"))?;
    let code = &file[..dot];
    usable_code(code).then_some(code)
}

impl PackTranslations {
    /// The language files of `(code, text)`; a file that is not a JSON object is skipped.
    pub fn from_files(files: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut files: Vec<(String, PackFile)> = files.into_iter().filter(|(code, _)| usable_code(code)).filter_map(|(code, text)| parse_file(&text).map(|file| (code, file))).take(MAX_FILES).collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files.dedup_by(|a, b| a.0 == b.0);
        Self { files }
    }

    /// The files of `<folder>/i18n`.
    pub fn read_folder(folder: &Path) -> Self {
        let Ok(entries) = std::fs::read_dir(folder.join(PACK_LANGUAGE_FOLDER)) else { return Self::default() };
        let files: Vec<(String, String)> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?.to_owned();
                let code = language_file_code(&format!("{PACK_LANGUAGE_FOLDER}/{name}"))?.to_owned();
                if entry.metadata().ok()?.len() > MAX_FILE_BYTES {
                    return None;
                }
                Some((code, std::fs::read_to_string(&path).ok()?))
            })
            .take(MAX_FILES)
            .collect();
        Self::from_files(files)
    }

    /// The files of `i18n/` inside the zip at `path`.
    pub fn read_zip(path: &Path) -> Self {
        let Ok(file) = std::fs::File::open(path) else { return Self::default() };
        let Ok(mut zip) = zip::ZipArchive::new(std::io::BufReader::new(file)) else { return Self::default() };
        let names: Vec<(String, String)> = zip.file_names().filter_map(Result::ok).filter_map(|name| language_file_code(&name).map(str::to_owned).map(|code| (name.into_owned(), code))).take(MAX_FILES).collect();
        let mut files = Vec::new();
        for (name, code) in names {
            let Ok(entry) = zip.by_name(&name) else { continue };
            let mut text = String::new();
            if entry.take(MAX_FILE_BYTES + 1).read_to_string(&mut text).is_ok() && text.len() as u64 <= MAX_FILE_BYTES {
                files.push((code, text));
            }
        }
        Self::from_files(files)
    }

    /// The files of a package at `path`: a folder or a `.zip`.
    pub fn read_package(path: &Path) -> Self {
        if path.is_dir() {
            Self::read_folder(path)
        } else {
            Self::read_zip(path)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The codes of the languages the pack brings.
    pub fn codes(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|(code, _)| code.as_str())
    }

    /// The pack's own texts.
    pub fn pack(&self) -> Localized {
        Localized { texts: self.files.iter().filter(|(_, file)| file.pack != TextSet::default()).map(|(code, file)| (code.clone(), file.pack.clone())).collect() }
    }

    /// The texts of item `id`; an id differing only in case is used when there is no exact one.
    pub fn item(&self, id: &str) -> Localized {
        let texts = self
            .files
            .iter()
            .filter_map(|(code, file)| file.items.get(id).or_else(|| file.items.iter().find(|(key, _)| key.eq_ignore_ascii_case(id)).map(|(_, texts)| texts)).map(|texts| (code.clone(), texts.clone())))
            .collect();
        Localized { texts }
    }

    /// The parameters' texts.
    pub fn params(&self) -> ParamTranslations {
        ParamTranslations { files: self.files.iter().filter(|(_, file)| !file.params.is_empty()).map(|(code, file)| (code.clone(), file.params.clone())).collect() }
    }
}

impl Localized {
    pub fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }

    /// The texts in language `code`, if the pack brings that language.
    pub fn get(&self, code: &str) -> Option<&TextSet> {
        self.texts.iter().find(|(language, _)| language == code).map(|(_, texts)| texts)
    }

    /// The name in `code`, else `english`.
    pub fn name<'a>(&'a self, code: &str, english: &'a str) -> &'a str {
        self.get(code).and_then(|texts| texts.name.as_deref()).unwrap_or(english)
    }

    /// The description in `code`, else `english`.
    pub fn description<'a>(&'a self, code: &str, english: &'a str) -> &'a str {
        self.get(code).and_then(|texts| texts.description.as_deref()).unwrap_or(english)
    }

    /// The category in `code`, else `english`.
    pub fn category<'a>(&'a self, code: &str, english: &'a str) -> &'a str {
        self.get(code).and_then(|texts| texts.category.as_deref()).unwrap_or(english)
    }

    /// The tags in `code`, else `english` when the language has none.
    pub fn tags<'a>(&'a self, code: &str, english: &'a [String]) -> &'a [String] {
        self.get(code).map(|texts| texts.tags.as_slice()).filter(|tags| !tags.is_empty()).unwrap_or(english)
    }

    /// `fields` with every language's names as more names, tags and categories as tags, and
    /// descriptions as the other fields.
    pub fn search<'a>(&'a self, mut fields: SearchFields<'a>) -> SearchFields<'a> {
        for (_, texts) in &self.texts {
            fields.names.extend(texts.name.as_deref());
            fields.tags.extend(texts.category.as_deref().into_iter().chain(texts.tags.iter().map(String::as_str)));
            fields.other.extend(texts.description.as_deref());
        }
        fields
    }
}

impl ParamTranslations {
    fn get(&self, code: &str, param: &str) -> Option<&ParamText> {
        self.files.iter().find(|(language, _)| language == code).and_then(|(_, params)| params.get(param))
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The label of `param` in `code`, if translated.
    pub fn label(&self, code: &str, param: &str) -> Option<&str> {
        self.get(code, param).and_then(|text| text.label.as_deref())
    }

    /// The description of `param` in `code`, else `english`.
    pub fn description<'a>(&'a self, code: &str, param: &str, english: &'a str) -> &'a str {
        self.get(code, param).and_then(|text| text.description.as_deref()).unwrap_or(english)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VI: &str = r#"{"name": "Mờ hộp", "tags": ["làm mờ"], "params": {"radius": {"label": "Bán kính"}}, "items": {"Pop": {"name": "Bật ra", "tags": ["bật"]}, "fade": {"description": "Hiện dần"}}}"#;

    fn translations() -> PackTranslations {
        PackTranslations::from_files([("vi".to_owned(), VI.to_owned()), ("fr".to_owned(), r#"{"name": "Flou"}"#.to_owned()), ("xx".to_owned(), "not json".to_owned())])
    }

    #[test]
    fn each_field_falls_back_to_the_english_one_by_one() {
        let translations = translations();
        assert_eq!(translations.codes().collect::<Vec<_>>(), ["fr", "vi"], "a broken file is skipped");
        let pack = translations.pack();
        assert_eq!(pack.name("vi", "Box Blur"), "Mờ hộp");
        assert_eq!(pack.description("vi", "Blurs a box"), "Blurs a box", "no description in vi");
        assert_eq!(pack.name("de", "Box Blur"), "Box Blur", "no file for de");
        assert_eq!(pack.tags("fr", &["blur".to_owned()]), ["blur"], "fr has no tags");
        let pop = translations.item("pop");
        assert_eq!(pop.name("vi", "Pop"), "Bật ra", "the id is matched ignoring case");
        let fade = translations.item("fade");
        assert_eq!((fade.name("vi", "Fade"), fade.description("vi", "")), ("Fade", "Hiện dần"));
        assert!(translations.item("missing").is_empty());
        let params = translations.params();
        assert_eq!(params.label("vi", "radius"), Some("Bán kính"));
        assert_eq!(params.label("vi", "other"), None);
        assert_eq!(params.description("vi", "radius", "Radius in pixels"), "Radius in pixels");
    }

    #[test]
    fn only_files_of_the_language_folder_are_language_files() {
        assert_eq!(language_file_code("i18n/vi.json"), Some("vi"));
        assert_eq!(language_file_code("I18N/pt-BR.JSON"), Some("pt-BR"));
        assert_eq!(language_file_code("i18n/sub/vi.json"), None);
        assert_eq!(language_file_code("items/vi.json"), None);
        assert_eq!(language_file_code("i18n/bad name.json"), None);
    }

    #[test]
    fn a_folder_and_a_zip_are_read_alike() {
        let dir = std::env::temp_dir().join(format!("gpufx-pack-i18n-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("folder/i18n")).unwrap();
        std::fs::write(dir.join("folder/i18n/vi.json"), VI).unwrap();
        let zip_path = dir.join("pack.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        zip.start_file("manifest.json", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zip, b"{}").unwrap();
        zip.start_file("i18n/vi.json", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zip, VI.as_bytes()).unwrap();
        zip.finish().unwrap();
        let folder = PackTranslations::read_package(&dir.join("folder"));
        let zipped = PackTranslations::read_package(&zip_path);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(folder.pack().name("vi", ""), "Mờ hộp");
        assert_eq!(folder, zipped);
    }

    #[test]
    fn a_search_finds_a_translated_name_tag_or_description() {
        let translations = translations();
        let pop = translations.item("pop");
        let fields = pop.search(SearchFields::new("Pop"));
        assert!(crate::search::Query::new("bat ra").score(&fields).is_some());
        assert!(crate::search::Query::new("bật").score(&fields).is_some());
        assert!(crate::search::Query::new("pop").score(&fields).is_some());
        assert!(crate::search::Query::new("mờ").score(&fields).is_none());
    }
}
