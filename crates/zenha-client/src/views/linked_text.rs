//! Text with clickable links, shared by chat messages and webhook cards.

use eframe::egui::{self, Color32, RichText};

use super::links::{Segment, segments};

pub fn linked_text(ui: &mut egui::Ui, text: &str, color: Color32) {
    let parts = segments(text);
    if matches!(parts.as_slice(), [] | [Segment::Text(_)]) {
        ui.label(RichText::new(text).color(color));
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for part in parts {
            match part {
                Segment::Text(plain) => {
                    ui.label(RichText::new(plain).color(color));
                }
                Segment::Link { label, url } => {
                    ui.hyperlink_to(label, url);
                }
            }
        }
    });
}
