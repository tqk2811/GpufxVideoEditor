//! Two small built-in packages that give a picture back the alpha an effect wrote over.
//!
//! Many effects write an alpha of one. Over a picture with see-through parts that paints those parts
//! solid, so such an effect's output gets its alpha back from what it was made of. The packages are
//! built in memory because they are part of how the compositor works, not effects a user picks.

use std::io::{Cursor, Write};
use std::sync::Arc;

use gpufx_core::{Effect, GpuContext, Package};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const RESTORE_MANIFEST: &str = r#"{
  "api": 1,
  "id": "io.github.tqk2811.editor-alpha-restore",
  "name": "Restore alpha",
  "used_for": ["gpufx"],
  "version": "1.0.0",
  "description": "The colours of the first input with the alpha of the second.",
  "inputs": [{ "name": "picture" }, { "name": "matte", "same_size": true }],
  "groups": [{ "passes": [{ "shader": "shaders/main.wgsl" }] }]
}"#;

const RESTORE_SHADER: &str = r#"@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    if (!gpufx_in_bounds(id)) { return; }
    let p = vec2i(id.xy);
    gpufx_write(id, vec4f(textureLoad(picture, p, 0).rgb, textureLoad(matte, p, 0).a));
}
"#;

const LARGER_MANIFEST: &str = r#"{
  "api": 1,
  "id": "io.github.tqk2811.editor-alpha-larger",
  "name": "Larger alpha",
  "used_for": ["gpufx"],
  "version": "1.0.0",
  "description": "The colours of the first input with the larger alpha of the other two.",
  "inputs": [{ "name": "picture" }, { "name": "first", "same_size": true }, { "name": "second", "same_size": true }],
  "groups": [{ "passes": [{ "shader": "shaders/main.wgsl" }] }]
}"#;

const LARGER_SHADER: &str = r#"@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    if (!gpufx_in_bounds(id)) { return; }
    let p = vec2i(id.xy);
    gpufx_write(id, vec4f(textureLoad(picture, p, 0).rgb, max(textureLoad(first, p, 0).a, textureLoad(second, p, 0).a)));
}
"#;

pub(crate) fn effect_of(context: &Arc<GpuContext>, manifest: &str, shader: &str) -> Result<Effect, String> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for (name, text) in [("manifest.json", manifest), ("shaders/main.wgsl", shader)] {
        writer.start_file(name, options).map_err(|e| format!("building the alpha package: {e}"))?;
        writer.write_all(text.as_bytes()).map_err(|e| format!("building the alpha package: {e}"))?;
    }
    let bytes = writer.finish().map_err(|e| format!("building the alpha package: {e}"))?.into_inner();
    let package = Package::from_zip_bytes(&bytes).map_err(|e| format!("the alpha package: {e}"))?;
    Effect::new(context.clone(), package).map_err(|e| format!("the alpha package: {e}"))
}

/// Takes `picture, matte`: the colours of the picture with the alpha of the matte.
pub(crate) fn restore_alpha_effect(context: &Arc<GpuContext>) -> Result<Effect, String> {
    effect_of(context, RESTORE_MANIFEST, RESTORE_SHADER)
}

/// Takes `picture, first, second`: the colours of the picture with the larger of the other two alphas.
pub(crate) fn larger_alpha_effect(context: &Arc<GpuContext>) -> Result<Effect, String> {
    effect_of(context, LARGER_MANIFEST, LARGER_SHADER)
}
