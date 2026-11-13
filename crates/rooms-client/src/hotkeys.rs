//! Conversão entre eventos de teclado do `egui` e o formato de atalhos do `global-hotkey`.

use eframe::egui::{self, Key};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    ToggleMute,
    ToggleDeafen,
}

impl HotkeyAction {
    pub const ALL: [HotkeyAction; 2] = [Self::ToggleMute, Self::ToggleDeafen];

    pub fn label(self) -> &'static str {
        match self {
            Self::ToggleMute => "Mutar microfone",
            Self::ToggleDeafen => "Ensurdecer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingError {
    NeedsModifier,
    Unsupported,
}

impl BindingError {
    pub fn message(self) -> &'static str {
        match self {
            Self::NeedsModifier => "Use Ctrl, Alt ou Shift junto com essa tecla.",
            Self::Unsupported => "Essa tecla não pode ser usada como atalho.",
        }
    }
}

pub fn binding_from_key(
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> Result<String, BindingError> {
    let token = key_token(key).ok_or(BindingError::Unsupported)?;
    let has_modifier = modifiers.shift || modifiers.ctrl || modifiers.alt;
    if !has_modifier && !allows_bare(key) {
        return Err(BindingError::NeedsModifier);
    }
    let mut out = String::new();
    if modifiers.shift {
        out.push_str("shift+");
    }
    if modifiers.ctrl {
        out.push_str("control+");
    }
    if modifiers.alt {
        out.push_str("alt+");
    }
    out.push_str(token);
    Ok(out)
}

pub fn binding_label(binding: &str) -> String {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut meta = false;
    let mut key = None;
    for part in binding.split('+').filter(|part| !part.is_empty()) {
        match part {
            "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "meta" => meta = true,
            other => key = Some(other),
        }
    }
    let mut parts = Vec::with_capacity(5);
    if ctrl {
        parts.push("Ctrl");
    }
    if alt {
        parts.push("Alt");
    }
    if shift {
        parts.push("Shift");
    }
    if meta {
        parts.push("Win");
    }
    if let Some(key) = key {
        parts.push(token_label(key));
    }
    parts.join("+")
}

fn token_label(token: &str) -> &str {
    if let Some(letter) = token.strip_prefix("Key")
        && letter.len() == 1
    {
        return letter;
    }
    if let Some(digit) = token.strip_prefix("Digit")
        && digit.len() == 1
    {
        return digit;
    }
    match token {
        "ArrowUp" => "Cima",
        "ArrowDown" => "Baixo",
        "ArrowLeft" => "Esquerda",
        "ArrowRight" => "Direita",
        "Space" => "Espaço",
        other => other,
    }
}

fn allows_bare(key: Key) -> bool {
    matches!(
        key,
        Key::F1
            | Key::F2
            | Key::F3
            | Key::F4
            | Key::F5
            | Key::F6
            | Key::F7
            | Key::F8
            | Key::F9
            | Key::F10
            | Key::F11
            | Key::F12
            | Key::F13
            | Key::F14
            | Key::F15
            | Key::F16
            | Key::F17
            | Key::F18
            | Key::F19
            | Key::F20
            | Key::F21
            | Key::F22
            | Key::F23
            | Key::F24
            | Key::Insert
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Pause
    )
}

fn key_token(key: Key) -> Option<&'static str> {
    Some(match key {
        Key::A => "KeyA",
        Key::B => "KeyB",
        Key::C => "KeyC",
        Key::D => "KeyD",
        Key::E => "KeyE",
        Key::F => "KeyF",
        Key::G => "KeyG",
        Key::H => "KeyH",
        Key::I => "KeyI",
        Key::J => "KeyJ",
        Key::K => "KeyK",
        Key::L => "KeyL",
        Key::M => "KeyM",
        Key::N => "KeyN",
        Key::O => "KeyO",
        Key::P => "KeyP",
        Key::Q => "KeyQ",
        Key::R => "KeyR",
        Key::S => "KeyS",
        Key::T => "KeyT",
        Key::U => "KeyU",
        Key::V => "KeyV",
        Key::W => "KeyW",
        Key::X => "KeyX",
        Key::Y => "KeyY",
        Key::Z => "KeyZ",
        Key::Num0 => "Digit0",
        Key::Num1 => "Digit1",
        Key::Num2 => "Digit2",
        Key::Num3 => "Digit3",
        Key::Num4 => "Digit4",
        Key::Num5 => "Digit5",
        Key::Num6 => "Digit6",
        Key::Num7 => "Digit7",
        Key::Num8 => "Digit8",
        Key::Num9 => "Digit9",
        Key::F1 => "F1",
        Key::F2 => "F2",
        Key::F3 => "F3",
        Key::F4 => "F4",
        Key::F5 => "F5",
        Key::F6 => "F6",
        Key::F7 => "F7",
        Key::F8 => "F8",
        Key::F9 => "F9",
        Key::F10 => "F10",
        Key::F11 => "F11",
        Key::F12 => "F12",
        Key::F13 => "F13",
        Key::F14 => "F14",
        Key::F15 => "F15",
        Key::F16 => "F16",
        Key::F17 => "F17",
        Key::F18 => "F18",
        Key::F19 => "F19",
        Key::F20 => "F20",
        Key::F21 => "F21",
        Key::F22 => "F22",
        Key::F23 => "F23",
        Key::F24 => "F24",
        Key::Space => "Space",
        Key::Enter => "Enter",
        Key::Tab => "Tab",
        Key::Backspace => "Backspace",
        Key::Insert => "Insert",
        Key::Delete => "Delete",
        Key::Home => "Home",
        Key::End => "End",
        Key::PageUp => "PageUp",
        Key::PageDown => "PageDown",
        Key::ArrowUp => "ArrowUp",
        Key::ArrowDown => "ArrowDown",
        Key::ArrowLeft => "ArrowLeft",
        Key::ArrowRight => "ArrowRight",
        Key::Minus => "Minus",
        Key::Equals => "Equal",
        Key::Comma => "Comma",
        Key::Period => "Period",
        Key::Semicolon => "Semicolon",
        Key::Quote => "Quote",
        Key::Backslash => "Backslash",
        Key::Slash => "Slash",
        Key::OpenBracket => "BracketLeft",
        Key::CloseBracket => "BracketRight",
        Key::Backtick => "Backquote",
        Key::Pause => "Pause",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Modifiers;

    const ALL_SUPPORTED_KEYS: &[Key] = &[
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
        Key::F13,
        Key::F14,
        Key::F15,
        Key::F16,
        Key::F17,
        Key::F18,
        Key::F19,
        Key::F20,
        Key::F21,
        Key::F22,
        Key::F23,
        Key::F24,
        Key::Space,
        Key::Enter,
        Key::Tab,
        Key::Backspace,
        Key::Insert,
        Key::Delete,
        Key::Home,
        Key::End,
        Key::PageUp,
        Key::PageDown,
        Key::ArrowUp,
        Key::ArrowDown,
        Key::ArrowLeft,
        Key::ArrowRight,
        Key::Minus,
        Key::Equals,
        Key::Comma,
        Key::Period,
        Key::Semicolon,
        Key::Quote,
        Key::Backslash,
        Key::Slash,
        Key::OpenBracket,
        Key::CloseBracket,
        Key::Backtick,
        Key::Pause,
    ];

    fn mods(ctrl: bool, alt: bool, shift: bool) -> Modifiers {
        Modifiers {
            ctrl,
            alt,
            shift,
            mac_cmd: false,
            command: false,
        }
    }

    #[test]
    fn maps_letters_digits_and_function_keys() {
        assert_eq!(
            binding_from_key(Key::M, mods(true, false, false)),
            Ok("control+KeyM".to_owned())
        );
        assert_eq!(
            binding_from_key(Key::Num4, mods(false, true, false)),
            Ok("alt+Digit4".to_owned())
        );
        assert_eq!(
            binding_from_key(Key::F9, Modifiers::NONE),
            Ok("F9".to_owned())
        );
        assert_eq!(
            binding_from_key(Key::OpenBracket, mods(true, false, false)),
            Ok("control+BracketLeft".to_owned())
        );
        assert_eq!(
            binding_from_key(Key::CloseBracket, mods(true, false, false)),
            Ok("control+BracketRight".to_owned())
        );
        assert_eq!(
            binding_from_key(Key::Equals, mods(true, false, false)),
            Ok("control+Equal".to_owned())
        );
        assert_eq!(
            binding_from_key(Key::Backtick, mods(true, false, false)),
            Ok("control+Backquote".to_owned())
        );
    }

    #[test]
    fn orders_modifiers_as_shift_control_alt_and_ignores_command() {
        let m = Modifiers {
            ctrl: true,
            alt: true,
            shift: true,
            mac_cmd: true,
            command: true,
        };
        assert_eq!(
            binding_from_key(Key::M, m),
            Ok("shift+control+alt+KeyM".to_owned())
        );
    }

    #[test]
    fn enforces_modifier_rule_and_rejects_unsupported_keys() {
        for key in [
            Key::F1,
            Key::F12,
            Key::F24,
            Key::Insert,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::Pause,
        ] {
            assert!(binding_from_key(key, Modifiers::NONE).is_ok(), "{key:?}");
        }
        for key in [
            Key::A,
            Key::Num1,
            Key::Space,
            Key::Enter,
            Key::Tab,
            Key::Backspace,
            Key::Delete,
            Key::ArrowUp,
            Key::Minus,
        ] {
            assert_eq!(
                binding_from_key(key, Modifiers::NONE),
                Err(BindingError::NeedsModifier),
                "{key:?}"
            );
        }
        let command_only = Modifiers {
            command: true,
            mac_cmd: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            binding_from_key(Key::M, command_only),
            Err(BindingError::NeedsModifier)
        );
        for key in [Key::Escape, Key::F25, Key::F35] {
            assert_eq!(
                binding_from_key(key, mods(true, false, false)),
                Err(BindingError::Unsupported),
                "{key:?}"
            );
            assert_eq!(
                binding_from_key(key, Modifiers::NONE),
                Err(BindingError::Unsupported),
                "{key:?}"
            );
        }
        assert_eq!(
            BindingError::NeedsModifier.message(),
            "Use Ctrl, Alt ou Shift junto com essa tecla."
        );
        assert_eq!(
            BindingError::Unsupported.message(),
            "Essa tecla não pode ser usada como atalho."
        );
    }

    #[test]
    fn formats_human_readable_labels() {
        assert_eq!(binding_label("shift+control+KeyM"), "Ctrl+Shift+M");
        assert_eq!(
            binding_label("shift+control+alt+meta+Digit1"),
            "Ctrl+Alt+Shift+Win+1"
        );
        assert_eq!(binding_label("F5"), "F5");
        assert_eq!(binding_label("control+ArrowUp"), "Ctrl+Cima");
        assert_eq!(binding_label("alt+ArrowDown"), "Alt+Baixo");
        assert_eq!(binding_label("shift+ArrowLeft"), "Shift+Esquerda");
        assert_eq!(binding_label("control+ArrowRight"), "Ctrl+Direita");
        assert_eq!(binding_label("control+Space"), "Ctrl+Espaço");
        assert_eq!(binding_label("alt+BracketLeft"), "Alt+BracketLeft");
        assert_eq!(HotkeyAction::ToggleMute.label(), "Mutar microfone");
        assert_eq!(HotkeyAction::ToggleDeafen.label(), "Ensurdecer");
        assert_eq!(
            HotkeyAction::ALL,
            [HotkeyAction::ToggleMute, HotkeyAction::ToggleDeafen]
        );
    }

    #[cfg(windows)]
    #[test]
    fn all_tokens_round_trip_through_global_hotkey() {
        use global_hotkey::hotkey::HotKey;

        for &key in ALL_SUPPORTED_KEYS {
            let m = if allows_bare(key) {
                Modifiers::NONE
            } else {
                mods(true, true, true)
            };
            let binding = binding_from_key(key, m).unwrap();
            let parsed: HotKey = binding.parse().unwrap_or_else(|err| {
                panic!("failed to parse {binding:?} for {key:?}: {err}");
            });
            assert_eq!(parsed.into_string(), binding, "mismatch for {key:?}");
        }
    }
}
