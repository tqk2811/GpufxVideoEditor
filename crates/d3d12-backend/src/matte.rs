//! AI background removal of a clip (see [`engine::ClipMatte`]).
//!
//! Per frame the compositor brings the source to the project size (the lead of the clip graph:
//! scale, then turn), runs a `gpufx_ai_matting` model on that picture with
//! [`gpufx_core::nn::FrameMatte`] and keeps the raw matte, at the model size, in a [`MatteCache`].
//! [`MatteShaper`] then turns the raw mattes into the alpha of the picture: the mean over the
//! temporal window, resized to the frame, the threshold, the edge shrink or grow (a min or max
//! filter), the feather (a gaussian blur), the inversion; and writes the picture with that alpha.
//! The clip's effects run after, on the cut-out picture.
//!
//! The temporal window is the current source frame and the `temporal` frames before it in the
//! source, found from the current frame's timestamp alone, so a frame gets the same matte in the
//! preview and the export whatever was shown before it.

use std::collections::HashMap;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine::{ClipMatte, MatteModel, MAX_MATTE_TEMPORAL};

/// The package folder of the matting models, under a package root's `packages` or the models folder.
pub const MATTING_FOLDER: &str = "gpufx_ai_matting";

/// The default memory budget of the raw matte cache, in bytes.
pub const MATTE_CACHE_BYTES: u64 = 256 * 1024 * 1024;

/// The environment variable naming an extra folder of matting models (`<id>.zip` or `<id>/`),
/// looked at first. For tests and development.
pub const MATTING_MODELS_ENV: &str = "GPUFX_MATTING_MODELS";

/// `%LOCALAPPDATA%\gpufx-editor\models`, where the editor installs the models it fetches or
/// imports (each under `gpufx_ai_matting\<id>.zip`).
pub fn user_models_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").filter(|dir| !dir.is_empty()).map(|dir| PathBuf::from(dir).join("gpufx-editor").join("models"))
}

/// The model `id` in `folder`: `<id>.zip`, or a folder `<id>` holding `model.json`.
pub fn model_in(folder: &Path, id: &str) -> Option<PathBuf> {
    let zip = folder.join(format!("{id}.zip"));
    if zip.is_file() {
        return Some(zip);
    }
    let dir = folder.join(id);
    dir.join("model.json").is_file().then_some(dir)
}

/// The folders a matting model is looked for in, in order: [`MATTING_MODELS_ENV`], the user's
/// models folder, then `<root>/packages/gpufx_ai_matting` of each package root.
pub fn model_folders(package_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(dir) = std::env::var_os(MATTING_MODELS_ENV).filter(|dir| !dir.is_empty()) {
        folders.push(PathBuf::from(dir));
    }
    if let Some(dir) = user_models_dir() {
        folders.push(dir.join(MATTING_FOLDER));
    }
    folders.extend(package_roots.iter().map(|root| root.join("packages").join(MATTING_FOLDER)));
    folders
}

/// Checks without a GPU that `path` is a `gpufx_ai_matting` model package.
pub fn check_matting_package(path: &Path) -> Result<(), String> {
    gpufx_core::nn::check_model_package(path, gpufx_core::used_for::AI_MATTING, None).map_err(|e| e.to_string())
}

/// Where the model of `model` is installed, if anywhere (see [`model_folders`]).
pub fn find_matting_model(model: MatteModel, package_roots: &[PathBuf]) -> Option<PathBuf> {
    let id = model.package_id()?;
    model_folders(package_roots).iter().find_map(|folder| model_in(folder, id))
}

/// The shaping of a matte in pixels of a frame `height` lines high: the edge (negative shrinks)
/// and the feather radius.
pub fn matte_radii(matte: &ClipMatte, height: u32) -> (i32, u32) {
    let scale = f64::from(height) / 1080.0;
    let edge = (matte.edge * scale).round();
    let feather = (matte.feather * scale).round();
    let limit = (engine::MAX_MATTE_PIXELS * scale).ceil().max(1.0);
    (edge.clamp(-limit, limit) as i32, feather.clamp(0.0, limit) as u32)
}

/// The timestamps of the temporal window of the frame at `pts`, the current one first, `step`
/// apart. Determined by the frame alone.
pub fn temporal_window(pts: i64, step: Option<i64>, temporal: u32) -> Vec<i64> {
    let count = match step {
        Some(step) if step > 0 => temporal.min(MAX_MATTE_TEMPORAL),
        _ => 0,
    };
    let step = step.unwrap_or(0);
    (0..=i64::from(count)).map(|i| pts.saturating_sub(i * step)).collect()
}

/// Names one raw matte: which model ran on which frame, brought to which size.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MatteKey {
    /// The file the frame was decoded from.
    pub origin: Arc<str>,
    pub pts: i64,
    pub model: MatteModel,
    /// The size the picture had when the model ran, and the quarter turns that brought it there.
    pub width: u32,
    pub height: u32,
    pub turns: u8,
}

/// A least recently used cache bounded by the bytes of its values.
pub struct MatteCache<K, V> {
    budget: u64,
    used: u64,
    tick: u64,
    entries: HashMap<K, (V, u64, u64)>,
}

impl<K: Eq + Hash + Clone, V: Clone> MatteCache<K, V> {
    pub fn new(budget: u64) -> Self {
        Self { budget, used: 0, tick: 0, entries: HashMap::new() }
    }

    /// The value of `key`, marked as used now.
    pub fn get(&mut self, key: &K) -> Option<V> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|(value, _, used)| {
            *used = tick;
            value.clone()
        })
    }

    /// Keeps `value` of `bytes` under `key`, letting the least recently used values go until the
    /// cache fits its budget again. The value just put in stays even when it alone is over it.
    pub fn insert(&mut self, key: K, value: V, bytes: u64) {
        self.tick += 1;
        if let Some((_, old, _)) = self.entries.insert(key.clone(), (value, bytes, self.tick)) {
            self.used -= old;
        }
        self.used += bytes;
        while self.used > self.budget && self.entries.len() > 1 {
            let oldest = self.entries.iter().filter(|(k, _)| **k != key).min_by_key(|(_, (_, _, used))| *used).map(|(k, _)| k.clone());
            let Some(oldest) = oldest else { break };
            if let Some((_, bytes, _)) = self.entries.remove(&oldest) {
                self.used -= bytes;
            }
        }
    }

    pub fn contains(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bytes held.
    pub fn bytes(&self) -> u64 {
        self.used
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.used = 0;
    }
}

/// The parameters of every shaping pass, as the shader reads them.
#[repr(C)]
#[derive(Clone, Copy)]
struct ShapeParams {
    width: u32,
    height: u32,
    count: u32,
    mode: u32,
    radius: i32,
    vertical: u32,
    threshold: f32,
    invert: u32,
}

impl ShapeParams {
    fn bytes(&self) -> [u8; 32] {
        let words = [self.width, self.height, self.count, self.mode, self.radius as u32, self.vertical, self.threshold.to_bits(), self.invert];
        let mut out = [0u8; 32];
        for (chunk, word) in out.chunks_exact_mut(4).zip(words) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        out
    }
}

const SHAPE_SHADER: &str = r#"
struct Params { size: vec2u, count: u32, mode: u32, radius: i32, vertical: u32, threshold: f32, invert: u32 }
@group(0) @binding(0) var<uniform> p: Params;

// gather: the mean of the raw mattes, each resized bilinearly (half-pixel centres) to the frame.
@group(0) @binding(1) var m0: texture_2d<f32>;
@group(0) @binding(2) var m1: texture_2d<f32>;
@group(0) @binding(3) var m2: texture_2d<f32>;
@group(0) @binding(4) var m3: texture_2d<f32>;
@group(0) @binding(5) var m4: texture_2d<f32>;
@group(0) @binding(6) var gathered: texture_storage_2d<r32float, write>;

fn bilinear(t: texture_2d<f32>, id: vec2u) -> f32 {
    let src = vec2i(textureDimensions(t));
    let pos = (vec2f(id) + 0.5) * vec2f(src) / vec2f(p.size) - 0.5;
    let base = floor(pos);
    let f = pos - base;
    let hi = src - 1;
    let a = clamp(vec2i(base), vec2i(0), hi);
    let b = clamp(vec2i(base) + 1, vec2i(0), hi);
    let v00 = textureLoad(t, vec2i(a.x, a.y), 0).r;
    let v10 = textureLoad(t, vec2i(b.x, a.y), 0).r;
    let v01 = textureLoad(t, vec2i(a.x, b.y), 0).r;
    let v11 = textureLoad(t, vec2i(b.x, b.y), 0).r;
    return mix(mix(v00, v10, f.x), mix(v01, v11, f.x), f.y);
}

@compute @workgroup_size(8, 8)
fn gather(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= p.size)) { return; }
    var sum = bilinear(m0, id.xy);
    if (p.count > 1u) { sum += bilinear(m1, id.xy); }
    if (p.count > 2u) { sum += bilinear(m2, id.xy); }
    if (p.count > 3u) { sum += bilinear(m3, id.xy); }
    if (p.count > 4u) { sum += bilinear(m4, id.xy); }
    var a = clamp(sum / f32(p.count), 0.0, 1.0);
    if (p.threshold > 0.0) { a = select(0.0, 1.0, a >= p.threshold); }
    textureStore(gathered, vec2i(id.xy), vec4f(a, 0.0, 0.0, 1.0));
}
"#;

const FILTER_SHADER: &str = r#"
struct Params { size: vec2u, count: u32, mode: u32, radius: i32, vertical: u32, threshold: f32, invert: u32 }
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<r32float, write>;

// One direction of a min (mode 0), max (mode 1) or gaussian (mode 2) filter of `radius` pixels.
@compute @workgroup_size(8, 8)
fn filter_pass(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= p.size)) { return; }
    let dir = select(vec2i(1, 0), vec2i(0, 1), p.vertical == 1u);
    let hi = vec2i(p.size) - 1;
    let c = vec2i(id.xy);
    var acc = textureLoad(src, c, 0).r;
    var weights = 1.0;
    let sigma = max(f32(p.radius) / 2.0, 0.5);
    for (var i = 1; i <= p.radius; i++) {
        let a = textureLoad(src, clamp(c + dir * i, vec2i(0), hi), 0).r;
        let b = textureLoad(src, clamp(c - dir * i, vec2i(0), hi), 0).r;
        if (p.mode == 0u) {
            acc = min(acc, min(a, b));
        } else if (p.mode == 1u) {
            acc = max(acc, max(a, b));
        } else {
            let w = exp(-f32(i * i) / (2.0 * sigma * sigma));
            acc += w * (a + b);
            weights += 2.0 * w;
        }
    }
    if (p.mode == 2u) { acc = acc / weights; }
    textureStore(dst, c, vec4f(acc, 0.0, 0.0, 1.0));
}
"#;

const APPLY_SHADER: &str = r#"
struct Params { size: vec2u, count: u32, mode: u32, radius: i32, vertical: u32, threshold: f32, invert: u32 }
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var alpha: texture_2d<f32>;
@group(0) @binding(2) var picture: texture_2d<f32>;
@group(0) @binding(3) var out: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn apply(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= p.size)) { return; }
    let c = vec2i(id.xy);
    var a = clamp(textureLoad(alpha, c, 0).r, 0.0, 1.0);
    if (p.invert == 1u) { a = 1.0 - a; }
    let px = textureLoad(picture, c, 0);
    textureStore(out, c, vec4f(px.rgb, px.a * a));
}
"#;

/// The passes that shape raw mattes into the alpha of a picture.
pub struct MatteShaper {
    gather: wgpu::ComputePipeline,
    filter: wgpu::ComputePipeline,
    apply: wgpu::ComputePipeline,
    /// Two frame-size single-channel textures the passes go back and forth between.
    scratch: Option<(u32, u32, wgpu::Texture, wgpu::Texture)>,
}

fn pipeline(device: &wgpu::Device, source: &str, entry: &str) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("matte shaping"), source: wgpu::ShaderSource::Wgsl(source.into()) });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some("matte shaping"), layout: None, module: &module, entry_point: Some(entry), compilation_options: Default::default(), cache: None })
}

/// A single-channel float texture the shaping passes read and write.
pub fn matte_texture(device: &wgpu::Device, width: u32, height: u32, render: bool) -> wgpu::Texture {
    let mut usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC;
    if render {
        usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("matte"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage,
        view_formats: &[],
    })
}

impl MatteShaper {
    pub fn new(device: &wgpu::Device) -> Self {
        Self { gather: pipeline(device, SHAPE_SHADER, "gather"), filter: pipeline(device, FILTER_SHADER, "filter_pass"), apply: pipeline(device, APPLY_SHADER, "apply"), scratch: None }
    }

    /// Records the shaping of `mattes` (raw, current frame first, at most five) by `matte` into the
    /// alpha of `picture`, written to `out` (an rgba8unorm storage view), both `width` x `height`.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, mattes: &[wgpu::TextureView], matte: &ClipMatte, picture: &wgpu::TextureView, out: &wgpu::TextureView, width: u32, height: u32) {
        assert!(!mattes.is_empty() && mattes.len() <= 5, "one to five mattes");
        if !matches!(&self.scratch, Some((w, h, ..)) if (*w, *h) == (width, height)) {
            self.scratch = Some((width, height, matte_texture(device, width, height, false), matte_texture(device, width, height, false)));
        }
        let (_, _, first, second) = self.scratch.as_ref().expect("made above");
        let views = [first.create_view(&Default::default()), second.create_view(&Default::default())];
        let (edge, feather) = matte_radii(matte, height);
        let base = ShapeParams { width, height, count: mattes.len() as u32, mode: 0, radius: 0, vertical: 0, threshold: matte.threshold as f32, invert: u32::from(matte.invert) };
        let uniform = |params: ShapeParams| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor { label: Some("matte params"), size: 32, usage: wgpu::BufferUsages::UNIFORM, mapped_at_creation: true });
            buffer.slice(..).get_mapped_range_mut().expect("mapped at creation").copy_from_slice(&params.bytes());
            buffer.unmap();
            buffer
        };
        let groups = (width.div_ceil(8), height.div_ceil(8));
        let dispatch = |encoder: &mut wgpu::CommandEncoder, pipeline: &wgpu::ComputePipeline, group: &wgpu::BindGroup| {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("matte shaping"), timestamp_writes: None });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(groups.0, groups.1, 1);
        };
        // gather into the first texture
        let params = uniform(base);
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() }];
        for slot in 0..5 {
            let view = &mattes[slot.min(mattes.len() - 1)];
            entries.push(wgpu::BindGroupEntry { binding: 1 + slot as u32, resource: wgpu::BindingResource::TextureView(view) });
        }
        entries.push(wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(&views[0]) });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("matte gather"), layout: &self.gather.get_bind_group_layout(0), entries: &entries });
        dispatch(encoder, &self.gather, &group);
        let mut current = 0;
        let mut passes = Vec::new();
        if edge != 0 {
            let mode = if edge < 0 { 0 } else { 1 };
            passes.push((mode, edge.unsigned_abs() as i32, 0));
            passes.push((mode, edge.unsigned_abs() as i32, 1));
        }
        if feather > 0 {
            passes.push((2, feather as i32, 0));
            passes.push((2, feather as i32, 1));
        }
        for (mode, radius, vertical) in passes {
            let params = uniform(ShapeParams { mode, radius, vertical, ..base });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("matte filter"),
                layout: &self.filter.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[current]) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[1 - current]) },
                ],
            });
            dispatch(encoder, &self.filter, &group);
            current = 1 - current;
        }
        let params = uniform(base);
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("matte apply"),
            layout: &self.apply.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[current]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(picture) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(out) },
            ],
        });
        dispatch(encoder, &self.apply, &group);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_lets_the_least_recently_used_go_first() {
        let mut cache = MatteCache::new(30);
        cache.insert("a", 1, 10);
        cache.insert("b", 2, 10);
        cache.insert("c", 3, 10);
        assert_eq!(cache.get(&"a"), Some(1), "a is now the newest");
        cache.insert("d", 4, 10);
        assert!(!cache.contains(&"b"), "b was the least recently used");
        assert!(cache.contains(&"a") && cache.contains(&"c") && cache.contains(&"d"));
        assert_eq!(cache.bytes(), 30);
        // A value over the budget alone stays, alone.
        cache.insert("big", 5, 100);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(&"big"), Some(5));
        // Replacing a value counts its new size only.
        cache.insert("big", 6, 5);
        assert_eq!(cache.bytes(), 5);
        cache.clear();
        assert!(cache.is_empty() && cache.bytes() == 0);
    }

    #[test]
    fn the_radii_follow_the_output_height() {
        let matte = ClipMatte { model: MatteModel::Fast, edge: -4.0, feather: 6.0, ..ClipMatte::default() };
        assert_eq!(matte_radii(&matte, 1080), (-4, 6));
        assert_eq!(matte_radii(&matte, 540), (-2, 3));
        assert_eq!(matte_radii(&matte, 2160), (-8, 12));
        assert_eq!(matte_radii(&ClipMatte::default(), 1080), (0, 0));
    }

    #[test]
    fn the_temporal_window_comes_from_the_frame_alone() {
        assert_eq!(temporal_window(1000, Some(100), 0), vec![1000]);
        assert_eq!(temporal_window(1000, Some(100), 3), vec![1000, 900, 800, 700]);
        assert_eq!(temporal_window(1000, None, 3), vec![1000], "no frame rate, no window");
        assert_eq!(temporal_window(1000, Some(100), 99).len(), MAX_MATTE_TEMPORAL as usize + 1);
    }

    #[test]
    fn models_are_found_as_zips_or_folders() {
        let root = std::env::temp_dir().join(format!("gpufx-matte-models-{}", std::process::id()));
        let folder = root.join("packages").join(MATTING_FOLDER);
        std::fs::create_dir_all(folder.join("modnet-webcam")).unwrap();
        assert_eq!(model_in(&folder, "modnet-webcam"), None, "a folder without model.json is not a model");
        std::fs::write(folder.join("modnet-webcam").join("model.json"), "{}").unwrap();
        assert_eq!(model_in(&folder, "modnet-webcam"), Some(folder.join("modnet-webcam")));
        std::fs::write(folder.join("modnet-webcam.zip"), "zip").unwrap();
        assert_eq!(model_in(&folder, "modnet-webcam"), Some(folder.join("modnet-webcam.zip")), "the zip wins");
        assert!(model_folders(std::slice::from_ref(&root)).contains(&folder));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
