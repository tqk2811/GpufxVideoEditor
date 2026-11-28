//! Motion paths: a clip moves along a curve written as two equations, `x = f(t)` and `y = g(t)`,
//! with an optional third for its rotation.
//!
//! The expressions are those of the `expr` parameters of the effects (crate `gpufx-expr`): numbers,
//! the variables below, `+ - * / ^`, parentheses and a fixed list of math functions.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `t` | seconds since the start of the clip |
//! | `d` | the length of the clip in seconds |
//! | `p` | `t / d`, from 0 to 1 |
//! | `dir` | rotation only: the heading of the path at `t`, in clockwise degrees like the Rotation property (0 is to the right) |
//!
//! `x` and `y` are offsets in parts of the frame width and height, added after the keyframes of
//! X and Y. The rotation is added to the Rotation property. A value that is not finite counts as 0.

use std::sync::OnceLock;

use gpufx_expr::Expr;
use serde::{Deserialize, Serialize};

/// Step of the finite difference that gives `dir`, in seconds.
const DIR_STEP: f64 = 1e-3;

/// A curve a clip moves along. Empty `x` or `y` is 0.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MotionPath {
    #[serde(default)]
    pub x: String,
    #[serde(default)]
    pub y: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<String>,
    /// The parsed expressions, made on first use. They are of the texts above as they were then:
    /// a path whose texts changed afterwards is parsed again on each use (build a new path instead).
    #[serde(skip)]
    compiled: OnceLock<(Texts, Option<Compiled>)>,
}

impl PartialEq for MotionPath {
    fn eq(&self, other: &Self) -> bool {
        self.x == other.x && self.y == other.y && self.rotation == other.rotation
    }
}

type Texts = (String, String, Option<String>);

#[derive(Debug, Clone)]
struct Compiled {
    x: Option<Expr>,
    y: Option<Expr>,
    rotation: Option<Expr>,
}

/// Which expression of a path a problem is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathField {
    X,
    Y,
    Rotation,
}

impl PathField {
    pub fn name(self) -> &'static str {
        match self {
            PathField::X => "x",
            PathField::Y => "y",
            PathField::Rotation => "rotation",
        }
    }
}

/// The variables of `x` and `y`.
pub fn position_vars() -> Vec<String> {
    ["t", "d", "p"].map(String::from).to_vec()
}

/// The variables of the rotation: those of the position and `dir`.
pub fn rotation_vars() -> Vec<String> {
    ["t", "d", "p", "dir"].map(String::from).to_vec()
}

fn parse_optional(text: &str, vars: &[String]) -> Result<Option<Expr>, String> {
    if text.trim().is_empty() {
        Ok(None)
    } else {
        Expr::parse(text, vars).map(Some).map_err(|e| e.to_string())
    }
}

fn finite(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

/// Ready-made paths offered by the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathPreset {
    Circle,
    FigureEight,
    SineWave,
    Spiral,
}

impl PathPreset {
    pub const ALL: [PathPreset; 4] = [PathPreset::Circle, PathPreset::FigureEight, PathPreset::SineWave, PathPreset::Spiral];

    pub fn path(self) -> MotionPath {
        let (x, y) = match self {
            PathPreset::Circle => ("0.2*cos(tau*p)", "0.2*sin(tau*p)"),
            PathPreset::FigureEight => ("0.25*sin(tau*p)", "0.15*sin(2*tau*p)"),
            PathPreset::SineWave => ("0.6*(p-0.5)", "0.1*sin(2*tau*p)"),
            PathPreset::Spiral => ("0.3*p*cos(2*tau*p)", "0.3*p*sin(2*tau*p)"),
        };
        MotionPath::new(x, y, None)
    }
}

impl MotionPath {
    pub fn new(x: impl Into<String>, y: impl Into<String>, rotation: Option<String>) -> Self {
        Self { x: x.into(), y: y.into(), rotation, compiled: OnceLock::new() }
    }

    fn texts(&self) -> Texts {
        (self.x.clone(), self.y.clone(), self.rotation.clone())
    }

    /// The parsed expressions, `None` when one does not parse.
    fn compile(&self) -> (Texts, Option<Compiled>) {
        let position = position_vars();
        let parsed = (|| {
            Some(Compiled {
                x: parse_optional(&self.x, &position).ok()?,
                y: parse_optional(&self.y, &position).ok()?,
                rotation: match &self.rotation {
                    Some(text) => parse_optional(text, &rotation_vars()).ok()?,
                    None => None,
                },
            })
        })();
        (self.texts(), parsed)
    }

    fn with_compiled<R>(&self, f: impl FnOnce(Option<&Compiled>) -> R) -> R {
        let cached = self.compiled.get_or_init(|| self.compile());
        if cached.0 == self.texts() {
            f(cached.1.as_ref())
        } else {
            f(self.compile().1.as_ref())
        }
    }

    /// The first expression that does not parse, and why.
    pub fn problem(&self) -> Option<(PathField, String)> {
        let position = position_vars();
        if let Err(e) = parse_optional(&self.x, &position) {
            return Some((PathField::X, e));
        }
        if let Err(e) = parse_optional(&self.y, &position) {
            return Some((PathField::Y, e));
        }
        if let Some(text) = &self.rotation {
            if let Err(e) = parse_optional(text, &rotation_vars()) {
                return Some((PathField::Rotation, e));
            }
        }
        None
    }

    /// Whether the path has a rotation expression (that is not empty).
    pub fn rotates(&self) -> bool {
        self.rotation.as_deref().is_some_and(|text| !text.trim().is_empty())
    }

    /// The offset `[x, y]` at `t` seconds of a clip `d` seconds long, in parts of the frame. 0 when
    /// an expression does not parse.
    pub fn offset_at(&self, t: f64, d: f64) -> [f64; 2] {
        self.with_compiled(|c| c.map_or([0.0, 0.0], |c| offset(c, t, d)))
    }

    /// The added rotation at `t` seconds of a clip `d` seconds long, in clockwise degrees.
    pub fn rotation_at(&self, t: f64, d: f64) -> f64 {
        self.with_compiled(|c| c.map_or(0.0, |c| rotation(c, t, d)))
    }

    /// The offset and the added rotation at once.
    pub fn sample(&self, t: f64, d: f64) -> ([f64; 2], f64) {
        self.with_compiled(|c| c.map_or(([0.0, 0.0], 0.0), |c| (offset(c, t, d), rotation(c, t, d))))
    }
}

fn eval(expr: &Option<Expr>, vars: &[f64]) -> f64 {
    expr.as_ref().map_or(0.0, |e| finite(e.eval(vars)))
}

fn position_values(t: f64, d: f64) -> [f64; 3] {
    let p = if d > 0.0 { t / d } else { 0.0 };
    [t, d, p]
}

fn offset(c: &Compiled, t: f64, d: f64) -> [f64; 2] {
    let vars = position_values(t, d);
    [eval(&c.x, &vars), eval(&c.y, &vars)]
}

/// The heading of the path at `t` in clockwise degrees (y goes down), from a central difference.
/// 0 where the path stands still.
fn heading(c: &Compiled, t: f64, d: f64) -> f64 {
    let ([x0, y0], [x1, y1]) = (offset(c, t - DIR_STEP, d), offset(c, t + DIR_STEP, d));
    let (dx, dy) = (x1 - x0, y1 - y0);
    if dx.abs() < 1e-12 && dy.abs() < 1e-12 {
        0.0
    } else {
        finite(dy.atan2(dx).to_degrees())
    }
}

fn rotation(c: &Compiled, t: f64, d: f64) -> f64 {
    let Some(expr) = &c.rotation else { return 0.0 };
    let [t, d, p] = position_values(t, d);
    finite(expr.eval(&[t, d, p, heading(c, t, d)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} != {b}");
    }

    #[test]
    fn empty_is_zero_and_vars_work() {
        let path = MotionPath::new("", "p", None);
        assert_eq!(path.offset_at(1.0, 4.0), [0.0, 0.25]);
        let path = MotionPath::new("t", "d", None);
        assert_eq!(path.offset_at(1.5, 3.0), [1.5, 3.0]);
        assert_eq!(MotionPath::new("p", "", None).offset_at(1.0, 0.0), [0.0, 0.0], "p of a clip of no length is 0");
    }

    #[test]
    fn non_finite_counts_as_zero() {
        let path = MotionPath::new("1/0", "log(0-1)", Some("1/0".into()));
        assert_eq!(path.sample(1.0, 2.0), ([0.0, 0.0], 0.0));
    }

    #[test]
    fn dir_follows_the_heading() {
        let right = MotionPath::new("t", "", Some("dir".into()));
        close(right.rotation_at(0.5, 1.0), 0.0);
        let down = MotionPath::new("", "t", Some("dir".into()));
        close(down.rotation_at(0.5, 1.0), 90.0);
        let up = MotionPath::new("", "0-t", Some("dir+10".into()));
        close(up.rotation_at(0.5, 1.0), -80.0);
        let still = MotionPath::new("", "", Some("dir".into()));
        assert_eq!(still.rotation_at(0.5, 1.0), 0.0);
    }

    #[test]
    fn problems_name_the_field() {
        assert_eq!(MotionPath::new("t", "q", None).problem().map(|p| p.0), Some(PathField::Y));
        assert_eq!(MotionPath::new("dir", "", None).problem().map(|p| p.0), Some(PathField::X), "dir is only for the rotation");
        assert_eq!(MotionPath::new("", "", Some("dir*2".into())).problem(), None);
        assert_eq!(MotionPath::new("", "", Some("sin(".into())).problem().map(|p| p.0), Some(PathField::Rotation));
        assert_eq!(MotionPath::new("sin(", "", None).offset_at(1.0, 2.0), [0.0, 0.0]);
    }

    #[test]
    fn changed_texts_are_parsed_again() {
        let mut path = MotionPath::new("1", "", None);
        assert_eq!(path.offset_at(0.0, 1.0)[0], 1.0);
        path.x = "2".into();
        assert_eq!(path.offset_at(0.0, 1.0)[0], 2.0, "the cache is of the old text");
        let cloned = path.clone();
        assert_eq!(cloned, path);
    }

    #[test]
    fn presets_parse() {
        for preset in PathPreset::ALL {
            assert_eq!(preset.path().problem(), None, "{preset:?}");
        }
        let circle = PathPreset::Circle.path().offset_at(0.0, 2.0);
        close(circle[0], 0.2);
        close(circle[1], 0.0);
    }

    #[test]
    fn serde_skips_the_cache_and_an_absent_rotation() {
        let path = MotionPath::new("t", "", None);
        path.offset_at(0.0, 1.0);
        let json = serde_json::to_string(&path).unwrap();
        assert_eq!(json, r#"{"x":"t","y":""}"#);
        let back: MotionPath = serde_json::from_str(&json).unwrap();
        assert_eq!(back, path);
    }
}
