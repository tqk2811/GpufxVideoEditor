//! Finds the gpufx effect package the `effect` of a clip animation item names, by its manifest
//! id, and checks the item against the package: the progress parameter must be a float of the
//! package and every static parameter must exist with a value of its type.
//!
//! A package that is not installed is not an error of the pack: the item plays its curve without
//! the effect, and a problem says so. A package whose parameters do not fit is left out the same
//! way. The editor and an export read the same way, so both play the same effects.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use engine::{PresetEffect, PACK_PRESET_SEPARATOR};
use serde_json::Value;

use crate::clip_animation_pack::{ClipAnimationManifest, ItemEffect, PackReadProblem};
use crate::packages::{read_package_info, PackageInfo, ParamKind};

/// How deep the groups of `packages/gpufx` may nest, as for the editor's package list.
const MAX_DEPTH: usize = 8;
/// The largest manifest read to learn the id of a package; gpufx sets no limit of its own, and a
/// real manifest is a few kilobytes.
const MAX_MANIFEST_BYTES: u64 = 1 << 20;

/// The effects of the items that can play, by the reference a clip names (`"<pack>/<item>"`), and
/// the items whose effect cannot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemEffects {
    pub ready: HashMap<String, PresetEffect>,
    pub problems: Vec<PackReadProblem>,
}

fn is_package(path: &Path) -> bool {
    path.join("manifest.json").is_file() || (path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
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

/// Reads all of `reader` when it holds at most [`MAX_MANIFEST_BYTES`].
fn read_limited(reader: impl Read) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_MANIFEST_BYTES).then_some(bytes)
}

/// The `id` of the manifest of the package at `path`, a folder or a zip, read without the rest.
/// A manifest above [`MAX_MANIFEST_BYTES`] is not read.
fn package_id(path: &Path) -> Option<String> {
    let bytes = if path.is_dir() {
        read_limited(std::fs::File::open(path.join("manifest.json")).ok()?)?
    } else {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
        let entry = archive.by_name("manifest.json").ok()?;
        if entry.size() > MAX_MANIFEST_BYTES {
            return None;
        }
        read_limited(entry)?
    };
    let manifest: Value = serde_json::from_slice(&bytes).ok()?;
    manifest.get("id")?.as_str().map(str::to_ascii_lowercase)
}

/// The gpufx packages under the roots with an id of `wanted` (lower case) by that id: the name an
/// effect gives the package and its path. The name is the absolute path, so a project's paths,
/// which look beside the project and then in each root by a relative name, cannot find another
/// package under the same relative path. The first root holding an id wins, and the search stops
/// once every wanted id is found.
fn index_packages(roots: &[PathBuf], wanted: &std::collections::HashSet<String>) -> HashMap<String, (String, PathBuf)> {
    let mut index = HashMap::new();
    for root in roots {
        let mut found = Vec::new();
        collect(&root.join("packages").join("gpufx"), 0, &mut found);
        for path in found {
            if index.len() == wanted.len() {
                return index;
            }
            let Some(id) = package_id(&path).filter(|id| wanted.contains(id)) else { continue };
            let Ok(absolute) = std::path::absolute(&path) else { continue };
            index.entry(id).or_insert((absolute.to_string_lossy().into_owned(), path));
        }
    }
    index
}

/// Whether `value` fits a parameter of `kind`.
fn fits(kind: ParamKind, value: &Value) -> bool {
    let numbers = |count: usize| value.as_array().is_some_and(|items| items.len() == count);
    match kind {
        ParamKind::Float => value.is_number(),
        ParamKind::Int => value.as_f64().is_some_and(|number| number.fract() == 0.0),
        ParamKind::Bool => value.is_boolean(),
        ParamKind::Vec2 => numbers(2),
        ParamKind::Vec3 => numbers(3),
        ParamKind::Vec4 | ParamKind::Color => numbers(4),
        ParamKind::Expr | ParamKind::Enum => false,
    }
}

/// Why `effect` cannot drive the package described by `info`, if it cannot.
pub fn effect_mismatch(effect: &ItemEffect, info: &PackageInfo) -> Option<String> {
    if info.inputs != 1 {
        return Some(format!("the package \"{}\" is not an effect of one picture", effect.package));
    }
    let kind_of = |name: &str| info.params.iter().find(|param| param.name == name).map(|param| param.kind);
    match kind_of(&effect.progress) {
        Some(ParamKind::Float) => {}
        Some(_) => return Some(format!("the parameter \"{}\" of \"{}\" is not a float", effect.progress, effect.package)),
        None => return Some(format!("the package \"{}\" has no parameter \"{}\"", effect.package, effect.progress)),
    }
    for (name, value) in &effect.params {
        match kind_of(name) {
            None => return Some(format!("the package \"{}\" has no parameter \"{name}\"", effect.package)),
            Some(kind) if !fits(kind, value) => return Some(format!("the value of the parameter \"{name}\" does not fit its type ({kind:?}) in \"{}\"", effect.package)),
            Some(_) => {}
        }
    }
    None
}

/// Finds and checks the effect of every item of `packs` against the gpufx packages under `roots`.
pub fn resolve_item_effects(roots: &[PathBuf], packs: &[ClipAnimationManifest]) -> ItemEffects {
    let mut effects = ItemEffects::default();
    if packs.iter().all(|pack| pack.items.iter().all(|item| item.effect.is_none())) {
        return effects;
    }
    let wanted = packs.iter().flat_map(|pack| &pack.items).filter_map(|item| item.effect.as_ref()).map(|effect| effect.package.to_ascii_lowercase()).collect();
    let index = index_packages(roots, &wanted);
    let mut infos: HashMap<PathBuf, Result<PackageInfo, String>> = HashMap::new();
    for pack in packs {
        let pack_id = pack.id.to_ascii_lowercase();
        for item in &pack.items {
            let Some(effect) = &item.effect else { continue };
            let problem = |message: String| PackReadProblem { pack: pack.id.clone(), message: format!("the effect of the item \"{}\" is left out: {message}", item.id) };
            let Some((name, path)) = index.get(&effect.package.to_ascii_lowercase()) else {
                effects.problems.push(problem(format!("the package \"{}\" is not installed", effect.package)));
                continue;
            };
            let info = infos.entry(path.clone()).or_insert_with(|| read_package_info(path));
            let mismatch = match info {
                Ok(info) => effect_mismatch(effect, info),
                Err(message) => Some(format!("the package \"{}\" cannot be read: {message}", effect.package)),
            };
            if let Some(message) = mismatch {
                effects.problems.push(problem(message));
                continue;
            }
            let reference = format!("{pack_id}{PACK_PRESET_SEPARATOR}{}", item.id);
            effects.ready.insert(reference, PresetEffect { package: name.clone(), progress: effect.progress.clone(), params: effect.params.clone() });
        }
    }
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip_animation_pack::{check_clip_animation_fields, parse_clip_animation_manifest, scan_clip_animation_folders};

    fn manifest(items: &str) -> String {
        format!(r#"{{"format":1,"type":"clip-animation","id":"fx","name":"N","version":"1.0.0","license":{{"spdx":"MIT","files":["LICENSE.txt"]}},"items":[{items}]}}"#)
    }

    fn item(id: &str, effect: &str) -> String {
        format!(r#"{{"id":"{id}","name":"I","kind":"in","default_duration":1,"effect":{effect}}}"#)
    }

    fn check(items: &str) -> Result<(), String> {
        let parsed = parse_clip_animation_manifest(manifest(items).as_bytes()).map_err(|e| e.to_string())?;
        check_clip_animation_fields(&parsed).map_err(|e| e.to_string())
    }

    #[test]
    fn the_reader_refuses_an_effect_of_the_wrong_shape() {
        assert_eq!(check(&item("a", r#"{"package":"io.x.wipe"}"#)), Ok(()), "an effect alone, progress by default");
        assert_eq!(check(&item("a", r#"{"package":"io.x.wipe","progress":"t","params":{"n":1.5,"on":true,"at":[0.5,0.5],"tint":[1,0,0,1]}}"#)), Ok(()));
        let refused = [
            (r#"{"package":"../escape"}"#, "not a package id"),
            (r#"{"package":"io.x.wipe","extra":1}"#, "unknown field"),
            (r#"{"progress":"t"}"#, "package"),
            (r#"{"package":"io.x.wipe","progress":" "}"#, "empty progress"),
            (r#"{"package":"io.x.wipe","params":{"progress":0}}"#, "progress parameter"),
            (r#"{"package":"io.x.wipe","params":{"s":"text"}}"#, "must be a number"),
            (r#"{"package":"io.x.wipe","params":{"v":[1,2,3,4,5]}}"#, "must be a number"),
            (r#"{"package":"io.x.wipe","params":{"v":[1,"a"]}}"#, "must be a number"),
            (r#""io.x.wipe""#, "invalid type"),
        ];
        for (effect, wanted) in refused {
            let message = check(&item("a", effect)).expect_err(effect);
            assert!(message.contains(wanted), "{effect}: {message}");
        }
        // Without an effect, a curve is still needed.
        assert!(check(r#"{"id":"a","name":"I","kind":"in","default_duration":1}"#).unwrap_err().contains("no track"));
    }

    /// The probe package with the id `id` at `relative` under `root`: a float `amount`, an int
    /// `count`, a vec2 `at` and an enum `side` (`left` or `right`).
    fn write_probe(root: &Path, relative: &str, id: &str) {
        let package = root.join(relative);
        std::fs::create_dir_all(package.join("shaders")).unwrap();
        let manifest = r#"{"api":1,"id":"ID","name":"Probe","used_for":["gpufx"],"inputs":[{"name":"src"}],
            "params":[{"name":"amount","type":"float","default":0},{"name":"count","type":"int","default":0},{"name":"at","type":"vec2","default":[0,0]},
                      {"name":"side","type":"enum","default":"left","values":["left","right"]}],
            "groups":[{"passes":[{"shader":"shaders/p.wgsl"}]}]}"#;
        std::fs::write(package.join("manifest.json"), manifest.replace("\"ID\"", &format!("\"{id}\""))).unwrap();
        std::fs::write(package.join("shaders/p.wgsl"), "@compute @workgroup_size(8, 8)\nfn main(@builtin(global_invocation_id) id: vec3u) {\n    if (!gpufx_in_bounds(id)) { return; }\n    gpufx_write(id, vec4f(params.amount, 0.0, 0.0, 1.0));\n}\n").unwrap();
    }

    /// A root holding the `fx` clip animation pack with `items` and a gpufx package `test/probe`
    /// with the id `test.probe` (see [`write_probe`]).
    fn root(tag: &str, items: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("gpufx-preset-effects-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_probe(&root, "packages/gpufx/test/probe", "Test.Probe");
        let pack = root.join("packages/clip-animation/fx");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("manifest.json"), manifest(items)).unwrap();
        root
    }

    #[test]
    fn an_effect_plays_only_with_its_package_installed_and_fitting() {
        let items = [
            item("ready", r#"{"package":"test.probe","progress":"amount","params":{"count":3,"at":[0.5,0.25]}}"#),
            item("missing", r#"{"package":"test.absent"}"#),
            item("no-param", r#"{"package":"test.probe"}"#),
            item("int-progress", r#"{"package":"test.probe","progress":"count"}"#),
            item("wrong-type", r#"{"package":"test.probe","progress":"amount","params":{"count":1.5}}"#),
            item("unknown-param", r#"{"package":"test.probe","progress":"amount","params":{"other":1}}"#),
        ]
        .join(",");
        let root = root("resolve", &items);
        let roots = [root.clone()];
        let scan = scan_clip_animation_folders(&roots);
        assert!(scan.problems.is_empty(), "a missing package is not an error of the pack: {:?}", scan.problems);
        let effects = resolve_item_effects(&roots, &scan.packs);
        let ready = &effects.ready["fx/ready"];
        assert_eq!((Path::new(&ready.package), ready.progress.as_str()), (std::path::absolute(root.join("packages/gpufx/test/probe")).unwrap().as_path(), "amount"));
        assert_eq!(effects.ready.len(), 1);
        let messages: Vec<&str> = effects.problems.iter().map(|problem| problem.message.as_str()).collect();
        assert_eq!(messages.len(), 5, "{messages:?}");
        for (item, wanted) in [("missing", "not installed"), ("no-param", "no parameter \"progress\""), ("int-progress", "not a float"), ("wrong-type", "does not fit"), ("unknown-param", "no parameter \"other\"")] {
            assert!(messages.iter().any(|message| message.contains(&format!("\"{item}\"")) && message.contains(wanted)), "{item}: {messages:?}");
        }

        // An item with only an effect plays nothing without it, so only the ready one is offered.
        let presets = crate::read_pack_presets(&roots);
        assert_eq!(presets.presets().iter().map(|preset| preset.reference.as_str()).collect::<Vec<_>>(), vec!["fx/ready"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_item_with_a_curve_keeps_it_when_its_effect_is_left_out() {
        let curve = r#"{"id":"moving","name":"I","kind":"in","default_duration":1,"curve":{"tracks":{"opacity":[{"t":0,"value":0},{"t":1,"value":1}]}},"effect":{"package":"test.absent"}}"#;
        let root = root("curve-kept", &[curve.to_owned(), item("bare", r#"{"package":"test.absent"}"#)].join(","));
        let roots = [root.clone()];
        let presets = crate::read_pack_presets(&roots);
        assert_eq!(presets.presets().iter().map(|preset| (preset.reference.as_str(), preset.effect.is_some())).collect::<Vec<_>>(), vec![("fx/moving", false)]);
        let scan = scan_clip_animation_folders(&roots);
        assert_eq!(resolve_item_effects(&roots, &scan.packs).problems.len(), 2, "both still say why");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The same relative path under two roots, each with another package: the preset plays the one
    /// whose id it named, whatever a project's paths would find first by that relative path.
    #[test]
    fn the_package_found_by_id_is_the_one_that_plays() {
        let first = std::env::temp_dir().join(format!("gpufx-preset-effects-shadow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&first);
        write_probe(&first, "packages/gpufx/test/probe", "test.other");
        let second = root("shadowed", &item("ready", r#"{"package":"test.probe","progress":"amount"}"#));
        let roots = [first.clone(), second.clone()];
        let scan = scan_clip_animation_folders(&roots);
        let effects = resolve_item_effects(&roots, &scan.packs);
        let package = &effects.ready["fx/ready"].package;
        let wanted = std::fs::canonicalize(second.join("packages/gpufx/test/probe")).unwrap();
        for paths in [crate::ProjectPaths::new(&first.join("project.json"), roots.to_vec()), crate::ProjectPaths::roots_only(Path::new("project.json"), roots.to_vec())] {
            let found = paths.package_path(package).expect("the package resolves");
            assert_eq!(std::fs::canonicalize(found).unwrap(), wanted);
        }
        let _ = std::fs::remove_dir_all(&first);
        let _ = std::fs::remove_dir_all(&second);
    }

    #[test]
    fn a_zip_with_an_oversized_manifest_is_skipped() {
        use std::io::Write;
        let root = root("oversized", &item("ready", r#"{"package":"test.big","progress":"amount"}"#));
        let mut writer = zip::ZipWriter::new(std::fs::File::create(root.join("packages/gpufx/big.zip")).unwrap());
        writer.start_file("manifest.json", zip::write::SimpleFileOptions::default()).unwrap();
        let padding = " ".repeat(2 << 20);
        writer.write_all(format!(r#"{{"id":"test.big","name":"Big"{padding}}}"#).as_bytes()).unwrap();
        writer.finish().unwrap();
        let roots = [root.clone()];
        let scan = scan_clip_animation_folders(&roots);
        let effects = resolve_item_effects(&roots, &scan.packs);
        assert!(effects.ready.is_empty());
        assert!(effects.problems[0].message.contains("not installed"), "{:?}", effects.problems);
        let _ = std::fs::remove_dir_all(&root);
    }
}
