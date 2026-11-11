//! The shape of the picture of each text clip, learnt by measuring it (shaping, no drawing), so
//! the frame on the picture and a place made from it have the shape the render gives the text.
//!
//! It measures with the shared rasterizer the compositor also uses, so the fonts are loaded once
//! for the whole editor; a measure holds its lock only for the shaping, never for a drawing.

use engine::TextContent;
use gpufx_text::TextRasterizer;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::Hasher;
use std::sync::{Arc, Mutex};

/// How many texts to remember; past this the memory starts over.
const MAX_REMEMBERED: usize = 1024;
/// How many of the latest texts are compared whole before anything is hashed: the frame asks
/// about the same few texts every repaint.
const RECENT: usize = 16;

/// What measuring one text said.
#[derive(Debug, Clone, PartialEq)]
pub struct TextShape {
    /// Width over height of the picture.
    pub aspect: f64,
    /// The system font family the text asked for and that is not installed, so the default font
    /// draws it; for a warning.
    pub missing_family: Option<String>,
}

/// The shapes of the texts asked about, by text and output height.
#[derive(Default)]
pub struct TextShapes {
    /// The shared system rasterizer unless set, taken the first time a text is asked about.
    rasterizer: Option<Arc<Mutex<TextRasterizer>>>,
    /// The latest texts asked about, newest last, so a repaint neither hashes nor locks.
    recent: Vec<(TextContent, u32, TextShape)>,
    /// Every text measured, by a hash of its JSON and the output height.
    known: HashMap<(u64, u32), TextShape>,
}

/// Loads the system fonts of the shared rasterizer on a thread of its own, so the first text
/// measured or drawn does not wait about a second for the font scan.
pub fn warm_up_shared() {
    let _ = std::thread::Builder::new().name("font-scan".to_owned()).spawn(|| {
        gpufx_text::shared();
    });
}

impl TextShapes {
    /// Shapes measured with `rasterizer` instead of the shared one with the system fonts.
    pub fn with_rasterizer(rasterizer: Arc<Mutex<TextRasterizer>>) -> TextShapes {
        TextShapes { rasterizer: Some(rasterizer), ..TextShapes::default() }
    }

    /// The shape of `text` in an output `output_height` pixels tall. A text that draws nothing
    /// is as wide as tall.
    pub fn shape(&mut self, text: &TextContent, output_height: u32) -> TextShape {
        if let Some((_, _, shape)) = self.recent.iter().rev().find(|(known, height, _)| *height == output_height && known == text) {
            return shape.clone();
        }
        let key = (text_hash(text), output_height);
        let shape = match self.known.get(&key) {
            Some(shape) => shape.clone(),
            None => {
                let shape = self.measure(text, output_height);
                if self.known.len() >= MAX_REMEMBERED {
                    self.known.clear();
                }
                self.known.insert(key, shape.clone());
                shape
            }
        };
        if self.recent.len() >= RECENT {
            self.recent.remove(0);
        }
        self.recent.push((text.clone(), output_height, shape.clone()));
        shape
    }

    fn measure(&mut self, text: &TextContent, output_height: u32) -> TextShape {
        let rasterizer = self.rasterizer.get_or_insert_with(gpufx_text::shared).clone();
        let mut rasterizer = rasterizer.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (width, height) = rasterizer.measure(text, output_height, 1.0);
        let missing_family = match &text.font {
            engine::FontRef::System { family } if !rasterizer.has_family(family) => Some(family.clone()),
            _ => None,
        };
        let aspect = if width == 0 || height == 0 { 1.0 } else { f64::from(width) / f64::from(height) };
        TextShape { aspect, missing_family }
    }
}

/// A hash of the JSON of `text`, written straight into the hasher without building the string.
fn text_hash(text: &TextContent) -> u64 {
    struct HashWriter(DefaultHasher);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.write(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(DefaultHasher::new());
    let _ = serde_json::to_writer(&mut writer, text);
    writer.0.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_of_text_is_wider_than_tall_and_is_measured_once() {
        let rasterizer = Arc::new(Mutex::new(TextRasterizer::embedded_only()));
        let mut shapes = TextShapes::with_rasterizer(rasterizer);
        let text = TextContent::new("Hello world", 0.1);
        let shape = shapes.shape(&text, 1080);
        assert!(shape.aspect > 2.0, "{shape:?}");
        assert_eq!(shape.missing_family, None);
        assert_eq!(shapes.shape(&text, 1080), shape);
        assert_eq!(shapes.known.len(), 1);
    }

    #[test]
    fn the_measured_shape_matches_the_drawn_picture() {
        let rasterizer = Arc::new(Mutex::new(TextRasterizer::embedded_only()));
        let mut shapes = TextShapes::with_rasterizer(rasterizer.clone());
        let text = TextContent::new("Two\nlines here", 0.1);
        let image = rasterizer.lock().unwrap().rasterize(&text, 720, 1.0);
        let drawn = f64::from(image.width) / f64::from(image.height);
        assert!((shapes.shape(&text, 720).aspect - drawn).abs() < 1e-9);
    }

    #[test]
    fn texts_past_the_recent_ones_are_found_by_hash() {
        let mut shapes = TextShapes::with_rasterizer(Arc::new(Mutex::new(TextRasterizer::embedded_only())));
        let texts: Vec<_> = (0..RECENT + 4).map(|i| TextContent::new(&format!("text {i}"), 0.1)).collect();
        let first: Vec<_> = texts.iter().map(|text| shapes.shape(text, 720)).collect();
        assert_eq!(shapes.recent.len(), RECENT);
        assert_eq!(shapes.shape(&texts[0], 720), first[0]);
        assert_eq!(shapes.known.len(), texts.len(), "nothing measured twice");
    }

    #[test]
    fn a_missing_font_is_reported() {
        let mut shapes = TextShapes::with_rasterizer(Arc::new(Mutex::new(TextRasterizer::embedded_only())));
        let mut text = TextContent::new("Hi", 0.1);
        text.font = engine::FontRef::System { family: "No Such Font 123".into() };
        assert_eq!(shapes.shape(&text, 720).missing_family.as_deref(), Some("No Such Font 123"));
    }
}
