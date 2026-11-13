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
