//! Task storage: one JSON file, every change is written atomically and
//! broadcast to all windows as `tasks-changed`.

use chrono::{NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::backup::{self, Backup};

/// Completed tasks are kept this long (for "done today" stats), then dropped.
const KEEP_DONE_MS: i64 = 14 * 24 * 60 * 60 * 1000;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    /// Local date `YYYY-MM-DD`, `None` = no deadline.
    #[serde(default)]
    pub due: Option<String>,
    /// Local time `HH:MM`, only meaningful together with `due`.
    #[serde(default)]
    pub time: Option<String>,
    /// Recurrence rule, interpreted by the UI (`{"kind":"daily"}` etc.); needs `due`.
    #[serde(default)]
    pub repeat: Option<Value>,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub done_at: Option<i64>,
    pub created: i64,
    /// Manual position in «Без срока» (dragged); `None` = by creation time.
    #[serde(default)]
    pub order: Option<f64>,
    /// `"<due> <time>"` of the occurrence a reminder was already shown for.
    #[serde(default)]
    pub notified: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct FileData {
    #[serde(default)]
    tasks: Vec<Task>,
}

pub struct Store {
    path: PathBuf,
    pub tasks: Mutex<Vec<Task>>,
    seq: AtomicU32,
    /// Why tasks.json couldn't be read; while set, nothing is saved (it would wipe
    /// the file) and a background retry keeps trying to load it.
    error: Mutex<Option<String>>,
    backup: Backup,
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// `Ok(None)` = no file yet. Retries briefly: at logon an antivirus or backup
/// tool may hold the file for a moment.
fn read_file(path: &Path, attempts: u32) -> io::Result<Option<Vec<u8>>> {
    let mut last = None;
    for i in 0..attempts {
        if i > 0 {
            thread::sleep(Duration::from_millis(300));
        }
        match fs::read(path) {
            Ok(b) => return Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => last = Some(e),
        }
    }
    Err(last.expect("at least one attempt"))
}

fn parse_tasks(bytes: &[u8]) -> serde_json::Result<Vec<Task>> {
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let data: FileData = serde_json::from_slice(body)?;
    let cutoff = now_ms() - KEEP_DONE_MS;
    Ok(data.tasks.into_iter().filter(|t| !t.done || t.done_at.unwrap_or(0) >= cutoff).collect())
}

fn to_json(tasks: &[Task]) -> Result<String, String> {
    serde_json::to_string_pretty(&FileData { tasks: tasks.to_vec() }).map_err(|e| e.to_string())
}

fn is_missing(path: &Path) -> bool {
    matches!(fs::metadata(path), Err(e) if e.kind() == io::ErrorKind::NotFound)
}

/// What the store starts from.
enum Loaded {
    File(Vec<Task>),
    Restored(PathBuf, Vec<Task>),
    Empty,
}

/// tasks.json, or the newest backup snapshot that parses when tasks.json is gone
/// (deleted, or a broken one set aside by `read_tasks`). Errors while a snapshot
/// can't be read: better to wait than to start from an older copy or from nothing.
fn load_or_restore(path: &Path, attempts: u32) -> Result<Loaded, String> {
    let tasks = read_tasks(path, attempts)?;
    if !is_missing(path) {
        return Ok(Loaded::File(tasks));
    }
    let unreadable = |e: io::Error| format!("tasks.json нет, а бэкап на D: не читается: {e}");
    for snapshot in backup::snapshots().map_err(unreadable)? {
        match read_file(&snapshot, 3) {
            Ok(Some(bytes)) => {
                if let Ok(tasks) = parse_tasks(&bytes) {
                    return Ok(Loaded::Restored(snapshot, tasks));
                }
            }
            Ok(None) => {} // pruned meanwhile
            Err(e) => return Err(unreadable(e)),
        }
    }
    Ok(Loaded::Empty)
}

/// Tasks from disk, or why they can't be loaded (then nothing may be saved).
fn read_tasks(path: &Path, attempts: u32) -> Result<Vec<Task>, String> {
    let bytes = match read_file(path, attempts) {
        Ok(None) => return Ok(Vec::new()),
        Ok(Some(b)) => b,
        Err(e) => return Err(format!("tasks.json не открывается: {e}")),
    };
    match parse_tasks(&bytes) {
        Ok(tasks) => Ok(tasks),
        Err(e) => {
            // Keep the broken file instead of silently overwriting it.
            let backup = path.with_extension(format!("broken-{}.json", now_ms()));
            match fs::rename(path, &backup) {
                Ok(()) => {
                    eprintln!("tasks.json is unreadable ({e}), moved to {backup:?}");
                    Ok(Vec::new())
                }
                Err(re) => Err(format!("tasks.json повреждён ({e}) и не переименовывается: {re}")),
            }
        }
    }
}

impl Store {
    pub fn load(path: PathBuf) -> Self {
        let loaded = load_or_restore(&path, 10);
        let store = Store {
            path,
            tasks: Mutex::new(Vec::new()),
            seq: AtomicU32::new(0),
            error: Mutex::new(loaded.as_ref().err().cloned()),
            backup: Backup::start(),
        };
        if let Ok(loaded) = loaded {
            let mut tasks = store.tasks.lock().unwrap();
            store.adopt(&mut tasks, loaded);
        }
        store
    }

    /// Puts freshly loaded tasks in place; `tasks` is the locked list and saving
    /// is enabled.
    fn adopt(&self, tasks: &mut Vec<Task>, loaded: Loaded) {
        match loaded {
            Loaded::File(loaded) => {
                // Today gets a snapshot even if nothing changes.
                if let Ok(json) = to_json(&loaded) {
                    self.backup.push_initial(json);
                }
                *tasks = loaded;
            }
            Loaded::Restored(from, restored) => {
                eprintln!("tasks.json is missing, restored from {from:?}");
                if let Err(e) = self.save(&restored) {
                    eprintln!("restored tasks not saved yet: {e}");
                }
                // Kept even if unsaved: the next change writes them.
                *tasks = restored;
            }
            Loaded::Empty => tasks.clear(),
        }
    }

    /// Waits briefly for the backup drive to get the latest state (on quit).
    pub fn flush_backup(&self) {
        self.backup.flush(Duration::from_secs(3));
    }

    pub fn save(&self, tasks: &[Task]) -> Result<(), String> {
        if let Some(e) = self.error.lock().unwrap().as_ref() {
            return Err(format!("Не сохранено: {e}"));
        }
        let json = to_json(tasks)?;
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
            f.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())?; // survive a power cut
        }
        let mut last = String::new();
        for i in 0..5 {
            if i > 0 {
                thread::sleep(Duration::from_millis(100));
            }
            match fs::rename(&tmp, &self.path) {
                Ok(()) => {
                    self.backup.push(json);
                    return Ok(());
                }
                Err(e) => last = e.to_string(), // file briefly held by AV/indexer
            }
        }
        Err(format!("Не сохранено: {last}"))
    }

    fn next_id(&self) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("{:x}{:04x}", now_ms(), n & 0xffff)
    }
}

fn valid_date(s: &str) -> bool {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

fn valid_time(s: &str) -> bool {
    s.len() == 5 && NaiveTime::parse_from_str(s, "%H:%M").is_ok()
}

fn opt_string(v: &Value, check: fn(&str) -> bool, field: &str) -> Result<Option<String>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if check(s) => Ok(Some(s.clone())),
        _ => Err(format!("invalid {field}: {v}")),
    }
}

fn clean_title(s: &str) -> Result<String, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("empty title".into());
    }
    Ok(t.chars().take(500).collect())
}

/// Time and recurrence only make sense with a date.
fn normalize(t: &mut Task) {
    if t.due.is_none() {
        t.time = None;
        t.repeat = None;
    }
    if !t.repeat.as_ref().is_some_and(Value::is_object) {
        t.repeat = None;
    }
}

/// Applies `f` to a copy; memory changes only if the file was written.
fn mutate<R>(
    app: &AppHandle,
    store: &Store,
    f: impl FnOnce(&mut Vec<Task>) -> Result<R, String>,
) -> Result<R, String> {
    let mut tasks = store.tasks.lock().unwrap();
    let mut next = tasks.clone();
    let out = f(&mut next)?;
    store.save(&next)?;
    *tasks = next;
    let _ = app.emit("tasks-changed", &*tasks);
    Ok(out)
}

#[tauri::command]
pub fn get_tasks(store: State<Store>) -> Vec<Task> {
    store.tasks.lock().unwrap().clone()
}

/// `None` when tasks.json is fine; otherwise why it can't be read.
#[tauri::command]
pub fn store_status(store: State<Store>) -> Option<String> {
    store.error.lock().unwrap().clone()
}

/// While tasks.json can't be read, keeps retrying; once it loads, the widgets
/// get the real list and saving is enabled again.
pub fn spawn_retry(app: AppHandle) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(5));
        let store = app.state::<Store>();
        if store.error.lock().unwrap().is_none() {
            continue;
        }
        if let Ok(loaded) = load_or_restore(&store.path, 1) {
            let mut tasks = store.tasks.lock().unwrap();
            *store.error.lock().unwrap() = None;
            store.adopt(&mut tasks, loaded);
            let _ = app.emit("tasks-changed", &*tasks);
            let _ = app.emit("store-status", None::<String>);
        }
    });
}

#[tauri::command]
pub fn add_task(
    app: AppHandle,
    store: State<Store>,
    title: String,
    due: Option<String>,
    time: Option<String>,
    repeat: Option<Value>,
) -> Result<Task, String> {
    let due = opt_string(&due.map(Value::String).unwrap_or(Value::Null), valid_date, "due")?;
    let time = opt_string(&time.map(Value::String).unwrap_or(Value::Null), valid_time, "time")?;
    let mut task = Task {
        id: store.next_id(),
        title: clean_title(&title)?,
        due,
        time,
        repeat,
        done: false,
        done_at: None,
        created: now_ms(),
        order: None,
        notified: None,
    };
    normalize(&mut task);
    mutate(&app, &store, |tasks| {
        tasks.push(task.clone());
        Ok(task)
    })
}

/// Applies a partial update. Allowed keys: title, due, time, repeat, done;
/// `null` clears an optional field.
#[tauri::command]
pub fn update_task(
    app: AppHandle,
    store: State<Store>,
    id: String,
    patch: Map<String, Value>,
) -> Result<(), String> {
    mutate(&app, &store, |tasks| {
        let task = tasks.iter_mut().find(|t| t.id == id).ok_or("task not found")?;
        let mut next = task.clone();
        for (key, v) in &patch {
            match key.as_str() {
                "title" => next.title = clean_title(v.as_str().ok_or("invalid title")?)?,
                "due" => next.due = opt_string(v, valid_date, "due")?,
                "time" => next.time = opt_string(v, valid_time, "time")?,
                "repeat" => next.repeat = Some(v.clone()),
                "order" => {
                    next.order = match v {
                        Value::Null => None,
                        _ => Some(v.as_f64().filter(|o| o.is_finite()).ok_or("invalid order")?),
                    }
                }
                "done" => {
                    next.done = v.as_bool().ok_or("invalid done")?;
                    next.done_at = if next.done { Some(now_ms()) } else { None };
                }
                other => return Err(format!("unknown field {other}")),
            }
        }
        // `notified` is left alone: it names the exact "<due> <time>" already
        // reminded about, so a moved task re-arms by itself and moving it back
        // (e.g. undo) doesn't remind twice.
        normalize(&mut next);
        *task = next;
        Ok(())
    })
}

/// Returns the removed task exactly as stored (incl. reminder state) for undo.
#[tauri::command]
pub fn delete_task(app: AppHandle, store: State<Store>, id: String) -> Result<Task, String> {
    mutate(&app, &store, |tasks| {
        let pos = tasks.iter().position(|t| t.id == id).ok_or("task not found")?;
        Ok(tasks.remove(pos))
    })
}

/// Undo of a delete: puts the task back with its original id, age and reminder state.
#[tauri::command]
pub fn restore_task(app: AppHandle, store: State<Store>, task: Task) -> Result<(), String> {
    let mut task = task;
    task.title = clean_title(&task.title)?;
    task.due = opt_string(&task.due.map(Value::String).unwrap_or(Value::Null), valid_date, "due")?;
    task.time = opt_string(&task.time.map(Value::String).unwrap_or(Value::Null), valid_time, "time")?;
    normalize(&mut task);
    mutate(&app, &store, |tasks| {
        if tasks.iter().any(|t| t.id == task.id) {
            return Err("task already exists".into());
        }
        tasks.push(task);
        Ok(())
    })
}
