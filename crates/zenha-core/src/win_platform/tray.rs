//! Windows system tray icon and popup menu via `Shell_NotifyIconW`.
#![expect(
    unsafe_code,
    reason = "Windows system tray and popup menu require Win32 Shell_NotifyIconW and user32 window APIs"
)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, GetDC, HBRUSH, RGBQUAD, ReleaseDC,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, ChangeWindowMessageFilterEx, CreateIconIndirect, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetCursorPos, GetMessageW, GetWindowLongPtrW, HCURSOR, HICON, ICONINFO, MF_CHECKED, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, MSGFLT_ALLOW, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow, SetWindowLongPtrW, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, UnregisterClassW,
    WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    WM_NULL, WM_RBUTTONUP, WNDCLASS_STYLES, WNDCLASSEXW, WS_OVERLAPPED,
};
use windows::core::{BOOL, PCWSTR};

use super::to_wide_null;

const TRAY_ICON_ID: u32 = 1;
const WM_TRAY_CALLBACK: u32 = WM_APP + 1;
const WM_TRAY_UPDATE: u32 = WM_APP + 2;

const MENU_OPEN: usize = 1;
const MENU_TOGGLE_MUTE: usize = 2;
const MENU_QUIT: usize = 3;

static NEXT_CLASS_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinTrayEvent {
    Open,
    ToggleMute,
    Quit,
}

struct SharedState {
    muted: Option<bool>,
    tooltip: String,
}

struct WndState {
    normal_icon: HICON,
    muted_icon: HICON,
    muted: Option<bool>,
    tooltip: String,
    shared: Arc<Mutex<SharedState>>,
    tx: Sender<WinTrayEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    taskbar_created_msg: u32,
    icon_added: bool,
}

/// Safe handle to a Win32 system tray icon running on its own message-loop thread.
pub struct WinTray {
    hwnd_raw: usize,
    shared: Arc<Mutex<SharedState>>,
    events: Receiver<WinTrayEvent>,
    thread: Option<JoinHandle<()>>,
}

impl WinTray {
    pub fn new(
        normal_rgba: Vec<u8>,
        muted_rgba: Vec<u8>,
        icon_size: u32,
        tooltip: &str,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let expected_len = (icon_size as usize)
            .checked_mul(icon_size as usize)
            .and_then(|px| px.checked_mul(4))
            .ok_or_else(|| "invalid tray icon dimensions".to_owned())?;
        if icon_size == 0 || normal_rgba.len() != expected_len || muted_rgba.len() != expected_len {
            return Err("invalid tray icon RGBA buffer size".to_owned());
        }

        let (event_tx, events) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<usize, String>>(1);
        let shared = Arc::new(Mutex::new(SharedState {
            muted: None,
            tooltip: tooltip.to_owned(),
        }));
        let thread_shared = Arc::clone(&shared);
        let initial_tooltip = tooltip.to_owned();

        let thread = thread::Builder::new()
            .name("zenha-win-tray".to_owned())
            .spawn(move || {
                run_tray_thread(
                    normal_rgba,
                    muted_rgba,
                    icon_size,
                    initial_tooltip,
                    thread_shared,
                    event_tx,
                    wake,
                    ready_tx,
                );
            })
            .map_err(|err| format!("failed to spawn tray thread: {err}"))?;

        match ready_rx.recv() {
            Ok(Ok(hwnd_raw)) => Ok(Self {
                hwnd_raw,
                shared,
                events,
                thread: Some(thread),
            }),
            Ok(Err(err)) => {
                let _ = thread.join();
                Err(err)
            }
            Err(_) => {
                let _ = thread.join();
                Err("tray thread terminated before initialization".to_owned())
            }
        }
    }

    pub fn drain_events(&self) -> Vec<WinTrayEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }

    pub fn set_state(&self, muted: Option<bool>, tooltip: &str) {
        if let Ok(mut guard) = self.shared.lock() {
            guard.muted = muted;
            guard.tooltip.clear();
            guard.tooltip.push_str(tooltip);
        }
        let hwnd = HWND(self.hwnd_raw as *mut c_void);
        // SAFETY: `hwnd` is owned by the live tray thread; posting an app message is thread-safe.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_TRAY_UPDATE, WPARAM(0), LPARAM(0)) };
    }
}

impl Drop for WinTray {
    fn drop(&mut self) {
        if self.hwnd_raw != 0 {
            let hwnd = HWND(self.hwnd_raw as *mut c_void);
            // SAFETY: `hwnd` belongs to the tray thread; `WM_CLOSE` asks it to destroy the window
            // and exit its message loop cleanly.
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "internal thread entry point passing owned startup resources without an extra wrapper struct"
)]
fn run_tray_thread(
    normal_rgba: Vec<u8>,
    muted_rgba: Vec<u8>,
    icon_size: u32,
    initial_tooltip: String,
    shared: Arc<Mutex<SharedState>>,
    tx: Sender<WinTrayEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    ready_tx: mpsc::SyncSender<Result<usize, String>>,
) {
    let normal_icon = match create_icon_from_rgba(&normal_rgba, icon_size) {
        Ok(icon) => icon,
        Err(err) => {
            let _ = ready_tx.send(Err(err));
            return;
        }
    };
    let muted_icon = match create_icon_from_rgba(&muted_rgba, icon_size) {
        Ok(icon) => icon,
        Err(err) => {
            // SAFETY: `normal_icon` was created by `create_icon_from_rgba` and is destroyed once.
            let _ = unsafe { DestroyIcon(normal_icon) };
            let _ = ready_tx.send(Err(err));
            return;
        }
    };

    // SAFETY: passing null retrieves the handle of the current executable module.
    let hinstance = match unsafe { GetModuleHandleW(PCWSTR::null()) } {
        Ok(module) => HINSTANCE::from(module),
        Err(err) => {
            destroy_icons(normal_icon, muted_icon);
            let _ = ready_tx.send(Err(format!("GetModuleHandleW failed: {err}")));
            return;
        }
    };

    let class_name = format!(
        "ZenhaTrayWindow_{}",
        NEXT_CLASS_ID.fetch_add(1, Ordering::Relaxed)
    );
    let class_wide = to_wide_null(&class_name);

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: WNDCLASS_STYLES(0),
        lpfnWndProc: Some(tray_wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: HICON::default(),
        hCursor: HCURSOR::default(),
        hbrBackground: HBRUSH::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(class_wide.as_ptr()),
        hIconSm: HICON::default(),
    };

    // SAFETY: `wc` is fully initialized and `class_wide` remains alive past `UnregisterClassW`.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        destroy_icons(normal_icon, muted_icon);
        let _ = ready_tx.send(Err("RegisterClassExW failed".to_owned()));
        return;
    }

    // Hidden top-level window (not HWND_MESSAGE) so `SetForegroundWindow` for popup menus
    // and `TaskbarCreated` broadcasts both work reliably.
    // SAFETY: class was just registered on this instance; all optional handles are `None`.
    let hwnd = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_wide.as_ptr()),
            PCWSTR::null(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    } {
        Ok(hwnd) => hwnd,
        Err(err) => {
            // SAFETY: `class_wide` was registered above on `hinstance`.
            let _ = unsafe { UnregisterClassW(PCWSTR(class_wide.as_ptr()), Some(hinstance)) };
            destroy_icons(normal_icon, muted_icon);
            let _ = ready_tx.send(Err(format!("CreateWindowExW failed: {err}")));
            return;
        }
    };

    let taskbar_msg_wide = to_wide_null("TaskbarCreated");
    // SAFETY: `taskbar_msg_wide` is null-terminated UTF-16.
    let taskbar_created_msg = unsafe { RegisterWindowMessageW(PCWSTR(taskbar_msg_wide.as_ptr())) };
    if taskbar_created_msg != 0 {
        // Allow `TaskbarCreated` through UIPI so elevated instances still re-register after Explorer restarts.
        // SAFETY: `hwnd` is a valid window owned by this thread; optional change filter struct is `None`.
        let _ =
            unsafe { ChangeWindowMessageFilterEx(hwnd, taskbar_created_msg, MSGFLT_ALLOW, None) };
    }

    let state = Box::new(RefCell::new(WndState {
        normal_icon,
        muted_icon,
        muted: None,
        tooltip: initial_tooltip,
        shared,
        tx,
        wake,
        taskbar_created_msg,
        icon_added: false,
    }));
    let state_ptr = Box::into_raw(state);

    // SAFETY: `hwnd` was created on this thread and `state_ptr` stays valid until reclaimed
    // after the message loop exits.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
    }

    {
        // SAFETY: `state_ptr` was just created on this thread and is not borrowed elsewhere.
        let cell = unsafe { &*state_ptr };
        let (icon, tooltip) = {
            let s = cell.borrow();
            (current_icon(&s), s.tooltip.clone())
        };
        let ok = add_notify_icon(hwnd, icon, &tooltip);
        cell.borrow_mut().icon_added = ok;
        if !ok {
            // Explorer/taskbar may not be ready yet (e.g. startup before explorer.exe or restart).
            // Keep the hidden window alive so `TaskbarCreated` or a later state update registers it.
            tracing::debug!("initial Shell_NotifyIconW(NIM_ADD) failed; waiting for taskbar");
        }
    }

    let _ = ready_tx.send(Ok(hwnd.0 as usize));

    let mut msg = MSG::default();
    // SAFETY: standard Win32 message loop for windows owned by this thread.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        // SAFETY: `msg` was populated by `GetMessageW`.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // SAFETY: the message loop has exited and `hwnd` has been destroyed; reclaim `WndState`,
    // unregister the class, and destroy both HICONs.
    unsafe {
        let cell = Box::from_raw(state_ptr);
        if cell.borrow().icon_added {
            delete_notify_icon(hwnd);
        }
        drop(cell);
        let _ = UnregisterClassW(PCWSTR(class_wide.as_ptr()), Some(hinstance));
    }
    destroy_icons(normal_icon, muted_icon);
}

fn destroy_icons(normal: HICON, muted: HICON) {
    // SAFETY: both icons were created by `CreateIconIndirect` and are destroyed once on exit.
    unsafe {
        let _ = DestroyIcon(normal);
        let _ = DestroyIcon(muted);
    }
}

fn create_icon_from_rgba(rgba: &[u8], size: u32) -> Result<HICON, String> {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [RGBQUAD::default()],
    };

    let mut bits: *mut c_void = ptr::null_mut();
    // SAFETY: `GetDC(None)` retrieves the screen DC, `CreateDIBSection` allocates a top-down
    // 32bpp DIB sized `size x size`, and `ReleaseDC` releases the DC immediately.
    let color_bmp = unsafe {
        let hdc = GetDC(None);
        let bmp = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
        if !hdc.is_invalid() {
            let _ = ReleaseDC(None, hdc);
        }
        bmp
    }
    .map_err(|err| format!("CreateDIBSection failed: {err}"))?;

    if bits.is_null() {
        // SAFETY: `color_bmp` was created by `CreateDIBSection` and is not selected into a DC.
        unsafe {
            let _ = DeleteObject(color_bmp.into());
        }
        return Err("CreateDIBSection returned null pixel buffer".to_owned());
    }

    // SAFETY: `bits` points to `size * size * 4` writable bytes (`rgba.len()`) owned by `color_bmp`.
    let dst = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), rgba.len()) };
    for (src_px, dst_px) in rgba.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        dst_px[0] = src_px[2];
        dst_px[1] = src_px[1];
        dst_px[2] = src_px[0];
        dst_px[3] = src_px[3];
    }

    // SAFETY: creates a monochrome 1bpp mask bitmap for `CreateIconIndirect`.
    let mask_bmp = unsafe { CreateBitmap(size as i32, size as i32, 1, 1, None) };
    if mask_bmp.is_invalid() {
        // SAFETY: `color_bmp` was created by `CreateDIBSection` and must be freed on error.
        unsafe {
            let _ = DeleteObject(color_bmp.into());
        }
        return Err("CreateBitmap failed for tray icon mask".to_owned());
    }

    let info = ICONINFO {
        fIcon: BOOL(1),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask_bmp,
        hbmColor: color_bmp,
    };

    // SAFETY: `info` references valid GDI bitmaps; `CreateIconIndirect` copies their data so
    // both temporary bitmaps are deleted immediately afterwards.
    let icon = unsafe {
        let res = CreateIconIndirect(&info);
        let _ = DeleteObject(color_bmp.into());
        let _ = DeleteObject(mask_bmp.into());
        res
    };

    icon.map_err(|err| format!("CreateIconIndirect failed: {err}"))
}

fn fill_wide_buf<const N: usize>(buf: &mut [u16; N], text: &str) {
    buf.fill(0);
    for (slot, unit) in buf
        .iter_mut()
        .take(N.saturating_sub(1))
        .zip(text.encode_utf16())
    {
        *slot = unit;
    }
}

fn current_icon(state: &WndState) -> HICON {
    if state.muted == Some(true) {
        state.muted_icon
    } else {
        state.normal_icon
    }
}

fn add_notify_icon(hwnd: HWND, icon: HICON, tooltip: &str) -> bool {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY_CALLBACK,
        hIcon: icon,
        ..NOTIFYICONDATAW::default()
    };
    fill_wide_buf(&mut nid.szTip, tooltip);
    // SAFETY: `nid` is properly initialized with `cbSize` and valid `hwnd` and `hIcon`.
    unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool()
}

fn update_notify_icon(hwnd: HWND, icon: HICON, tooltip: &str) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_ICON | NIF_TIP,
        hIcon: icon,
        ..NOTIFYICONDATAW::default()
    };
    fill_wide_buf(&mut nid.szTip, tooltip);
    // SAFETY: `nid` is properly initialized with `cbSize` and valid `hwnd` and `hIcon`.
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

fn delete_notify_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_ID,
        ..NOTIFYICONDATAW::default()
    };
    // SAFETY: `nid` identifies the registered icon on `hwnd`.
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

fn run_context_menu(hwnd: HWND, muted: Option<bool>) -> Option<WinTrayEvent> {
    // SAFETY: creates a transient popup menu destroyed before returning.
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return None;
    };

    let open_label = to_wide_null("Abrir");
    let mute_label = to_wide_null("Microfone desligado");
    let quit_label = to_wide_null("Sair");

    let mute_flags = match muted {
        None => MF_STRING | MF_GRAYED | MF_UNCHECKED,
        Some(false) => MF_STRING | MF_UNCHECKED,
        Some(true) => MF_STRING | MF_CHECKED,
    };

    // SAFETY: `menu` is valid and all label buffers remain alive during `AppendMenuW`.
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, PCWSTR(open_label.as_ptr()));
        let _ = AppendMenuW(
            menu,
            mute_flags,
            MENU_TOGGLE_MUTE,
            PCWSTR(mute_label.as_ptr()),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, PCWSTR(quit_label.as_ptr()));
    }

    let mut pt = POINT::default();
    // SAFETY: `pt` is a valid out-pointer; `SetForegroundWindow`, `TrackPopupMenu`,
    // `PostMessageW(WM_NULL)`, and `DestroyMenu` follow the Win32 tray menu sequence.
    let cmd = unsafe {
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let selected = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        selected.0 as usize
    };

    match cmd {
        MENU_OPEN => Some(WinTrayEvent::Open),
        MENU_TOGGLE_MUTE => Some(WinTrayEvent::ToggleMute),
        MENU_QUIT => Some(WinTrayEvent::Quit),
        _ => None,
    }
}

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: `GWLP_USERDATA` holds either 0 or the `RefCell<WndState>` pointer owned by this thread.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<WndState>;
    if ptr.is_null() {
        // SAFETY: forwarding unhandled window messages to the default procedure.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }

    // SAFETY: `ptr` is non-null, properly aligned, and lives on this thread until after the
    // message loop exits. Using `&RefCell<WndState>` keeps re-entrant Win32 calls sound.
    let cell = unsafe { &*ptr };

    let taskbar_created_msg = cell.borrow().taskbar_created_msg;
    if taskbar_created_msg != 0 && msg == taskbar_created_msg {
        let (icon, tooltip) = {
            let state = cell.borrow();
            (current_icon(&state), state.tooltip.clone())
        };
        let added = add_notify_icon(hwnd, icon, &tooltip);
        cell.borrow_mut().icon_added = added;
        return LRESULT(0);
    }

    match msg {
        WM_TRAY_CALLBACK => {
            let event = (lparam.0 as u32) & 0xFFFF;
            match event {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    let (tx, wake) = {
                        let state = cell.borrow();
                        (state.tx.clone(), Arc::clone(&state.wake))
                    };
                    if tx.send(WinTrayEvent::Open).is_ok() {
                        wake();
                    }
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let (muted, tx, wake) = {
                        let state = cell.borrow();
                        (state.muted, state.tx.clone(), Arc::clone(&state.wake))
                    };
                    if let Some(selected) = run_context_menu(hwnd, muted)
                        && tx.send(selected).is_ok()
                    {
                        wake();
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TRAY_UPDATE => {
            let (icon, tooltip, icon_added) = {
                let mut state = cell.borrow_mut();
                let updated = state
                    .shared
                    .lock()
                    .ok()
                    .map(|guard| (guard.muted, guard.tooltip.clone()));
                if let Some((muted, tooltip)) = updated {
                    state.muted = muted;
                    state.tooltip = tooltip;
                }
                (
                    current_icon(&state),
                    state.tooltip.clone(),
                    state.icon_added,
                )
            };
            if icon_added {
                update_notify_icon(hwnd, icon, &tooltip);
            } else if add_notify_icon(hwnd, icon, &tooltip) {
                cell.borrow_mut().icon_added = true;
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: destroys the tray window on its owning thread, synchronously sending `WM_DESTROY`.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            let was_added = {
                let mut state = cell.borrow_mut();
                let added = state.icon_added;
                state.icon_added = false;
                added
            };
            if was_added {
                delete_notify_icon(hwnd);
            }
            // SAFETY: clear userdata so any final messages do not dereference `WndState`.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        // SAFETY: default handling for all other Win32 messages.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_wide_buf_null_terminates_and_truncates_safely() {
        let mut buf = [0xFFFFu16; 6];
        fill_wide_buf(&mut buf, "Zenha");
        assert_eq!(
            buf,
            [
                b'Z' as u16,
                b'e' as u16,
                b'n' as u16,
                b'h' as u16,
                b'a' as u16,
                0
            ]
        );

        fill_wide_buf(&mut buf, "Zenha muito longo");
        assert_eq!(buf[5], 0);
        assert_eq!(
            &buf[..5],
            &[
                b'Z' as u16,
                b'e' as u16,
                b'n' as u16,
                b'h' as u16,
                b'a' as u16
            ]
        );
    }
}
