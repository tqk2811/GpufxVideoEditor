//! The number field of the panels. It changes the project only when the user changed the number.

use egui::emath::round_to_decimals;
use egui::{DragValue, Response, Ui};
use std::ops::RangeInclusive;

/// What the field writes back for `old` when its text is left as it was: `old` as shown, rounded
/// to `decimals`, then clamped to the range as every text edit is.
fn unedited_text_value(old: f64, decimals: usize, range: &RangeInclusive<f64>) -> f64 {
    round_to_decimals(old, decimals).max(*range.start()).min(*range.end())
}

/// A [`DragValue`] showing at most `decimals` decimals. The bool is true when the user really
/// changed `value`; otherwise `value` is left as it was. An existing value outside `range` stays:
/// only an edit is clamped.
pub(super) fn drag_field(ui: &mut Ui, value: &mut f64, range: RangeInclusive<f64>, speed: f64, decimals: usize, suffix: &str, size: Option<[f32; 2]>) -> (Response, bool) {
    let old = *value;
    let field = DragValue::new(value).range(range.clone()).speed(speed).max_decimals(decimals).suffix(suffix).update_while_editing(false).clamp_existing_to_range(false);
    let response = match size {
        Some(size) => ui.add_sized(size, field),
        None => ui.add(field),
    };
    let changed = response.changed() && *value != unedited_text_value(old, decimals, &range);
    if !changed {
        *value = old;
    }
    (response, changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect};

    #[test]
    fn text_left_alone_gives_the_rounded_clamped_value() {
        assert_eq!(unedited_text_value(1.033_333_3, 3, &(0.0..=10.0)), 1.033);
        assert_eq!(unedited_text_value(70.0, 1, &(-60.0..=60.0)), 60.0);
    }

    /// Clicks into the field, then clicks elsewhere, as a user who only looked at the number does.
    fn click_in_and_out(start: f64, range: RangeInclusive<f64>) -> (f64, bool) {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 200.0));
        let mut value = start;
        let mut changed_ever = false;
        let field_rect = std::cell::Cell::new(Rect::NOTHING);
        let mut frame = |events: Vec<Event>| {
            let raw = RawInput { screen_rect: Some(screen), events, ..Default::default() };
            let mut output = ctx.run_ui(raw, |ui| {
                let (response, changed) = drag_field(ui, &mut value, range.clone(), 0.02, 3, " s", None);
                field_rect.set(response.rect);
                changed_ever |= changed;
            });
            output.textures_delta.clear();
        };
        frame(vec![]);
        frame(vec![]);
        let inside = field_rect.get().center();
        let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame(vec![Event::PointerMoved(inside)]);
        frame(vec![press(inside, true)]);
        frame(vec![press(inside, false)]);
        frame(vec![]);
        let outside = Pos2::new(350.0, 180.0);
        frame(vec![Event::PointerMoved(outside)]);
        frame(vec![press(outside, true)]);
        frame(vec![press(outside, false)]);
        frame(vec![]);
        (value, changed_ever)
    }

    #[test]
    fn drawing_leaves_an_out_of_range_value_alone() {
        let ctx = egui::Context::default();
        let mut value = 70.0;
        for _ in 0..2 {
            let mut output = ctx.run_ui(RawInput::default(), |ui| assert!(!drag_field(ui, &mut value, -60.0..=60.0, 0.1, 1, "", None).1));
            output.textures_delta.clear();
        }
        assert_eq!(value, 70.0);
    }

    #[test]
    fn clicking_into_a_field_and_out_changes_nothing() {
        let start = 93_000.0 / 90_000.0;
        assert_eq!(click_in_and_out(start, 0.0..=f64::MAX), (start, false));
    }

    #[test]
    fn clicking_into_a_field_holding_an_out_of_range_value_changes_nothing() {
        assert_eq!(click_in_and_out(70.0, -60.0..=60.0), (70.0, false));
    }
}
