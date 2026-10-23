//! Windows integration: desktop layer, backdrop, autostart, toast identity.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::path::Path;
use tauri::WebviewWindow;
use windows::{
    core::{w, HSTRING, PCWSTR},
    Win32::{
        Foundation::{HWND, POINT},
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON},
            Shell::{SetCurrentProcessExplicitAppUserModelID, ShellExecuteW},
            WindowsAndMessaging::{
                FindWindowW, GetAncestor, GetCursorPos, GetSystemMetrics, GetWindowLongPtrW, SetWindowLongPtrW,
                WindowFromPoint, GA_ROOT, GWLP_HWNDPARENT, SM_SWAPBUTTON, SW_SHOWNORMAL,
            },
        },
    },
};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

pub const APP_ID: &str = "DeskTasks.Widgets";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "DeskTasks";

fn hwnd(win: &WebviewWindow) -> Option<HWND> {
    match win.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
        _ => None,
    }
}

/// HWND as a plain number, to hand to another thread (HWND itself isn't Send).
pub fn hwnd_value(win: &WebviewWindow) -> Option<isize> {
    hwnd(win).map(|h| h.0 as isize)
}

pub fn hwnd_from(value: isize) -> HWND {
    HWND(value as *mut _)
}

/// Top-level window actually visible under the mouse (z-order aware) and the
/// cursor position in physical px.
pub fn window_under_cursor() -> Option<(isize, i32, i32)> {
    unsafe {
        let mut p = POINT::default();
        GetCursorPos(&mut p).ok()?;
        let w = WindowFromPoint(p);
        if w.0.is_null() {
            return None;
        }
        Some((GetAncestor(w, GA_ROOT).0 as isize, p.x, p.y))
    }
}

/// Primary mouse button still held (a drag cancelled with Esc ends while held).
pub fn primary_button_down() -> bool {
    unsafe {
        let vk = if GetSystemMetrics(SM_SWAPBUTTON) != 0 { VK_RBUTTON } else { VK_LBUTTON };
        GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0
    }
}

/// Opens an http(s) link from a task in the default browser.
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    let url = url.trim();
    let lower = url.to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("only http(s) links can be opened".into());
    }
    let res = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    // ShellExecute reports success as a value > 32.
    if res.0 as isize > 32 { Ok(()) } else { Err(format!("couldn't open link ({})", res.0 as isize)) }
}

/// Makes the desktop (Progman) the owner of the widget, so "Show desktop"
/// (Win+D) leaves it on screen like a desktop gadget. Cheap to call again:
/// it only writes when the owner changed (e.g. after Explorer restarted).
pub fn pin_to_desktop(win: &WebviewWindow) {
    let Some(h) = hwnd(win) else { return };
    unsafe {
        if let Ok(progman) = FindWindowW(w!("Progman"), None) {
            if GetWindowLongPtrW(h, GWLP_HWNDPARENT) != progman.0 as isize {
                SetWindowLongPtrW(h, GWLP_HWNDPARENT, progman.0 as isize);
            }
        }
    }
}

pub fn set_app_id() {
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID));
    }
}

/// Registers the AUMID so toasts from this unpackaged exe show our name/icon.
pub fn register_toast_app(data_dir: &Path) {
    let icon = data_dir.join("icon.png");
    let _ = std::fs::write(&icon, include_bytes!("../icons/icon.png"));
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok((key, _)) = hkcu.create_subkey(format!(r"Software\Classes\AppUserModelId\{APP_ID}")) {
        let _ = key.set_value("DisplayName", &"Задачи");
        let _ = key.set_value("IconUri", &icon.to_string_lossy().to_string());
    }
}

/// Starts the app with Windows. Only the installed copy (`DeskTasks.exe`,
/// see tools/install.ps1) registers; raw build output (`desk-tasks.exe`) never does.
pub fn register_autostart() {
    let Ok(path) = std::env::current_exe() else { return };
    let installed = path.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("DeskTasks.exe"));
    if !installed {
        return;
    }
    let exe = path.to_string_lossy().to_string();
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok((key, _)) = hkcu.create_subkey(RUN_KEY) {
        let _ = key.set_value(RUN_VALUE, &format!("\"{exe}\""));
    }
}
