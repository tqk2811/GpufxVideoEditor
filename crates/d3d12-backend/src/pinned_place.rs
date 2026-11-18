//! The built-in package that draws a picture with its four corners pinned to four points of the
//! frame (corner pin), for an overlay that follows a planar track in perspective.
//!
//! Each output pixel is taken back into the picture by the inverse homography, so the sampling is
//! perspective correct (no split into two triangles, no affine bend along the diagonal). Colours
//! are filtered premultiplied and the edge is smooth, as in [`crate::rotated_place`]. Built in
//! memory, as the other placing packages are.

use std::sync::Arc;

use engine::tracker::homography::Homography;
use gpufx_core::{Effect, GpuContext};

use crate::alpha_merge::effect_of;

const MANIFEST: &str = r#"{
  "api": 1,
  "id": "io.github.tqk2811.editor-pinned-place",
  "name": "Pinned place",
  "used_for": ["gpufx"],
  "version": "1.0.0",
  "description": "Draws the input warped to four corners on transparent, in premultiplied colours.",
  "inputs": [{ "name": "picture" }],
  "params": [
    { "name": "h0", "type": "float", "default": 1, "min": -1000000000, "max": 1000000000 },
    { "name": "h1", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h2", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h3", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h4", "type": "float", "default": 1, "min": -1000000000, "max": 1000000000 },
    { "name": "h5", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h6", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h7", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h8", "type": "float", "default": 1, "min": -1000000000, "max": 1000000000 },
    { "name": "opacity", "type": "float", "default": 1, "min": 0, "max": 1 }
  ],
  "groups": [{ "passes": [{ "shader": "shaders/main.wgsl" }] }]
}"#;

const SHADER: &str = r#"fn premultiplied_texel(p: vec2i, limit: vec2i) -> vec4f {
    let sample = textureLoad(picture, clamp(p, vec2i(0), limit), 0);
    return vec4f(sample.rgb * sample.a, sample.a);
}

// The point of the picture (0 to 1 across it) shown at pixel `pixel`, and its third coordinate.
fn back(pixel: vec2f) -> vec3f {
    let p = pixel / globals.size;
    return vec3f(
        params.h0 * p.x + params.h1 * p.y + params.h2,
        params.h3 * p.x + params.h4 * p.y + params.h5,
        params.h6 * p.x + params.h7 * p.y + params.h8);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    if (!gpufx_in_bounds(id)) { return; }
    let centre = vec2f(id.xy) + 0.5;
    let q = back(centre);
    // The map is scaled so the third coordinate is positive over the picture: a pixel past the
    // horizon of the plane maps to a negative one and shows nothing.
    if (q.z <= 1e-9) {
        gpufx_write(id, vec4f(0.0));
        return;
    }
    let uv = q.xy / q.z;
    // How far the picture's edge is, in output pixels: the distance in the picture over how fast
    // the picture point moves per pixel there.
    let qx = back(centre + vec2f(1.0, 0.0));
    let qy = back(centre + vec2f(0.0, 1.0));
    let du = qx.xy / max(qx.z, 1e-9) - uv;
    let dv = qy.xy / max(qy.z, 1e-9) - uv;
    let rate = max(vec2f(length(vec2f(du.x, dv.x)), length(vec2f(du.y, dv.y))), vec2f(1e-9));
    let inside = min(uv, vec2f(1.0) - uv) / rate;
    let coverage = clamp(min(inside.x, inside.y) + 0.5, 0.0, 1.0);
    if (coverage <= 0.0) {
        gpufx_write(id, vec4f(0.0));
        return;
    }
    let dimensions = vec2f(textureDimensions(picture));
    let limit = vec2i(dimensions) - vec2i(1);
    let position = uv * dimensions - 0.5;
    let cell = floor(position);
    let blend = position - cell;
    let origin = vec2i(cell);
    let upper = mix(premultiplied_texel(origin, limit), premultiplied_texel(origin + vec2i(1, 0), limit), blend.x);
    let lower = mix(premultiplied_texel(origin + vec2i(0, 1), limit), premultiplied_texel(origin + vec2i(1, 1), limit), blend.x);
    gpufx_write(id, mix(upper, lower, blend.y) * coverage * params.opacity);
}
"#;

/// Takes one picture; writes it with its corners at the points the parameters `h0`..`h8` say (see
/// [`pin_params`]), with premultiplied colours, on transparent.
pub(crate) fn pinned_place_effect(context: &Arc<GpuContext>) -> Result<Effect, String> {
    effect_of(context, MANIFEST, SHADER)
}

/// The parameters `h0`..`h8` that pin the picture's corners (top-left, top-right, bottom-right,
/// bottom-left) to `corners`, in parts of the frame: the map from the frame back to the picture,
/// scaled so its third coordinate is positive inside the picture. `None` when the corners do not
/// make a convex quadrilateral.
pub(crate) fn pin_params(corners: &[[f64; 2]; 4], frame_aspect: f64) -> Option<[f64; 9]> {
    // Convexity is a matter of shape, so it is checked in a frame of square pixels.
    let square = corners.map(|[x, y]| [x * frame_aspect, y]);
    if !engine::tracker::homography::is_convex(&square) || !corners.iter().flatten().all(|v| v.is_finite()) {
        return None;
    }
    let forward = Homography::from_quads(&[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], corners)?;
    let back = forward.inverse()?;
    let middle = [corners.iter().map(|c| c[0]).sum::<f64>() / 4.0, corners.iter().map(|c| c[1]).sum::<f64>() / 4.0];
    let m = back.m;
    let w = m[6] * middle[0] + m[7] * middle[1] + m[8];
    let sign = if w < 0.0 { -1.0 } else { 1.0 };
    Some(m.map(|v| v * sign))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parameters_take_each_corner_back_to_its_picture_corner_with_a_positive_scale() {
        let corners = [[0.35, 0.2], [0.7, 0.3], [0.7, 0.6], [0.3, 0.75]];
        let m = pin_params(&corners, 16.0 / 9.0).unwrap();
        for (corner, uv) in corners.iter().zip([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]) {
            let w = m[6] * corner[0] + m[7] * corner[1] + m[8];
            assert!(w > 0.0);
            let (u, v) = ((m[0] * corner[0] + m[1] * corner[1] + m[2]) / w, (m[3] * corner[0] + m[4] * corner[1] + m[5]) / w);
            assert!((u - uv[0]).abs() < 1e-9 && (v - uv[1]).abs() < 1e-9);
        }
        assert!(pin_params(&[corners[0], corners[2], corners[1], corners[3]], 1.0).is_none());
    }
}
