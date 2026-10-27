//! Windows integration: desktop layer, backdrop, autostart, toast identity.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use tauri::WebviewWindow;
use windows::{
    core::{w, HSTRING, PCWSTR},
    Win32::{
        Foundation::{ERROR_TIMEOUT, HWND, POINT},
        System::Threading::{AttachThreadInput, GetCurrentThreadId},
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
        },
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON},
            Shell::{
                FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SetCurrentProcessExplicitAppUserModelID,
                ShellExecuteW, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
            },
            WindowsAndMessaging::{
                BringWindowToTop, FindWindowW, GetAncestor, GetCursorPos, GetForegroundWindow, GetSystemMetrics,
                GetWindowLongPtrW, GetWindowThreadProcessId, IsHungAppWindow, SetForegroundWindow, SetWindowLongPtrW,
                WindowFromPoint,
                GA_ROOT, GWLP_HWNDPARENT, SM_SWAPBUTTON, SW_SHOWNORMAL,
            },
        },
    },
};
use windows_future::{AsyncStatus, IAsyncOperation, IAsyncOperationWithProgress};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

pub const APP_ID: &str = "SimpleDeskWidgets.App";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "SimpleDeskWidgets";
// Under the old name (Desk Tasks): cleaned up.
const OLD_APP_ID: &str = "DeskTasks.Widgets";
const OLD_RUN_VALUE: &str = "DeskTasks";

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
        return Err("Открываются только ссылки на сайты (http и https)".into());
    }
    let res = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    // ShellExecute reports success as a value > 32.
    if res.0 as isize > 32 { Ok(()) } else { Err("Ссылка не открылась — проверьте, что в Windows выбран браузер по умолчанию".into()) }
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
        let _ = key.set_value("DisplayName", &"Simple Desk Widgets");
        let _ = key.set_value("IconUri", &icon.to_string_lossy().to_string());
    }
    let _ = hkcu.delete_subkey_all(format!(r"Software\Classes\AppUserModelId\{OLD_APP_ID}"));
}

/// Windows is in light mode for apps (Settings → Personalisation → Colours).
pub fn system_light() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("AppsUseLightTheme"))
        .is_ok_and(|v| v != 0)
}

/// For the pages: whether Windows is in light mode now (they follow it on «system»).
#[tauri::command]
pub fn system_theme_light() -> bool {
    system_light()
}

/// Starts the app with Windows, or stops doing so. Only the installed copy
/// (`SimpleDeskWidgets.exe`, see tools/install.ps1) touches it; raw build
/// output (`simple-desk-widgets.exe`) never does.
pub fn set_autostart(on: bool) {
    let Some(path) = crate::updater::installed_exe() else { return };
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok((key, _)) = hkcu.create_subkey(RUN_KEY) else { return };
    let _ = key.delete_value(OLD_RUN_VALUE);
    if on {
        let _ = key.set_value(RUN_VALUE, &format!("\"{}\"", path.to_string_lossy()));
    } else {
        let _ = key.delete_value(RUN_VALUE);
    }
}

/// Asks for a folder with the standard Windows dialog; `None` if cancelled.
/// Runs on its own STA thread (the shell dialog needs one).
pub fn pick_folder(owner: Option<isize>, start: Option<String>) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // SAFETY: COM on a fresh thread; every interface is released before it ends.
        let picked = unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let picked = (|| -> windows::core::Result<String> {
                let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
                dialog.SetOptions(dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)?;
                if let Some(start) = start.filter(|s| Path::new(s).is_dir()) {
                    if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start), None) {
                        let _ = dialog.SetFolder(&item);
                    }
                }
                dialog.Show(owner.map(hwnd_from))?;
                let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
                let path = name.to_string();
                CoTaskMemFree(Some(name.0 as *const _));
                Ok(path?)
            })();
            CoUninitialize();
            picked.ok()
        };
        let _ = tx.send(picked);
    });
    rx.recv().ok().flatten()
}

/// The window is the one in front (has the keyboard).
pub fn is_foreground(win: &WebviewWindow) -> bool {
    hwnd(win).is_some_and(|h| unsafe { GetForegroundWindow() } == h)
}

/// Puts the window in front with the keyboard. Windows may refuse a plain
/// SetForegroundWindow (the focus-stealing lock, e.g. when the hotkey's moment
/// has passed); joining the front window's input queue for the call lifts it
/// without sending any keys. True if it's in front afterwards.
pub fn bring_to_front(win: &WebviewWindow) -> bool {
    let Some(h) = hwnd(win) else { return false };
    // SAFETY: plain calls; the input queues are detached again right after.
    unsafe {
        if GetForegroundWindow() == h || SetForegroundWindow(h).as_bool() && GetForegroundWindow() == h {
            return true;
        }
        let front = GetForegroundWindow();
        // Not with a frozen app: the switch would wait on it, and so would we.
        if !front.0.is_null() && IsHungAppWindow(front).as_bool() {
            return false;
        }
        let theirs = GetWindowThreadProcessId(front, None);
        let ours = GetCurrentThreadId();
        let joined = theirs != 0 && theirs != ours && AttachThreadInput(ours, theirs, true).as_bool();
        let _ = BringWindowToTop(h);
        let _ = SetForegroundWindow(h);
        if joined {
            let _ = AttachThreadInput(ours, theirs, false);
        }
        GetForegroundWindow() == h
    }
}

/// `finish` for an operation without progress reports (media sessions, streams).
pub fn finish_op<T>(op: IAsyncOperation<T>, timeout: Duration) -> windows::core::Result<T>
where
    T: windows::core::RuntimeType + 'static,
{
    let until = Instant::now() + timeout;
    while op.Status()? == AsyncStatus::Started {
        if Instant::now() >= until {
            let _ = op.Cancel();
            return Err(windows::core::Error::new(ERROR_TIMEOUT.to_hresult(), "не ответил вовремя"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    op.GetResults()
}

/// Waits for a WinRT operation (an HTTP request, a read), cancelling it after
/// `timeout` so a stalled server can't hang the caller's thread.
pub fn finish<T, P>(op: IAsyncOperationWithProgress<T, P>, timeout: Duration) -> windows::core::Result<T>
where
    T: windows::core::RuntimeType + 'static,
    P: windows::core::RuntimeType + 'static,
{
    let until = Instant::now() + timeout;
    while op.Status()? == AsyncStatus::Started {
        if Instant::now() >= until {
            let _ = op.Cancel();
            return Err(windows::core::Error::new(ERROR_TIMEOUT.to_hresult(), "сервер не ответил вовремя"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    op.GetResults()
}