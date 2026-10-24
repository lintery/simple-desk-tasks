//! Updates from GitHub releases. A newer release's DeskTasks.exe is downloaded,
//! its minisign signature checked against the key built into the app, and put
//! in place of the running exe (which Windows lets us rename); the app then
//! restarts into it. Releases are made with tools/release.ps1.
//!
//! A new version that doesn't come up is rolled back: until it has run for a
//! while (`confirm`), a marker next to the exe says it's on trial, and the next
//! start that finds the marker (and the trial process gone) puts the old exe
//! back and skips that version.

use crate::desktop::{finish, APP_ID};
use minisign_verify::{PublicKey, Signature};
use serde::Serialize;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};
use tauri_winrt_notification::Toast;
use windows::{
    core::{HSTRING, PWSTR},
    Foundation::Uri,
    Storage::Streams::DataReader,
    Web::Http::HttpClient,
    Win32::{
        Foundation::{CloseHandle, STILL_ACTIVE, WAIT_OBJECT_0},
        System::{
            Threading::{
                GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            },
            WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
        },
    },
};

const RELEASES: &str = "https://api.github.com/repos/lintery/simple-desk-tasks/releases/latest";
const ASSET: &str = "DeskTasks.exe";
const PUBLIC_KEY: &str = include_str!("../updater.pub");
const AFTER_UPDATE: &str = "--after-update";
const FIRST_CHECK: Duration = Duration::from_secs(120);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// A downloaded update waits this long between tries while the user is busy.
const RETRY_INSTALL: Duration = Duration::from_secs(60);
/// A new version that has run this long works.
const TRIAL: Duration = Duration::from_secs(20);
const API_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Next to the exe: the version on trial and its process id.
const TRIAL_FILE: &str = "DeskTasks.trial";
/// Next to the exe: a version that was rolled back and isn't installed again.
const SKIP_FILE: &str = "DeskTasks.skip";

#[derive(Serialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub current: String,
    pub latest: Option<String>,
    /// "idle" | "checking" | "uptodate" | "ready" | "error" | "unsupported"
    pub state: String,
    pub message: Option<String>,
    /// When this status was reached (unix ms), 0 = not yet.
    pub checked_at: i64,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);
/// A verified update waiting to be installed.
static READY: Mutex<Option<Ready>> = Mutex::new(None);

#[derive(Clone)]
struct Ready {
    version: String,
    /// DeskTasks.new.exe next to the exe.
    file: PathBuf,
    /// Its signature, checked again right before the swap.
    sig: Vec<u8>,
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn set_status(app: &AppHandle, state: &str, latest: Option<String>, message: Option<String>) -> Status {
    let status = Status { current: current_version(), latest, state: state.to_string(), message, checked_at: crate::store::now_ms() };
    *STATUS.lock().unwrap() = Some(status.clone());
    let _ = app.emit("update-status", &status);
    status
}

/// The installed copy (DeskTasks.exe) updates itself; a dev build doesn't.
fn installed_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(ASSET)).then_some(exe)
}

/// `a` is a newer version than `b` ("1.10.0" > "1.9.2"; a leading "v" is ignored).
fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> { v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    let (a, b) = (parse(a), parse(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

fn get(url: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let fetch = || -> windows::core::Result<Vec<u8>> {
        let client = HttpClient::new()?;
        let headers = client.DefaultRequestHeaders()?;
        headers.UserAgent()?.TryParseAdd(&HSTRING::from("SimpleDeskTasks-updater"))?;
        headers.TryAppendWithoutValidation(&HSTRING::from("Accept"), &HSTRING::from("application/vnd.github+json, */*"))?;
        let buffer = finish(client.GetBufferAsync(&Uri::CreateUri(&HSTRING::from(url))?)?, timeout)?;
        let mut bytes = vec![0u8; buffer.Length()? as usize];
        DataReader::FromBuffer(&buffer)?.ReadBytes(&mut bytes)?;
        Ok(bytes)
    };
    fetch().map_err(|e| e.message().to_string())
}

/// Latest release: its version and the download links of the exe and its signature.
fn latest() -> Result<(String, String, String), String> {
    let json: serde_json::Value = serde_json::from_slice(&get(RELEASES, API_TIMEOUT)?).map_err(|e| e.to_string())?;
    let tag = json["tag_name"].as_str().ok_or("в релизе нет версии")?.trim_start_matches('v').to_string();
    let link = |name: &str| {
        json["assets"].as_array().into_iter().flatten().find(|a| a["name"] == name).and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_string)
    };
    let exe = link(ASSET).ok_or("в релизе нет DeskTasks.exe")?;
    let sig = link(&format!("{ASSET}.minisig")).ok_or("в релизе нет подписи")?;
    Ok((tag, exe, sig))
}

/// The exe is what the release key signed; returns the signature's trusted
/// comment ("file:DeskTasks.exe version:1.2.0").
fn verify(exe: &[u8], sig: &[u8]) -> Result<String, String> {
    let key = PublicKey::from_base64(PUBLIC_KEY.trim()).map_err(|e| e.to_string())?;
    let sig = Signature::decode(std::str::from_utf8(sig).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    key.verify(exe, &sig, false).map_err(|_| "подпись обновления не сходится".to_string())?;
    Ok(sig.trusted_comment().to_string())
}

/// The version signed along with the exe: the tag of a release isn't signed,
/// so an old exe put on a newer tag would otherwise install over and over.
fn signed_version(comment: &str) -> Option<&str> {
    comment.split_whitespace().find_map(|w| w.strip_prefix("version:")).map(|v| v.trim_start_matches('v'))
}

fn skipped(exe: &Path) -> Option<String> {
    fs::read_to_string(exe.with_file_name(SKIP_FILE)).ok().map(|s| s.trim().to_string())
}

/// Looks for a newer release and, if there is one, downloads and verifies it
/// (then it's READY). Runs on the caller's thread.
pub fn check(app: &AppHandle) -> Status {
    let Some(exe) = installed_exe() else {
        return set_status(app, "unsupported", None, Some("Обновляется только установленная копия".into()));
    };
    if BUSY.swap(true, Ordering::SeqCst) {
        return STATUS.lock().unwrap().clone().unwrap_or_default();
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            BUSY.store(false, Ordering::SeqCst);
        }
    }
    let _done = Done;
    // SAFETY: plain call; WinRT needs the thread initialized before use.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    if let Some(ready) = READY.lock().unwrap().clone() {
        return set_status(app, "ready", Some(ready.version), None);
    }
    set_status(app, "checking", None, None);
    let result = (|| -> Result<Option<String>, String> {
        let (tag, exe_url, sig_url) = latest()?;
        if !newer(&tag, &current_version()) {
            return Ok(None);
        }
        if skipped(&exe).as_deref() == Some(tag.as_str()) {
            return Err(format!("версия {tag} не запустилась и была откачена, ждём следующую"));
        }
        let (bytes, sig) = (get(&exe_url, DOWNLOAD_TIMEOUT)?, get(&sig_url, API_TIMEOUT)?);
        let comment = verify(&bytes, &sig)?;
        let version = signed_version(&comment).ok_or("в подписи обновления нет версии")?.to_string();
        if version != tag || !newer(&version, &current_version()) {
            return Err(format!("подписана версия {version}, а релиз — {tag}"));
        }
        let file = exe.with_file_name("DeskTasks.new.exe");
        let write = || -> std::io::Result<()> {
            let mut f = fs::File::create(&file)?;
            f.write_all(&bytes)?;
            f.sync_all()
        };
        write().map_err(|e| e.to_string())?;
        *READY.lock().unwrap() = Some(Ready { version: version.clone(), file, sig });
        Ok(Some(version))
    })();
    match result {
        Ok(Some(version)) => set_status(app, "ready", Some(version), None),
        Ok(None) => set_status(app, "uptodate", Some(current_version()), None),
        Err(e) => set_status(app, "error", None, Some(e)),
    }
}

/// Puts the downloaded update in place and restarts into it. A failed try
/// drops the download, so the next check fetches it afresh.
pub fn install(app: &AppHandle) -> Result<(), String> {
    let ready = READY.lock().unwrap().take().ok_or("нет скачанного обновления")?;
    let result = swap_and_restart(app, &ready);
    if let Err(e) = &result {
        let _ = fs::remove_file(&ready.file);
        set_status(app, "error", None, Some(e.clone()));
    }
    result
}

fn swap_and_restart(app: &AppHandle, ready: &Ready) -> Result<(), String> {
    let exe = installed_exe().ok_or("обновляется только установленная копия")?;
    // The file sat on disk since the download: check it once more.
    let bytes = fs::read(&ready.file).map_err(|e| e.to_string())?;
    verify(&bytes, &ready.sig)?;
    let old = exe.with_file_name("DeskTasks.old.exe");
    let trial = exe.with_file_name(TRIAL_FILE);
    let _ = fs::remove_file(&old);
    fs::rename(&exe, &old).map_err(|e| e.to_string())?;
    // Back to the running version; if even that fails, say so — the app wouldn't start next time.
    let undo = |why: String| -> String {
        let _ = fs::remove_file(&trial);
        match fs::rename(&old, &exe) {
            Ok(()) => why,
            Err(e) => format!("{why}; не удалось вернуть DeskTasks.exe ({e}) — он лежит рядом как DeskTasks.old.exe"),
        }
    };
    if let Err(e) = fs::rename(&ready.file, &exe) {
        return Err(undo(e.to_string()));
    }
    if let Err(e) = Trial::new(&ready.version).save(&trial) {
        let _ = fs::rename(&exe, &ready.file);
        return Err(undo(e.to_string()));
    }
    app.state::<crate::layout::Layout>().flush();
    app.state::<crate::store::Store>().flush_backup();
    match Command::new(&exe).args([AFTER_UPDATE, &std::process::id().to_string(), &ready.version]).spawn() {
        // Its pid right away: a launch meanwhile sees the trial as running.
        Ok(child) => {
            let _ = Trial { pid: Some(child.id()), ..Trial::new(&ready.version) }.save(&trial);
        }
        Err(e) => {
            let _ = fs::rename(&exe, &ready.file);
            return Err(undo(e.to_string()));
        }
    }
    app.exit(0);
    Ok(())
}

/// DeskTasks.trial: the version on trial, the process running it, and how
/// many starts didn't get as far as confirming it.
#[derive(Debug, PartialEq)]
struct Trial {
    version: String,
    pid: Option<u32>,
    failed: u32,
}

impl Trial {
    fn new(version: &str) -> Trial {
        Trial { version: version.to_string(), pid: None, failed: 0 }
    }

    fn parse(text: &str) -> Trial {
        let mut lines = text.lines().map(str::trim);
        Trial {
            version: lines.next().unwrap_or("").to_string(),
            pid: lines.next().and_then(|p| p.parse().ok()),
            failed: lines.next().and_then(|n| n.parse().ok()).unwrap_or(0),
        }
    }

    fn load(path: &Path) -> Option<Trial> {
        fs::read_to_string(path).ok().map(|t| Trial::parse(&t))
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        fs::write(path, format!("{}\n{}\n{}", self.version, self.pid.map_or(String::new(), |p| p.to_string()), self.failed))
    }
}

/// Process `pid` is running `exe` (not some other program that got the pid).
fn runs(pid: u32, exe: &Path) -> bool {
    // SAFETY: a handle we close; the buffer outlives the call.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let running = GetExitCodeProcess(process, &mut code).is_ok()
            && code == STILL_ACTIVE.0 as u32
            && QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(process);
        running && String::from_utf16_lossy(&buf[..len as usize]).eq_ignore_ascii_case(&exe.to_string_lossy())
    }
}

/// First thing in `main`, before the single-instance check.
///
/// Started by `install`: waits for the old process to quit (the check would
/// hand over to it otherwise), notes itself as the version on trial and
/// returns that version. Otherwise, a version on trial that twice didn't get
/// as far as `confirm` (it crashes on start) is rolled back: the old exe goes
/// back in place, that version is skipped, and the app restarts.
pub fn startup() -> Option<String> {
    let exe = installed_exe()?;
    let path = exe.with_file_name(TRIAL_FILE);
    let args: Vec<String> = std::env::args().collect();
    if let Some(at) = args.iter().position(|a| a == AFTER_UPDATE) {
        if let Some(pid) = args.get(at + 1).and_then(|p| p.parse::<u32>().ok()) {
            // SAFETY: a handle we close; waiting at most a minute.
            unsafe {
                if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                    let _ = WaitForSingleObject(process, 60_000) == WAIT_OBJECT_0;
                    let _ = CloseHandle(process);
                }
            }
        }
        let version = args.get(at + 2).cloned();
        if let Some(mut trial) = Trial::load(&path) {
            trial.pid = Some(std::process::id());
            let _ = trial.save(&path);
        }
        return version;
    }
    let mut trial = Trial::load(&path)?;
    let old = exe.with_file_name("DeskTasks.old.exe");
    // Not the version on trial any more (installed by hand), or nothing to go back to.
    if trial.version != current_version() || !old.exists() {
        let _ = fs::remove_file(&path);
        return None;
    }
    if trial.pid.is_some_and(|pid| runs(pid, &exe)) {
        return None; // the trial runs; this start is a second instance
    }
    // One start that didn't confirm may just have been quit early: try again.
    if trial.failed == 0 {
        trial.failed = 1;
        trial.pid = Some(std::process::id());
        let _ = trial.save(&path);
        return None;
    }
    let bad = exe.with_file_name("DeskTasks.bad.exe");
    let _ = fs::remove_file(&bad);
    if fs::rename(&exe, &bad).is_ok() {
        if fs::rename(&old, &exe).is_ok() {
            let _ = fs::write(exe.with_file_name(SKIP_FILE), &trial.version);
            let _ = fs::remove_file(&path);
            if Command::new(&exe).spawn().is_ok() {
                std::process::exit(0);
            }
        } else {
            let _ = fs::rename(&bad, &exe);
        }
    }
    None
}

/// This version works: it's kept, and the one it replaced is deleted.
/// After setup (`confirm`) and on a clean quit.
pub fn keep() {
    let Some(exe) = installed_exe() else { return };
    let path = exe.with_file_name(TRIAL_FILE);
    if let Some(trial) = Trial::load(&path) {
        if trial.version != current_version() {
            return;
        }
        let _ = fs::remove_file(&path);
    }
    let _ = fs::remove_file(exe.with_file_name("DeskTasks.bad.exe"));
    let old = exe.with_file_name("DeskTasks.old.exe");
    for _ in 0..20 {
        if fs::remove_file(&old).is_ok() || !old.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// After setup: once this version has run for a while, it's kept.
pub fn confirm() {
    thread::spawn(|| {
        thread::sleep(TRIAL);
        keep();
    });
}

pub fn announce_updated(version: &str) {
    let _ = Toast::new(APP_ID).title("Задачи обновлены").text1(&format!("Версия {version}")).show();
}

/// Restarting now would lose what the user is doing: a focus session, text
/// typed in a window of the app, the settings being changed.
fn user_busy(app: &AppHandle) -> bool {
    crate::focus::busy(app)
        || app.webview_windows().values().any(|w| w.is_focused().unwrap_or(false))
        || app.get_webview_window(crate::SETTINGS).is_some()
        || app.get_webview_window(crate::QUICK).is_some_and(|w| w.is_visible().unwrap_or(false))
}

/// Background checks while «Обновлять автоматически» is on; a downloaded
/// update goes in when the user isn't in the middle of something.
pub fn spawn(app: AppHandle) {
    thread::spawn(move || {
        thread::sleep(FIRST_CHECK);
        loop {
            if crate::settings::get().auto_update {
                if READY.lock().unwrap().is_none() {
                    check(&app);
                }
                if READY.lock().unwrap().is_some() {
                    if user_busy(&app) {
                        thread::sleep(RETRY_INSTALL);
                        continue;
                    }
                    let _ = install(&app); // a failure shows in the status
                }
            }
            thread::sleep(CHECK_EVERY);
        }
    });
}

#[tauri::command]
pub fn update_status() -> Status {
    STATUS.lock().unwrap().clone().unwrap_or_else(|| Status {
        current: current_version(),
        state: if installed_exe().is_some() { "idle" } else { "unsupported" }.into(),
        ..Default::default()
    })
}

#[tauri::command]
pub async fn update_check(app: AppHandle) -> Status {
    tauri::async_runtime::spawn_blocking(move || check(&app)).await.unwrap_or_default()
}

#[tauri::command]
pub fn update_install(app: AppHandle) -> Result<(), String> {
    install(&app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("1.1.0", "1.0.0"));
        assert!(newer("v1.10.0", "1.9.9"));
        assert!(newer("2.0", "1.99.99"));
        assert!(!newer("1.0.0", "1.0.0"));
        assert!(!newer("1.0.0", "1.0.1"));
        assert!(newer("1.0.0.1", "1.0.0"));
    }

    #[test]
    fn built_in_key_parses() {
        assert!(PublicKey::from_base64(PUBLIC_KEY.trim()).is_ok());
    }

    /// The blob was signed with the release key by tools/signer.
    #[test]
    fn release_signatures_verify_and_tampering_fails() {
        let blob = include_bytes!("../testdata/update-blob.bin");
        let sig = include_bytes!("../testdata/update-blob.bin.minisig");
        assert!(verify(blob, sig).is_ok());
        let mut tampered = blob.to_vec();
        tampered[0] ^= 1;
        assert!(verify(&tampered, sig).is_err());
    }

    #[test]
    fn trial_file_round_trips() {
        let t = Trial { version: "1.2.0".into(), pid: Some(4242), failed: 1 };
        let dir = std::env::temp_dir().join(format!("sdt-trial-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(TRIAL_FILE);
        t.save(&path).unwrap();
        assert_eq!(Trial::load(&path), Some(t));
        let _ = fs::remove_dir_all(&dir);
        // As the old process writes it before the new one starts.
        assert_eq!(Trial::parse("1.2.0\n\n0"), Trial::new("1.2.0"));
    }

    #[test]
    fn version_comes_from_the_signed_comment() {
        assert_eq!(signed_version("file:DeskTasks.exe version:1.2.0"), Some("1.2.0"));
        assert_eq!(signed_version("version:v2.0 file:DeskTasks.exe"), Some("2.0"));
        assert_eq!(signed_version("file:DeskTasks.exe"), None);
    }
}
