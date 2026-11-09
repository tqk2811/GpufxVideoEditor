//! How a package parameter is edited: which widget its manifest entry asks for, and reading and
//! writing its value in a project's parameter map. No window here.

use d3d12_backend::{ParamInfo, ParamKind};
use serde_json::{json, Map, Value};

/// The parameter a transition's clock drives; never edited by hand.
const PROGRESS: &str = "progress";

#[derive(Debug, Clone, PartialEq)]
pub enum ParamWidget {
    Slider { min: f64, max: f64, integer: bool },
    Number { min: Option<f64>, max: Option<f64>, integer: bool },
    Checkbox,
    Color,
    Dropdown(Vec<(String, i32)>),
    Vector { components: usize, min: Option<f64>, max: Option<f64> },
    Expression,
}

pub fn widget_for(info: &ParamInfo) -> ParamWidget {
    let integer = info.kind == ParamKind::Int;
    match info.kind {
        ParamKind::Float | ParamKind::Int => match (info.min, info.max) {
            (Some(min), Some(max)) if min.is_finite() && max.is_finite() && max > min => ParamWidget::Slider { min, max, integer },
            (min, max) => ParamWidget::Number { min, max, integer },
        },
        ParamKind::Bool => ParamWidget::Checkbox,
        ParamKind::Color => ParamWidget::Color,
        ParamKind::Enum => ParamWidget::Dropdown(info.choices.clone()),
        ParamKind::Vec2 => ParamWidget::Vector { components: 2, min: info.min, max: info.max },
        ParamKind::Vec3 => ParamWidget::Vector { components: 3, min: info.min, max: info.max },
        ParamKind::Vec4 => ParamWidget::Vector { components: 4, min: info.min, max: info.max },
        ParamKind::Expr => ParamWidget::Expression,
    }
}

/// The parameters offered in the inspector: everything but the transition clock.
pub fn editable(params: &[ParamInfo]) -> impl Iterator<Item = &ParamInfo> {
    params.iter().filter(|info| info.name != PROGRESS)
}

/// `value` inside the range of `info`, rounded when the parameter is an integer.
pub fn clamp_number(info: &ParamInfo, value: f64) -> f64 {
    let value = if value.is_finite() { value } else { 0.0 };
    let value = if info.kind == ParamKind::Int { value.round() } else { value };
    let value = info.min.map_or(value, |min| value.max(min));
    info.max.map_or(value, |max| value.min(max))
}

fn stated<'a>(params: &'a Map<String, Value>, info: &'a ParamInfo) -> Option<&'a Value> {
    params.get(&info.name).or(info.default.as_ref())
}

/// A number from the project or the manifest. A parameter with neither is 0, as gpufx treats it.
pub fn number_value(params: &Map<String, Value>, info: &ParamInfo) -> f64 {
    let value = stated(params, info).and_then(|value| value.as_f64().or_else(|| value.as_bool().map(|flag| f64::from(u8::from(flag))))).unwrap_or(0.0);
    clamp_number(info, value)
}

pub fn bool_value(params: &Map<String, Value>, info: &ParamInfo) -> bool {
    stated(params, info).is_some_and(|value| value.as_bool().unwrap_or_else(|| value.as_f64().is_some_and(|number| number != 0.0)))
}

/// The numbers of a vector value; components not stated are 0.
pub fn vector_value(params: &Map<String, Value>, info: &ParamInfo, components: usize) -> Vec<f64> {
    let parsed = match stated(params, info) {
        Some(Value::Array(items)) => items.iter().map(|item| item.as_f64().unwrap_or(0.0)).collect(),
        Some(Value::String(text)) => text.split(',').map(|part| part.trim().parse().unwrap_or(0.0)).collect(),
        Some(Value::Number(number)) => vec![number.as_f64().unwrap_or(0.0)],
        _ => Vec::new(),
    };
    (0..components).map(|index| parsed.get(index).copied().unwrap_or(0.0)).collect()
}

/// A colour as RGBA from 0 to 1. Text may be `#rrggbb[aa]` or `r,g,b[,a]`, as in gpufx.
pub fn color_value(params: &Map<String, Value>, info: &ParamInfo) -> [f32; 4] {
    let numbers: Vec<f64> = match stated(params, info) {
        Some(Value::Array(items)) => items.iter().map(|item| item.as_f64().unwrap_or(0.0)).collect(),
        Some(Value::String(text)) => parse_color_text(text),
        _ => Vec::new(),
    };
    let channel = |index: usize, missing: f64| numbers.get(index).copied().unwrap_or(missing).clamp(0.0, 1.0) as f32;
    [channel(0, 0.0), channel(1, 0.0), channel(2, 0.0), channel(3, 1.0)]
}

fn parse_color_text(text: &str) -> Vec<f64> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('#') {
        if (hex.len() == 6 || hex.len() == 8) && hex.is_ascii() {
            return (0..hex.len() / 2).map(|i| f64::from(u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or(0)) / 255.0).collect();
        }
        return Vec::new();
    }
    text.split(',').map(|part| part.trim().parse().unwrap_or(0.0)).collect()
}

/// The number of the chosen enum value: stated as a number or by name, else the first choice.
pub fn enum_value(params: &Map<String, Value>, info: &ParamInfo) -> i32 {
    let first = info.choices.first().map_or(0, |choice| choice.1);
    match stated(params, info) {
        Some(Value::Number(number)) => number.as_f64().map_or(first, |value| value.round() as i32),
        Some(Value::String(name)) => info.choices.iter().find(|choice| choice.0 == *name).map_or(first, |choice| choice.1),
        _ => first,
    }
}

pub fn expression_value(params: &Map<String, Value>, info: &ParamInfo) -> String {
    stated(params, info).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// The map with `name` set. The others are left as they were.
pub fn with_value(params: &Map<String, Value>, name: &str, value: Value) -> Map<String, Value> {
    let mut next = params.clone();
    next.insert(name.to_owned(), value);
    next
}

/// The JSON of a number, written as an integer for an integer parameter.
pub fn number_json(info: &ParamInfo, value: f64) -> Value {
    let value = clamp_number(info, value);
    if info.kind == ParamKind::Int {
        json!(value as i64)
    } else {
        json!(value)
    }
}

/// The map with a colour set.
pub fn color_json(color: [f32; 4]) -> Value {
    json!([f64::from(color[0]), f64::from(color[1]), f64::from(color[2]), f64::from(color[3])])
}

/// A vector with each component clamped to the range of `info`.
pub fn vector_json(info: &ParamInfo, values: &[f64]) -> Value {
    let clamped = values.iter().map(|&value| {
        let value = if value.is_finite() { value } else { 0.0 };
        let value = info.min.map_or(value, |min| value.max(min));
        info.max.map_or(value, |max| value.min(max))
    });
    Value::Array(clamped.map(|value| json!(value)).collect())
}

/// Short display name of a parameter: `radius_v` becomes `Radius v`.
pub fn display_name(info: &ParamInfo) -> String {
    let spaced = info.name.replace('_', " ");
    let mut chars = spaced.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().collect::<String>() + chars.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(kind: ParamKind) -> ParamInfo {
        ParamInfo { name: "a".to_owned(), kind, default: None, min: None, max: None, description: String::new(), vars: Vec::new(), choices: Vec::new() }
    }

    fn ranged(kind: ParamKind, min: f64, max: f64) -> ParamInfo {
        ParamInfo { min: Some(min), max: Some(max), ..info(kind) }
    }

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn a_ranged_number_is_a_slider_and_one_without_a_range_a_numeric_field() {
        assert_eq!(widget_for(&ranged(ParamKind::Float, -1.0, 1.0)), ParamWidget::Slider { min: -1.0, max: 1.0, integer: false });
        assert_eq!(widget_for(&ranged(ParamKind::Int, 0.0, 64.0)), ParamWidget::Slider { min: 0.0, max: 64.0, integer: true });
        assert_eq!(widget_for(&info(ParamKind::Float)), ParamWidget::Number { min: None, max: None, integer: false });
        let half_open = ParamInfo { min: Some(0.0), ..info(ParamKind::Int) };
        assert_eq!(widget_for(&half_open), ParamWidget::Number { min: Some(0.0), max: None, integer: true });
        assert_eq!(widget_for(&ranged(ParamKind::Float, 2.0, 2.0)), ParamWidget::Number { min: Some(2.0), max: Some(2.0), integer: false }, "a slider over nothing is useless");
    }

    #[test]
    fn each_other_type_has_its_widget() {
        assert_eq!(widget_for(&info(ParamKind::Bool)), ParamWidget::Checkbox);
        assert_eq!(widget_for(&info(ParamKind::Color)), ParamWidget::Color);
        assert_eq!(widget_for(&info(ParamKind::Expr)), ParamWidget::Expression);
        let choice = ParamInfo { choices: vec![("x".to_owned(), 0)], ..info(ParamKind::Enum) };
        assert_eq!(widget_for(&choice), ParamWidget::Dropdown(vec![("x".to_owned(), 0)]));
        assert_eq!(widget_for(&ranged(ParamKind::Vec2, 0.0, 1.0)), ParamWidget::Vector { components: 2, min: Some(0.0), max: Some(1.0) });
        assert_eq!(widget_for(&info(ParamKind::Vec4)), ParamWidget::Vector { components: 4, min: None, max: None });
    }

    #[test]
    fn a_transition_clock_is_not_offered() {
        let params = vec![ParamInfo { name: "progress".to_owned(), ..info(ParamKind::Float) }, ParamInfo { name: "softness".to_owned(), ..info(ParamKind::Float) }];
        let names: Vec<&str> = editable(&params).map(|param| param.name.as_str()).collect();
        assert_eq!(names, vec!["softness"]);
    }

    #[test]
    fn numbers_are_clamped_and_integers_rounded() {
        let int = ranged(ParamKind::Int, -1.0, 64.0);
        assert_eq!(clamp_number(&int, 3.6), 4.0);
        assert_eq!(clamp_number(&int, 900.0), 64.0);
        assert_eq!(clamp_number(&int, -5.0), -1.0);
        assert_eq!(clamp_number(&ranged(ParamKind::Float, 0.0, 1.0), 0.25), 0.25);
        assert_eq!(clamp_number(&info(ParamKind::Float), f64::NAN), 0.0);
        assert_eq!(number_json(&int, 3.6), json!(4));
        assert!(number_json(&int, 3.6).is_i64(), "an integer parameter stays a JSON integer");
    }

    #[test]
    fn a_missing_value_uses_the_manifest_default_inside_its_range() {
        let param = ParamInfo { default: Some(json!(0.5)), ..ranged(ParamKind::Float, 0.0, 1.0) };
        assert_eq!(number_value(&Map::new(), &param), 0.5);
        assert_eq!(number_value(&map(json!({ "a": 0.25 })), &param), 0.25);
        assert_eq!(number_value(&map(json!({ "a": 9 })), &param), 1.0, "a value out of range is shown clamped");
    }

    #[test]
    fn booleans_may_be_stored_as_numbers() {
        let param = info(ParamKind::Bool);
        assert!(!bool_value(&Map::new(), &param));
        assert!(bool_value(&map(json!({ "a": true })), &param));
        assert!(bool_value(&map(json!({ "a": 1 })), &param));
        assert!(!bool_value(&map(json!({ "a": 0 })), &param));
    }

    #[test]
    fn colours_parse_from_arrays_hex_and_comma_text() {
        let param = info(ParamKind::Color);
        assert_eq!(color_value(&map(json!({ "a": [1.0, 0.5, 0.0] })), &param), [1.0, 0.5, 0.0, 1.0]);
        assert_eq!(color_value(&map(json!({ "a": "#ff000080" })), &param), [1.0, 0.0, 0.0, 128.0 / 255.0]);
        assert_eq!(color_value(&map(json!({ "a": "0,1,0" })), &param), [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(color_value(&Map::new(), &param), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(color_json([1.0, 0.0, 0.0, 1.0]), json!([1.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn enums_read_a_number_or_a_name() {
        let param = ParamInfo { choices: vec![("add".to_owned(), 2), ("multiply".to_owned(), 5)], ..info(ParamKind::Enum) };
        assert_eq!(enum_value(&Map::new(), &param), 2, "the first choice");
        assert_eq!(enum_value(&map(json!({ "a": 5 })), &param), 5);
        assert_eq!(enum_value(&map(json!({ "a": "multiply" })), &param), 5);
        assert_eq!(enum_value(&map(json!({ "a": "other" })), &param), 2);
    }

    #[test]
    fn vectors_fill_missing_components_and_clamp() {
        let param = ranged(ParamKind::Vec2, 0.0, 1.0);
        assert_eq!(vector_value(&map(json!({ "a": [0.5] })), &param, 2), vec![0.5, 0.0]);
        assert_eq!(vector_value(&map(json!({ "a": "0.25,0.75" })), &param, 2), vec![0.25, 0.75]);
        assert_eq!(vector_json(&param, &[2.0, -1.0]), json!([1.0, 0.0]));
    }

    #[test]
    fn setting_a_value_leaves_the_others() {
        let base = map(json!({ "a": 1, "b": 2 }));
        assert_eq!(with_value(&base, "a", json!(7)), map(json!({ "a": 7, "b": 2 })));
    }

    #[test]
    fn names_are_shown_with_spaces() {
        let param = ParamInfo { name: "radius_v".to_owned(), ..info(ParamKind::Int) };
        assert_eq!(display_name(&param), "Radius v");
    }
}
