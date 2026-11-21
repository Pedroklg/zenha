//! Global keyboard and mouse side-button (`Mouse4`/`Mouse5`) hotkeys via Win32 low-level hooks.
#![expect(
    unsafe_code,
    reason = "Global hotkeys and Mouse4/Mouse5 capture require Win32 low-level hooks (WH_KEYBOARD_LL / WH_MOUSE_LL)"
)]

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_0, VK_A, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END,
    VK_F1, VK_HOME, VK_INSERT, VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_NUMPAD0, VK_OEM_1, VK_OEM_2,
    VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA, VK_OEM_MINUS, VK_OEM_PERIOD,
    VK_OEM_PLUS, VK_PAUSE, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_RWIN, VK_SHIFT, VK_SPACE, VK_TAB,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, MSG,
    MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN,
    WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1,
    XBUTTON2,
};
use windows::core::PCWSTR;

const WM_HOTKEYS_SYNC: u32 = WM_APP + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyTrigger {
    Key(u16),
    MouseX1,
    MouseX2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct HotkeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl HotkeyModifiers {
    pub const fn is_empty(self) -> bool {
        !self.ctrl && !self.alt && !self.shift && !self.meta
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HotkeySpec {
    pub modifiers: HotkeyModifiers,
    pub trigger: HotkeyTrigger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinHotkeyEvent {
    pub id: u32,
    pub pressed: bool,
}

struct SyncRequest {
    bindings: Vec<(u32, HotkeySpec)>,
    reply: SyncSender<Result<(), String>>,
}

struct HookRuntime {
    bindings: Vec<(u32, HotkeySpec)>,
    pressed_ids: HashSet<u32>,
    tx: Sender<WinHotkeyEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    kb_hook: Option<HHOOK>,
    mouse_hook: Option<HHOOK>,
}

thread_local! {
    static HOOK_STATE: RefCell<Option<HookRuntime>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid hotkey binding")]
pub struct ParseHotkeyError;

/// Parses a binding string (e.g. `"control+KeyM"`, `"F9"`, `"Mouse4"`, `"shift+Mouse5"`)
/// into a [`HotkeySpec`].
pub fn parse_binding_spec(binding: &str) -> Result<HotkeySpec, ParseHotkeyError> {
    if binding.is_empty() {
        return Err(ParseHotkeyError);
    }
    let mut modifiers = HotkeyModifiers::default();
    let mut trigger = None;

    for part in binding.split('+') {
        if part.is_empty() {
            return Err(ParseHotkeyError);
        }
        match part {
            "control" | "ctrl" => modifiers.ctrl = true,
            "alt" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            "meta" | "super" | "win" => modifiers.meta = true,
            token => {
                if trigger.is_some() {
                    return Err(ParseHotkeyError);
                }
                trigger = Some(parse_trigger(token).ok_or(ParseHotkeyError)?);
            }
        }
    }

    let trigger = trigger.ok_or(ParseHotkeyError)?;
    Ok(HotkeySpec { modifiers, trigger })
}

fn parse_trigger(token: &str) -> Option<HotkeyTrigger> {
    match token {
        "Mouse4" => return Some(HotkeyTrigger::MouseX1),
        "Mouse5" => return Some(HotkeyTrigger::MouseX2),
        _ => {}
    }

    if let Some(letter) = token.strip_prefix("Key")
        && letter.len() == 1
    {
        let byte = letter.as_bytes()[0];
        if byte.is_ascii_uppercase() {
            return Some(HotkeyTrigger::Key(VK_A.0 + u16::from(byte - b'A')));
        }
        return None;
    }

    if let Some(digit) = token.strip_prefix("Digit")
        && digit.len() == 1
    {
        let byte = digit.as_bytes()[0];
        if byte.is_ascii_digit() {
            return Some(HotkeyTrigger::Key(VK_0.0 + u16::from(byte - b'0')));
        }
        return None;
    }

    if let Some(digit) = token.strip_prefix("Numpad")
        && digit.len() == 1
    {
        let byte = digit.as_bytes()[0];
        if byte.is_ascii_digit() {
            return Some(HotkeyTrigger::Key(VK_NUMPAD0.0 + u16::from(byte - b'0')));
        }
        return None;
    }

    if let Some(num) = token.strip_prefix('F')
        && let Ok(n) = num.parse::<u16>()
        && (1..=24).contains(&n)
    {
        return Some(HotkeyTrigger::Key(VK_F1.0 + (n - 1)));
    }

    let vk = match token {
        "Space" => VK_SPACE,
        "Enter" => VK_RETURN,
        "Tab" => VK_TAB,
        "Backspace" => VK_BACK,
        "Insert" => VK_INSERT,
        "Delete" => VK_DELETE,
        "Home" => VK_HOME,
        "End" => VK_END,
        "PageUp" => VK_PRIOR,
        "PageDown" => VK_NEXT,
        "ArrowUp" => VK_UP,
        "ArrowDown" => VK_DOWN,
        "ArrowLeft" => VK_LEFT,
        "ArrowRight" => VK_RIGHT,
        "Pause" => VK_PAUSE,
        "Minus" => VK_OEM_MINUS,
        "Equal" => VK_OEM_PLUS,
        "BracketLeft" => VK_OEM_4,
        "BracketRight" => VK_OEM_6,
        "Backslash" => VK_OEM_5,
        "Semicolon" => VK_OEM_1,
        "Quote" => VK_OEM_7,
        "Backquote" => VK_OEM_3,
        "Comma" => VK_OEM_COMMA,
        "Period" => VK_OEM_PERIOD,
        "Slash" => VK_OEM_2,
        _ => return None,
    };
    Some(HotkeyTrigger::Key(vk.0))
}

/// Safe handle to a dedicated Win32 thread listening for global keyboard and mouse hotkeys.
pub struct WinHotkeys {
    thread_id: u32,
    pending: Arc<Mutex<Option<SyncRequest>>>,
    events: Receiver<WinHotkeyEvent>,
    thread: Option<JoinHandle<()>>,
}

impl WinHotkeys {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        let (event_tx, events) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<u32>(1);
        let pending: Arc<Mutex<Option<SyncRequest>>> = Arc::new(Mutex::new(None));
        let thread_pending = Arc::clone(&pending);

        let thread = thread::Builder::new()
            .name("zenha-win-hotkeys".to_owned())
            .spawn(move || {
                run_hotkey_thread(thread_pending, event_tx, wake, ready_tx);
            })
            .map_err(|err| format!("failed to spawn hotkey thread: {err}"))?;

        let thread_id = ready_rx
            .recv()
            .map_err(|_| "hotkey thread terminated before initialization".to_owned())?;

        Ok(Self {
            thread_id,
            pending,
            events,
            thread: Some(thread),
        })
    }

    pub fn set_bindings(&self, bindings: &[(u32, HotkeySpec)]) -> Result<(), String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        {
            let Ok(mut guard) = self.pending.lock() else {
                return Err("hotkey sync mutex poisoned".to_owned());
            };
            *guard = Some(SyncRequest {
                bindings: bindings.to_vec(),
                reply: reply_tx,
            });
        }

        // SAFETY: `self.thread_id` is the Win32 thread ID whose message queue was created
        // before `WinHotkeys::new` returned.
        let posted =
            unsafe { PostThreadMessageW(self.thread_id, WM_HOTKEYS_SYNC, WPARAM(0), LPARAM(0)) };
        if let Err(err) = posted {
            return Err(format!("PostThreadMessageW failed: {err}"));
        }

        reply_rx
            .recv()
            .map_err(|_| "hotkey thread exited during sync".to_owned())?
    }

    pub fn drain_events(&self) -> Vec<WinHotkeyEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }
}

impl Drop for WinHotkeys {
    fn drop(&mut self) {
        if self.thread_id != 0 {
            // SAFETY: posting `WM_QUIT` instructs the hotkey thread's `GetMessageW` loop to exit.
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn run_hotkey_thread(
    pending: Arc<Mutex<Option<SyncRequest>>>,
    tx: Sender<WinHotkeyEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    ready_tx: SyncSender<u32>,
) {
    let mut msg = MSG::default();
    // Force creation of this thread's Win32 message queue before sharing the thread ID.
    // SAFETY: `msg` is a live local; `PeekMessageW` with `PM_NOREMOVE` initializes the queue.
    unsafe {
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
    }

    HOOK_STATE.with(|slot| {
        *slot.borrow_mut() = Some(HookRuntime {
            bindings: Vec::new(),
            pressed_ids: HashSet::new(),
            tx,
            wake,
            kb_hook: None,
            mouse_hook: None,
        });
    });

    // SAFETY: `GetCurrentThreadId` has no preconditions.
    let tid = unsafe { GetCurrentThreadId() };
    let _ = ready_tx.send(tid);

    // SAFETY: standard Win32 message loop required for low-level hook callbacks.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        if msg.hwnd.0.is_null() && msg.message == WM_HOTKEYS_SYNC {
            let req = pending.lock().ok().and_then(|mut guard| guard.take());
            if let Some(req) = req {
                let res = apply_bindings(req.bindings);
                let _ = req.reply.send(res);
            }
            continue;
        }
        // SAFETY: `msg` was populated by `GetMessageW`.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    HOOK_STATE.with(|slot| {
        if let Some(mut runtime) = slot.borrow_mut().take() {
            unhook(&mut runtime.kb_hook);
            unhook(&mut runtime.mouse_hook);
        }
    });
}

fn apply_bindings(new_bindings: Vec<(u32, HotkeySpec)>) -> Result<(), String> {
    let mut wake_to_call: Option<Arc<dyn Fn() + Send + Sync>> = None;
    let res = HOOK_STATE.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(runtime) = borrow.as_mut() else {
            return Err("hotkey runtime unavailable".to_owned());
        };

        // Release any currently-held bindings that are removed or changed so PTT cannot stick.
        let mut to_release = Vec::new();
        for &id in &runtime.pressed_ids {
            let old_spec = runtime.bindings.iter().find(|(b_id, _)| *b_id == id);
            let still_matches = old_spec.is_some_and(|(_, spec)| {
                new_bindings
                    .iter()
                    .any(|(n_id, n_spec)| *n_id == id && n_spec == spec)
            });
            if !still_matches {
                to_release.push(id);
            }
        }
        let mut woke = false;
        for id in to_release {
            runtime.pressed_ids.remove(&id);
            if runtime
                .tx
                .send(WinHotkeyEvent { id, pressed: false })
                .is_ok()
            {
                woke = true;
            }
        }
        if woke {
            wake_to_call = Some(Arc::clone(&runtime.wake));
        }

        let need_kb = new_bindings
            .iter()
            .any(|(_, spec)| matches!(spec.trigger, HotkeyTrigger::Key(_)));
        let need_mouse = new_bindings.iter().any(|(_, spec)| {
            matches!(
                spec.trigger,
                HotkeyTrigger::MouseX1 | HotkeyTrigger::MouseX2
            )
        });

        // SAFETY: passing null returns the current module handle for `SetWindowsHookExW`.
        let hinst = unsafe { GetModuleHandleW(PCWSTR::null()) }
            .ok()
            .map(HINSTANCE::from);

        if need_kb {
            if runtime.kb_hook.is_none() {
                // SAFETY: installs global low-level keyboard hook serviced by this thread's loop.
                match unsafe {
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), hinst, 0)
                } {
                    Ok(hook) => runtime.kb_hook = Some(hook),
                    Err(err) => {
                        return Err(format!("SetWindowsHookExW(WH_KEYBOARD_LL) failed: {err}"));
                    }
                }
            }
        } else {
            unhook(&mut runtime.kb_hook);
        }

        if need_mouse {
            if runtime.mouse_hook.is_none() {
                // SAFETY: installs global low-level mouse hook serviced by this thread's loop.
                match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), hinst, 0) } {
                    Ok(hook) => runtime.mouse_hook = Some(hook),
                    Err(err) => {
                        return Err(format!("SetWindowsHookExW(WH_MOUSE_LL) failed: {err}"));
                    }
                }
            }
        } else {
            unhook(&mut runtime.mouse_hook);
        }

        runtime.bindings = new_bindings;
        Ok(())
    });
    if let Some(wake) = wake_to_call {
        wake();
    }
    res
}

fn unhook(hook: &mut Option<HHOOK>) {
    if let Some(handle) = hook.take() {
        // SAFETY: `handle` was returned by `SetWindowsHookExW` and is unhooked once here.
        let _ = unsafe { UnhookWindowsHookEx(handle) };
    }
}

fn current_modifiers() -> HotkeyModifiers {
    let is_down = |vk: VIRTUAL_KEY| -> bool {
        // SAFETY: `GetAsyncKeyState` accepts any virtual key code with no preconditions.
        unsafe { (GetAsyncKeyState(i32::from(vk.0)) as u16 & 0x8000) != 0 }
    };
    HotkeyModifiers {
        ctrl: is_down(VK_CONTROL),
        alt: is_down(VK_MENU),
        shift: is_down(VK_SHIFT),
        meta: is_down(VK_LWIN) || is_down(VK_RWIN),
    }
}

fn handle_trigger_event(trigger: HotkeyTrigger, pressed: bool) {
    let wake = HOOK_STATE.with(|slot| {
        let Ok(mut borrow) = slot.try_borrow_mut() else {
            return None;
        };
        let runtime = borrow.as_mut()?;

        let mut woke = false;
        if pressed {
            let mods = current_modifiers();
            let matching_ids = matching_binding_ids(&runtime.bindings, trigger, mods);
            for id in matching_ids {
                if runtime.pressed_ids.insert(id)
                    && runtime
                        .tx
                        .send(WinHotkeyEvent { id, pressed: true })
                        .is_ok()
                {
                    woke = true;
                }
            }
        } else {
            let mut released = Vec::new();
            for &(id, spec) in &runtime.bindings {
                if spec.trigger == trigger && runtime.pressed_ids.remove(&id) {
                    released.push(id);
                }
            }
            for id in released {
                if runtime
                    .tx
                    .send(WinHotkeyEvent { id, pressed: false })
                    .is_ok()
                {
                    woke = true;
                }
            }
        }
        woke.then(|| Arc::clone(&runtime.wake))
    });
    if let Some(wake) = wake {
        wake();
    }
}

fn matching_binding_ids(
    bindings: &[(u32, HotkeySpec)],
    trigger: HotkeyTrigger,
    mods: HotkeyModifiers,
) -> Vec<u32> {
    let is_mouse = matches!(trigger, HotkeyTrigger::MouseX1 | HotkeyTrigger::MouseX2);
    let has_exact_modified = is_mouse
        && !mods.is_empty()
        && bindings.iter().any(|(_, spec)| {
            spec.trigger == trigger && !spec.modifiers.is_empty() && spec.modifiers == mods
        });

    let mut out = Vec::new();
    for &(id, spec) in bindings {
        if spec.trigger != trigger {
            continue;
        }
        let matched = if spec.modifiers == mods {
            true
        } else {
            // Bare Mouse4/Mouse5 also fires while holding Shift/Ctrl/Alt in games (e.g. walk/crouch)
            // unless a more specific modified mouse binding matched.
            is_mouse && spec.modifiers.is_empty() && !has_exact_modified
        };
        if matched {
            out.push(id);
        }
    }
    out
}

unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let msg = wparam.0 as u32;
        let pressed = match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => Some(true),
            WM_KEYUP | WM_SYSKEYUP => Some(false),
            _ => None,
        };
        if let Some(pressed) = pressed {
            let ptr = lparam.0 as *const KBDLLHOOKSTRUCT;
            if !ptr.is_null() {
                // SAFETY: for `WH_KEYBOARD_LL` with `code == HC_ACTION`, Windows passes a valid
                // pointer to `KBDLLHOOKSTRUCT` in `lparam`.
                let vk = unsafe { (*ptr).vkCode } as u16;
                handle_trigger_event(HotkeyTrigger::Key(vk), pressed);
            }
        }
    }
    // SAFETY: always pass through to the next hook so Zenha never blocks user input.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let msg = wparam.0 as u32;
        if msg == WM_XBUTTONDOWN || msg == WM_XBUTTONUP {
            let ptr = lparam.0 as *const MSLLHOOKSTRUCT;
            if !ptr.is_null() {
                // SAFETY: for `WH_MOUSE_LL` with `code == HC_ACTION`, Windows passes a valid
                // pointer to `MSLLHOOKSTRUCT` in `lparam`.
                let mouse_data = unsafe { (*ptr).mouseData };
                let xbutton = ((mouse_data >> 16) & 0xFFFF) as u16;
                let trigger = match xbutton {
                    XBUTTON1 => Some(HotkeyTrigger::MouseX1),
                    XBUTTON2 => Some(HotkeyTrigger::MouseX2),
                    _ => None,
                };
                if let Some(trigger) = trigger {
                    handle_trigger_event(trigger, msg == WM_XBUTTONDOWN);
                }
            }
        }
    }
    // SAFETY: always pass through to the next hook so mouse movement/clicks are never blocked.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_letters_digits_function_keys_punctuation_numpad_and_mouse() {
        assert_eq!(
            parse_binding_spec("control+KeyM"),
            Ok(HotkeySpec {
                modifiers: HotkeyModifiers {
                    ctrl: true,
                    ..HotkeyModifiers::default()
                },
                trigger: HotkeyTrigger::Key(VK_A.0 + (b'M' - b'A') as u16),
            })
        );
        assert_eq!(
            parse_binding_spec("shift+control+alt+meta+Digit4"),
            Ok(HotkeySpec {
                modifiers: HotkeyModifiers {
                    ctrl: true,
                    alt: true,
                    shift: true,
                    meta: true,
                },
                trigger: HotkeyTrigger::Key(VK_0.0 + 4),
            })
        );
        assert_eq!(
            parse_binding_spec("F1"),
            Ok(HotkeySpec {
                modifiers: HotkeyModifiers::default(),
                trigger: HotkeyTrigger::Key(VK_F1.0),
            })
        );
        assert_eq!(
            parse_binding_spec("F24"),
            Ok(HotkeySpec {
                modifiers: HotkeyModifiers::default(),
                trigger: HotkeyTrigger::Key(VK_F1.0 + 23),
            })
        );
        assert_eq!(
            parse_binding_spec("Numpad5"),
            Ok(HotkeySpec {
                modifiers: HotkeyModifiers::default(),
                trigger: HotkeyTrigger::Key(VK_NUMPAD0.0 + 5),
            })
        );
        assert_eq!(
            parse_binding_spec("Mouse4"),
            Ok(HotkeySpec {
                modifiers: HotkeyModifiers::default(),
                trigger: HotkeyTrigger::MouseX1,
            })
        );
        assert_eq!(
            parse_binding_spec("control+Mouse5"),
            Ok(HotkeySpec {
                modifiers: HotkeyModifiers {
                    ctrl: true,
                    ..HotkeyModifiers::default()
                },
                trigger: HotkeyTrigger::MouseX2,
            })
        );

        for token in [
            "Space",
            "Enter",
            "Tab",
            "Backspace",
            "Insert",
            "Delete",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "ArrowUp",
            "ArrowDown",
            "ArrowLeft",
            "ArrowRight",
            "Pause",
            "Minus",
            "Equal",
            "BracketLeft",
            "BracketRight",
            "Backslash",
            "Semicolon",
            "Quote",
            "Backquote",
            "Comma",
            "Period",
            "Slash",
        ] {
            assert!(parse_binding_spec(token).is_ok(), "{token}");
        }
    }

    #[test]
    fn rejects_invalid_bindings() {
        for bad in [
            "",
            "control+",
            "+KeyM",
            "control",
            "F0",
            "F25",
            "Keya",
            "KeyAB",
            "Mouse3",
            "UnknownKey",
            "KeyA+KeyB",
        ] {
            assert_eq!(parse_binding_spec(bad), Err(ParseHotkeyError), "{bad}");
        }
    }

    #[test]
    fn bare_mouse_binding_fires_while_crouching_or_walking_unless_modified_exists() {
        let bare = (
            1,
            HotkeySpec {
                modifiers: HotkeyModifiers::default(),
                trigger: HotkeyTrigger::MouseX1,
            },
        );
        let ctrl_shift = HotkeyModifiers {
            ctrl: true,
            shift: true,
            ..HotkeyModifiers::default()
        };
        assert_eq!(
            matching_binding_ids(&[bare], HotkeyTrigger::MouseX1, ctrl_shift),
            vec![1]
        );

        let modified = (
            2,
            HotkeySpec {
                modifiers: ctrl_shift,
                trigger: HotkeyTrigger::MouseX1,
            },
        );
        assert_eq!(
            matching_binding_ids(&[bare, modified], HotkeyTrigger::MouseX1, ctrl_shift),
            vec![2]
        );

        // Bare keyboard key still requires exact modifier match.
        let bare_key = (
            3,
            HotkeySpec {
                modifiers: HotkeyModifiers::default(),
                trigger: HotkeyTrigger::Key(VK_A.0),
            },
        );
        assert!(
            matching_binding_ids(&[bare_key], HotkeyTrigger::Key(VK_A.0), ctrl_shift).is_empty()
        );
    }

    #[test]
    fn win_hotkeys_lifecycle_supports_keyboard_and_mouse_bindings() {
        let hotkeys = WinHotkeys::new(Arc::new(|| {})).unwrap();
        let bindings = vec![
            (1, parse_binding_spec("control+KeyM").unwrap()),
            (2, parse_binding_spec("Mouse4").unwrap()),
            (3, parse_binding_spec("shift+Mouse5").unwrap()),
        ];
        hotkeys.set_bindings(&bindings).unwrap();
        assert!(hotkeys.drain_events().is_empty());
        // Updating to empty bindings unhooks both keyboard and mouse hooks cleanly.
        hotkeys.set_bindings(&[]).unwrap();
        drop(hotkeys);
    }
}
