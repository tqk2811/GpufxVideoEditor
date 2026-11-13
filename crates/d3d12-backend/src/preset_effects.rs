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

/// The `id` of the manifest of the package at `path`, a folder or a zip, read without the rest.
fn package_id(path: &Path) -> Option<String> {
    let bytes = if path.is_dir() {
        std::fs::read(path.join("manifest.json")).ok()?
    } else {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
        let mut entry = archive.by_name("manifest.json").ok()?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).ok()?;
        bytes
    };
    let manifest: Value = serde_json::from_slice(&bytes).ok()?;
    manifest.get("id")?.as_str().map(str::to_ascii_lowercase)
}

/// The gpufx packages under the roots by their manifest id (lower case): the name a project gives
/// the package (its path under the root) and its path. The first root holding an id wins.
fn index_packages(roots: &[PathBuf]) -> HashMap<String, (String, PathBuf)> {
    let mut index = HashMap::new();
    for root in roots {
        let mut found = Vec::new();
        collect(&root.join("packages").join("gpufx"), 0, &mut found);
        for path in found {
            let Some(id) = package_id(&path) else { continue };
            let name = path.strip_prefix(root).unwrap_or(&path).components().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            index.entry(id).or_insert((name, path));
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
    let index = index_packages(roots);
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

    /// A root holding the `fx` clip animation pack with `items` and a gpufx package `test/probe`
    /// with the id `test.probe`, a float `amount`, an int `count` and a vec2 `at`.
    fn root(tag: &str, items: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("gpufx-preset-effects-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let package = root.join("packages/gpufx/test/probe");
        std::fs::create_dir_all(package.join("shaders")).unwrap();
        std::fs::write(package.join("manifest.json"), r#"{"api":1,"id":"Test.Probe","name":"Probe","used_for":["gpufx"],"inputs":[{"name":"src"}],
            "params":[{"name":"amount","type":"float","default":0},{"name":"count","type":"int","default":0},{"name":"at","type":"vec2","default":[0,0]}],
            "groups":[{"passes":[{"shader":"shaders/p.wgsl"}]}]}"#).unwrap();
        std::fs::write(package.join("shaders/p.wgsl"), "@compute @workgroup_size(8, 8)\nfn main(@builtin(global_invocation_id) id: vec3u) {\n    if (!gpufx_in_bounds(id)) { return; }\n    gpufx_write(id, vec4f(params.amount, 0.0, 0.0, 1.0));\n}\n").unwrap();
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
        assert_eq!((ready.package.as_str(), ready.progress.as_str()), ("packages/gpufx/test/probe", "amount"));
        assert_eq!(effects.ready.len(), 1);
        let messages: Vec<&str> = effects.problems.iter().map(|problem| problem.message.as_str()).collect();
        assert_eq!(messages.len(), 5, "{messages:?}");
        for (item, wanted) in [("missing", "not installed"), ("no-param", "no parameter \"progress\""), ("int-progress", "not a float"), ("wrong-type", "does not fit"), ("unknown-param", "no parameter \"other\"")] {
            assert!(messages.iter().any(|message| message.contains(&format!("\"{item}\"")) && message.contains(wanted)), "{item}: {messages:?}");
        }

        // The curves of every item still play; only the ready one has an effect.
        let presets = crate::read_pack_presets(&roots);
        assert_eq!(presets.presets().len(), 6);
        assert_eq!(presets.presets().iter().filter(|preset| preset.effect.is_some()).count(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
