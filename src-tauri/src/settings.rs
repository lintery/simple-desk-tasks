//! User settings (settings.json): focus durations, the backup folder,
//! autostart, updates. Read anywhere with `get()`; changed from the settings
//! window (`set_settings`), which saves them and tells every window.

use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{ErrorKind, Write},
    path::PathBuf,
    sync::{OnceLock, RwLock},
};
use tauri::{AppHandle, Emitter};

pub const DEFAULT_BACKUP_DIR: &str = r"D:\backups\simple-desk-tasks";
const NOT_SAVED: &str = "Настройки не сохранены";
/// Calendars of the account that aren't meetings and are left out: Mail.ru's
/// holidays and «what happened on this day» feed.
const HIDDEN_CALENDARS: [&str; 1] = ["события в этот день"];

/// One of HIDDEN_CALENDARS, whatever the case and spacing of its name.
pub fn hidden_calendar(name: &str) -> bool {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    HIDDEN_CALENDARS.contains(&name.as_str())
}

/// One focus mode, in minutes; `every` focus sessions earn a long break (0 = never).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModeLen {
    pub focus: u32,
    pub short: u32,
    pub long: u32,
    pub every: u32,
}

impl ModeLen {
    const fn new(focus: u32, short: u32, long: u32, every: u32) -> Self {
        ModeLen { focus, short, long, every }
    }

    fn sanitize(&mut self) {
        self.focus = self.focus.clamp(1, 240);
        self.short = self.short.clamp(1, 60);
        self.long = self.long.clamp(1, 90);
        self.every = self.every.min(12);
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Modes {
    pub pomodoro: ModeLen,
    pub focus50: ModeLen,
    pub deep90: ModeLen,
}

impl Default for Modes {
    fn default() -> Self {
        Modes {
            pomodoro: ModeLen::new(25, 5, 15, 4),
            focus50: ModeLen::new(50, 10, 10, 0),
            deep90: ModeLen::new(90, 20, 20, 0),
        }
    }
}

impl Modes {
    pub fn get(&self, mode: &str) -> ModeLen {
        match mode {
            "focus50" => self.focus50,
            "deep90" => self.deep90,
            _ => self.pomodoro,
        }
    }
}

/// A calendar found on the CalDAV account.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CalendarRef {
    pub url: String,
    pub name: String,
    /// `#rrggbb`, or empty.
    pub color: String,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub modes: Modes,
    pub backup_enabled: bool,
    pub backup_dir: String,
    pub autostart: bool,
    pub auto_update: bool,
    /// Mail.ru account of the calendar (its app password is in the Windows
    /// Credential Manager, never here); empty = not connected.
    pub calendar_email: String,
    pub calendars: Vec<CalendarRef>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            modes: Modes::default(),
            backup_enabled: true,
            backup_dir: DEFAULT_BACKUP_DIR.to_string(),
            autostart: true,
            auto_update: true,
            calendar_email: String::new(),
            calendars: Vec::new(),
        }
    }
}

impl Settings {
    fn sanitize(&mut self) {
        for m in [&mut self.modes.pomodoro, &mut self.modes.focus50, &mut self.modes.deep90] {
            m.sanitize();
        }
        self.calendars.retain(|c| !hidden_calendar(&c.name));
        self.backup_dir = self.backup_dir.trim().to_string();
        if self.backup_dir.is_empty() {
            self.backup_dir = DEFAULT_BACKUP_DIR.to_string();
        }
    }
}

struct State {
    path: PathBuf,
    value: RwLock<Settings>,
    /// settings.json couldn't be read at startup: it's left alone.
    locked: bool,
}

static STATE: OnceLock<State> = OnceLock::new();

/// Reads settings.json (defaults if it's missing). Call once at startup. A
/// file that doesn't parse is set aside as settings.broken-….json rather than
/// overwritten by the next save.
pub fn load(path: PathBuf) {
    // A file held by an antivirus or backup tool may refuse a read for a moment.
    let mut read = fs::read(&path);
    for _ in 0..10 {
        match &read {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                std::thread::sleep(std::time::Duration::from_millis(200));
                read = fs::read(&path);
            }
            _ => break,
        }
    }
    let (mut value, locked) = match read {
        Err(e) if e.kind() == ErrorKind::NotFound => (Settings::default(), false),
        // Still unreadable: run on defaults but don't save over the real file.
        Err(_) => (Settings::default(), true),
        Ok(bytes) => {
            let parsed = std::str::from_utf8(&bytes).ok().and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok());
            let value = parsed.unwrap_or_else(|| {
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
                let _ = fs::rename(&path, path.with_file_name(format!("settings.broken-{stamp}.json")));
                Settings::default()
            });
            (value, false)
        }
    };
    value.sanitize();
    let _ = STATE.set(State { path, value: RwLock::new(value), locked });
}

/// settings.json couldn't be read at startup, so `get()` holds defaults, not
/// the user's choices: autostart isn't applied from them until the next start.
/// (Backups still go to the default folder: harmless, and a lost tasks.json
/// can still be restored.)
pub fn locked() -> bool {
    STATE.get().is_some_and(|s| s.locked)
}

pub fn get() -> Settings {
    STATE.get().map(|s| s.value.read().unwrap().clone()).unwrap_or_default()
}

/// Changes settings from Rust (the calendar connecting), saves and tells everyone.
pub fn update(app: &AppHandle, f: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
    let state = STATE.get().ok_or(NOT_SAVED)?;
    let next = {
        let mut current = state.value.write().unwrap();
        let mut next = current.clone();
        f(&mut next);
        next.sanitize();
        save(state, &next)?;
        *current = next.clone();
        // Under the lock, so two changes can't reach the windows swapped.
        let _ = app.emit("settings-changed", &next);
        next
    };
    Ok(next)
}

fn save(state: &State, value: &Settings) -> Result<(), String> {
    if state.locked {
        return Err("Настройки не сохраняются: при запуске файл настроек не открылся — перезапустите приложение".into());
    }
    let json = serde_json::to_string_pretty(value).map_err(|_| NOT_SAVED.to_string())?;
    let tmp = state.path.with_extension("json.tmp");
    let write = || -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()
    };
    write().map_err(|e| crate::errors::file(&e, NOT_SAVED))?;
    fs::rename(&tmp, &state.path).map_err(|e| crate::errors::file(&e, NOT_SAVED))
}

/// What the settings window sends, applied over `current`. The calendar
/// account is the calendar's to change (connect / disconnect), so a page that
/// hasn't heard of a change yet can't undo it: only the on/off switches of
/// calendars the account still has are taken from it.
fn merge(current: &Settings, page: Settings) -> Settings {
    let calendars = current
        .calendars
        .iter()
        .map(|c| CalendarRef {
            enabled: page.calendars.iter().find(|p| p.url == c.url).map_or(c.enabled, |p| p.enabled),
            ..c.clone()
        })
        .collect();
    let mut next = Settings { calendar_email: current.calendar_email.clone(), calendars, ..page };
    next.sanitize();
    next
}

#[tauri::command]
pub fn get_settings() -> Settings {
    get()
}

/// Saves new settings and applies them: autostart, the focus timer's idle
/// length; everyone else reads them when needed or on `settings-changed`.
#[tauri::command]
pub fn set_settings(app: AppHandle, settings: Settings) -> Result<Settings, String> {
    let state = STATE.get().ok_or(NOT_SAVED)?;
    let next = {
        let mut current = state.value.write().unwrap();
        let next = merge(&current, settings);
        save(state, &next)?;
        *current = next.clone();
        let _ = app.emit("settings-changed", &next);
        next
    };
    crate::desktop::set_autostart(next.autostart);
    crate::focus::settings_changed(&app);
    crate::calendar::refresh();
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_get_defaults_and_bad_values_are_clamped() {
        let mut s: Settings =
            serde_json::from_str(r#"{"modes":{"pomodoro":{"focus":0,"short":500,"long":5,"every":99}},"backupDir":"  "}"#)
                .unwrap();
        s.sanitize();
        assert_eq!(s.modes.pomodoro, ModeLen::new(1, 60, 5, 12));
        assert_eq!(s.modes.deep90, ModeLen::new(90, 20, 20, 0));
        assert_eq!(s.backup_dir, DEFAULT_BACKUP_DIR);
        assert!(s.backup_enabled && s.autostart && s.auto_update);
    }

    #[test]
    fn page_cannot_undo_calendar_changes() {
        let cal = |url: &str, enabled| CalendarRef { url: url.into(), name: url.into(), enabled, ..Default::default() };
        let current = Settings { calendar_email: "me@mail.ru".into(), calendars: vec![cal("a", true), cal("b", true)], ..Default::default() };
        // A page from before the connect, switching "b" off, an unknown "c" on and autostart off.
        let page = Settings { autostart: false, calendars: vec![cal("b", false), cal("c", true)], ..Default::default() };
        let next = merge(&current, page);
        assert_eq!(next.calendar_email, "me@mail.ru");
        assert_eq!(next.calendars, vec![cal("a", true), cal("b", false)]);
        assert!(!next.autostart);
        // After a disconnect the page's account doesn't come back.
        let gone = merge(&Settings::default(), current);
        assert!(gone.calendar_email.is_empty() && gone.calendars.is_empty());
    }

    #[test]
    fn holiday_feed_is_left_out() {
        let cal = |name: &str| CalendarRef { url: name.into(), name: name.into(), enabled: true, ..Default::default() };
        let mut s = Settings { calendars: vec![cal("Основной"), cal("События в этот день"), cal("СОБЫТИЯ в\u{a0}этот  день")], ..Default::default() };
        s.sanitize();
        assert_eq!(s.calendars, vec![cal("Основной")]);
    }

    #[test]
    fn unknown_mode_falls_back_to_pomodoro() {
        assert_eq!(Modes::default().get("nonsense"), ModeLen::new(25, 5, 15, 4));
    }
}
