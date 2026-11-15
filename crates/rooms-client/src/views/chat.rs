use eframe::egui::{self, CornerRadius, Margin, RichText, Stroke};
use rooms_protocol::MAX_CHAT_MESSAGE_LEN;

use crate::chat_images::{ChatImages, ThumbState, ViewerImage, fit_within};
use crate::format::{DISPLAY_UTC_OFFSET_MINUTES, format_hhmm, format_size};
use crate::icons;
use crate::state::{ChatEntry, ChatState, ImageViewer, UserAction};
use crate::theme;
use crate::ui_model::{ChatMessageView, FileTransferView, ParticipantView, TransferStateView};

/// Exibe o visualizador modal com a imagem selecionada do chat.
pub fn viewer(
    ctx: &egui::Context,
    viewer: Option<&ImageViewer>,
    image_cache: &mut ViewerImage,
) -> Vec<UserAction> {
    let Some(viewer) = viewer else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    let mut close = false;
    let screen = ctx.content_rect().size();
    let max_width = (screen.x - 96.0).max(160.0);
    let max_height = (screen.y - 180.0).max(160.0);
    let frame = egui::Frame::window(&ctx.global_style())
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16));

    let modal = egui::Modal::new(egui::Id::unique(("chat_image_viewer", &viewer.id)))
        .backdrop_color(egui::Color32::from_black_alpha(190))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_min_width(260.0_f32.min(max_width));
            ui.set_max_width(max_width);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&viewer.name).font(theme::semibold(14.0)));
                ui.weak(format!("· {}", format_size(viewer.size)));
            });
            ui.add_space(10.0);

            match image_cache.texture(ctx, &viewer.id, &viewer.path) {
                ThumbState::Ready(texture) => {
                    let size = texture.size_vec2();
                    let (width, height) = fit_within(size.x, size.y, max_width, max_height);
                    ui.vertical_centered(|ui| {
                        ui.add(
                            egui::Image::new(&texture)
                                .fit_to_exact_size(egui::vec2(width, height))
                                .corner_radius(CornerRadius::same(6)),
                        );
                    });
                }
                ThumbState::Loading => {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.spinner();
                    });
                    ui.add_space(24.0);
                }
                ThumbState::Failed => {
                    ui.add_space(16.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Não foi possível abrir a imagem.").color(theme::ERROR),
                        );
                    });
                    ui.add_space(16.0);
                }
            }

            if let Some(error) = &viewer.error {
                ui.add_space(8.0);
                ui.label(RichText::new(error).color(theme::ERROR));
            }

            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Abrir no Windows").clicked() {
                    actions.push(UserAction::OpenImageExternally(viewer.path.clone()));
                }
                if !viewer.outgoing && ui.button("Salvar").clicked() {
                    actions.push(UserAction::SaveReceivedFile {
                        source: viewer.path.clone(),
                        name: viewer.name.clone(),
                    });
                }
                if ui.button("Fechar").clicked() {
                    close = true;
                }
            });
        });

    if close || modal.should_close() {
        actions.push(UserAction::CloseImageViewer);
    }
    actions
}

const THUMB_MAX: (f32, f32) = (240.0, 180.0);
const HEADER_HEIGHT: f32 = 64.0;

pub fn show(
    ui: &mut egui::Ui,
    chat: &mut ChatState,
    participants: &[ParticipantView],
    local_identity: &str,
    images: &mut ChatImages,
) -> Vec<UserAction> {
    let mut actions = Vec::new();
    let panel_frame = egui::Frame::new()
        .fill(theme::PANEL)
        .inner_margin(Margin::ZERO);

    egui::Panel::right("chat_panel")
        .default_size(320.0)
        .size_range(240.0..=480.0)
        .frame(panel_frame)
        .show(ui, |ui| {
            egui::Panel::top("chat_header")
                .exact_size(HEADER_HEIGHT)
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(Margin::symmetric(16, 0)),
                )
                .show(ui, |ui| {
                    let rect = ui.max_rect();
                    ui.allocate_ui_with_layout(
                        rect.size(),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(
                                RichText::new("Chat da sala")
                                    .font(theme::display(16.0))
                                    .color(theme::TEXT),
                            );
                        },
                    );
                    let full = ui.clip_rect();
                    ui.painter().hline(
                        full.x_range(),
                        rect.bottom(),
                        Stroke::new(1.0, theme::BORDER),
                    );
                });

            egui::Panel::bottom("chat_input")
                .show_separator_line(false)
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(Margin::same(12)),
                )
                .show(ui, |ui| {
                    let full = ui.clip_rect();
                    let top = ui.max_rect().top() - 12.0;
                    ui.painter()
                        .hline(full.x_range(), top, Stroke::new(1.0, theme::BORDER));
                    notice(ui, chat, &mut actions);
                    if input(ui, chat, &mut actions) {
                        actions.push(UserAction::SendChat);
                    }
                });

            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(Margin::same(16)),
                )
                .show(ui, |ui| {
                    messages(ui, chat, participants, local_identity, images, &mut actions);
                });
        });
    actions
}

fn author_color(
    sender: &str,
    outgoing: bool,
    participants: &[ParticipantView],
    local_identity: &str,
) -> egui::Color32 {
    if outgoing {
        return theme::name_color(local_identity, true);
    }
    if let Some(p) = participants
        .iter()
        .find(|p| p.name == sender || p.identity == sender)
    {
        let is_self = !local_identity.is_empty() && p.identity == local_identity;
        theme::name_color(&p.identity, is_self)
    } else {
        let is_self = !local_identity.is_empty() && sender == local_identity;
        theme::name_color(sender, is_self)
    }
}

fn notice(ui: &mut egui::Ui, chat: &ChatState, actions: &mut Vec<UserAction>) {
    let Some(text) = &chat.notice else {
        return;
    };
    ui.horizontal_wrapped(|ui| {
        ui.weak(text);
        if ui.small_button("Fechar").clicked() {
            actions.push(UserAction::DismissChatNotice);
        }
    });
    ui.add_space(4.0);
}

fn messages(
    ui: &mut egui::Ui,
    chat: &ChatState,
    participants: &[ParticipantView],
    local_identity: &str,
    images: &mut ChatImages,
    actions: &mut Vec<UserAction>,
) {
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if chat.has_more {
                if ui
                    .add_enabled(
                        !chat.history_loading,
                        egui::Button::new("Carregar anteriores"),
                    )
                    .clicked()
                {
                    actions.push(UserAction::LoadOlderChat);
                }
                ui.add_space(6.0);
            }
            if chat.messages.is_empty() {
                ui.weak("Nenhuma mensagem ainda.");
            }
            for entry in &chat.messages {
                match entry {
                    ChatEntry::Text(message) => {
                        text_entry(ui, message, local_identity, actions);
                        ui.add_space(10.0);
                    }
                    ChatEntry::File(file) => {
                        file_entry(ui, file, participants, local_identity, images, actions);
                        ui.add_space(10.0);
                    }
                }
            }
        });
}

fn text_entry(
    ui: &mut egui::Ui,
    message: &ChatMessageView,
    local_identity: &str,
    actions: &mut Vec<UserAction>,
) {
    let is_self = !local_identity.is_empty() && message.sender_id == local_identity;
    let color = theme::name_color(&message.sender_id, is_self);
    let entry = ui.push_id(&message.id, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(&message.sender)
                    .font(theme::semibold(14.0))
                    .color(color),
            );
            ui.weak(format_hhmm(
                message.sent_at_unix_ms,
                DISPLAY_UTC_OFFSET_MINUTES,
            ));
            if message.saved {
                let (icon_rect, icon_resp) =
                    ui.allocate_exact_size(egui::Vec2::splat(13.0), egui::Sense::hover());
                if ui.is_rect_visible(icon_rect) {
                    icons::bookmark(ui.painter(), icon_rect, theme::TEXT_WEAK);
                }
                icon_resp.on_hover_text("Mensagem salva");
            }
        });
        ui.label(RichText::new(&message.text).color(theme::TEXT_SECONDARY));
    });
    if message.saved && is_self {
        entry.response.container_context_menu(|ui| {
            if ui.button("Apagar mensagem").clicked() {
                actions.push(UserAction::DeleteChat(message.id.clone()));
                ui.close();
            }
        });
    }
}

fn file_entry(
    ui: &mut egui::Ui,
    file: &FileTransferView,
    participants: &[ParticipantView],
    local_identity: &str,
    images: &mut ChatImages,
    actions: &mut Vec<UserAction>,
) {
    let color = author_color(&file.sender, file.outgoing, participants, local_identity);
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(&file.sender)
                .font(theme::semibold(14.0))
                .color(color),
        );
        ui.weak(format_hhmm(
            file.sent_at_unix_ms,
            DISPLAY_UTC_OFFSET_MINUTES,
        ));
    });
    ui.add_space(4.0);

    egui::Frame::new()
        .fill(theme::CONTROL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            let label = format!("{} ({})", file.name, format_size(file.size));
            match &file.state {
                TransferStateView::InProgress { bytes } => {
                    ui.label(RichText::new(label).font(theme::semibold(13.0)));
                    let fraction = if file.size == 0 {
                        0.0
                    } else {
                        (*bytes as f32 / file.size as f32).clamp(0.0, 1.0)
                    };
                    let text = if file.outgoing {
                        "Enviando…"
                    } else {
                        "Recebendo…"
                    };
                    ui.add(egui::ProgressBar::new(fraction).text(text));
                }
                TransferStateView::Failed(message) => {
                    ui.label(RichText::new(&file.name).font(theme::semibold(13.0)));
                    ui.colored_label(theme::ERROR, format!("Falhou: {message}"));
                }
                TransferStateView::Done { path } => {
                    let thumb = if file.is_image {
                        images.texture(ui.ctx(), &file.id, path)
                    } else {
                        ThumbState::Failed
                    };
                    match thumb {
                        ThumbState::Ready(texture) => {
                            let size = texture.size_vec2();
                            let (width, height) =
                                fit_within(size.x, size.y, THUMB_MAX.0, THUMB_MAX.1);
                            let response = ui
                                .add(
                                    egui::Image::new(&texture)
                                        .fit_to_exact_size(egui::vec2(width, height))
                                        .corner_radius(CornerRadius::same(6))
                                        .sense(egui::Sense::click()),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text("Clique para ampliar");
                            if response.clicked() {
                                actions.push(UserAction::OpenImageViewer(file.id.clone()));
                            }
                        }
                        ThumbState::Loading => {
                            ui.spinner();
                        }
                        ThumbState::Failed => {
                            ui.label(RichText::new(label).font(theme::semibold(13.0)));
                        }
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.weak(format!("{} · {}", file.name, format_size(file.size)));
                        if file.outgoing {
                            ui.weak("· enviado");
                        } else if ui.small_button("Salvar").clicked() {
                            actions.push(UserAction::SaveReceivedFile {
                                source: path.clone(),
                                name: file.name.clone(),
                            });
                        }
                    });
                }
            }
        });
}

fn input(ui: &mut egui::Ui, chat: &mut ChatState, actions: &mut Vec<UserAction>) -> bool {
    let mut submitted = false;
    ui.horizontal(|ui| {
        let attach = ui
            .small_button("Anexar")
            .on_hover_text("Enviar imagem ou arquivo, até 50 MB");
        if attach.clicked() {
            actions.push(UserAction::PickFile);
        }
        submitted = ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut chat.save, "Salvar");
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut chat.draft)
                            .hint_text("Escreva uma mensagem…")
                            .char_limit(MAX_CHAT_MESSAGE_LEN)
                            .desired_width(f32::INFINITY),
                    );
                    let enter =
                        response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if enter {
                        response.request_focus();
                    }
                    enter
                })
                .inner
            })
            .inner;
    });
    if chat.show_counter() {
        ui.weak(format!("{}/{}", chat.draft_len(), MAX_CHAT_MESSAGE_LEN));
    }
    submitted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn author_color_uses_self_color_for_local_user_and_palette_for_others() {
        let participants = vec![
            ParticipantView {
                identity: "id-self".to_owned(),
                name: "Você".to_owned(),
                muted: false,
                speaking: false,
            },
            ParticipantView {
                identity: "id-ana".to_owned(),
                name: "Ana".to_owned(),
                muted: false,
                speaking: false,
            },
        ];
        assert_eq!(
            author_color("Você", false, &participants, "id-self"),
            theme::ACCENT_TEXT
        );
        assert_eq!(
            author_color("Qualquer", true, &participants, "id-self"),
            theme::ACCENT_TEXT
        );
        assert_eq!(
            author_color("Ana", false, &participants, "id-self"),
            theme::name_color("id-ana", false)
        );
    }
}
