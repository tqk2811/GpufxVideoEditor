//! The font packs installed on this machine, for the font list of the text inspector and the agent.
//! No window here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use d3d12_backend::ProjectPaths;
use engine::FontRef;

use crate::audio_library::LibraryProblem;
use crate::font_install::{check_font_fields, parse_font_manifest, FONT_TYPE};

/// One font of a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryFont {
    /// The id of its pack, lower-cased as its folder is.
    pub pack: String,
    pub pack_name: String,
    /// The font file inside the pack, with forward slashes, as a [`FontRef::Pack`] names it.
    pub file: String,
    /// The name the manifest gives it.
    pub name: String,
    /// The family of the file's first face, or `None` when it did not read as a font.
    pub family: Option<String>,
    /// Where the file is.
    pub path: PathBuf,
}

impl LibraryFont {
    /// The font a text clip stores to draw with this one.
    pub fn font_ref(&self) -> FontRef {
        FontRef::Pack { pack: self.pack.clone(), file: self.file.clone() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontLibrary {
    /// Every font of every pack, packs by name and fonts as their manifest lists them.
    pub fonts: Vec<LibraryFont>,
    pub problems: Vec<LibraryProblem>,
    /// The first scan has finished.
    pub loaded: bool,
}

impl FontLibrary {
    /// The font `pack` and `file` name, the id compared ignoring case.
    pub fn find(&self, pack: &str, file: &str) -> Option<&LibraryFont> {
        self.fonts.iter().find(|font| font.pack.eq_ignore_ascii_case(pack) && font.file == file)
    }

    /// The font of `font` when it is a font of an installed pack.
    pub fn find_ref(&self, font: &FontRef) -> Option<&LibraryFont> {
        match font {
            FontRef::Pack { pack, file } => self.find(pack, file),
            _ => None,
        }
    }
}

fn read_pack(root: &Path, folder: &Path, library: &mut FontLibrary) -> Option<Vec<LibraryFont>> {
    let label = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let mut problem = |message: String| library.problems.push(LibraryProblem { pack: label.clone(), message });
    let bytes = match std::fs::read(folder.join("manifest.json")) {
        Ok(bytes) => bytes,
        Err(error) => {
            problem(error.to_string());
            return None;
        }
    };
    let manifest = match parse_font_manifest(&bytes).and_then(|manifest| check_font_fields(&manifest).map(|()| manifest)) {
        Ok(manifest) => manifest,
        Err(error) => {
            problem(format!("{error:?}"));
            return None;
        }
    };
    if manifest.id.to_ascii_lowercase() != label.to_ascii_lowercase() {
        problem(format!("its manifest does not match the folder ({})", manifest.id));
        return None;
    }
    // Each font goes through what drawing a text goes through, so nothing listed can leave the pack.
    let paths = ProjectPaths::roots_only(Path::new("library.json"), vec![root.to_path_buf()]);
    let pack = manifest.id.to_ascii_lowercase();
    let mut fonts = Vec::new();
    for item in manifest.items {
        let path = match paths.font_pack_path(&pack, &item.file) {
            Ok(path) => path,
            Err(error) => {
                problem(error.to_string());
                continue;
            }
        };
        // This scan runs off the UI thread.
        let family = gpufx_text::font_file_family(&path);
        if family.is_none() {
            problem(format!("{} is not a readable font", item.file));
        }
        fonts.push(LibraryFont { pack: pack.clone(), pack_name: manifest.name.clone(), file: item.file, name: item.name, family, path });
    }
    Some(fonts)
}

/// Reads the packs under every root. A pack in an earlier root hides one of the same id in a later
/// one, as it does when a text resolves it.
pub fn scan_font_packs(roots: &[PathBuf]) -> FontLibrary {
    let mut library = FontLibrary { loaded: true, ..FontLibrary::default() };
    let mut seen: HashSet<String> = HashSet::new();
    let mut packs = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root.join("packages").join(FONT_TYPE)) else { continue };
        let mut folders: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
        folders.sort();
        for folder in folders {
            let id = folder.file_name().map_or_else(String::new, |name| name.to_string_lossy().to_ascii_lowercase());
            if seen.contains(&id) {
                continue;
            }
            if let Some(fonts) = read_pack(root, &folder, &mut library) {
                seen.insert(id);
                packs.push(fonts);
            }
        }
    }
    packs.sort_by_key(|fonts| fonts.first().map(|font| font.pack_name.to_lowercase()));
    library.fonts = packs.into_iter().flatten().collect();
    library
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-font-library-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_pack(root: &Path, folder: &str, name: &str, files: &[&str]) {
        let path = root.join("packages/font").join(folder);
        std::fs::create_dir_all(path.join("fonts")).unwrap();
        for file in files {
            std::fs::write(path.join(file), b"not a font").unwrap();
        }
        let manifest = format!(r#"{{"format":1,"type":"font","id":"{folder}","name":"{name}","version":"1.0.0","license":{{"spdx":"OFL-1.1","files":["OFL.txt"]}},"items":[{{"id":"a","file":"fonts/a.ttf","name":"A"}},{{"id":"b","file":"fonts/b.otf","name":"B"}}]}}"#);
        std::fs::write(path.join("manifest.json"), manifest).unwrap();
    }

    #[test]
    fn a_scan_reads_each_pack_once_and_reports_what_is_broken() {
        let dir = scratch("scan");
        let (shipped, user) = (dir.join("shipped"), dir.join("user"));
        write_pack(&shipped, "shared", "From Shipped", &["fonts/a.ttf", "fonts/b.otf"]);
        write_pack(&user, "shared", "From User", &["fonts/a.ttf", "fonts/b.otf"]);
        write_pack(&user, "other", "Another", &["fonts/a.ttf"]);
        std::fs::create_dir_all(user.join("packages/font/broken")).unwrap();
        std::fs::write(user.join("packages/font/broken/manifest.json"), b"not json").unwrap();

        let library = scan_font_packs(&[shipped.clone(), user]);

        assert!(library.loaded);
        let fonts: Vec<(&str, &str)> = library.fonts.iter().map(|font| (font.pack_name.as_str(), font.file.as_str())).collect();
        assert_eq!(fonts, vec![("Another", "fonts/a.ttf"), ("From Shipped", "fonts/a.ttf"), ("From Shipped", "fonts/b.otf")], "packs by name, the first root wins and a missing file is left out");
        let found = library.find("SHARED", "fonts/b.otf").unwrap();
        assert_eq!(found.path, shipped.join("packages/font/shared/fonts").join("b.otf"));
        assert_eq!(found.family, None, "the bytes are not a font");
        assert_eq!(library.find_ref(&found.font_ref()), Some(found));
        assert_eq!(library.find_ref(&FontRef::System { family: "A".to_owned() }), None);
        // The broken manifest, the missing file and the three files that are not fonts.
        assert_eq!(library.problems.len(), 5, "{:?}", library.problems);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The editor finds a pack's font where the compositor does, before the scan of the library has
    /// finished, so no missing-font warning flashes while the editor opens.
    #[test]
    fn the_editor_resolves_a_pack_font_through_the_package_roots_before_the_scan() {
        let dir = scratch("resolve");
        write_pack(&dir, "open-fonts", "Open Fonts", &["fonts/a.ttf"]);
        let mut state = crate::editor::EditorState::default();
        assert!(!state.font_library.loaded, "no scan yet");
        state.font_paths = ProjectPaths::roots_only(Path::new(crate::editor::FONT_PATHS_FILE), vec![dir.clone()]);
        let mut text = engine::TextContent::new("Pack", 0.1);
        text.font = FontRef::Pack { pack: "Open-Fonts".to_owned(), file: "fonts/a.ttf".to_owned() };
        let expected = state.font_paths.font_pack_path("open-fonts", "fonts/a.ttf").unwrap();
        assert_eq!(state.resolved_font(&text).font, FontRef::File { path: expected.to_string_lossy().into_owned() });
        text.font = FontRef::Pack { pack: "open-fonts".to_owned(), file: "fonts/missing.ttf".to_owned() };
        assert_eq!(state.resolved_font(&text).font, text.font, "a font not there stays, to be named as missing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pack made of a Windows font installs, lists its family and draws with it rather than the
    /// default. Skipped without the font, or fails then when `GPUFX_REQUIRE_FONTS=1`.
    #[test]
    fn a_pack_of_a_real_font_installs_lists_its_family_and_draws_with_it() {
        let windows_font = PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts").join("arial.ttf");
        let Ok(font) = std::fs::read(&windows_font) else {
            assert!(std::env::var("GPUFX_REQUIRE_FONTS").is_ok_and(|value| value != "1") || std::env::var("GPUFX_REQUIRE_FONTS").is_err(), "GPUFX_REQUIRE_FONTS=1 but {} is missing", windows_font.display());
            eprintln!("skipped: {} is missing", windows_font.display());
            return;
        };
        let dir = scratch("real");
        let root = dir.join("root");
        let manifest = r#"{"format":1,"type":"font","id":"Test-Fonts","name":"Test Fonts","version":"1.0.0","license":{"spdx":"OFL-1.1","files":["OFL.txt"]},"items":[{"id":"arial","file":"fonts/arial.ttf","name":"Arial Regular"}]}"#;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in [("manifest.json", manifest.as_bytes()), ("OFL.txt", b"test only".as_slice()), ("fonts/arial.ttf", font.as_slice())] {
            writer.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut writer, content).unwrap();
        }
        let zip_path = dir.join("fonts.zip");
        std::fs::write(&zip_path, writer.finish().unwrap().into_inner()).unwrap();
        crate::package_import::install_zip(&zip_path, &root, false).unwrap();

        let library = scan_font_packs(std::slice::from_ref(&root));
        assert!(library.problems.is_empty(), "{:?}", library.problems);
        let font = library.find("test-fonts", "fonts/arial.ttf").expect("listed");
        assert_eq!(font.family.as_deref(), Some("Arial"));

        let mut state = crate::editor::EditorState::default();
        state.font_library = library.clone();
        let mut text = engine::TextContent::new("Pack", 0.1);
        text.font = font.font_ref();
        let mut rasterizer = gpufx_text::TextRasterizer::with_font_files(&[]);

        state.font_paths = ProjectPaths::roots_only(Path::new(crate::editor::FONT_PATHS_FILE), vec![root.clone()]);
        let resolved = state.resolved_font(&text);
        assert_eq!(resolved.font, FontRef::File { path: font.path.to_string_lossy().into_owned() });
        assert_eq!(rasterizer.missing_font(&resolved), None, "drawn with the pack's font");
        let image = rasterizer.rasterize(&resolved, 720, 1.0);
        assert!(!image.no_font && image.width > 0, "only the pack's font is loaded, and it draws");

        state.font_paths = ProjectPaths::roots_only(Path::new(crate::editor::FONT_PATHS_FILE), Vec::new());
        assert_eq!(state.resolved_font(&text).font, text.font, "a pack not installed stays, to be named as missing");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
