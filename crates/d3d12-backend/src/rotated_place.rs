//! The built-in package that draws a picture turned around its middle.
//!
//! The overlay package has no rotation, and turning needs what it does not do: a smooth edge, and
//! filtering of premultiplied colours so a see-through corner cannot bleed its hidden colour along
//! the edge. Built in memory, as the alpha packages are, because it is part of how the compositor works.

use std::sync::Arc;

use gpufx_core::{Effect, GpuContext};

use crate::alpha_merge::effect_of;

const MANIFEST: &str = r#"{
  "api": 1,
  "id": "io.github.tqk2811.editor-rotated-place",
  "name": "Rotated place",
  "used_for": ["gpufx"],
  "version": "1.0.0",
  "description": "Draws the input as a turned rectangle on transparent, in premultiplied colours.",
  "inputs": [{ "name": "picture" }],
  "params": [
    { "name": "x", "type": "float", "default": 0, "min": -8, "max": 8 },
    { "name": "y", "type": "float", "default": 0, "min": -8, "max": 8 },
    { "name": "width", "type": "float", "default": 1, "min": 0, "max": 8 },
    { "name": "height", "type": "float", "default": 1, "min": 0, "max": 1000 },
    { "name": "rotation", "type": "float", "default": 0, "min": -3600, "max": 3600 },
    { "name": "opacity", "type": "float", "default": 1, "min": 0, "max": 1 }
  ],
  "groups": [{ "passes": [{ "shader": "shaders/main.wgsl" }] }]
}"#;

const SHADER: &str = r#"fn premultiplied_texel(p: vec2i, limit: vec2i) -> vec4f {
    let sample = textureLoad(picture, clamp(p, vec2i(0), limit), 0);
    return vec4f(sample.rgb * sample.a, sample.a);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    if (!gpufx_in_bounds(id)) { return; }
    // In output pixels, so a turn keeps the shape of the picture whatever the frame's is.
    let size = vec2f(params.width, params.height) * globals.size;
    let centre = (vec2f(params.x, params.y) + vec2f(params.width, params.height) * 0.5) * globals.size;
    let turn = radians(params.rotation);
    let cosine = cos(turn);
    let sine = sin(turn);
    let d = vec2f(id.xy) + 0.5 - centre;
    let inner = vec2f(cosine * d.x + sine * d.y, cosine * d.y - sine * d.x) + size * 0.5;
    // A pixel on the border is covered by how far inside the rectangle it reaches.
    let margin = min(min(inner.x, inner.y), min(size.x - inner.x, size.y - inner.y));
    let coverage = clamp(margin + 0.5, 0.0, 1.0);
    if (coverage <= 0.0) {
        gpufx_write(id, vec4f(0.0));
        return;
    }
    let dimensions = vec2f(textureDimensions(picture));
    let limit = vec2i(dimensions) - vec2i(1);
    let position = inner / size * dimensions - 0.5;
    let cell = floor(position);
    let blend = position - cell;
    let origin = vec2i(cell);
    let upper = mix(premultiplied_texel(origin, limit), premultiplied_texel(origin + vec2i(1, 0), limit), blend.x);
    let lower = mix(premultiplied_texel(origin + vec2i(0, 1), limit), premultiplied_texel(origin + vec2i(1, 1), limit), blend.x);
    gpufx_write(id, mix(upper, lower, blend.y) * coverage * params.opacity);
}
"#;

/// Takes one picture; writes it turned, with premultiplied colours, on transparent.
pub(crate) fn rotated_place_effect(context: &Arc<GpuContext>) -> Result<Effect, String> {
    effect_of(context, MANIFEST, SHADER)
}
