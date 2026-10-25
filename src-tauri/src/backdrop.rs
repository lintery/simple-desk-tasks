//! Wallpaper behind the glass. Widgets live on the desktop layer, so the only
//! thing under them is the wallpaper: we serve it to the page (`wall://`) and
//! tell each window where it sits on its monitor, so the page can draw the
//! matching piece of wallpaper and refract it.

use serde::Serialize;
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, UNIX_EPOCH},
};
use tauri::{
    http::{Request, Response},
    AppHandle, Emitter, Manager, UriSchemeContext, WebviewWindow, Wry,
};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

/// Monitor and window geometry in physical px; the page does the fit math
/// once it knows the image size.
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Backdrop {
    mon_w: u32,
    mon_h: u32,
    /// Window's top-left corner relative to the monitor's top-left corner.
    win_x: i32,
    win_y: i32,
    scale: f64,
    /// Windows wallpaper style: "fill", "fit", "stretch", "center", "span", "tile".
    style: &'static str,
    /// Changes whenever the wallpaper file changes (cache buster).
    ver: u64,
}

fn desktop_key() -> Option<RegKey> {
    RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Control Panel\Desktop").ok()
}

/// The image Windows actually shows (already transcoded by the shell).
fn wallpaper_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let transcoded = PathBuf::from(appdata).join(r"Microsoft\Windows\Themes\TranscodedWallpaper");
    if transcoded.is_file() {
        return Some(transcoded);
    }
    let p: String = desktop_key()?.get_value("WallPaper").ok()?;
    let p = PathBuf::from(p);
    p.is_file().then_some(p)
}

fn wallpaper_style() -> &'static str {
    let Some(key) = desktop_key() else { return "fill" };
    let style: String = key.get_value("WallpaperStyle").unwrap_or_default();
    let tile: String = key.get_value("TileWallpaper").unwrap_or_default();
    match (style.trim(), tile.trim()) {
        (_, "1") => "tile",
        ("0", _) => "center",
        ("2", _) => "stretch",
        ("6", _) => "fit",
        ("22", _) => "span",
        _ => "fill",
    }
}

fn wallpaper_version() -> u64 {
    wallpaper_path()
        .and_then(|p| fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn compute(win: &WebviewWindow) -> Option<Backdrop> {
    let mon = win.current_monitor().ok()??;
    let pos = win.outer_position().ok()?;
    if pos.x <= -30000 {
        return None;
    }
    let style = wallpaper_style();
    // "Span" lays one picture over all monitors together: the area it fills is
    // their bounding box, not this monitor.
    let (x0, y0, w, h) = match (style, win.available_monitors()) {
        ("span", Ok(all)) if !all.is_empty() => {
            let left = all.iter().map(|m| m.position().x).min().unwrap_or(0);
            let top = all.iter().map(|m| m.position().y).min().unwrap_or(0);
            let right = all.iter().map(|m| m.position().x + m.size().width as i32).max().unwrap_or(0);
            let bottom = all.iter().map(|m| m.position().y + m.size().height as i32).max().unwrap_or(0);
            (left, top, (right - left).max(1) as u32, (bottom - top).max(1) as u32)
        }
        _ => (mon.position().x, mon.position().y, mon.size().width, mon.size().height),
    };
    Some(Backdrop {
        mon_w: w,
        mon_h: h,
        win_x: pos.x - x0,
        win_y: pos.y - y0,
        scale: mon.scale_factor(),
        style,
        ver: wallpaper_version(),
    })
}

/// Pushes the current geometry to the window (on move/resize/wallpaper change).
pub fn send(win: &WebviewWindow) {
    if let Some(b) = compute(win) {
        let _ = win.emit_to(win.label(), "backdrop", b);
    }
}

#[tauri::command]
pub fn get_backdrop(window: WebviewWindow) -> Option<Backdrop> {
    compute(&window)
}

/// `wall://` → the current wallpaper image.
pub fn serve(_ctx: UriSchemeContext<'_, Wry>, _req: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let bytes = wallpaper_path().and_then(|p| fs::read(p).ok());
    match bytes {
        Some(b) => {
            let mime = if b.starts_with(&[0x89, b'P', b'N', b'G']) { "image/png" } else { "image/jpeg" };
            Response::builder()
                .header("Content-Type", mime)
                .header("Cache-Control", "no-cache")
                .body(b)
                .unwrap()
        }
        None => Response::builder().status(404).body(Vec::new()).unwrap(),
    }
}

/// Re-sends geometry to every widget when the wallpaper file changes.
pub fn spawn_watcher(app: AppHandle, labels: &'static [(&'static str, &'static str)]) {
    thread::spawn(move || {
        let mut ver = wallpaper_version();
        let mut style = wallpaper_style();
        loop {
            thread::sleep(Duration::from_secs(5));
            let (v, s) = (wallpaper_version(), wallpaper_style());
            if v != ver || s != style {
                ver = v;
                style = s;
                for (label, _) in labels {
                    if let Some(w) = app.get_webview_window(label) {
                        send(&w);
                    }
                }
            }
        }
    });
}
