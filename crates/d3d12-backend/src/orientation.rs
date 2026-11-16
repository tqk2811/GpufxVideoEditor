//! How a video is turned when shown: the display matrix of its stream (phones record upright video
//! as a landscape stream with a 90 or 270 degree rotation), as clockwise quarter turns.

use std::sync::Arc;

use gpufx_core::{Effect, GpuContext};

use crate::alpha_merge::effect_of;
use crate::ffi;

const TURN_MANIFEST: &str = r#"{
  "api": 1,
  "id": "io.github.tqk2811.editor-turn",
  "name": "Turn",
  "used_for": ["gpufx"],
  "version": "1.0.0",
  "description": "Turns the picture clockwise by quarter turns; odd turns swap its width and height.",
  "inputs": [{ "name": "picture" }],
  "params": [{ "name": "turns", "type": "int", "default": 0, "min": 0, "max": 3 }],
  "groups": [{ "passes": [{ "shader": "shaders/main.wgsl" }] }]
}"#;

// For an odd number of turns the output swaps the input's width and height: the clip graph
// scales the stored picture to the turned frame size first, so this is an exact texel remap,
// with no second resampling.
const TURN_SHADER: &str = r#"@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    if (!gpufx_in_bounds(id)) { return; }
    let size = vec2i(textureDimensions(picture));
    let p = vec2i(id.xy);
    // The texel of the unturned picture each output pixel shows (turn_rgba does the same).
    var source = p;
    if (params.turns == 1) {
        source = vec2i(p.y, size.y - 1 - p.x);
    } else if (params.turns == 2) {
        source = vec2i(size.x - 1 - p.x, size.y - 1 - p.y);
    } else if (params.turns == 3) {
        source = vec2i(size.x - 1 - p.y, p.x);
    }
    gpufx_write(id, textureLoad(picture, clamp(source, vec2i(0), size - vec2i(1)), 0));
}

fn gpufx_output_size(out_index: u32) -> vec2u {
    let s = gpufx_input_size(0u);
    if (params.turns == 1 || params.turns == 3) {
        return vec2u(s.y, s.x);
    }
    return s;
}
"#;

/// The built-in effect that turns a picture by `turns` clockwise quarter turns; an odd number of
/// turns swaps its width and height.
pub(crate) fn turn_effect(context: &Arc<GpuContext>, turns: u8) -> Result<Effect, String> {
    let mut effect = effect_of(context, TURN_MANIFEST, TURN_SHADER)?;
    effect.params_mut().set("turns", &[f64::from(turns % 4)]).map_err(|e| format!("turn.turns: {e}"))?;
    Ok(effect)
}

/// Clockwise quarter turns (0 to 3) that show a stream the way it is meant to be seen, from the
/// counterclockwise angle `av_display_rotation_get` reads off its display matrix. FFmpeg's own
/// autorotate turns the same way. Angles that are not a multiple of 90 degrees go to the nearest.
pub fn turns_of_rotation(counterclockwise_degrees: f64) -> u8 {
    if !counterclockwise_degrees.is_finite() {
        return 0;
    }
    ((-counterclockwise_degrees / 90.0).round() as i64).rem_euclid(4) as u8
}

/// The quarter turns of `stream`'s display matrix, 0 without one. Flips in the matrix are ignored.
///
/// # Safety
/// `stream` must be a valid stream of an open format context.
pub unsafe fn stream_turns(stream: *const ffi::AVStream) -> u8 {
    let parameters = (*stream).codecpar;
    if parameters.is_null() {
        return 0;
    }
    let side = ffi::av_packet_side_data_get((*parameters).coded_side_data, (*parameters).nb_coded_side_data, ffi::AV_PKT_DATA_DISPLAYMATRIX);
    if side.is_null() || (*side).size < 9 * std::mem::size_of::<i32>() {
        return 0;
    }
    let matrix: *const i32 = (*side).data.cast();
    if is_mirrored(&*matrix.cast::<[i32; 9]>()) {
        eprintln!("orientation: the display matrix of the video mirrors the picture; only its rotation is applied");
    }
    turns_of_rotation(ffi::av_display_rotation_get(matrix))
}

/// Whether a display matrix (FFmpeg's 3x3 layout, 16.16 fixed point) mirrors the picture: its
/// 2x2 part has a negative determinant.
pub fn is_mirrored(matrix: &[i32; 9]) -> bool {
    let determinant = i64::from(matrix[0]) * i64::from(matrix[4]) - i64::from(matrix[1]) * i64::from(matrix[3]);
    determinant < 0
}

/// Width and height of a `width` x `height` picture once turned by `turns`.
pub fn turned_size<T>(width: T, height: T, turns: u8) -> (T, T) {
    if turns % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    }
}

/// `rgba` (`width` x `height`, tight rows) turned clockwise by `turns` quarter turns, with its new size.
pub fn turn_rgba(rgba: &[u8], width: u32, height: u32, turns: u8) -> (Vec<u8>, u32, u32) {
    let (w, h) = (width as usize, height as usize);
    let turns = turns % 4;
    if turns == 0 {
        return (rgba.to_vec(), width, height);
    }
    let (out_w, out_h) = turned_size(w, h, turns);
    let mut out = vec![0_u8; rgba.len()];
    for y in 0..out_h {
        for x in 0..out_w {
            // The source pixel each output pixel shows: a clockwise turn moves the top-left corner
            // to the top-right one.
            let (sx, sy) = match turns {
                1 => (y, h - 1 - x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (w - 1 - y, x),
            };
            let from = (sy * w + sx) * 4;
            let to = (y * out_w + x) * 4;
            out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }
    (out, out_w as u32, out_h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_display_rotation_becomes_clockwise_turns() {
        assert_eq!(turns_of_rotation(0.0), 0);
        // A phone held upright stores -90: shown after one clockwise turn, as FFmpeg's autorotate does.
        assert_eq!(turns_of_rotation(-90.0), 1);
        assert_eq!(turns_of_rotation(90.0), 3);
        assert_eq!(turns_of_rotation(180.0), 2);
        assert_eq!(turns_of_rotation(-180.0), 2);
        assert_eq!(turns_of_rotation(270.0), 1);
        assert_eq!(turns_of_rotation(-89.0), 1, "the nearest quarter turn");
        assert_eq!(turns_of_rotation(f64::NAN), 0);
    }

    #[test]
    fn a_flipped_display_matrix_is_seen_as_mirrored() {
        let one = 1 << 16;
        assert!(!is_mirrored(&[one, 0, 0, 0, one, 0, 0, 0, 1 << 30]), "identity");
        assert!(!is_mirrored(&[0, -one, 0, one, 0, 0, 0, 0, 1 << 30]), "a quarter turn");
        assert!(is_mirrored(&[-one, 0, 0, 0, one, 0, 0, 0, 1 << 30]), "a horizontal flip");
        assert!(is_mirrored(&[0, one, 0, one, 0, 0, 0, 0, 1 << 30]), "a turn with a flip");
    }

    #[test]
    fn turning_moves_the_corners_clockwise() {
        // 2 x 1: red on the left, blue on the right.
        let picture = [255, 0, 0, 255, 0, 0, 255, 255];
        let (once, w, h) = turn_rgba(&picture, 2, 1, 1);
        assert_eq!((w, h), (1, 2));
        assert_eq!(once, [255, 0, 0, 255, 0, 0, 255, 255], "left goes to the top");
        let (twice, ..) = turn_rgba(&picture, 2, 1, 2);
        assert_eq!(twice, [0, 0, 255, 255, 255, 0, 0, 255]);
        let (thrice, w, h) = turn_rgba(&picture, 2, 1, 3);
        assert_eq!((w, h), (1, 2));
        assert_eq!(thrice, [0, 0, 255, 255, 255, 0, 0, 255], "left goes to the bottom");
        assert_eq!(turned_size(1920, 1080, 1), (1080, 1920));
        assert_eq!(turned_size(1920, 1080, 2), (1920, 1080));
    }
}
