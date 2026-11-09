//! Turns a captured RGBA frame into the PNG an agent gets.

/// `rgba` averaged down to `width` x `height`. Each target pixel covers its share of the source.
fn shrink(rgba: &[u8], source_width: u32, source_height: u32, width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    let span = |index: u32, target: u32, source: u32| {
        let first = u64::from(index) * u64::from(source) / u64::from(target);
        let last = (u64::from(index) + 1) * u64::from(source) / u64::from(target);
        first as usize..(last.max(first + 1)) as usize
    };
    for y in 0..height {
        for x in 0..width {
            let mut sum = [0u64; 4];
            let mut count = 0u64;
            for row in span(y, height, source_height) {
                for column in span(x, width, source_width) {
                    let at = (row * source_width as usize + column) * 4;
                    for channel in 0..4 {
                        sum[channel] += u64::from(rgba[at + channel]);
                    }
                    count += 1;
                }
            }
            out.extend(sum.map(|value| (value / count) as u8));
        }
    }
    out
}

/// The picture no wider than `max_width`, keeping its shape, as PNG. Returns the size written too.
pub fn encode_png(rgba: &[u8], width: u32, height: u32, max_width: u32) -> Result<(Vec<u8>, u32, u32), String> {
    let (target_width, target_height) = if width > max_width { (max_width, (u64::from(height) * u64::from(max_width) / u64::from(width)).max(1) as u32) } else { (width, height) };
    let shrunk;
    let pixels = if (target_width, target_height) == (width, height) {
        rgba
    } else {
        shrunk = shrink(rgba, width, height, target_width, target_height);
        &shrunk
    };
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, target_width, target_height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| format!("encoding the PNG failed: {e}"))?;
    writer.write_image_data(pixels).map_err(|e| format!("encoding the PNG failed: {e}"))?;
    writer.finish().map_err(|e| format!("encoding the PNG failed: {e}"))?;
    Ok((bytes, target_width, target_height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        pixels.truncate(info.buffer_size());
        (info.width, info.height, pixels)
    }

    #[test]
    fn a_small_picture_keeps_every_pixel() {
        let rgba = [10, 20, 30, 255, 40, 50, 60, 255];
        let (bytes, width, height) = encode_png(&rgba, 2, 1, 1280).unwrap();
        assert_eq!((width, height), (2, 1));
        assert_eq!(decode(&bytes), (2, 1, rgba.to_vec()));
    }

    #[test]
    fn a_wide_picture_is_averaged_down_keeping_its_shape() {
        let mut rgba = Vec::new();
        for value in [0u8, 100, 200, 100] {
            rgba.extend([value, value, value, 255]);
        }
        let (bytes, width, height) = encode_png(&rgba, 4, 1, 2).unwrap();
        assert_eq!((width, height), (2, 1));
        assert_eq!(decode(&bytes).2, [50, 50, 50, 255, 150, 150, 150, 255]);
    }

    #[test]
    fn a_very_flat_picture_keeps_one_row() {
        let rgba = vec![9; 1000 * 4];
        assert_eq!(encode_png(&rgba, 1000, 1, 10).unwrap().2, 1);
    }
}
