//! The shape of the picture of each text clip, learnt by rasterizing it, so the frame on the
//! picture and a place made from it have the shape the render gives the text.

use engine::TextContent;
use gpufx_text::TextRasterizer;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// How many texts to remember; past this the memory starts over.
const MAX_REMEMBERED: usize = 1024;

/// What rasterizing one text said.
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
    known: HashMap<(String, u32), TextShape>,
}

impl TextShapes {
    /// Shapes measured with `rasterizer` instead of the shared one with the system fonts.
    pub fn with_rasterizer(rasterizer: Arc<Mutex<TextRasterizer>>) -> TextShapes {
        TextShapes { rasterizer: Some(rasterizer), known: HashMap::new() }
    }

    /// The shape of `text` in an output `output_height` pixels tall. A text that draws nothing
    /// is as wide as tall.
    pub fn shape(&mut self, text: &TextContent, output_height: u32) -> TextShape {
        let key = (serde_json::to_string(text).unwrap_or_default(), output_height);
        if let Some(shape) = self.known.get(&key) {
            return shape.clone();
        }
        let rasterizer = self.rasterizer.get_or_insert_with(gpufx_text::shared).clone();
        let image = rasterizer.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).rasterize(text, output_height, 1.0);
        let aspect = if image.width == 0 || image.height == 0 { 1.0 } else { f64::from(image.width) / f64::from(image.height) };
        let shape = TextShape { aspect, missing_family: image.missing_family };
        if self.known.len() >= MAX_REMEMBERED {
            self.known.clear();
        }
        self.known.insert(key, shape.clone());
        shape
    }
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
    fn a_missing_font_is_reported() {
        let mut shapes = TextShapes::with_rasterizer(Arc::new(Mutex::new(TextRasterizer::embedded_only())));
        let mut text = TextContent::new("Hi", 0.1);
        text.font = engine::FontRef::System { family: "No Such Font 123".into() };
        assert_eq!(shapes.shape(&text, 720).missing_family.as_deref(), Some("No Such Font 123"));
    }
}
