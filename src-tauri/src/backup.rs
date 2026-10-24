//! Copy of tasks.json on another drive: one snapshot per day holding that day's
//! last state, the newest `KEEP` days kept. Written off the UI path by one
//! worker, so a sleeping or missing drive never delays a save.
//!
//! When an app session first writes a day's snapshot and the one already there
//! isn't what this session started from, that one is kept as
//! `tasks-<date>.before-<HHMMSS>.json`, so a session that started from a wrong
//! or stale state can't silently replace the day's good copy.

use crate::desktop::APP_ID;
use chrono::NaiveDate;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, SystemTime},
};
use tauri_winrt_notification::Toast;

const DIR: &str = r"D:\backups\simple-desk-tasks";
const KEEP: usize = 30;
const PREFIX: &str = "tasks-";
const MAIN: &str = ".json";
const BEFORE: &str = ".before";
/// What Windows reports for a drive letter that doesn't exist at all.
const ERROR_INVALID_DRIVE: i32 = 15;

enum Msg {
    Save { json: String, only_if_absent: bool },
    Flush(mpsc::Sender<()>),
}

pub struct Backup {
    tx: mpsc::Sender<Msg>,
}

impl Backup {
    pub fn start() -> Self {
        Self::start_in(PathBuf::from(DIR), true)
    }

    fn start_in(dir: PathBuf, toasts: bool) -> Self {
        let (tx, rx) = mpsc::channel::<Msg>();
        thread::spawn(move || {
            let mut worker = Worker { dir, baseline: None, written: None, failing: false, toasts };
            while let Ok(first) = rx.recv() {
                // Only the newest state matters; drop ones superseded meanwhile.
                let mut pending: Option<(String, bool)> = None;
                let mut acks = Vec::new();
                let mut next = Some(first);
                while let Some(msg) = next {
                    match msg {
                        Msg::Save { json, only_if_absent } => {
                            // The first state queued is what this session started from.
                            worker.baseline.get_or_insert_with(|| json.clone());
                            let keep_existing = only_if_absent && pending.as_ref().is_none_or(|p| p.1);
                            pending = Some((json, keep_existing));
                        }
                        Msg::Flush(ack) => acks.push(ack),
                    }
                    next = rx.try_recv().ok();
                }
                if let Some((json, only_if_absent)) = pending {
                    worker.write(&json, only_if_absent);
                }
                for ack in acks {
                    let _ = ack.send(());
                }
            }
        });
        Backup { tx }
    }

    /// Queues the full tasks.json contents after a change.
    pub fn push(&self, json: String) {
        let _ = self.tx.send(Msg::Save { json, only_if_absent: false });
    }

    /// Makes sure today has a snapshot, without replacing one already there.
    pub fn push_initial(&self, json: String) {
        let _ = self.tx.send(Msg::Save { json, only_if_absent: true });
    }

    /// Waits, at most `timeout`, until everything queued so far is written.
    pub fn flush(&self, timeout: Duration) {
        let (ack, done) = mpsc::channel();
        if self.tx.send(Msg::Flush(ack)).is_ok() {
            let _ = done.recv_timeout(timeout);
        }
    }
}

struct Worker {
    dir: PathBuf,
    /// The tasks.json contents this session started from.
    baseline: Option<String>,
    /// The day this session has already written a snapshot for.
    written: Option<String>,
    failing: bool,
    toasts: bool,
}

impl Worker {
    fn write(&mut self, json: &str, only_if_absent: bool) {
        let now = chrono::Local::now();
        let date = now.format("%Y-%m-%d").to_string();
        let owned = self.written.as_deref() == Some(date.as_str());
        let stamp = now.format("%H%M%S").to_string();
        let session = Session { baseline: self.baseline.as_deref(), owned, stamp: &stamp };
        match write_snapshot(&self.dir, &date, json, only_if_absent, &session) {
            Ok(wrote) => {
                if wrote {
                    self.written = Some(date);
                }
                self.failing = false;
            }
            Err(e) => {
                eprintln!("backup to {:?} failed: {e}", self.dir);
                // Once per failure streak, so a missing drive doesn't toast on every edit.
                if !self.failing && self.toasts {
                    let _ = Toast::new(APP_ID)
                        .title("Бэкап задач не записался")
                        .text1(&format!("{}: {e}", self.dir.display()))
                        .show();
                }
                self.failing = true;
            }
        }
    }
}

/// Snapshots on the backup drive, newest first: each day's main copy, then its
/// `.before` copies newest first. No folder or no such drive means no
/// snapshots; any other error (drive not ready, access denied) is returned.
pub fn snapshots() -> io::Result<Vec<PathBuf>> {
    match snapshots_in(Path::new(DIR)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(ERROR_INVALID_DRIVE) => {
            Ok(Vec::new())
        }
        other => other,
    }
}

/// Date of a snapshot file and, for a `.before` copy, the `(HHMMSS, n)` from its
/// name: `tasks-<date>.json` or `tasks-<date>.before-<HHMMSS>[-n].json`.
fn parse_name(path: &Path) -> Option<(NaiveDate, Option<(u32, u32)>)> {
    let rest = path.file_name()?.to_str()?.strip_prefix(PREFIX)?.strip_suffix(MAIN)?;
    let date = rest.get(..10)?;
    let before = match &rest[10..] {
        "" => None,
        tail => {
            let stamp = tail.strip_prefix(BEFORE)?.strip_prefix('-')?;
            let (time, n) = match stamp.split_once('-') {
                Some((time, n)) => (time, n.parse().ok()?),
                None => (stamp, 1),
            };
            if time.len() != 6 || !time.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some((time.parse().ok()?, n))
        }
    };
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok().map(|d| (d, before))
}

fn snapshots_in(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found: Vec<(NaiveDate, Option<(u32, u32)>, Option<SystemTime>, PathBuf)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Some((date, before)) = parse_name(&path) {
            let modified = entry.metadata().and_then(|m| m.modified()).ok();
            found.push((date, before, modified, path));
        }
    }
    // Newest day first; within a day the main copy first, then the `.before`
    // copies by when their contents were written (a rename keeps that time),
    // newest first; equal times fall back to the name's time and number.
    found.sort_by(|a, b| {
        b.0.cmp(&a.0).then(a.1.is_some().cmp(&b.1.is_some())).then(b.2.cmp(&a.2)).then(b.1.cmp(&a.1))
    });
    Ok(found.into_iter().map(|(.., p)| p).collect())
}

fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            // Briefly held by an antivirus or sync tool.
            Err(_) if attempt < 4 => attempt += 1,
            Err(e) => return Err(e),
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// What a write needs to know about the app session doing it.
struct Session<'a> {
    /// The tasks.json contents the session started from.
    baseline: Option<&'a str>,
    /// The session already wrote today's snapshot, so it may simply replace it.
    owned: bool,
    /// Current time `HHMMSS`, names a set-aside copy.
    stamp: &'a str,
}

/// A name for a set-aside copy that no other file has.
fn before_name(dir: &Path, date: &str, stamp: &str) -> PathBuf {
    let mut path = dir.join(format!("{PREFIX}{date}{BEFORE}-{stamp}{MAIN}"));
    let mut n = 1;
    while path.exists() {
        n += 1;
        path = dir.join(format!("{PREFIX}{date}{BEFORE}-{stamp}-{n}{MAIN}"));
    }
    path
}

/// Writes the day's snapshot. One this session didn't write is kept as a
/// `.before` copy unless it's exactly what the session started from (a plain
/// restart). Returns `false` when `only_if_absent` and there already is one.
fn write_snapshot(dir: &Path, date: &str, json: &str, only_if_absent: bool, session: &Session) -> io::Result<bool> {
    fs::create_dir_all(dir)?;
    let file = dir.join(format!("{PREFIX}{date}{MAIN}"));
    let exists = match fs::metadata(&file) {
        Ok(_) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    if exists && only_if_absent {
        return Ok(false);
    }
    let tmp = file.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    if exists && !session.owned {
        // Unreadable counts as different: keep it rather than guess.
        let current = fs::read_to_string(&file).ok();
        let same = current.as_deref().is_some_and(|c| Some(c) == session.baseline || c == json);
        if !same {
            rename_retry(&file, &before_name(dir, date, session.stamp))?;
        }
    }
    rename_retry(&tmp, &file)?;
    if let Err(e) = prune(dir, &file) {
        eprintln!("pruning old backups in {dir:?} failed: {e}");
    }
    Ok(true)
}

/// Drops snapshots of all but the newest `KEEP` days, never the one just
/// written (with a clock set in the past it would look like the oldest).
fn prune(dir: &Path, keep: &Path) -> io::Result<()> {
    let all = snapshots_in(dir)?;
    let mut days: Vec<NaiveDate> = all.iter().filter_map(|p| parse_name(p).map(|k| k.0)).collect();
    days.dedup();
    let Some(&oldest_kept) = days.get(KEEP.saturating_sub(1)) else { return Ok(()) };
    for path in all {
        if path != keep && parse_name(&path).is_some_and(|(d, _)| d < oldest_kept) {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sdt-backup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn day(i: u64) -> String {
        let start = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        (start + chrono::Days::new(i)).format("%Y-%m-%d").to_string()
    }

    const DAY: &str = "2026-10-24";

    fn session<'a>(baseline: &'a str, owned: bool, stamp: &'a str) -> Session<'a> {
        Session { baseline: Some(baseline), owned, stamp }
    }

    fn read(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join(name)).unwrap()
    }

    /// Sets a file's modification time `secs` seconds into the past.
    fn age(dir: &Path, name: &str, secs: u64) {
        let file = fs::OpenOptions::new().write(true).open(dir.join(name)).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(secs)).unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        snapshots_in(dir).unwrap().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn same_session_replaces_its_own_snapshot() {
        let dir = temp_dir("same-day");
        assert!(write_snapshot(&dir, DAY, "first", false, &session("first", false, "090000")).unwrap());
        assert!(write_snapshot(&dir, DAY, "second", false, &session("first", true, "090100")).unwrap());
        assert_eq!(names(&dir), ["tasks-2026-10-24.json"]);
        assert_eq!(read(&dir, "tasks-2026-10-24.json"), "second");
        assert!(!dir.join("tasks-2026-10-24.json.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_restart_just_continues_the_days_snapshot() {
        let dir = temp_dir("restart");
        write_snapshot(&dir, DAY, "A", false, &session("A", false, "090000")).unwrap();
        // Next session loaded exactly what the last one left.
        assert!(!write_snapshot(&dir, DAY, "A", true, &session("A", false, "100000")).unwrap());
        write_snapshot(&dir, DAY, "B", false, &session("A", false, "100100")).unwrap();
        assert_eq!(names(&dir), ["tasks-2026-10-24.json"]);
        assert_eq!(read(&dir, "tasks-2026-10-24.json"), "B");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sessions_from_a_stale_state_never_lose_the_good_copy() {
        let dir = temp_dir("stale");
        write_snapshot(&dir, DAY, "good", false, &session("good", false, "090000")).unwrap();
        age(&dir, "tasks-2026-10-24.json", 300);
        // Two later sessions that both started from the same stale tasks.json.
        assert!(!write_snapshot(&dir, DAY, "stale", true, &session("stale", false, "100000")).unwrap());
        assert_eq!(read(&dir, "tasks-2026-10-24.json"), "good");
        write_snapshot(&dir, DAY, "stale+1", false, &session("stale", false, "100100")).unwrap();
        age(&dir, "tasks-2026-10-24.json", 200);
        write_snapshot(&dir, DAY, "stale+2", false, &session("stale", false, "110000")).unwrap();
        assert_eq!(
            names(&dir),
            ["tasks-2026-10-24.json", "tasks-2026-10-24.before-110000.json", "tasks-2026-10-24.before-100100.json"]
        );
        assert_eq!(read(&dir, "tasks-2026-10-24.before-100100.json"), "good");
        assert_eq!(read(&dir, "tasks-2026-10-24.before-110000.json"), "stale+1");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_aside_copies_get_unique_names() {
        let dir = temp_dir("unique");
        write_snapshot(&dir, DAY, "one", false, &session("x", false, "090000")).unwrap();
        write_snapshot(&dir, DAY, "two", false, &session("y", false, "100000")).unwrap();
        write_snapshot(&dir, DAY, "three", false, &session("z", false, "100000")).unwrap();
        assert_eq!(read(&dir, "tasks-2026-10-24.before-100000.json"), "one");
        assert_eq!(read(&dir, "tasks-2026-10-24.before-100000-2.json"), "two");
        assert_eq!(read(&dir, "tasks-2026-10-24.json"), "three");
        let first = "tasks-2026-10-24.before-100000.json";
        let second = "tasks-2026-10-24.before-100000-2.json";
        // Listed by when they were written, even against the name order...
        age(&dir, first, 200);
        age(&dir, second, 300);
        assert_eq!(names(&dir), ["tasks-2026-10-24.json", first, second]);
        // ...and by the name's time and number when the times are equal.
        let same = SystemTime::now() - Duration::from_secs(100);
        for name in [first, second] {
            fs::OpenOptions::new().write(true).open(dir.join(name)).unwrap().set_modified(same).unwrap();
        }
        assert_eq!(names(&dir), ["tasks-2026-10-24.json", second, first]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recognizes_only_snapshot_names() {
        let ok = |n: &str| parse_name(Path::new(n));
        assert_eq!(ok("tasks-2026-10-24.json").map(|k| k.1), Some(None));
        assert_eq!(ok("tasks-2026-10-24.before-103000.json").map(|k| k.1), Some(Some((103000, 1))));
        assert_eq!(ok("tasks-2026-10-24.before-103000-12.json").map(|k| k.1), Some(Some((103000, 12))));
        for bad in [
            "tasks-2026-10-24.json.tmp",
            "tasks-2026-10-24.before.json",
            "tasks-2026-10-24.before-.json",
            "tasks-2026-10-24.before-1030.json",
            "tasks-2026-10-24.before-103000-.json",
            "tasks-2026-10-24.before-103000-2-3.json",
            "tasks-2026-10-24.copy.json",
            "tasks-2026-1-5.json",
            "tasks-notes.json",
            "readme.txt",
        ] {
            assert_eq!(ok(bad), None, "{bad}");
        }
    }

    #[test]
    fn keeps_newest_days_and_ignores_foreign_files() {
        let dir = temp_dir("prune");
        fs::create_dir_all(&dir).unwrap();
        let foreign = ["tasks-notes.json", "tasks-2026-1-5.json", "readme.txt"];
        for f in foreign {
            fs::write(dir.join(f), "x").unwrap();
        }
        let old_before = format!("tasks-{}.before-120000.json", day(0));
        fs::write(dir.join(&old_before), "x").unwrap();
        for i in 0..(KEEP as u64 + 5) {
            write_snapshot(&dir, &day(i), "{}", false, &session("{}", false, "120000")).unwrap();
        }
        let all = snapshots_in(&dir).unwrap();
        assert_eq!(all.len(), KEEP);
        assert_eq!(parse_name(&all[0]).unwrap().0.format("%Y-%m-%d").to_string(), day(KEEP as u64 + 4));
        assert_eq!(parse_name(&all[KEEP - 1]).unwrap().0.format("%Y-%m-%d").to_string(), day(5));
        assert!(!dir.join(old_before).exists());
        for f in foreign {
            assert!(dir.join(f).exists());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clock_in_the_past_keeps_the_new_snapshot() {
        let dir = temp_dir("past");
        for i in 10..(10 + KEEP as u64) {
            write_snapshot(&dir, &day(i), "{}", false, &session("{}", false, "120000")).unwrap();
        }
        write_snapshot(&dir, &day(0), "now", false, &session("{}", false, "120000")).unwrap();
        assert_eq!(read(&dir, &format!("tasks-{}.json", day(0))), "now");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn worker_writes_the_newest_push_and_flushes() {
        let dir = temp_dir("worker");
        let backup = Backup::start_in(dir.clone(), false);
        backup.push_initial("initial".into());
        backup.push("old".into());
        backup.push("new".into());
        let started = Instant::now();
        backup.flush(Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(5), "flush timed out");
        let all = snapshots_in(&dir).unwrap();
        assert_eq!(fs::read_to_string(&all[0]).unwrap(), "new");
        drop(backup);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_dir_is_an_error_here_but_empty_for_callers() {
        assert!(snapshots_in(&temp_dir("absent")).is_err());
    }
}
