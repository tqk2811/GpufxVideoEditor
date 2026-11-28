//! The built-in package that hides a region of a picture: a censor clip, or an effect limited to a
//! region of its clip.
//!
//! It mixes the picture with a changed copy of it through a rectangle or an ellipse, turned and
//! with a soft edge, or a closed polygon in parts of that box (even-odd, its soft edge measured to
//! the nearest side, its points a row of the third input), or through that shape warped onto a quadrilateral (a region lying in
//! perspective on a planar track). The changed copy is the second input (a blur made before), blocks of the
//! picture made here (pixelate), or one colour. Built in memory, as the alpha packages are, because
//! it is part of how the compositor works.

use std::sync::Arc;

use gpufx_core::{Effect, GpuContext};

use crate::alpha_merge::effect_of;

const MANIFEST: &str = r#"{
  "api": 1,
  "id": "io.github.tqk2811.editor-censor-mix",
  "name": "Censor mix",
  "used_for": ["gpufx"],
  "version": "1.0.0",
  "description": "Mixes the picture with a hidden copy through a turned rectangle or ellipse with a soft edge.",
  "inputs": [{ "name": "picture" }, { "name": "hidden", "same_size": true }, { "name": "points" }],
  "params": [
    { "name": "x", "type": "float", "default": 0, "min": -16, "max": 16 },
    { "name": "y", "type": "float", "default": 0, "min": -16, "max": 16 },
    { "name": "width", "type": "float", "default": 1, "min": 0, "max": 16 },
    { "name": "height", "type": "float", "default": 1, "min": 0, "max": 1000 },
    { "name": "rotation", "type": "float", "default": 0, "min": -100000, "max": 100000 },
    { "name": "feather", "type": "float", "default": 0, "min": 0, "max": 100000 },
    { "name": "ellipse", "type": "float", "default": 0, "min": 0, "max": 1 },
    { "name": "opacity", "type": "float", "default": 1, "min": 0, "max": 1 },
    { "name": "mode", "type": "float", "default": 0, "min": 0, "max": 2 },
    { "name": "block", "type": "float", "default": 1, "min": 1, "max": 100000 },
    { "name": "red", "type": "float", "default": 0, "min": 0, "max": 1 },
    { "name": "green", "type": "float", "default": 0, "min": 0, "max": 1 },
    { "name": "blue", "type": "float", "default": 0, "min": 0, "max": 1 },
    { "name": "alpha", "type": "float", "default": 1, "min": 0, "max": 1 },
    { "name": "blank", "type": "float", "default": 0, "min": 0, "max": 1 },
    { "name": "stretch", "type": "float", "default": 1, "min": 0.0001, "max": 10000 },
    { "name": "count", "type": "float", "default": 0, "min": 0, "max": 256 },
    { "name": "quad", "type": "float", "default": 0, "min": 0, "max": 1 },
    { "name": "quad_width", "type": "float", "default": 1, "min": 0, "max": 1000000 },
    { "name": "quad_height", "type": "float", "default": 1, "min": 0, "max": 1000000 },
    { "name": "h0", "type": "float", "default": 1, "min": -1000000000, "max": 1000000000 },
    { "name": "h1", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h2", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h3", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h4", "type": "float", "default": 1, "min": -1000000000, "max": 1000000000 },
    { "name": "h5", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h6", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h7", "type": "float", "default": 0, "min": -1000000000, "max": 1000000000 },
    { "name": "h8", "type": "float", "default": 1, "min": -1000000000, "max": 1000000000 }
  ],
  "groups": [{ "passes": [{ "shader": "shaders/main.wgsl" }] }]
}"#;

const SHADER: &str = r#"fn picture_at(p: vec2i) -> vec4f {
    if (params.blank > 0.5) { return vec4f(0.0, 0.0, 0.0, 1.0); }
    let limit = vec2i(textureDimensions(picture)) - vec2i(1);
    return textureLoad(picture, clamp(p, vec2i(0), limit), 0);
}

// Point `i` of the polygon, in pixels from the middle of a box of half size `half`. Each
// coordinate is 16 bits over two channels of the row, high byte first, 0 to 65535 across the box.
fn polygon_point(i: i32, half: vec2f) -> vec2f {
    let c = round(textureLoad(points, vec2i(i, 0), 0) * 255.0);
    let part = vec2f(c.x * 256.0 + c.y, c.z * 256.0 + c.w) / 65535.0;
    return (part - 0.5) * half * 2.0;
}

// How far inside the closed polygon `local` is, in pixels: the distance to the nearest side,
// negative outside. Inside by the even-odd rule.
fn polygon_inside(local: vec2f, half: vec2f) -> f32 {
    let n = i32(params.count);
    var nearest = 1e30;
    var odd = false;
    var b = polygon_point(n - 1, half);
    for (var i = 0; i < n; i = i + 1) {
        let a = polygon_point(i, half);
        let edge = b - a;
        let t = clamp(dot(local - a, edge) / max(dot(edge, edge), 1e-12), 0.0, 1.0);
        nearest = min(nearest, length(local - a - edge * t));
        if ((a.y > local.y) != (b.y > local.y) && local.x < edge.x * (local.y - a.y) / (b.y - a.y) + a.x) {
            odd = !odd;
        }
        b = a;
    }
    if (odd) { return nearest; }
    return -nearest;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    if (!gpufx_in_bounds(id)) { return; }
    let p = vec2i(id.xy);
    let original = picture_at(p);
    // In pixels of the picture as it is meant to be seen: the frame may hold a picture of another
    // shape stretched over it, and `stretch` is how many of those pixels one frame pixel is across
    // (1 for a region of the frame itself). So a turn, an ellipse, the soft edge and the blocks keep
    // the shape they have on the picture.
    let unit = vec2f(max(params.stretch, 0.0001), 1.0);
    let size = vec2f(params.width, params.height) * globals.size * unit;
    let centre = (vec2f(params.x, params.y) + vec2f(params.width, params.height) * 0.5) * globals.size;
    let turn = radians(params.rotation);
    let cosine = cos(turn);
    let sine = sin(turn);
    let d = (vec2f(id.xy) + 0.5 - centre) * unit;
    var local = vec2f(cosine * d.x + sine * d.y, cosine * d.y - sine * d.x);
    var half = max(size * 0.5, vec2f(0.0001));
    if (params.quad > 0.5) {
        // In perspective: the pixel taken back into the box (0 to 1 across it) by the inverse
        // homography, then measured in pixels of a box of the quadrilateral's mean size. The map
        // is scaled so the third coordinate is positive inside; past the horizon nothing changes.
        let f = (vec2f(id.xy) + 0.5) / globals.size;
        let q = vec3f(
            params.h0 * f.x + params.h1 * f.y + params.h2,
            params.h3 * f.x + params.h4 * f.y + params.h5,
            params.h6 * f.x + params.h7 * f.y + params.h8);
        if (q.z <= 1e-9) {
            gpufx_write(id, original);
            return;
        }
        half = max(vec2f(params.quad_width, params.quad_height) * 0.5, vec2f(0.0001));
        local = (q.xy / q.z - 0.5) * half * 2.0;
    }
    // How far inside the edge the pixel is, in pixels.
    var inside = 0.0;
    if (params.count >= 3.0) {
        inside = polygon_inside(local, half);
    } else if (params.ellipse > 0.5) {
        inside = (1.0 - length(local / half)) * min(half.x, half.y);
    } else {
        let room = half - abs(local);
        inside = min(room.x, room.y);
    }
    var mask = clamp(inside + 0.5, 0.0, 1.0);
    if (params.feather >= 1.0) {
        mask = smoothstep(0.0, params.feather, inside);
    }
    mask = mask * params.opacity;
    if (mask <= 0.0) {
        gpufx_write(id, original);
        return;
    }
    var changed = vec4f(params.red, params.green, params.blue, params.alpha);
    if (params.mode < 0.5) {
        changed = textureLoad(hidden, p, 0);
    } else if (params.mode < 1.5) {
        // The mean of a few points spread over the block, on an upright grid that starts at the
        // middle of the region, so the blocks move with the region.
        let block = max(params.block, 1.0);
        let cell = floor(d / block) * block;
        let steps = i32(min(ceil(block), 16.0));
        var sum = vec4f(0.0);
        for (var j = 0; j < steps; j = j + 1) {
            for (var i = 0; i < steps; i = i + 1) {
                let q = centre + (cell + (vec2f(f32(i), f32(j)) + 0.5) * block / f32(steps)) / unit;
                sum = sum + picture_at(vec2i(floor(q)));
            }
        }
        changed = sum / f32(steps * steps);
    }
    gpufx_write(id, mix(original, changed, mask));
}
"#;

/// The width of the row of points the censor mix reads: the most points of a polygon.
pub(crate) const POINTS_WIDTH: u32 = engine::polygon::MAX_POLYGON_POINTS as u32;

/// The row of pixels holding `points` (in parts of the box) for the censor mix: each coordinate
/// as 16 bits over two channels, high byte first. The rest of the row is zero.
pub(crate) fn points_row(points: &[[f64; 2]]) -> Vec<u8> {
    let mut rgba = vec![0; POINTS_WIDTH as usize * 4];
    for (pixel, point) in rgba.chunks_exact_mut(4).zip(points) {
        let [x, y] = point.map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16);
        pixel.copy_from_slice(&[(x >> 8) as u8, x as u8, (y >> 8) as u8, y as u8]);
    }
    rgba
}

/// Takes `picture, hidden, points`; see the module documentation.
pub(crate) fn censor_mix_effect(context: &Arc<GpuContext>) -> Result<Effect, String> {
    effect_of(context, MANIFEST, SHADER)
}
