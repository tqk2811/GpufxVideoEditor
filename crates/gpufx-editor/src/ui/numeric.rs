//! The number field of the panels. It changes the project only when the user changed the number.

use egui::emath::round_to_decimals;
use egui::{DragValue, Response, Ui};
use std::ops::RangeInclusive;

/// `range` grown to hold `old`, so that text left as it was comes back as `old` and only a value
/// the user entered can land on a bound.
fn range_holding(old: f64, range: &RangeInclusive<f64>) -> RangeInclusive<f64> {
    range.start().min(old)..=range.end().max(old)
}

/// A [`DragValue`] showing at most `decimals` decimals. The bool is true when the user really
/// changed `value`; otherwise `value` is left as it was. An existing value outside `range` stays:
/// only an edit is clamped.
pub(super) fn drag_field(ui: &mut Ui, value: &mut f64, range: RangeInclusive<f64>, speed: f64, decimals: usize, suffix: &str, size: Option<[f32; 2]>) -> (Response, bool) {
    let old = *value;
    let field = DragValue::new(value).range(range_holding(old, &range)).speed(speed).max_decimals(decimals).suffix(suffix).update_while_editing(false).clamp_existing_to_range(false);
    let response = match size {
        Some(size) => ui.add_sized(size, field),
        None => ui.add(field),
    };
    let changed = response.changed() && *value != round_to_decimals(old, decimals);
    // Not `f64::clamp`: it panics on a reversed range, which a package manifest can state.
    *value = if changed { value.max(*range.start()).min(*range.end()) } else { old };
    (response, changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, Pos2, RawInput, Rect};

    #[test]
    fn the_range_of_the_field_grows_only_to_hold_the_old_value() {
        assert_eq!(range_holding(5.0, &(0.0..=10.0)), 0.0..=10.0);
        assert_eq!(range_holding(70.0, &(-60.0..=60.0)), -60.0..=70.0);
        assert_eq!(range_holding(-80.0, &(-60.0..=60.0)), -80.0..=60.0);
    }

    /// Clicks into the field, then clicks elsewhere, as a user who only looked at the number does.
    fn click_in_and_out(start: f64, range: RangeInclusive<f64>) -> (f64, bool) {
        edit_field(start, range, None)
    }

    /// As [`click_in_and_out`], typing `typed` over the whole field in between.
    fn edit_field(start: f64, range: RangeInclusive<f64>, typed: Option<&str>) -> (f64, bool) {
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
        if let Some(typed) = typed {
            let select_all = Event::Key { key: egui::Key::A, physical_key: Some(egui::Key::A), pressed: true, repeat: false, modifiers: Modifiers::COMMAND };
            frame(vec![select_all, Event::Text(typed.to_owned())]);
        }
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
        assert_eq!(click_in_and_out(-80.0, -60.0..=60.0), (-80.0, false));
    }

    #[test]
    fn typing_the_bound_over_an_out_of_range_value_is_an_edit() {
        assert_eq!(edit_field(-80.0, -60.0..=60.0, Some("-60")), (-60.0, true));
        assert_eq!(edit_field(70.0, -60.0..=60.0, Some("60")), (60.0, true));
    }

    /// A manifest may state min above max, and a vector value is not clamped when it is read, so
    /// the field can hold one outside both bounds. The upper bound wins, as in `clamp_number`.
    #[test]
    fn typing_into_a_field_with_a_reversed_range_does_not_panic() {
        assert_eq!(edit_field(20.0, 10.0..=0.0, Some("15")), (0.0, true));
    }

    #[test]
    fn typing_past_the_range_is_clamped_even_toward_an_out_of_range_value() {
        assert_eq!(edit_field(-80.0, -60.0..=60.0, Some("-70")), (-60.0, true));
        assert_eq!(edit_field(5.0, 0.0..=10.0, Some("99")), (10.0, true));
    }
}
