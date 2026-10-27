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

use crate::{
    backup::{self, Backup},
    errors,
};

// What the widgets say when a change can't be made.
const NOT_SAVED: &str = "Задачи не сохранены";
const GONE: &str = "Этой задачи уже нет — её удалили или изменили в другом окне";
const NOT_CHANGED: &str = "Не получилось изменить задачу";

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
    /// Flagged as important: marked and listed first within its day.
    #[serde(default)]
    pub important: bool,
    /// A checklist inside the task.
    #[serde(default)]
    pub subtasks: Vec<Subtask>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Subtask {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub done: bool,
}

/// A task holds at most this many subtasks.
const MAX_SUBTASKS: usize = 50;

/// Subtasks as the page sent them, checked: each needs an id and some text.
fn clean_subtasks(v: &Value) -> Result<Vec<Subtask>, String> {
    let list: Vec<Subtask> = serde_json::from_value(v.clone()).map_err(|_| NOT_CHANGED.to_string())?;
    if list.len() > MAX_SUBTASKS {
        return Err(format!("В задаче может быть не больше {MAX_SUBTASKS} подзадач"));
    }
    let mut seen = std::collections::HashSet::new();
    list.into_iter()
        .map(|s| {
            let id = s.id.trim().to_string();
            if id.is_empty() || id.len() > 40 || !seen.insert(id.clone()) {
                return Err(NOT_CHANGED.to_string());
            }
            Ok(Subtask { id, title: clean_title(&s.title)?, done: s.done })
        })
        .collect()
}

#[derive(Serialize, Default)]
struct FileData {
    tasks: Vec<Task>,
}

/// The file as read: each task kept as raw JSON until it's checked on its own.
#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    tasks: Vec<Value>,
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

/// The tasks of a file, and how many of its records had to be mended or
/// dropped. One bad record (a hand edit, an old version) costs only itself:
/// a missing id or creation time is filled in; one without a title is left out.
fn parse_tasks(bytes: &[u8]) -> serde_json::Result<(Vec<Task>, usize)> {
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let data: RawFile = serde_json::from_slice(body)?;
    let cutoff = now_ms() - KEEP_DONE_MS;
    let mut mended = 0;
    let mut tasks = Vec::with_capacity(data.tasks.len());
    for (i, raw) in data.tasks.into_iter().enumerate() {
        if let Ok(task) = serde_json::from_value::<Task>(raw.clone()) {
            tasks.push(task);
            continue;
        }
        mended += 1;
        let Value::Object(mut map) = raw else { continue };
        if !map.get("title").and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty()) {
            continue;
        }
        map.entry("id").or_insert_with(|| Value::String(format!("{:x}r{i:04x}", now_ms())));
        map.entry("created").or_insert_with(|| Value::from(now_ms()));
        if let Ok(task) = serde_json::from_value::<Task>(Value::Object(map)) {
            tasks.push(task);
        }
    }
    tasks.retain(|t| !t.done || t.done_at.unwrap_or(0) >= cutoff);
    Ok((tasks, mended))
}

fn to_json(tasks: &[Task]) -> Result<String, String> {
    serde_json::to_string_pretty(&FileData { tasks: tasks.to_vec() }).map_err(|_| "Задачи не сохранены: не получилось их записать".to_string())
}

fn is_missing(path: &Path) -> bool {
    matches!(fs::metadata(path), Err(e) if e.kind() == io::ErrorKind::NotFound)
}

/// Written next to tasks.json once tasks have been saved here.
const SAVED_MARKER: &str = "saved";

/// The app has been used here: tasks were saved, or the user set something up
/// (each of those files is written only by a user's action).
fn ran_before(tasks: &Path) -> bool {
    let dir = tasks.parent().unwrap_or(Path::new("."));
    [SAVED_MARKER, "layout.json", "focus.json", "settings.json"].iter().any(|f| dir.join(f).exists())
}

/// What the store starts from.
enum Loaded {
    /// tasks.json, and whether bad records had to be mended (then it's saved back).
    File(Vec<Task>, bool),
    Restored(PathBuf, Vec<Task>),
    Empty,
}

/// tasks.json, or the newest backup snapshot that parses when tasks.json is gone
/// (deleted, or a broken one set aside by `read_tasks`). Errors while a snapshot
/// can't be read: better to wait than to start from an older copy or from nothing.
fn load_or_restore(path: &Path, attempts: u32) -> Result<Loaded, String> {
    let (tasks, mended) = read_tasks(path, attempts)?;
    if !is_missing(path) {
        return Ok(Loaded::File(tasks, mended));
    }
    let unreadable = |e: io::Error| errors::file(&e, "Файла задач нет, а бэкап не читается");
    let snapshots = match backup::snapshots() {
        Ok(s) => s,
        // A first start has nothing to restore: an unready backup drive (a
        // card reader, a locked or network disk) mustn't keep it from saving.
        Err(_) if !ran_before(path) => return Ok(Loaded::Empty),
        Err(e) => return Err(unreadable(e)),
    };
    for snapshot in snapshots {
        match read_file(&snapshot, 3) {
            Ok(Some(bytes)) => {
                if let Ok((tasks, _)) = parse_tasks(&bytes) {
                    return Ok(Loaded::Restored(snapshot, tasks));
                }
            }
            Ok(None) => {} // pruned meanwhile
            Err(e) => return Err(unreadable(e)),
        }
    }
    Ok(Loaded::Empty)
}

/// Tasks from disk and whether any records were mended, or why they can't
/// be loaded (then nothing may be saved).
fn read_tasks(path: &Path, attempts: u32) -> Result<(Vec<Task>, bool), String> {
    let bytes = match read_file(path, attempts) {
        Ok(None) => return Ok((Vec::new(), false)),
        Ok(Some(b)) => b,
        Err(e) => return Err(errors::file(&e, "Файл задач не открывается")),
    };
    match parse_tasks(&bytes) {
        Ok((tasks, 0)) => Ok((tasks, false)),
        Ok((tasks, mended)) => {
            // The file as it was is kept aside; the mended list is saved over it
            // right away (see adopt), so this happens once.
            let copy = path.with_extension(format!("broken-{}.json", now_ms()));
            let _ = fs::write(&copy, &bytes);
            eprintln!("tasks.json: {mended} record(s) mended or dropped, original kept as {copy:?}");
            Ok((tasks, true))
        }
        Err(e) => {
            // Keep the broken file instead of silently overwriting it.
            let backup = path.with_extension(format!("broken-{}.json", now_ms()));
            match fs::rename(path, &backup) {
                Ok(()) => {
                    eprintln!("tasks.json is unreadable ({e}), moved to {backup:?}");
                    Ok((Vec::new(), false))
                }
                Err(re) => {
                    eprintln!("tasks.json is unreadable ({e}) and stays in place: {re}");
                    Err(errors::file(&re, "Файл задач повреждён, а отложить его в сторону не получается"))
                }
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
            Loaded::File(loaded, mended) => {
                self.mark_used(); // a readable tasks.json: used here (an upgrade has no marker yet)
                if mended {
                    if let Err(e) = self.save(&loaded) {
                        eprintln!("mended tasks not saved yet: {e}");
                    }
                } else if let Ok(json) = to_json(&loaded) {
                    // Today gets a snapshot even if nothing changes.
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

    /// Leaves the marker that tasks have been kept here (see ran_before).
    fn mark_used(&self) {
        let marker = self.path.with_file_name(SAVED_MARKER);
        if !marker.exists() {
            let _ = fs::write(marker, b"");
        }
    }

    /// Waits briefly for the backup drive to get the latest state (on quit).
    pub fn flush_backup(&self) {
        self.backup.flush(Duration::from_secs(3));
    }

    pub fn save(&self, tasks: &[Task]) -> Result<(), String> {
        if let Some(e) = self.error.lock().unwrap().as_ref() {
            return Err(format!("Не сохранено. {e}"));
        }
        let json = to_json(tasks)?;
        let tmp = self.path.with_extension("json.tmp");
        {
            let unsaved = |e: io::Error| errors::file(&e, NOT_SAVED);
            let mut f = fs::File::create(&tmp).map_err(unsaved)?;
            f.write_all(json.as_bytes()).map_err(unsaved)?;
            f.sync_all().map_err(unsaved)?; // survive a power cut
        }
        let mut last = None;
        for i in 0..5 {
            if i > 0 {
                thread::sleep(Duration::from_millis(100));
            }
            match fs::rename(&tmp, &self.path) {
                Ok(()) => {
                    self.backup.push(json);
                    self.mark_used();
                    return Ok(());
                }
                Err(e) => last = Some(e), // file briefly held by AV/indexer
            }
        }
        Err(last.map_or_else(|| NOT_SAVED.to_string(), |e| errors::file(&e, NOT_SAVED)))
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
        _ => Err(if field == "time" { "Не понял время" } else { "Не понял дату" }.to_string()),
    }
}

fn clean_title(s: &str) -> Result<String, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("Задача пустая — напишите хоть слово".into());
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
    important: Option<bool>,
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
        important: important.unwrap_or(false),
        subtasks: Vec::new(),
    };
    normalize(&mut task);
    mutate(&app, &store, |tasks| {
        tasks.push(task.clone());
        Ok(task)
    })
}

/// Applies a partial update. Allowed keys: title, due, time, repeat, done,
/// order, important, subtasks (the whole list); `null` clears an optional field.
#[tauri::command]
pub fn update_task(
    app: AppHandle,
    store: State<Store>,
    id: String,
    patch: Map<String, Value>,
) -> Result<(), String> {
    mutate(&app, &store, |tasks| {
        let task = tasks.iter_mut().find(|t| t.id == id).ok_or(GONE)?;
        let mut next = task.clone();
        for (key, v) in &patch {
            match key.as_str() {
                "title" => next.title = clean_title(v.as_str().ok_or(NOT_CHANGED)?)?,
                "due" => next.due = opt_string(v, valid_date, "due")?,
                "time" => next.time = opt_string(v, valid_time, "time")?,
                "repeat" => next.repeat = Some(v.clone()),
                "order" => {
                    next.order = match v {
                        Value::Null => None,
                        _ => Some(v.as_f64().filter(|o| o.is_finite()).ok_or(NOT_CHANGED)?),
                    }
                }
                "done" => {
                    next.done = v.as_bool().ok_or(NOT_CHANGED)?;
                    next.done_at = if next.done { Some(now_ms()) } else { None };
                }
                "important" => next.important = v.as_bool().ok_or(NOT_CHANGED)?,
                "subtasks" => next.subtasks = clean_subtasks(v)?,
                _ => return Err(NOT_CHANGED.into()),
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
        let pos = tasks.iter().position(|t| t.id == id).ok_or(GONE)?;
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
    task.subtasks = clean_subtasks(&serde_json::to_value(&task.subtasks).unwrap_or(Value::Null))?;
    normalize(&mut task);
    mutate(&app, &store, |tasks| {
        if tasks.iter().any(|t| t.id == task.id) {
            return Ok(()); // already back (a double undo)
        }
        tasks.push(task);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_bad_record_costs_only_itself() {
        let file = br#"{"tasks":[
            {"id":"a","title":"ok","created":1},
            {"title":"no id or created"},
            {"id":"c","created":3},
            "junk"
        ]}"#;
        let (tasks, mended) = parse_tasks(file).unwrap();
        let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["ok", "no id or created"]);
        assert_eq!(mended, 3);
        assert!(!tasks[1].id.is_empty());
        assert!(parse_tasks(b"not json").is_err());
    }

    #[test]
    fn subtasks_are_checked() {
        let ok = serde_json::json!([{"id":"a","title":"  купить  ","done":true},{"id":"b","title":"позвонить"}]);
        let list = clean_subtasks(&ok).unwrap();
        assert_eq!(list[0], Subtask { id: "a".into(), title: "купить".into(), done: true });
        assert!(!list[1].done);
        assert!(clean_subtasks(&serde_json::json!([{"id":"a","title":"x"},{"id":"a","title":"y"}])).is_err());
        assert!(clean_subtasks(&serde_json::json!([{"id":"a","title":"  "}])).is_err());
        assert!(clean_subtasks(&serde_json::json!([{"id":"","title":"x"}])).is_err());
        assert!(clean_subtasks(&serde_json::json!("junk")).is_err());
        let many: Vec<Value> = (0..=MAX_SUBTASKS).map(|i| serde_json::json!({"id": i.to_string(), "title": "x"})).collect();
        assert!(clean_subtasks(&Value::Array(many)).is_err());
    }

    #[test]
    fn old_tasks_have_no_flag_and_no_subtasks() {
        let (tasks, mended) = parse_tasks(br#"{"tasks":[{"id":"a","title":"t","created":1}]}"#).unwrap();
        assert_eq!(mended, 0);
        assert!(!tasks[0].important && tasks[0].subtasks.is_empty());
    }
}