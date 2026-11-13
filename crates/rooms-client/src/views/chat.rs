use eframe::egui;
use rooms_protocol::MAX_CHAT_MESSAGE_LEN;

use crate::chat_images::{ChatImages, ThumbState, fit_within};
use crate::format::{DISPLAY_UTC_OFFSET_MINUTES, format_hhmm, format_size};
use crate::state::{ChatEntry, ChatState, UserAction};
use crate::ui_model::{FileTransferView, TransferStateView};

const THUMB_MAX: (f32, f32) = (240.0, 180.0);
const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(220, 80, 80);

pub fn show(
    ui: &mut egui::Ui,
    room: &str,
    chat: &mut ChatState,
    images: &mut ChatImages,
) -> Vec<UserAction> {
    let mut actions = Vec::new();
    egui::Panel::right("chat_panel")
        .default_size(300.0)
        .size_range(220.0..=480.0)
        .show(ui, |ui| {
            ui.heading(format!("Chat · {room}"));
            ui.separator();

            egui::Panel::bottom("chat_input")
                .show_separator_line(false)
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    notice(ui, chat, &mut actions);
                    if input(ui, chat, &mut actions) {
                        actions.push(UserAction::SendChat);
                    }
                });

            egui::CentralPanel::default().show(ui, |ui| messages(ui, chat, images, &mut actions));
        });
    actions
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
}

fn messages(
    ui: &mut egui::Ui,
    chat: &ChatState,
    images: &mut ChatImages,
    actions: &mut Vec<UserAction>,
) {
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if chat.messages.is_empty() {
                ui.weak("Nenhuma mensagem ainda.");
            }
            for entry in &chat.messages {
                match entry {
                    ChatEntry::Text(message) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.weak(format_hhmm(
                                message.sent_at_unix_ms,
                                DISPLAY_UTC_OFFSET_MINUTES,
                            ));
                            ui.strong(&message.sender);
                            ui.label(&message.text);
                        });
                    }
                    ChatEntry::File(file) => file_entry(ui, file, images, actions),
                }
            }
        });
}

fn file_entry(
    ui: &mut egui::Ui,
    file: &FileTransferView,
    images: &mut ChatImages,
    actions: &mut Vec<UserAction>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.weak(format_hhmm(
            file.sent_at_unix_ms,
            DISPLAY_UTC_OFFSET_MINUTES,
        ));
        ui.strong(&file.sender);
    });
    let label = format!("{} ({})", file.name, format_size(file.size));
    match &file.state {
        TransferStateView::InProgress { bytes } => {
            ui.label(label);
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
            ui.label(&file.name);
            ui.colored_label(ERROR_COLOR, format!("Falhou: {message}"));
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
                    let (width, height) = fit_within(size.x, size.y, THUMB_MAX.0, THUMB_MAX.1);
                    ui.add(egui::Image::new(&texture).fit_to_exact_size(egui::vec2(width, height)));
                }
                ThumbState::Loading => {
                    ui.spinner();
                }
                ThumbState::Failed => {
                    ui.label(label);
                }
            }
            if file.outgoing {
                ui.weak("enviado");
            } else if ui.small_button("Salvar").clicked() {
                actions.push(UserAction::SaveReceivedFile {
                    source: path.clone(),
                    name: file.name.clone(),
                });
            }
        }
    }
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
        let response = ui.add(
            egui::TextEdit::singleline(&mut chat.draft)
                .hint_text("Escreva uma mensagem")
                .char_limit(MAX_CHAT_MESSAGE_LEN)
                .desired_width(f32::INFINITY),
        );
        submitted = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if submitted {
            response.request_focus();
        }
    });
    if chat.show_counter() {
        ui.weak(format!("{}/{}", chat.draft_len(), MAX_CHAT_MESSAGE_LEN));
    }
    submitted
}
