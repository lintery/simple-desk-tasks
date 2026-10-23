//! Widget placement: second monitor by default, positions remembered per
//! monitor, re-placed when monitors are plugged in or out.

use crate::WIDGETS;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewWindow};

/// Logical px relative to the monitor's work-area origin.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Geom {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Saved {
    monitor: String,
    geom: Geom,
}

pub struct Layout {
    path: PathBuf,
    saved: Mutex<HashMap<String, Saved>>,
    dirty: AtomicBool,
    /// Move/resize events caused by our own placement are ignored until then.
    quiet_until: Mutex<Instant>,
    /// Monitor set the widgets were last placed for. When it differs, moves come
    /// from Windows shuffling windows between screens, not from the user.
    placed_for: Mutex<String>,
}

/// Widgets are pinned unless the tray option «Разрешить перемещение» is on
/// (not persisted: every start is pinned).
pub static MOVABLE: AtomicBool = AtomicBool::new(false);

const MARGIN: f64 = 28.0;
const GAP: f64 = 20.0;
const WIDTH: f64 = 340.0;

impl Layout {
    pub fn load(path: PathBuf) -> Self {
        let saved = fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Layout {
            path,
            saved: Mutex::new(saved),
            dirty: AtomicBool::new(false),
            quiet_until: Mutex::new(Instant::now()),
            placed_for: Mutex::new(String::new()),
        }
    }

    pub fn flush(&self) {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return;
        }
        let json = serde_json::to_string_pretty(&*self.saved.lock().unwrap()).unwrap_or_default();
        let tmp = self.path.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, &self.path);
        }
    }
}

fn monitor_key(m: &Monitor) -> String {
    match m.name() {
        Some(n) if !n.is_empty() => n.clone(),
        _ => format!("{},{}", m.position().x, m.position().y),
    }
}

fn monitors_signature(mons: &[Monitor]) -> String {
    let mut parts: Vec<String> = mons
        .iter()
        .map(|m| {
            let wa = m.work_area();
            format!(
                "{}@{},{} {}x{} s{}",
                monitor_key(m),
                wa.position.x,
                wa.position.y,
                wa.size.width,
                wa.size.height,
                m.scale_factor()
            )
        })
        .collect();
    parts.sort();
    parts.join("|")
}

/// First non-primary monitor, otherwise the primary one.
fn target_monitor(app: &AppHandle, mons: &[Monitor]) -> Option<Monitor> {
    let primary = app.primary_monitor().ok().flatten();
    let primary_pos = primary.as_ref().map(|p| *p.position());
    mons.iter()
        .find(|m| Some(*m.position()) != primary_pos)
        .cloned()
        .or(primary)
        .or_else(|| mons.first().cloned())
}

fn default_geom(label: &str, m: &Monitor) -> Geom {
    let wa = m.work_area();
    let s = m.scale_factor();
    let area_h = wa.size.height as f64 / s;
    let tall = (area_h - 2.0 * MARGIN).min(560.0);
    let idx = WIDGETS.iter().position(|(l, _)| *l == label).unwrap_or(0) as f64;
    let h = if label == "nodate" { tall.min(420.0) } else { tall };
    Geom { x: MARGIN + idx * (WIDTH + GAP), y: MARGIN, w: WIDTH, h }
}

/// Keeps the widget fully inside the monitor's work area.
fn clamp(g: Geom, m: &Monitor) -> Geom {
    let wa = m.work_area();
    let s = m.scale_factor();
    let (aw, ah) = (wa.size.width as f64 / s, wa.size.height as f64 / s);
    let w = g.w.clamp(240.0, aw.max(240.0));
    let h = g.h.clamp(160.0, ah.max(160.0));
    Geom { x: g.x.clamp(0.0, (aw - w).max(0.0)), y: g.y.clamp(0.0, (ah - h).max(0.0)), w, h }
}

/// Physical rect of a geometry on a monitor.
fn rect(m: &Monitor, g: Geom) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let wa = m.work_area();
    let s = m.scale_factor();
    let pos = PhysicalPosition::new(wa.position.x + (g.x * s).round() as i32, wa.position.y + (g.y * s).round() as i32);
    let size = PhysicalSize::new((g.w * s).round() as u32, (g.h * s).round() as u32);
    (pos, size)
}

fn apply(layout: &Layout, win: &WebviewWindow, m: &Monitor, g: Geom) {
    *layout.quiet_until.lock().unwrap() = Instant::now() + Duration::from_millis(1500);
    let (pos, size) = rect(m, g);
    // Moving onto a monitor with another DPI rescales the window, so size goes last.
    let _ = win.set_position(pos);
    let _ = win.set_size(size);
}

/// Where a widget belongs: its saved monitor if present, else the target monitor.
fn home(app: &AppHandle, mons: &[Monitor], label: &str) -> Option<(Monitor, Geom)> {
    let saved = app.state::<Layout>().saved.lock().unwrap().get(label).cloned();
    let on_saved_monitor = saved
        .as_ref()
        .and_then(|s| mons.iter().find(|m| monitor_key(m) == s.monitor).map(|m| (m.clone(), s.geom)));
    let (mon, geom) = match on_saved_monitor {
        Some(found) => found,
        None => {
            let m = target_monitor(app, mons)?;
            let g = saved.map(|s| s.geom).unwrap_or_else(|| default_geom(label, &m));
            (m, g)
        }
    };
    let g = clamp(geom, &mon);
    Some((mon, g))
}

fn place_one(app: &AppHandle, mons: &[Monitor], label: &str) {
    let Some(win) = app.get_webview_window(label) else { return };
    let Some((mon, geom)) = home(app, mons, label) else { return };
    apply(&app.state::<Layout>(), &win, &mon, geom);
}

/// Pinned widgets: puts back any widget that isn't where it belongs (moved via
/// the window menu, Win+Shift+Arrow, a monitor blip…). Called every second.
fn enforce_pinned(app: &AppHandle, mons: &[Monitor]) {
    for (label, _) in WIDGETS {
        let Some(win) = app.get_webview_window(label) else { continue };
        if win.is_minimized().unwrap_or(false) {
            continue;
        }
        let Some((mon, geom)) = home(app, mons, label) else { continue };
        let (want_pos, want_size) = rect(&mon, geom);
        let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) else { continue };
        let off = (pos.x - want_pos.x).abs().max((pos.y - want_pos.y).abs())
            .max((size.width as i32 - want_size.width as i32).abs())
            .max((size.height as i32 - want_size.height as i32).abs());
        if off > 2 {
            apply(&app.state::<Layout>(), &win, &mon, geom);
        }
    }
}

pub fn place_all(app: &AppHandle) {
    let mons = app.available_monitors().unwrap_or_default();
    *app.state::<Layout>().placed_for.lock().unwrap() = monitors_signature(&mons);
    for (label, _) in WIDGETS {
        place_one(app, &mons, label);
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.show();
        }
    }
}

pub fn reset(app: &AppHandle) {
    let layout = app.state::<Layout>();
    layout.saved.lock().unwrap().clear();
    layout.dirty.store(true, Ordering::SeqCst);
    place_all(app);
}

/// Pinned widgets: puts a widget back where it belongs after something other
/// than our own placement moved it. Monitor changes are left to the watcher.
pub fn snap_back(app: &AppHandle, label: &str) {
    let layout = app.state::<Layout>();
    if Instant::now() < *layout.quiet_until.lock().unwrap() {
        return; // anything missed here is caught by enforce_pinned within a second
    }
    if app.get_webview_window(label).is_some_and(|w| w.is_minimized().unwrap_or(false)) {
        return;
    }
    let mons = app.available_monitors().unwrap_or_default();
    if monitors_signature(&mons) != *layout.placed_for.lock().unwrap() {
        return;
    }
    place_one(app, &mons, label);
}

/// Called on every move/resize of an unpinned widget; remembers where the user put it.
pub fn remember(app: &AppHandle, label: &str) {
    let layout = app.state::<Layout>();
    if Instant::now() < *layout.quiet_until.lock().unwrap() {
        return;
    }
    let Some(win) = app.get_webview_window(label) else { return };
    if win.is_minimized().unwrap_or(false) || win.is_maximized().unwrap_or(false) {
        return;
    }
    let mons = app.available_monitors().unwrap_or_default();
    if monitors_signature(&mons) != *layout.placed_for.lock().unwrap() {
        return; // a screen came or went; the watcher re-places the widgets
    }
    let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) else { return };
    if pos.x <= -30000 || size.width == 0 {
        return;
    }
    let Ok(Some(m)) = win.current_monitor() else { return };
    let wa = m.work_area();
    let s = m.scale_factor();
    let geom = Geom {
        x: (pos.x - wa.position.x) as f64 / s,
        y: (pos.y - wa.position.y) as f64 / s,
        w: size.width as f64 / s,
        h: size.height as f64 / s,
    };
    layout.saved.lock().unwrap().insert(label.to_string(), Saved { monitor: monitor_key(&m), geom });
    layout.dirty.store(true, Ordering::SeqCst);
}

/// Centers the quick-add window on the monitor under the mouse cursor.
pub fn center_on_cursor_monitor(app: &AppHandle, win: &WebviewWindow, w: f64, h: f64) {
    let mons = app.available_monitors().unwrap_or_default();
    let cursor = app.cursor_position().ok();
    let mon = cursor
        .and_then(|c| {
            mons.iter().find(|m| {
                let (p, sz) = (m.position(), m.size());
                c.x >= p.x as f64
                    && c.y >= p.y as f64
                    && c.x < (p.x + sz.width as i32) as f64
                    && c.y < (p.y + sz.height as i32) as f64
            })
        })
        .cloned()
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(m) = mon else { return };
    let wa = m.work_area();
    let s = m.scale_factor();
    let (pw, ph) = ((w * s).round() as i32, (h * s).round() as i32);
    let x = wa.position.x + (wa.size.width as i32 - pw) / 2;
    let y = wa.position.y + (wa.size.height as i32 * 28 / 100);
    let _ = win.set_position(PhysicalPosition::new(x, y));
    let _ = win.set_size(PhysicalSize::new(pw as u32, ph as u32));
}

/// Background upkeep: saves layout, follows monitor changes, keeps widgets
/// pinned to the desktop and brings them back if something minimized them.
pub fn spawn_watchers(app: AppHandle) {
    thread::spawn(move || {
        let mut tick: u32 = 0;
        loop {
            thread::sleep(Duration::from_secs(1));
            tick = tick.wrapping_add(1);
            for (label, _) in WIDGETS {
                if let Some(w) = app.get_webview_window(label) {
                    if w.is_minimized().unwrap_or(false) {
                        let _ = w.unminimize();
                    }
                }
            }
            if !MOVABLE.load(Ordering::Relaxed) {
                let layout = app.state::<Layout>();
                let quiet = Instant::now() < *layout.quiet_until.lock().unwrap();
                let mons = app.available_monitors().unwrap_or_default();
                let same_screens = monitors_signature(&mons) == *layout.placed_for.lock().unwrap();
                if !quiet && same_screens && !mons.is_empty() {
                    enforce_pinned(&app, &mons);
                }
            }
            if tick % 3 == 0 {
                let layout = app.state::<Layout>();
                let mons = app.available_monitors().unwrap_or_default();
                let now = monitors_signature(&mons);
                let changed = now != *layout.placed_for.lock().unwrap();
                if changed && !mons.is_empty() {
                    *layout.placed_for.lock().unwrap() = now;
                    for (label, _) in WIDGETS {
                        place_one(&app, &mons, label);
                    }
                }
                // Explorer restarts recreate the desktop window; re-own to the new one.
                for (label, _) in WIDGETS {
                    if let Some(w) = app.get_webview_window(label) {
                        crate::desktop::pin_to_desktop(&w);
                    }
                }
                layout.flush();
            }
        }
    });
}
