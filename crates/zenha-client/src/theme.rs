//! Visual theme, typography, color palette and Zenha avatars.

use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin,
    RichText, Sense, Stroke, TextStyle, Vec2, Visuals,
};

const INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
const SPACE_GROTESK_BOLD: &[u8] = include_bytes!("../assets/fonts/SpaceGrotesk-Bold.ttf");
pub const APP_ICON_PNG: &[u8] = include_bytes!("../assets/zenha.png");

const SEMIBOLD_FAMILY: &str = "semibold";
const DISPLAY_FAMILY: &str = "display";

pub const BG: Color32 = Color32::from_rgb(0x0F, 0x11, 0x15);
pub const PANEL: Color32 = Color32::from_rgb(0x17, 0x1A, 0x21);
pub const PANEL_DARK: Color32 = Color32::from_rgb(0x12, 0x14, 0x1A);
pub const CONTROL: Color32 = Color32::from_rgb(0x1F, 0x23, 0x2D);
pub const CONTROL_HOVER: Color32 = Color32::from_rgb(0x28, 0x2D, 0x3A);
pub const BORDER: Color32 = Color32::from_rgb(0x2A, 0x2F, 0x3A);
pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xEA, 0xF0);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xC9, 0xCE, 0xD8);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(0x9A, 0xA3, 0xB2);
pub const ACCENT: Color32 = Color32::from_rgb(0x3D, 0xD6, 0x8C);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x06, 0x21, 0x0F);
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0x8E, 0xF0, 0xBF);
pub const ACTIVE_ROOM_BG: Color32 = Color32::from_rgb(0x14, 0x29, 0x1F);
pub const SPEAKING: Color32 = Color32::from_rgb(0x5C, 0xC8, 0xFF);
pub const STATUS_PENDING: Color32 = Color32::from_rgb(0xF5, 0xC4, 0x51);
pub const ERROR: Color32 = Color32::from_rgb(0xFF, 0x9B, 0x9B);
pub const ERROR_BG: Color32 = Color32::from_rgb(0x2A, 0x17, 0x17);
pub const ERROR_BORDER: Color32 = Color32::from_rgb(0x5A, 0x2A, 0x2A);
pub const INACTIVE_DOT: Color32 = Color32::from_rgb(0x4A, 0x51, 0x60);
pub const LIVE_BG: Color32 = Color32::from_rgb(0x3A, 0x14, 0x14);
pub const LIVE_DOT: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x6B);
pub const VIDEO_BG: Color32 = Color32::from_rgb(0x05, 0x06, 0x07);

const AVATAR_PALETTE: [(Color32, Color32); 6] = [
    (
        Color32::from_rgb(0x5B, 0x8D, 0xEF),
        Color32::from_rgb(0x06, 0x12, 0x2B),
    ),
    (
        Color32::from_rgb(0xC0, 0x84, 0xFC),
        Color32::from_rgb(0x1E, 0x0B, 0x33),
    ),
    (
        Color32::from_rgb(0xF4, 0x72, 0xB6),
        Color32::from_rgb(0x33, 0x08, 0x1D),
    ),
    (
        Color32::from_rgb(0x2D, 0xD4, 0xBF),
        Color32::from_rgb(0x04, 0x2F, 0x2A),
    ),
    (
        Color32::from_rgb(0xFB, 0x92, 0x3C),
        Color32::from_rgb(0x2E, 0x12, 0x03),
    ),
    (
        Color32::from_rgb(0xA3, 0xE6, 0x35),
        Color32::from_rgb(0x1A, 0x2E, 0x05),
    ),
];

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(Arc::from(SEMIBOLD_FAMILY)))
}

pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(Arc::from(DISPLAY_FAMILY)))
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    install_style(ctx);
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        "inter_regular".to_owned(),
        Arc::new(FontData::from_static(INTER_REGULAR)),
    );
    fonts.font_data.insert(
        "inter_semibold".to_owned(),
        Arc::new(FontData::from_static(INTER_SEMIBOLD)),
    );
    fonts.font_data.insert(
        "space_grotesk_bold".to_owned(),
        Arc::new(FontData::from_static(SPACE_GROTESK_BOLD)),
    );

    let fallback_chain: Vec<String> = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "inter_regular".to_owned());

    let mut semibold_chain = vec!["inter_semibold".to_owned(), "inter_regular".to_owned()];
    semibold_chain.extend(fallback_chain.iter().cloned());
    fonts
        .families
        .insert(FontFamily::Name(Arc::from(SEMIBOLD_FAMILY)), semibold_chain);

    let mut display_chain = vec!["space_grotesk_bold".to_owned(), "inter_regular".to_owned()];
    display_chain.extend(fallback_chain);
    fonts
        .families
        .insert(FontFamily::Name(Arc::from(DISPLAY_FAMILY)), display_chain);

    ctx.set_fonts(fonts);
}

fn install_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        let mut visuals = Visuals::dark();
        visuals.override_text_color = None;
        visuals.weak_text_color = Some(TEXT_WEAK);
        visuals.hyperlink_color = ACCENT;
        visuals.faint_bg_color = PANEL_DARK;
        visuals.extreme_bg_color = BG;
        visuals.code_bg_color = CONTROL;
        visuals.warn_fg_color = ACCENT;
        visuals.error_fg_color = ERROR;
        visuals.window_fill = PANEL;
        visuals.panel_fill = PANEL;
        visuals.window_corner_radius = CornerRadius::same(12);
        visuals.window_stroke = Stroke::new(1.0, BORDER);
        visuals.selection.bg_fill = ACCENT;
        visuals.selection.stroke = Stroke::new(1.0, ON_ACCENT);

        let cr = CornerRadius::same(8);
        let border_stroke = Stroke::new(1.0, BORDER);
        let text_stroke = Stroke::new(1.0, TEXT);

        visuals.widgets.noninteractive.bg_fill = PANEL;
        visuals.widgets.noninteractive.weak_bg_fill = PANEL;
        visuals.widgets.noninteractive.bg_stroke = border_stroke;
        visuals.widgets.noninteractive.fg_stroke = text_stroke;
        visuals.widgets.noninteractive.corner_radius = cr;

        visuals.widgets.inactive.bg_fill = CONTROL;
        visuals.widgets.inactive.weak_bg_fill = CONTROL;
        visuals.widgets.inactive.bg_stroke = border_stroke;
        visuals.widgets.inactive.fg_stroke = text_stroke;
        visuals.widgets.inactive.corner_radius = cr;

        visuals.widgets.hovered.bg_fill = CONTROL_HOVER;
        visuals.widgets.hovered.weak_bg_fill = CONTROL_HOVER;
        visuals.widgets.hovered.bg_stroke = border_stroke;
        visuals.widgets.hovered.fg_stroke = text_stroke;
        visuals.widgets.hovered.corner_radius = cr;

        visuals.widgets.active.bg_fill = CONTROL_HOVER;
        visuals.widgets.active.weak_bg_fill = CONTROL_HOVER;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        visuals.widgets.active.fg_stroke = text_stroke;
        visuals.widgets.active.corner_radius = cr;

        visuals.widgets.open.bg_fill = CONTROL_HOVER;
        visuals.widgets.open.weak_bg_fill = CONTROL_HOVER;
        visuals.widgets.open.bg_stroke = Stroke::new(1.0, ACCENT);
        visuals.widgets.open.fg_stroke = text_stroke;
        visuals.widgets.open.corner_radius = cr;

        style.visuals = visuals;

        style
            .text_styles
            .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        );
        style.text_styles.insert(TextStyle::Heading, display(20.0));
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        );

        style.spacing.interact_size.y = 32.0;
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
    });
}

pub fn central_frame() -> egui::Frame {
    egui::Frame::new().fill(BG).inner_margin(Margin::same(16))
}

pub fn accent_button(label: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(label.to_owned())
            .font(semibold(14.0))
            .color(ON_ACCENT),
    )
    .fill(ACCENT)
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(8))
}

pub fn avatar_color(identity: &str, is_self: bool) -> (Color32, Color32) {
    if is_self {
        return (ACCENT, ON_ACCENT);
    }
    let sum: usize = identity.bytes().map(usize::from).sum();
    AVATAR_PALETTE[sum % AVATAR_PALETTE.len()]
}

pub fn name_color(identity: &str, is_self: bool) -> Color32 {
    if is_self {
        ACCENT_TEXT
    } else {
        avatar_color(identity, false).0
    }
}

pub fn initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect())
}

pub fn avatar(
    ui: &mut egui::Ui,
    size: f32,
    identity: &str,
    name: &str,
    avatar_version: Option<&str>,
    is_self: bool,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    if let Some(texture) = crate::avatars::texture(ui.ctx(), identity, avatar_version) {
        let radius = CornerRadius::same((size * 0.5).round() as u8);
        egui::Image::new(&texture)
            .fit_to_exact_size(Vec2::splat(size))
            .corner_radius(radius)
            .paint_at(ui, rect);
        return;
    }
    let colors = avatar_color(identity, is_self);
    let painter = ui.painter();
    painter.circle_filled(rect.center(), size * 0.5, colors.0);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial(name),
        semibold((size * 0.42).round()),
        colors.1,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_color_returns_accent_for_self() {
        assert_eq!(avatar_color("any-id", true), (ACCENT, ON_ACCENT));
        assert_eq!(avatar_color("", true), (ACCENT, ON_ACCENT));
    }

    #[test]
    fn avatar_color_is_deterministic_and_uses_palette_for_others() {
        let c1 = avatar_color("user-ana", false);
        let c2 = avatar_color("user-ana", false);
        assert_eq!(c1, c2);
        assert!(AVATAR_PALETTE.contains(&c1));
        assert_ne!(c1, (ACCENT, ON_ACCENT));

        let c_other = avatar_color("user-bob", false);
        assert!(AVATAR_PALETTE.contains(&c_other));
        assert_ne!(c1, c_other);
    }

    #[test]
    fn name_color_matches_self_and_avatar_primary() {
        assert_eq!(name_color("user-ana", true), ACCENT_TEXT);
        assert_eq!(
            name_color("user-ana", false),
            avatar_color("user-ana", false).0
        );
    }

    #[test]
    fn initial_extracts_uppercase_first_letter_or_fallback() {
        assert_eq!(initial("ana"), "A");
        assert_eq!(initial("  lucas "), "L");
        assert_eq!(initial("éverton"), "É");
        assert_eq!(initial(""), "?");
        assert_eq!(initial("   "), "?");
    }
}
