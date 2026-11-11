//! Icons drawn with egui shapes, so they need nothing from the fonts.

use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Vec2};

use super::theme;

/// The side of a keyframe button.
const KEYFRAME_BUTTON_SIDE: f32 = 16.0;

/// A diamond that adds or removes a keyframe: filled on a keyframe, outlined in the accent colour
/// when the row has keyframes elsewhere, and dim otherwise.
pub fn keyframe_button(ui: &mut egui::Ui, on_keyframe: bool, keyed: bool, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(KEYFRAME_BUTTON_SIDE), Sense::click());
    let center = rect.center();
    let radius = rect.width() * 0.35;
    let color = if !ui.is_enabled() {
        theme::text_dim().gamma_multiply(0.5)
    } else if on_keyframe || keyed {
        theme::accent_text()
    } else {
        theme::text_dim()
    };
    let points = vec![center + Vec2::new(0.0, -radius), center + Vec2::new(radius, 0.0), center + Vec2::new(0.0, radius), center + Vec2::new(-radius, 0.0)];
    let fill = if on_keyframe { color } else { Color32::TRANSPARENT };
    ui.painter().add(Shape::convex_polygon(points, fill, Stroke::new(1.25, color)));
    response.on_hover_text(tooltip)
}

/// A small arrow that moves the playhead to the keyframe before (`forward` false) or after the
/// playhead. Off when there is none that way.
pub fn keyframe_step_button(ui: &mut egui::Ui, forward: bool, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(KEYFRAME_BUTTON_SIDE), Sense::click());
    let color = if !ui.is_enabled() {
        theme::text_dim().gamma_multiply(0.5)
    } else if response.hovered() {
        theme::accent_text()
    } else {
        theme::text_dim()
    };
    let (center, reach, half) = (rect.center(), rect.width() * 0.22, rect.height() * 0.28);
    let direction = if forward { 1.0 } else { -1.0 };
    let points = vec![center + Vec2::new(-reach * direction, -half), center + Vec2::new(reach * direction, 0.0), center + Vec2::new(-reach * direction, half)];
    ui.painter().add(Shape::line(points, Stroke::new(1.5, color)));
    response.on_hover_text(tooltip)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Film,
    Music,
    Sticker,
    Text,
    Sparkle,
    Transition,
    Upload,
    Plus,
    Play,
    Stop,
    Undo,
    Redo,
    Scissors,
    TrimLeft,
    TrimRight,
    Trash,
    Fullscreen,
    Exit,
    ZoomIn,
    ZoomOut,
    Export,
    Lock,
    Unlock,
    Eye,
    EyeOff,
    Magnet,
    Warning,
}

const ICON_STROKE: f32 = 1.5;

fn arc(center: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    const STEPS: usize = 14;
    (0..=STEPS).map(|i| {
        let angle = from + (to - from) * i as f32 / STEPS as f32;
        center + Vec2::angled(angle) * radius
    }).collect()
}

fn line(painter: &Painter, points: Vec<Pos2>, stroke: Stroke) {
    painter.add(Shape::line(points, stroke));
}

fn arrow_head(painter: &Painter, tip: Pos2, direction: Vec2, size: f32, stroke: Stroke) {
    let back = -direction.normalized() * size;
    line(painter, vec![tip + back + back.rot90() * 0.7, tip, tip + back - back.rot90() * 0.7], stroke);
}

/// Paints `icon` in the middle of `rect`, about `rect.height()` tall.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let s = rect.height() * 0.5;
    let stroke = Stroke::new(ICON_STROKE, color);
    let p = |x: f32, y: f32| c + Vec2::new(x, y) * s;
    match icon {
        Icon::Film => {
            let body = Rect::from_center_size(c, Vec2::new(s * 1.7, s * 1.5));
            painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Inside);
            for x in [-0.45, 0.45] {
                painter.line_segment([p(x, -0.72), p(x, 0.72)], stroke);
            }
            painter.line_segment([p(-0.85, 0.0), p(0.85, 0.0)], stroke);
        }
        Icon::Sticker => {
            painter.circle_stroke(p(0.0, 0.0), s * 0.8, stroke);
            painter.circle_filled(p(-0.3, -0.2), s * 0.12, stroke.color);
            painter.circle_filled(p(0.3, -0.2), s * 0.12, stroke.color);
            line(painter, vec![p(-0.4, 0.3), p(-0.15, 0.5), p(0.15, 0.5), p(0.4, 0.3)], stroke);
        }
        Icon::Text => {
            line(painter, vec![p(-0.75, -0.55), p(-0.75, -0.8), p(0.75, -0.8), p(0.75, -0.55)], stroke);
            line(painter, vec![p(0.0, -0.8), p(0.0, 0.8)], stroke);
            line(painter, vec![p(-0.3, 0.8), p(0.3, 0.8)], stroke);
        }
        Icon::Music => {
            painter.circle_stroke(p(-0.35, 0.55), s * 0.25, stroke);
            painter.circle_stroke(p(0.5, 0.4), s * 0.25, stroke);
            line(painter, vec![p(-0.1, 0.55), p(-0.1, -0.7), p(0.75, -0.85), p(0.75, 0.4)], stroke);
        }
        Icon::Transition => line(painter, vec![p(-0.85, -0.7), p(-0.85, 0.7), p(0.85, -0.7), p(0.85, 0.7), p(-0.85, -0.7)], stroke),
        Icon::Sparkle => {
            let star = |center: Pos2, size: f32| {
                let q = |x: f32, y: f32| center + Vec2::new(x, y) * size;
                vec![q(0.0, -1.0), q(0.2, -0.2), q(1.0, 0.0), q(0.2, 0.2), q(0.0, 1.0), q(-0.2, 0.2), q(-1.0, 0.0), q(-0.2, -0.2), q(0.0, -1.0)]
            };
            line(painter, star(p(-0.15, 0.1), s * 0.9), stroke);
            line(painter, star(p(0.65, -0.65), s * 0.32), stroke);
        }
        Icon::Upload => {
            line(painter, vec![p(-0.8, 0.45), p(-0.8, 0.85), p(0.8, 0.85), p(0.8, 0.45)], stroke);
            painter.line_segment([p(0.0, 0.45), p(0.0, -0.75)], stroke);
            arrow_head(painter, p(0.0, -0.8), Vec2::new(0.0, -1.0), s * 0.55, stroke);
        }
        Icon::Plus => {
            painter.line_segment([p(-0.7, 0.0), p(0.7, 0.0)], stroke);
            painter.line_segment([p(0.0, -0.7), p(0.0, 0.7)], stroke);
        }
        Icon::Play => {
            painter.add(Shape::convex_polygon(vec![p(-0.5, -0.8), p(0.8, 0.0), p(-0.5, 0.8)], color, Stroke::NONE));
        }
        Icon::Stop => {
            painter.rect_filled(Rect::from_center_size(c, Vec2::splat(s * 1.3)), CornerRadius::same(2), color);
        }
        Icon::Undo => {
            line(painter, arc(p(0.05, 0.1), s * 0.7, -2.7, 0.9), stroke);
            arrow_head(painter, p(-0.65, -0.2), Vec2::new(-0.6, 1.0), s * 0.5, stroke);
        }
        Icon::Redo => {
            line(painter, arc(p(-0.05, 0.1), s * 0.7, 2.24, 5.84), stroke);
            arrow_head(painter, p(0.65, -0.2), Vec2::new(0.6, 1.0), s * 0.5, stroke);
        }
        Icon::Scissors => {
            painter.circle_stroke(p(-0.55, -0.6), s * 0.28, stroke);
            painter.circle_stroke(p(-0.55, 0.6), s * 0.28, stroke);
            painter.line_segment([p(-0.3, -0.45), p(0.9, 0.5)], stroke);
            painter.line_segment([p(-0.3, 0.45), p(0.9, -0.5)], stroke);
        }
        Icon::TrimLeft => {
            painter.line_segment([p(-0.75, -0.85), p(-0.75, 0.85)], stroke);
            painter.line_segment([p(0.8, 0.0), p(-0.4, 0.0)], stroke);
            arrow_head(painter, p(-0.45, 0.0), Vec2::new(-1.0, 0.0), s * 0.5, stroke);
        }
        Icon::TrimRight => {
            painter.line_segment([p(0.75, -0.85), p(0.75, 0.85)], stroke);
            painter.line_segment([p(-0.8, 0.0), p(0.4, 0.0)], stroke);
            arrow_head(painter, p(0.45, 0.0), Vec2::new(1.0, 0.0), s * 0.5, stroke);
        }
        Icon::Trash => {
            painter.line_segment([p(-0.8, -0.55), p(0.8, -0.55)], stroke);
            line(painter, vec![p(-0.6, -0.55), p(-0.45, 0.85), p(0.45, 0.85), p(0.6, -0.55)], stroke);
            line(painter, vec![p(-0.28, -0.55), p(-0.28, -0.85), p(0.28, -0.85), p(0.28, -0.55)], stroke);
        }
        Icon::Fullscreen => {
            for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                line(painter, vec![p(x * 0.8, y * 0.3), p(x * 0.8, y * 0.8), p(x * 0.3, y * 0.8)], stroke);
            }
        }
        Icon::Exit => {
            for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                line(painter, vec![p(x * 0.8, y * 0.3), p(x * 0.3, y * 0.3), p(x * 0.3, y * 0.8)], stroke);
            }
        }
        Icon::ZoomIn | Icon::ZoomOut => {
            painter.circle_stroke(p(-0.15, -0.15), s * 0.6, stroke);
            painter.line_segment([p(0.3, 0.3), p(0.85, 0.85)], stroke);
            painter.line_segment([p(-0.45, -0.15), p(0.15, -0.15)], stroke);
            if icon == Icon::ZoomIn {
                painter.line_segment([p(-0.15, -0.45), p(-0.15, 0.15)], stroke);
            }
        }
        Icon::Export => {
            let body = Rect::from_center_size(p(0.0, 0.1), Vec2::new(s * 1.7, s * 1.3));
            painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Inside);
            painter.add(Shape::convex_polygon(vec![p(-0.25, -0.2), p(0.45, 0.1), p(-0.25, 0.4)], color, Stroke::NONE));
        }
        Icon::Lock | Icon::Unlock => {
            let body = Rect::from_min_max(p(-0.7, -0.1), p(0.7, 0.85));
            painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Inside);
            // Closed, the shackle comes down into the body on both sides; open, its right leg stops short.
            let (left, right) = if icon == Icon::Lock { (-0.1, -0.1) } else { (-0.1, -0.4) };
            let mut shackle = vec![p(-0.42, left)];
            shackle.extend(arc(p(0.0, -0.45), s * 0.42, std::f32::consts::PI, std::f32::consts::TAU));
            shackle.push(p(0.42, right));
            line(painter, shackle, stroke);
        }
        Icon::Eye | Icon::EyeOff => {
            let lid = |flip: f32| -> Vec<Pos2> {
                (0..=14).map(|i| {
                    let x = -0.9 + 1.8 * i as f32 / 14.0;
                    p(x, flip * 0.62 * (1.0 - (x / 0.9) * (x / 0.9)))
                }).collect()
            };
            line(painter, lid(-1.0), stroke);
            line(painter, lid(1.0), stroke);
            painter.circle_stroke(c, s * 0.26, stroke);
            if icon == Icon::EyeOff {
                painter.line_segment([p(-0.75, -0.75), p(0.75, 0.75)], stroke);
            }
        }
        Icon::Magnet => {
            // A U: the lower half of two circles, the legs going up, the tips closed off.
            line(painter, arc(p(0.0, 0.05), s * 0.65, 0.0, std::f32::consts::PI), stroke);
            line(painter, arc(p(0.0, 0.05), s * 0.25, 0.0, std::f32::consts::PI), stroke);
            for x in [-1.0, 1.0] {
                painter.line_segment([p(x * 0.65, 0.05), p(x * 0.65, -0.8)], stroke);
                painter.line_segment([p(x * 0.25, 0.05), p(x * 0.25, -0.8)], stroke);
                painter.line_segment([p(x * 0.25, -0.8), p(x * 0.65, -0.8)], stroke);
                painter.line_segment([p(x * 0.25, -0.4), p(x * 0.65, -0.4)], stroke);
            }
        }
        Icon::Warning => {
            line(painter, vec![p(0.0, -0.85), p(0.95, 0.8), p(-0.95, 0.8), p(0.0, -0.85)], stroke);
            painter.line_segment([p(0.0, -0.3), p(0.0, 0.25)], stroke);
            painter.circle_filled(p(0.0, 0.52), s * 0.07, color);
        }
    }
}

/// A filled button with an icon before its text, the pair centred. `width` of `None` fits the text.
pub fn label_button(ui: &mut egui::Ui, icon: Icon, text: &str, fill: Color32, text_color: Color32, width: Option<f32>) -> Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, text_color);
    let content = 22.0 + galley.size().x;
    let size = Vec2::new(width.unwrap_or(content + 24.0), 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered() && ui.is_enabled();
    ui.painter().rect_filled(rect, CornerRadius::same(theme::CONTROL_RADIUS), if hovered { fill.gamma_multiply(1.25) } else { fill });
    let start = rect.center().x - content / 2.0;
    paint(ui.painter(), Rect::from_min_size(Pos2::new(start, rect.center().y - 7.0), Vec2::splat(14.0)), icon, text_color);
    ui.painter().galley(Pos2::new(start + 22.0, rect.center().y - galley.size().y / 2.0), galley, text_color);
    response
}

/// A square icon button; `selected` keeps the accent background.
pub fn button(ui: &mut egui::Ui, icon: Icon, tooltip: &str, selected: bool) -> Response {
    button_of_size(ui, icon, tooltip, selected, 28.0)
}

/// [`button`] of a side of `side` points, for tight rows.
pub fn button_of_size(ui: &mut egui::Ui, icon: Icon, tooltip: &str, selected: bool, side: f32) -> Response {
    let size = Vec2::splat(side);
    let enabled = ui.is_enabled();
    let (rect, response) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let fill = if selected {
        theme::accent_soft()
    } else if response.hovered() && enabled {
        theme::panel_raised()
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(theme::CONTROL_RADIUS), fill);
    }
    let color = if !enabled {
        theme::tick()
    } else if selected {
        theme::accent_text()
    } else {
        theme::text()
    };
    paint(ui.painter(), rect.shrink(side * 0.21), icon, color);
    response.on_hover_text(tooltip)
}
