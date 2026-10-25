//! The move from the old name (Desk Tasks) to Simple Desk Widgets: what the
//! app kept under the old name comes along, nothing is lost.
//!
//! Everything is copied, never moved: if a new version fails and is rolled
//! back to one from before the rename, that one still finds its data where
//! it left it — and whatever it changes there comes over again next time.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};
use windows::{
    core::HSTRING,
    Win32::{
        Foundation::CloseHandle,
        System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS},
        UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK},
    },
};

/// The app's identifier before the rename (tauri.conf.json): its data folders
/// in %APPDATA% and %LOCALAPPDATA%.
const OLD_IDENTIFIER: &str = "app.desktasks.widgets";
/// The identifier now (must match tauri.conf.json).
const IDENTIFIER: &str = "app.simpledeskwidgets";
/// In a new folder: the old one's data is in; it holds the newest change time
/// of what was copied (unix ms).
const MARKER: &str = ".copied-from-desk-tasks";
/// Of the local folder only the downloaded sounds are worth keeping (the rest
/// is the web view's cache and a log).
const LOCAL_KEPT: [&str; 1] = ["sounds"];
/// The old app's exe, as installed and as built.
const OLD_EXES: [&str; 2] = ["DeskTasks.exe", "desk-tasks.exe"];

/// Before anything opens the data folders. The old data comes over the first
/// time, and again if the old version has written to it since (after a
/// rollback). The old app must not be running meanwhile: what it saves after
/// the copy would be lost.
pub fn data_folders() {
    let (Some(roaming), Some(local)) = (std::env::var_os("APPDATA"), std::env::var_os("LOCALAPPDATA")) else { return };
    let (old, new) = (Path::new(&roaming).join(OLD_IDENTIFIER), Path::new(&roaming).join(IDENTIFIER));
    if !old.is_dir() || !stale(&old, &new.join(MARKER)) {
        return;
    }
    if old_app_running() {
        stop("Сначала закройте старую версию (Desk Tasks) в трее, потом запустите Simple Desk Widgets снова: она перенесёт ваши задачи и настройки.");
    }
    let newest = newest(&old).unwrap_or(0);
    if copy_dir(&old, &new, &[]).is_err() || fs::write(new.join(MARKER), newest.to_string()).is_err() {
        stop("Не получилось перенести задачи и настройки из прежней версии (Desk Tasks). Проверьте, что на диске есть место, и запустите приложение снова.");
    }
    let (old, new) = (Path::new(&local).join(OLD_IDENTIFIER), Path::new(&local).join(IDENTIFIER));
    if old.is_dir() {
        let _ = copy_dir(&old, &new, &LOCAL_KEPT);
    }
}

/// The old data isn't in yet, or changed after it was copied (compared with
/// the old files' own times: a clock gone wrong once doesn't copy again and again).
fn stale(old: &Path, marker: &Path) -> bool {
    let Some(copied) = fs::read_to_string(marker).ok().and_then(|s| s.trim().parse::<u128>().ok()) else { return true };
    newest(old).is_some_and(|t| t > copied)
}

/// The newest change of the files directly in `dir`, unix ms.
fn newest(dir: &Path) -> Option<u128> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .filter_map(|m| m.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .max()
}

/// Copies `from` into `to` (files over files), only the entries named in
/// `only` when it isn't empty. Any file that couldn't be copied is an error.
fn copy_dir(from: &Path, to: &Path, only: &[&str]) -> io::Result<()> {
    fs::create_dir_all(to)?;
    let mut result = Ok(());
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if !only.is_empty() && !only.iter().any(|o| name.to_string_lossy().eq_ignore_ascii_case(o)) {
            continue;
        }
        let target = to.join(&name);
        let copied = if entry.file_type()?.is_dir() { copy_dir(&entry.path(), &target, &[]) } else { fs::copy(entry.path(), &target).map(|_| ()) };
        if copied.is_err() {
            result = copied;
        }
    }
    result
}

/// Another process of the old exe. Not one of our own name: after an update
/// in place the new version runs as DeskTasks.exe itself (a second start of
/// it isn't the old app) — except the version this one replaced, named in
/// the update's arguments, if it still hasn't quit.
fn old_app_running() -> bool {
    let me = std::process::id();
    let own = std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
    let replaced = crate::updater::replaced_pid();
    // SAFETY: a snapshot handle we close; the entry is sized as the API asks.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut entry).is_ok();
        while ok && !found {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            let pid = entry.th32ProcessID;
            found = pid != me
                && OLD_EXES.iter().any(|e| name.eq_ignore_ascii_case(e))
                && (!name.eq_ignore_ascii_case(&own) || Some(pid) == replaced);
            ok = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Says why and quits: starting without the data would look like it's gone.
fn stop(text: &str) -> ! {
    // SAFETY: plain call with live strings.
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("Simple Desk Widgets"), MB_OK | MB_ICONWARNING);
    }
    std::process::exit(1);
}

/// The backup folder under its old default name: its snapshots come over to
/// the new default (once; the old folder is left as it is). The answer is
/// the folder to use from now on, if it changes.
pub fn backup_folder(dir: &str, old_default: &str, new_default: &str) -> Option<String> {
    let at_old = dir.eq_ignore_ascii_case(old_default);
    if !at_old && !dir.eq_ignore_ascii_case(new_default) {
        return None;
    }
    let (old, new) = (Path::new(old_default), PathBuf::from(new_default));
    if old.is_dir() && !new.join(MARKER).exists() {
        let copied = copy_missing(old, &new).and_then(|()| fs::write(new.join(MARKER), ""));
        if copied.is_err() {
            // Not all of it came over: stay with the old folder for now.
            return None;
        }
    }
    at_old.then(|| new_default.to_string())
}

/// The files of `from` that `to` doesn't have yet.
fn copy_missing(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_file() && !target.exists() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_is_the_configured_one() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], IDENTIFIER);
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("sdw-rename-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn backups_are_copied_to_the_new_folder() {
        let root = scratch("backup");
        let (old, new) = (root.join("simple-desk-tasks"), root.join("simple-desk-widgets"));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("tasks-2026-10-24.json"), "[old]").unwrap();
        fs::write(old.join("tasks-2026-10-25.json"), "[old]").unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("tasks-2026-10-25.json"), "[new]").unwrap();
        let (o, n) = (old.to_string_lossy().to_string(), new.to_string_lossy().to_string());
        assert_eq!(backup_folder(r"E:\elsewhere", &o, &n), None);
        assert_eq!(backup_folder(&o.to_uppercase(), &o, &n), Some(n.clone()));
        // Copied, not moved; what the new folder had stays.
        assert!(old.join("tasks-2026-10-24.json").exists());
        assert_eq!(fs::read_to_string(new.join("tasks-2026-10-24.json")).unwrap(), "[old]");
        assert_eq!(fs::read_to_string(new.join("tasks-2026-10-25.json")).unwrap(), "[new]");
        // Once: a snapshot pruned from the new folder doesn't come back.
        fs::remove_file(new.join("tasks-2026-10-24.json")).unwrap();
        assert_eq!(backup_folder(&n, &o, &n), None);
        assert!(!new.join("tasks-2026-10-24.json").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn settings_with_the_new_default_still_get_the_old_backups() {
        let root = scratch("default");
        let (old, new) = (root.join("old"), root.join("new"));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("tasks-2026-10-25.json"), "[]").unwrap();
        let (o, n) = (old.to_string_lossy().to_string(), new.to_string_lossy().to_string());
        assert_eq!(backup_folder(&n, &o, &n), None);
        assert!(new.join("tasks-2026-10-25.json").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn old_data_comes_over_again_after_a_rollback() {
        let root = scratch("data");
        let (old, new) = (root.join("old"), root.join("new"));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("tasks.json"), "[1]").unwrap();
        assert!(stale(&old, &new.join(MARKER)));
        copy_dir(&old, &new, &[]).unwrap();
        fs::write(new.join(MARKER), newest(&old).unwrap().to_string()).unwrap();
        assert!(!stale(&old, &new.join(MARKER)));
        assert_eq!(fs::read_to_string(new.join("tasks.json")).unwrap(), "[1]");
        // The old version ran again and saved.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(old.join("tasks.json"), "[1,2]").unwrap();
        assert!(stale(&old, &new.join(MARKER)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn of_the_local_folder_only_the_sounds() {
        let root = scratch("local");
        let (old, new) = (root.join("old"), root.join("new"));
        fs::create_dir_all(old.join("sounds")).unwrap();
        fs::create_dir_all(old.join("EBWebView")).unwrap();
        fs::write(old.join("sounds").join("rain.ogg"), "x").unwrap();
        fs::write(old.join("hotkey.log"), "x").unwrap();
        copy_dir(&old, &new, &LOCAL_KEPT).unwrap();
        assert!(new.join("sounds").join("rain.ogg").exists());
        assert!(!new.join("EBWebView").exists() && !new.join("hotkey.log").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
