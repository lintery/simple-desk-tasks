#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backdrop;
mod backup;
mod calendar;
mod desktop;
mod focus;
mod ical;
mod layout;
mod media;
mod notify;
mod selection;
mod net;
mod settings;
mod sounds;
mod store;
mod updater;

use std::sync::{
    atomic::{AtomicBool, AtomicI64, Ordering},
    OnceLock,
};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// Widget windows: label and title; the task lists left to right, the focus
/// strip under them, the player card under «Без срока», the calendar on the right.
pub const WIDGETS: [(&str, &str); 6] = [
    ("today", "Сегодня"),
    ("upcoming", "Предстоящие"),
    ("nodate", "Без срока"),
    (focus::LABEL, "Фокус"),
    (media::LABEL, "Плеер"),
    (calendar::LABEL, "Календарь"),
];
const QUICK: &str = "quick";
/// The settings window: an ordinary window, created when opened and freed on close.
const SETTINGS: &str = "settings";
/// Section the settings window should open at (e.g. "calendar").
static SETTINGS_SECTION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// Card is 600×~136; the rest is a transparent margin for its shadow.
const QUICK_SIZE: (f64, f64) = (640.0, 180.0);
/// Quick-add hotkeys in order of preference; the first free one is used.
/// It also grabs any text selected in the app in front (see selection.rs).
const HOTKEYS: [(&str, &str); 3] =
    [("alt+n", "Alt+N"), ("ctrl+alt+n", "Ctrl+Alt+N"), ("alt+shift+n", "Alt+Shift+N")];
static HOTKEY_IN_USE: OnceLock<&'static str> = OnceLock::new();
/// When the quick popup last hid itself on focus loss (ms). A tray click first
/// steals focus, so without this the same click would re-open it.
static QUICK_AUTO_HIDDEN_AT: AtomicI64 = AtomicI64::new(0);
/// Set while the hotkey is copying the selection from the app in front.
static CAPTURING: AtomicBool = AtomicBool::new(false);
/// (token, task id) of the drag in progress between widgets (see `task_drag`).
static DRAGGING: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);
use layout::MOVABLE;
/// Same for every window (WebView2 shares one browser process only when the
/// args match). Tauri's defaults plus one renderer for all our same-origin pages;
/// sound may start without a click, so a phase-end chime plays after a restart.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --process-per-site --disable-gpu --autoplay-policy=no-user-gesture-required";

fn main() {
    // Before the single-instance check: the version we replaced may still be
    // closing, or a new version may have failed to start and needs rolling back.
    let updated_to = updater::startup();
    desktop::set_app_id();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| layout::place_all(app)))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_quick(app, true);
                    }
                })
                .build(),
        )
        .register_uri_scheme_protocol("wall", backdrop::serve)
        .invoke_handler(tauri::generate_handler![
            backdrop::get_backdrop,
            store::get_tasks,
            store::add_task,
            store::update_task,
            store::delete_task,
            store::restore_task,
            store::store_status,
            desktop::open_url,
            focus::focus_state,
            focus::focus_action,
            focus::focus_mode,
            focus::focus_prefs,
            media::media_state,
            media::media_control,
            settings::get_settings,
            settings::set_settings,
            show_settings,
            take_settings_section,
            pick_folder,
            app_version,
            calendar::calendar_state,
            calendar::calendar_connect,
            calendar::calendar_disconnect,
            calendar::calendar_refresh,
            sounds::sound_recording,
            updater::update_status,
            updater::update_check,
            updater::update_install,
            hide_quick,
            hotkey_label,
            widgets_movable,
            task_drag,
            drop_at_cursor,
        ])
        .setup(move |app| {
            let handle = app.handle();
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            // First: the store (backup folder) and the timer (durations) read them.
            settings::load(data_dir.join("settings.json"));
            app.manage(store::Store::load(data_dir.join("tasks.json")));
            app.manage(layout::Layout::load(data_dir.join("layout.json")));
            app.manage(focus::Focus::load(data_dir.join("focus.json")));
            if !settings::locked() {
                desktop::set_autostart(settings::get().autostart);
            }
            desktop::register_toast_app(&data_dir);

            for (label, title) in WIDGETS {
                create_widget(handle, label, title)?;
            }
            create_quick(handle)?;
            layout::place_all(handle);
            for (keys, label) in HOTKEYS {
                match app.global_shortcut().register(keys) {
                    Ok(()) => {
                        let _ = HOTKEY_IN_USE.set(label);
                        break;
                    }
                    Err(e) => eprintln!("hotkey {label} is taken: {e}"),
                }
            }
            build_tray(handle)?;
            layout::spawn_watchers(handle.clone());
            backdrop::spawn_watcher(handle.clone(), &WIDGETS);
            notify::spawn(handle.clone());
            store::spawn_retry(handle.clone());
            focus::spawn(handle.clone());
            media::spawn(handle.clone());
            updater::spawn(handle.clone());
            calendar::spawn(handle.clone());
            if let Some(version) = &updated_to {
                updater::announce_updated(version);
            }
            updater::confirm();
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Desk Tasks");
}

fn create_widget(app: &AppHandle, label: &str, title: &str) -> tauri::Result<()> {
    let page = match label {
        focus::LABEL => "focus.html",
        media::LABEL => "player.html",
        calendar::LABEL => "calendar.html",
        _ => "index.html",
    };
    let (min_w, min_h) = layout::min_size(label);
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .additional_browser_args(BROWSER_ARGS)
        // Tasks are dragged between widgets with HTML drag & drop, which
        // Tauri's own file-drop handler would swallow.
        .disable_drag_drop_handler()
        // Never leave the app's own pages (e.g. a link dropped onto a widget).
        .on_navigation(|url| url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost"))
        .title(title)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false) // pinned; see MOVABLE
        // A double click on the drag region would otherwise maximize the widget.
        .maximizable(false)
        .minimizable(false)
        .skip_taskbar(true)
        .always_on_bottom(true)
        .visible(false)
        .focused(false)
        .min_inner_size(min_w, min_h)
        .inner_size(340.0, 480.0)
        .build()?;
    desktop::pin_to_desktop(&win);
    let app2 = app.clone();
    let win2 = win.clone();
    let label2 = label.to_string();
    win.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => api.prevent_close(),
        WindowEvent::Moved(_) | WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
            if MOVABLE.load(Ordering::Relaxed) {
                layout::remember(&app2, &label2);
            } else {
                // Pinned: a move from the keyboard (Alt+Space → Move, Win+Shift+←/→)
                // is undone instead of saved.
                layout::snap_back(&app2, &label2);
            }
            backdrop::send(&win2);
        }
        _ => {}
    });
    Ok(())
}

fn create_quick(app: &AppHandle) -> tauri::Result<()> {
    let win = WebviewWindowBuilder::new(app, QUICK, WebviewUrl::App("quick.html".into()))
        .additional_browser_args(BROWSER_ARGS)
        .title("Новая задача")
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .visible(false)
        .inner_size(QUICK_SIZE.0, QUICK_SIZE.1)
        .build()?;
    let win2 = win.clone();
    win.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let _ = win2.hide();
        }
        WindowEvent::Focused(false) => {
            if win2.is_visible().unwrap_or(false) {
                QUICK_AUTO_HIDDEN_AT.store(store::now_ms(), Ordering::Relaxed);
                let _ = win2.hide();
            }
        }
        _ => {}
    });
    Ok(())
}

/// Opens/closes the quick-add popup. From the hotkey (`grab_selection`), text
/// selected in the app in front is copied into it.
fn toggle_quick(app: &AppHandle, grab_selection: bool) {
    let Some(win) = app.get_webview_window(QUICK) else { return };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        return;
    }
    if store::now_ms() - QUICK_AUTO_HIDDEN_AT.load(Ordering::Relaxed) < 300 {
        return; // this very click just closed it
    }
    let owner = desktop::hwnd_value(&win);
    match owner.filter(|_| grab_selection) {
        // Capturing waits for the hotkey's keys to be released: off the main thread.
        Some(owner) => {
            if CAPTURING.swap(true, Ordering::SeqCst) {
                return; // a repeat press while the previous one is still copying
            }
            let app = app.clone();
            std::thread::spawn(move || {
                // Released even if capturing bails out early.
                struct Done;
                impl Drop for Done {
                    fn drop(&mut self) {
                        CAPTURING.store(false, Ordering::SeqCst);
                    }
                }
                let text = {
                    let _done = Done;
                    selection::capture(desktop::hwnd_from(owner))
                };
                show_quick(&app, text);
            });
        }
        None => show_quick(app, None),
    }
}

fn show_quick(app: &AppHandle, text: Option<String>) {
    let Some(win) = app.get_webview_window(QUICK) else { return };
    layout::center_on_cursor_monitor(app, &win, QUICK_SIZE.0, QUICK_SIZE.1);
    let _ = win.show();
    let _ = win.set_focus();
    let _ = win.emit_to(QUICK, "quick-open", text);
}

/// Opens the settings window (or brings it forward), optionally at a section.
fn open_settings(app: &AppHandle, section: Option<&str>) -> tauri::Result<()> {
    *SETTINGS_SECTION.lock().unwrap() = section.map(str::to_string);
    if let Some(win) = app.get_webview_window(SETTINGS) {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        let _ = win.emit_to(SETTINGS, "settings-section", section);
        return Ok(());
    }
    WebviewWindowBuilder::new(app, SETTINGS, WebviewUrl::App("settings.html".into()))
        .additional_browser_args(BROWSER_ARGS)
        .on_navigation(|url| url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost"))
        .title("Настройки · Задачи")
        .inner_size(560.0, 660.0)
        .resizable(false)
        .maximizable(false)
        .theme(Some(tauri::Theme::Dark))
        .background_color(tauri::window::Color(23, 23, 26, 255))
        .center()
        .build()?;
    Ok(())
}

#[tauri::command]
async fn show_settings(app: AppHandle, section: Option<String>) -> Result<(), String> {
    open_settings(&app, section.as_deref()).map_err(|e| e.to_string())
}

/// The section the settings window was opened for, once.
#[tauri::command]
fn take_settings_section() -> Option<String> {
    SETTINGS_SECTION.lock().unwrap().take()
}

/// A folder picked in the standard dialog (owned by the settings window).
#[tauri::command]
async fn pick_folder(app: AppHandle, start: Option<String>) -> Option<String> {
    let owner = app.get_webview_window(SETTINGS).and_then(|w| desktop::hwnd_value(&w));
    tauri::async_runtime::spawn_blocking(move || desktop::pick_folder(owner, start)).await.ok().flatten()
}

#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

#[tauri::command]
fn hide_quick(app: AppHandle) {
    if let Some(win) = app.get_webview_window(QUICK) {
        let _ = win.hide();
    }
}

#[tauri::command]
fn hotkey_label() -> &'static str {
    HOTKEY_IN_USE.get().copied().unwrap_or("")
}

/// Which task is being dragged right now (set by the source widget). Drag data
/// doesn't reliably cross between WebView2 windows, so every widget learns the
/// id from here (`task-drag` event) and the drop target uses it.
/// Each drag has its own `token`; clearing (`id: None`) only applies while that
/// token is still the current drag, so a late clear from drag A can't wipe a
/// drag B that started meanwhile — even of the same task.
#[tauri::command]
fn task_drag(app: AppHandle, id: Option<String>, token: String) {
    let mut current = DRAGGING.lock().unwrap();
    match id {
        Some(id) => *current = Some((token, id)),
        None if current.as_ref().is_some_and(|(t, _)| *t == token) => *current = None,
        None => return,
    }
    let _ = app.emit("task-drag", current.as_ref().map(|(_, id)| id.clone()));
}

/// Fallback when no window accepted a dragged task: if the mouse was released
/// over another widget that is really visible there, hand it the drop
/// (`task-dropped`, y in CSS px from the window's top). Not when the drag was
/// cancelled with Esc (the button is still held then).
#[tauri::command]
fn drop_at_cursor(app: AppHandle, from: String, id: String) {
    if desktop::primary_button_down() {
        return;
    }
    let Some((under, _, y)) = desktop::window_under_cursor() else { return };
    for (label, _) in WIDGETS {
        if label == from {
            continue;
        }
        let Some(win) = app.get_webview_window(label) else { continue };
        if desktop::hwnd_value(&win) != Some(under) {
            continue;
        }
        let (Ok(pos), Ok(scale)) = (win.outer_position(), win.scale_factor()) else { return };
        let css_y = (y - pos.y) as f64 / scale;
        let _ = win.emit_to(label, "task-dropped", serde_json::json!({ "id": id, "y": css_y }));
        return;
    }
}

#[tauri::command]
fn widgets_movable() -> bool {
    MOVABLE.load(Ordering::Relaxed)
}

/// Pins or unpins the widgets: resizing in Rust, the drag region in each page.
fn set_movable(app: &AppHandle, movable: bool) {
    MOVABLE.store(movable, Ordering::Relaxed);
    for (label, _) in WIDGETS {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.set_resizable(movable);
        }
    }
    let _ = app.emit("widgets-movable", movable);
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let add_label = match HOTKEY_IN_USE.get() {
        Some(keys) => format!("Новая задача\t{keys}"),
        None => "Новая задача".to_string(),
    };
    let add = MenuItem::with_id(app, "add", add_label, true, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "Показать виджеты", true, None::<&str>)?;
    let movable = CheckMenuItem::with_id(app, "movable", "Разрешить перемещение", true, false, None::<&str>)?;
    let reset = MenuItem::with_id(app, "reset", "Расставить по умолчанию", true, None::<&str>)?;
    let prefs = MenuItem::with_id(app, "settings", "Настройки…", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Выйти", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&add, &show, &movable, &reset, &prefs, &sep, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Задачи")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "add" => toggle_quick(app, false),
            "show" => layout::place_all(app),
            // The check mark toggles itself on click; mirror its new state.
            "movable" => set_movable(app, movable.is_checked().unwrap_or(false)),
            "reset" => layout::reset(app),
            "settings" => {
                let _ = open_settings(app, None);
            }
            "quit" => {
                app.state::<layout::Layout>().flush();
                app.state::<store::Store>().flush_backup();
                // Quit by hand: an updated version on trial evidently works.
                updater::keep();
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                toggle_quick(tray.app_handle(), false);
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}
