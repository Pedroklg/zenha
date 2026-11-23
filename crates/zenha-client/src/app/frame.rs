//! Frame loop: `eframe::App` with the per-frame logic and the interface drawing.

use std::time::{Duration, Instant};

use eframe::egui;

use crate::format::server_host;
#[cfg(windows)]
use crate::platform::hotkeys::HotkeyAction;
#[cfg(windows)]
use crate::platform::settings::VoiceMode;
#[cfg(windows)]
use crate::platform::tray::{Tray, TrayAction};
use crate::state::{AppState, Screen, UserAction, display_name_error, room_display_title};
use crate::views;

use super::App;

const VIDEO_REPAINT_INTERVAL: Duration = Duration::from_millis(33);

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_session(ctx);
        self.sync_renderers();
        self.prune_avatars(ctx);
        self.pump_update();
        self.pump_update_download(ctx);
        self.pump_demo_update(ctx);
        self.pump_personal_hook();
        #[cfg(windows)]
        self.pump_forwarded_links(ctx);
        let now = Instant::now();
        #[cfg(windows)]
        if let Some(keys) = &mut self.global_keys {
            let ptt = self.state.audio.voice_mode == VoiceMode::PushToTalk;
            if let Some(failed) = keys.sync(&self.state.hotkeys.registrations(ptt)) {
                self.state.set_hotkey_failures(failed);
            }
            for (action, pressed) in keys.events() {
                match action {
                    HotkeyAction::ToggleMute | HotkeyAction::ToggleDeafen => {
                        if pressed {
                            self.dispatch(ctx, vec![UserAction::HotkeyPressed(action)]);
                        }
                    }
                    HotkeyAction::PushToTalk => {
                        for command in self.state.push_to_talk(pressed, now) {
                            self.session.send(command);
                        }
                    }
                }
            }
        }
        let (commands, wait) = self.state.poll_push_to_talk(now);
        for command in commands {
            self.session.send(command);
        }
        if let Some(wait) = wait {
            ctx.request_repaint_after(wait);
        }
        #[cfg(windows)]
        if let Some(actions) = self.tray.as_mut().map(Tray::actions) {
            for action in actions {
                match action {
                    TrayAction::Open => self.bring_to_front(ctx),
                    TrayAction::ToggleMute => {
                        self.dispatch(ctx, vec![UserAction::ToggleMute]);
                    }
                    TrayAction::Quit => {
                        self.quitting = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
            if let Some(tray) = &mut self.tray {
                tray.sync(self.state.tray_muted());
            }
        }
        self.sync_ui_visible(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let hidden = self.is_hidden(ui.ctx());
        self.renderer.set_paused(hidden);
        self.preview_renderer
            .set_paused(hidden || self.state.is_share_preview_collapsed());
        self.sync_renderers();
        self.pump_saves();
        self.pump_image_opens();
        self.pump_pasted_images(ui.ctx());
        if let Some(label) = self.banner {
            views::screens::demo_banner(ui, label);
        }

        let had_popup_before_views =
            ui.ctx().any_popup_open() || egui::Popup::is_any_open(ui.ctx());
        let prev_text_edit_focused = ui.ctx().data(|d| {
            d.get_temp::<bool>(prev_text_edit_focus_id())
                .unwrap_or(false)
        });
        let had_hotkey_capture = self.state.hotkeys.capturing.is_some();

        let show_password = self.state.show_password_field();
        let ptt_hint = self.state.push_to_talk_hint();
        let update_banner = self.state.visible_update_banner();
        let had_admin_dialog = self.state.admin.dialog.is_some();
        let had_update_dialog = self.state.is_update_confirming();
        let can_save_name = self.state.can_save_name();
        let name_error = display_name_error(&self.state.name_draft);
        let client_id = self.state.client_id().to_owned();
        let AppState {
            screen,
            form,
            name_draft,
            onboarding,
            lobby_voice,
            lobby_error,
            lobby_notice,
            rooms,
            audio,
            hotkeys,
            close_to_tray,
            room_sounds,
            muted_on_join,
            invite_copied,
            picker,
            personal_hook,
            admin,
            avatar_version,
            avatar_notice,
            room_password,
            ..
        } = &mut self.state;
        let can_admin = !matches!(screen, Screen::Onboarding);
        let (in_room, mic_muted) = match &*screen {
            Screen::InRoom(room) => (true, room.muted),
            _ => (false, lobby_voice.muted),
        };
        let is_admin = admin.active;
        let renderer = &mut self.renderer;
        let preview_renderer = &mut self.preview_renderer;
        let images = &mut self.images;
        let mut actions = match screen {
            Screen::Onboarding => views::onboarding::show(ui, form, onboarding, show_password),
            Screen::Lobby => views::room::show_lobby(
                ui,
                &form.display_name,
                (&client_id, avatar_version.as_deref()),
                *lobby_voice,
                rooms,
                server_host(&form.server_url),
                lobby_error.as_deref(),
                lobby_notice.as_deref(),
                update_banner.as_ref(),
                is_admin,
            ),
            Screen::Connecting { room } => {
                views::screens::connecting(ui, room_display_title(rooms, room))
            }
            Screen::InRoom(room) => views::room::show(
                ui,
                room,
                rooms,
                server_host(&form.server_url),
                renderer,
                preview_renderer,
                images,
                audio.stream_volume,
                &audio.participant_volumes,
                ptt_hint.as_deref(),
                update_banner.as_ref(),
                is_admin,
            ),
        };
        actions.extend(views::settings::show(
            ui.ctx(),
            views::settings::SettingsView {
                server_url: &form.server_url,
                name_draft,
                name_error,
                can_save_name,
                in_room,
                mic_muted,
                audio,
                hotkeys,
                close_to_tray: *close_to_tray,
                room_sounds: *room_sounds,
                muted_on_join: *muted_on_join,
                invite_copied: *invite_copied,
                can_admin,
                personal_hook,
                admin,
                rooms,
                avatar_identity: &client_id,
                avatar_version: avatar_version.as_deref(),
                avatar_notice: avatar_notice.as_deref(),
            },
        ));
        actions.extend(views::room::admin_dialog(
            ui.ctx(),
            &mut admin.dialog,
            rooms,
        ));
        actions.extend(views::room::room_password_dialog(ui.ctx(), room_password));
        actions.extend(views::room::update_confirm_dialog(
            ui.ctx(),
            had_update_dialog,
        ));
        actions.extend(views::share::picker(
            ui.ctx(),
            picker,
            &mut self.picker_thumbnails,
        ));
        let open_viewer = match screen {
            Screen::InRoom(room) => room.chat.viewer.as_ref(),
            _ => None,
        };
        actions.extend(views::chat::viewer(
            ui.ctx(),
            open_viewer,
            &mut self.viewer_image,
        ));
        let already_toggled_fullscreen = actions
            .iter()
            .any(|action| matches!(action, UserAction::ToggleFullscreen));
        let blocking_overlay_open = had_popup_before_views
            || had_admin_dialog
            || had_update_dialog
            || self.state.admin.dialog.is_some()
            || self.state.is_update_confirming()
            || ui.ctx().any_popup_open()
            || egui::Popup::is_any_open(ui.ctx())
            || had_hotkey_capture;
        let text_input_focused = ui.ctx().text_edit_focused() || prev_text_edit_focused;
        let paste_action_triggered = actions.iter().any(|action| {
            matches!(
                action,
                UserAction::SendPastedImage | UserAction::CancelPastedImage | UserAction::SendChat
            )
        });
        if self.state.has_pending_paste()
            && self.state.can_paste_chat_image()
            && !blocking_overlay_open
            && !paste_action_triggered
        {
            if ui
                .ctx()
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                actions.push(UserAction::CancelPastedImage);
            } else if self.state.chat_draft_is_empty()
                && ui.ctx().memory(|m| m.focused()).is_none()
                && ui
                    .ctx()
                    .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
            {
                actions.push(UserAction::SendPastedImage);
            }
        }
        let fullscreen = self.state.is_fullscreen();
        if fullscreen
            && !already_toggled_fullscreen
            && !blocking_overlay_open
            && !text_input_focused
            && ui
                .ctx()
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            actions.push(UserAction::ToggleFullscreen);
        }
        let text_edit_focused = ui.ctx().text_edit_focused();
        ui.ctx()
            .data_mut(|d| d.insert_temp(prev_text_edit_focus_id(), text_edit_focused));
        let actions = self.resolve_effects(ui.ctx(), actions);
        self.dispatch(ui.ctx(), actions);
        for url in take_steam_urls(ui.ctx()) {
            crate::dialogs::open_with_shell(&url);
        }
        self.prune_images();
        self.prune_avatars(ui.ctx());
        self.sync_renderers();

        if ui.ctx().input(|i| i.viewport().close_requested())
            && self.state.close_to_tray
            && self.has_tray()
            && !self.quitting
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.hidden_to_tray = true;
            self.renderer.set_paused(true);
            self.preview_renderer.set_paused(true);
            self.sync_ui_visible(ui.ctx());
        }

        let fullscreen = self.state.is_fullscreen();
        if fullscreen != self.fullscreen {
            self.fullscreen = fullscreen;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
        }
        if !self.is_hidden(ui.ctx()) {
            let wait = if self.state.is_watching()
                || (self.state.is_sharing_local() && !self.state.is_share_preview_collapsed())
            {
                Some(VIDEO_REPAINT_INTERVAL)
            } else {
                self.state.next_refresh_wait(Instant::now())
            };
            if let Some(wait) = wait {
                ui.ctx().request_repaint_after(wait);
            }
        }
    }
}

/// egui hands every URL to the default web browser, which does not know `steam://`.
fn take_steam_urls(ctx: &egui::Context) -> Vec<String> {
    ctx.output_mut(|output| {
        let mut taken = Vec::new();
        output.commands.retain(|command| match command {
            egui::OutputCommand::OpenUrl(open) if open.url.starts_with("steam://") => {
                taken.push(open.url.clone());
                false
            }
            _ => true,
        });
        taken
    })
}

fn prev_text_edit_focus_id() -> egui::Id {
    egui::Id::unique("prev_text_edit_focused")
}

#[cfg(test)]
mod steam_link_tests {
    use eframe::egui;

    #[test]
    fn steam_urls_are_taken_and_web_urls_stay() {
        let ctx = egui::Context::default();
        ctx.open_url(egui::OpenUrl::same_tab("steam://connect/1.2.3.4:27015/xyz"));
        ctx.open_url(egui::OpenUrl::new_tab("https://gamersclub.com.br/j/1/a"));
        assert_eq!(
            super::take_steam_urls(&ctx),
            vec!["steam://connect/1.2.3.4:27015/xyz".to_owned()]
        );
        let left = ctx.output(|o| o.commands.clone());
        assert!(
            matches!(left.as_slice(), [egui::OutputCommand::OpenUrl(open)] if open.url.starts_with("https://"))
        );
    }
}
