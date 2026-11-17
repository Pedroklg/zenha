//! Ícones vetoriais desenhados via `egui::Painter`.

use std::f32::consts::PI;

use eframe::egui::{
    self, Color32, CornerRadius, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Vec2,
};

use crate::theme;

const STROKE_WIDTH: f32 = 1.8;
const ARC_SEGMENTS: usize = 12;

pub fn mic(painter: &Painter, rect: Rect, color: Color32, off: bool) {
    let stroke = Stroke::new(STROKE_WIDTH, color);
    let c = rect.center();
    let w = rect.width();
    let h = rect.height();

    let cap_w = w * 0.32;
    let cap_h = h * 0.46;
    let cap_top = rect.top() + h * 0.08;
    let cap_rect = Rect::from_min_size(
        Pos2::new(c.x - cap_w * 0.5, cap_top),
        Vec2::new(cap_w, cap_h),
    );
    let cap_radius = (cap_w * 0.5).round().clamp(1.0, 16.0) as u8;
    painter.rect_stroke(
        cap_rect,
        CornerRadius::same(cap_radius),
        stroke,
        StrokeKind::Inside,
    );

    let arc_center = Pos2::new(c.x, cap_rect.top() + cap_h * 0.62);
    let arc_r = w * 0.28;
    let arc_points: Vec<Pos2> = (0..=ARC_SEGMENTS)
        .map(|i| {
            let t = i as f32 / ARC_SEGMENTS as f32;
            let angle = t * PI;
            Pos2::new(
                arc_center.x + arc_r * angle.cos(),
                arc_center.y + arc_r * angle.sin(),
            )
        })
        .collect();
    painter.line(arc_points, stroke);

    let stem_top = arc_center.y + arc_r;
    let stem_bottom = rect.bottom() - h * 0.10;
    painter.line_segment(
        [Pos2::new(c.x, stem_top), Pos2::new(c.x, stem_bottom)],
        stroke,
    );

    if off {
        draw_slash(painter, rect, stroke);
    }
}

pub fn headphones(painter: &Painter, rect: Rect, color: Color32, off: bool) {
    let stroke = Stroke::new(STROKE_WIDTH, color);
    let c = rect.center();
    let w = rect.width();
    let h = rect.height();

    let arc_r = w * 0.36;
    let arc_center = Pos2::new(c.x, rect.top() + h * 0.48);
    let arc_points: Vec<Pos2> = (0..=ARC_SEGMENTS)
        .map(|i| {
            let t = i as f32 / ARC_SEGMENTS as f32;
            let angle = PI + t * PI;
            Pos2::new(
                arc_center.x + arc_r * angle.cos(),
                arc_center.y + arc_r * angle.sin(),
            )
        })
        .collect();
    painter.line(arc_points, stroke);

    let cup_w = w * 0.18;
    let cup_h = h * 0.34;
    let cup_top = arc_center.y - h * 0.04;
    let cup_radius = (cup_w * 0.45).round().clamp(1.0, 12.0) as u8;

    let left_cup = Rect::from_min_size(
        Pos2::new(arc_center.x - arc_r - cup_w * 0.35, cup_top),
        Vec2::new(cup_w, cup_h),
    );
    let right_cup = Rect::from_min_size(
        Pos2::new(arc_center.x + arc_r - cup_w * 0.65, cup_top),
        Vec2::new(cup_w, cup_h),
    );
    painter.rect_stroke(
        left_cup,
        CornerRadius::same(cup_radius),
        stroke,
        StrokeKind::Inside,
    );
    painter.rect_stroke(
        right_cup,
        CornerRadius::same(cup_radius),
        stroke,
        StrokeKind::Inside,
    );

    if off {
        draw_slash(painter, rect, stroke);
    }
}

fn draw_slash(painter: &Painter, rect: Rect, stroke: Stroke) {
    let inset = rect.width() * 0.12;
    painter.line_segment(
        [
            Pos2::new(rect.left() + inset, rect.top() + inset),
            Pos2::new(rect.right() - inset, rect.bottom() - inset),
        ],
        stroke,
    );
}

pub fn padlock(painter: &Painter, rect: Rect, color: Color32) {
    let stroke = Stroke::new(1.5, color);
    let c = rect.center();
    let w = rect.width();
    let h = rect.height();
    let body = Rect::from_min_max(
        Pos2::new(c.x - w * 0.32, rect.top() + h * 0.46),
        Pos2::new(c.x + w * 0.32, rect.bottom() - h * 0.08),
    );
    painter.rect_filled(body, CornerRadius::same(2), color);
    let r = w * 0.2;
    let shackle: Vec<Pos2> = (0..=ARC_SEGMENTS)
        .map(|i| {
            let angle = PI + PI * i as f32 / ARC_SEGMENTS as f32;
            Pos2::new(
                c.x + r * angle.cos(),
                body.top() - h * 0.1 + r * angle.sin(),
            )
        })
        .collect();
    painter.line_segment([Pos2::new(c.x - r, body.top()), shackle[0]], stroke);
    painter.line_segment(
        [Pos2::new(c.x + r, body.top()), shackle[ARC_SEGMENTS]],
        stroke,
    );
    painter.line(shackle, stroke);
}

pub fn bookmark(painter: &Painter, rect: Rect, color: Color32) {
    let w = rect.width();
    let h = rect.height();
    let left = rect.left() + w * 0.22;
    let right = rect.right() - w * 0.22;
    let top = rect.top() + h * 0.12;
    let bottom = rect.bottom() - h * 0.10;
    let notch = bottom - h * 0.24;
    let top_left = Pos2::new(left, top);
    let points = vec![
        top_left,
        Pos2::new(right, top),
        Pos2::new(right, bottom),
        Pos2::new(rect.center().x, notch),
        Pos2::new(left, bottom),
        top_left,
    ];
    painter.line(points, Stroke::new(1.5, color));
}

pub fn gear(painter: &Painter, rect: Rect, color: Color32, _off: bool) {
    let stroke = Stroke::new(STROKE_WIDTH, color);
    let tooth_stroke = Stroke::new(STROKE_WIDTH * 1.15, color);
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.5;

    for i in 0..8 {
        let angle = i as f32 * (PI * 0.25);
        let dir = Vec2::new(angle.cos(), angle.sin());
        painter.line_segment([c + dir * (r * 0.62), c + dir * (r * 0.96)], tooth_stroke);
    }
    painter.circle_stroke(c, r * 0.66, stroke);
    painter.circle_stroke(c, r * 0.28, stroke);
}

pub fn temporary_clock(painter: &Painter, rect: Rect, color: Color32) {
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.42;
    let stroke = Stroke::new(1.5, color);
    painter.circle_stroke(center, radius, stroke);
    painter.line_segment(
        [center, Pos2::new(center.x, center.y - radius * 0.56)],
        stroke,
    );
    painter.line_segment(
        [
            center,
            Pos2::new(center.x + radius * 0.46, center.y + radius * 0.24),
        ],
        stroke,
    );
}

pub fn paperclip(painter: &Painter, rect: Rect, color: Color32, _off: bool) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let stroke = Stroke::new(1.6, color);

    let r_outer = s * 0.285;
    let r_top = s * 0.19;
    let r_inner = s * 0.095;
    let x_outer_right = c.x + r_outer;
    let x_inner_left = c.x - r_inner;

    let top_y = rect.top() + r_top + s * 0.06;
    let bottom_y = rect.bottom() - r_outer - s * 0.06;
    let inner_bottom_y = rect.bottom() - r_outer - s * 0.12;

    let mut points = Vec::with_capacity(ARC_SEGMENTS * 3 + 5);
    points.push(Pos2::new(x_outer_right, top_y + s * 0.14));
    for i in 0..=ARC_SEGMENTS {
        let angle = (i as f32 / ARC_SEGMENTS as f32) * PI;
        points.push(Pos2::new(
            c.x + r_outer * angle.cos(),
            bottom_y + r_outer * angle.sin(),
        ));
    }
    let top_center = Pos2::new(c.x - r_inner, top_y);
    for i in 0..=ARC_SEGMENTS {
        let angle = PI + (i as f32 / ARC_SEGMENTS as f32) * PI;
        points.push(Pos2::new(
            top_center.x + r_top * angle.cos(),
            top_center.y + r_top * angle.sin(),
        ));
    }
    for i in 0..=ARC_SEGMENTS {
        let angle = (i as f32 / ARC_SEGMENTS as f32) * PI;
        points.push(Pos2::new(
            c.x + r_inner * angle.cos(),
            inner_bottom_y + r_inner * angle.sin(),
        ));
    }
    points.push(Pos2::new(x_inner_left, top_y + s * 0.16));
    painter.line(points, stroke);
}

pub fn accent_icon_button(
    ui: &mut egui::Ui,
    size: f32,
    icon: fn(&Painter, Rect, Color32),
    active: bool,
    tooltip: &str,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = ui.is_enabled() && response.hovered();
        let (bg, border, fg) = if active {
            let bg = if hovered {
                theme::CONTROL_HOVER
            } else {
                theme::ACTIVE_ROOM_BG
            };
            (bg, theme::ACCENT, theme::ACCENT_TEXT)
        } else {
            let bg = if hovered {
                theme::CONTROL_HOVER
            } else {
                theme::CONTROL
            };
            (bg, theme::BORDER, theme::TEXT_WEAK)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(6), bg);
        painter.rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        let icon_rect = rect.shrink(size * 0.24);
        icon(painter, icon_rect, fg);
    }
    response.on_hover_text(tooltip)
}

pub fn icon_button(
    ui: &mut egui::Ui,
    size: f32,
    icon: fn(&Painter, Rect, Color32, bool),
    off: bool,
    tooltip: &str,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = ui.is_enabled() && response.hovered();
        let (bg, border, fg) = if off {
            (theme::ERROR_BG, theme::ERROR_BORDER, theme::ERROR)
        } else if hovered {
            (theme::CONTROL_HOVER, theme::BORDER, theme::TEXT)
        } else {
            (theme::CONTROL, theme::BORDER, theme::TEXT)
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(8), bg);
        painter.rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        let icon_rect = rect.shrink(size * 0.27);
        icon(painter, icon_rect, fg, off);
    }
    response.on_hover_text(tooltip)
}
