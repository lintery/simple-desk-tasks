//! Recorded focus sounds: seamless loops of field recordings, published as the
//! sounds-v1 release (made with tools/sounds). Each is downloaded the first
//! time it's picked, checked against the SHA-256 built in here and kept in the
//! app's local data folder; the focus page plays it from the bytes it gets back.

use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::Path,
    sync::Mutex,
    time::Duration,
};
use tauri::{ipc::Response, AppHandle, Manager};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

const BASE: &str = "https://github.com/lintery/simple-desk-tasks/releases/download/sounds-v1/";
const TIMEOUT: Duration = Duration::from_secs(60);

/// Sound id (as in ui/lib/sounds.js) and the SHA-256 of its loop, `<id>.opus`.
const RECORDINGS: [(&str, &str); 14] = [
    ("rain-light", "d629837ae986eceb2e2ee444a48d63bc2dd4bf310802e1350b006167a24fc1d0"),
    ("rain", "723a8e7e21b23a3a1fc2e49cb6a0584ab015b283ce7dcb8c55b1098a96f7546d"),
    ("rain-heavy", "0820f351d8ae9afe7de46524811d3f4234007ff75d8436fd2d549fe7729ed395"),
    ("rain-wind", "12c46bfc9b2af0d04d0d5d14cd721fbc2ccca1dca2c8d5e5a248bd90f0647e5b"),
    ("rain-thunder", "687807ea046ebc60a9927ff9ff4abcc9289ce1552c2e6c4e5e0df677b89c66a8"),
    ("rain-roof", "3653ef93764c95e77b40581f7a04d69dd8c1e37ce4df5ac69efac3f0cf70ad83"),
    ("waves", "f65602e6ad8ce3c5174fb6c195817dfa2a089cd662ab4da0c1d4615925f24178"),
    ("fire", "72a20dfe5673a74da01b43772d54beb678041db086a7cd4805f020f604bf0796"),
    ("wind", "89eb232dc8b153fd9e2f5a762943e9c41faef05ac150a7e8f7f6a126de38a879"),
    ("stream", "b3b8781093df7648daf9a0decccfd83706bcc93d7ad90a90a5b13386e3f21591"),
    ("night", "1ca34f259e361e9b107d663b2b583b30935774896d2563441bfd08ac7f261fc4"),
    ("forest", "f6a5ccfc50ee1bf7992e960700afd0cbaca541194c0f9b988d4c7ba9813d15c2"),
    ("fan", "21632013d75f9aee6b8914d2513e917bab65483ae9a1e2a47bf2c93873e2b32c"),
    ("train", "b95a6f648fa7a8effb89d2f81b2f45341a494438bcbd366a4a4d53d2529cad0e"),
];

/// One download at a time (a sound picked twice quickly is fetched once).
static FETCHING: Mutex<()> = Mutex::new(());

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The loop as kept in `dir`, if it's there and intact.
fn cached(dir: &Path, id: &str, sha: &str) -> Option<Vec<u8>> {
    fs::read(dir.join(format!("{id}.opus"))).ok().filter(|b| sha256_hex(b) == sha)
}

/// Downloads a loop with `fetch`, checks it and keeps it in `dir` (a failed
/// save only means it's fetched again next time).
fn fetch_and_keep(dir: &Path, id: &str, sha: &str, fetch: impl FnOnce(&str) -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    let bytes = fetch(&format!("{BASE}{id}.opus"))?;
    if sha256_hex(&bytes) != sha {
        return Err("скачанный звук не сходится с ожидаемым".into());
    }
    let path = dir.join(format!("{id}.opus"));
    let tmp = dir.join(format!("{id}.opus.tmp"));
    let save = || -> std::io::Result<()> {
        fs::create_dir_all(dir)?;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, &path)
    };
    if save().is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(bytes)
}

fn load(dir: &Path, id: &str, fetch: impl FnOnce(&str) -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    let sha = RECORDINGS.iter().find(|(i, _)| *i == id).map(|(_, s)| *s).ok_or("для этого звука нет записи")?;
    if let Some(bytes) = cached(dir, id, sha) {
        return Ok(bytes);
    }
    let _one = FETCHING.lock().unwrap_or_else(|e| e.into_inner());
    // It may have arrived while we waited for another download.
    match cached(dir, id, sha) {
        Some(bytes) => Ok(bytes),
        None => fetch_and_keep(dir, id, sha, fetch),
    }
}

/// The loop of a recorded sound (Opus in Ogg), from the disk or downloaded.
#[tauri::command]
pub async fn sound_recording(app: AppHandle, id: String) -> Result<Response, String> {
    // Local, not roaming: they're re-downloadable and shouldn't travel with a profile.
    let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?.join("sounds");
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        // SAFETY: plain call; WinRT needs the thread initialized before use.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        load(&dir, &id, |url| crate::net::get(url, TIMEOUT).map_err(|e| format!("Не удалось скачать звук: {e}")))
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(Response::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sdt-sounds-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn every_recording_is_a_sound_of_the_focus_page() {
        let page = include_str!("../../ui/lib/sounds.js");
        for (id, sha) in RECORDINGS {
            assert!(page.contains(&format!("id: '{id}'")), "{id}");
            assert_eq!(sha.len(), 64, "{id}");
        }
    }

    #[test]
    fn downloads_once_then_reads_the_kept_copy() {
        let dir = temp_dir("once");
        let body = b"loop".to_vec();
        let sha = sha256_hex(&body);
        let calls = Cell::new(0);
        let fetch = |url: &str| {
            calls.set(calls.get() + 1);
            assert_eq!(url, format!("{BASE}fan.opus"));
            Ok(body.clone())
        };
        assert_eq!(fetch_and_keep(&dir, "fan", &sha, fetch), Ok(body.clone()));
        assert_eq!(cached(&dir, "fan", &sha), Some(body.clone()));
        assert_eq!(calls.get(), 1);
        // A damaged copy isn't used.
        fs::write(dir.join("fan.opus"), b"lo0p").unwrap();
        assert_eq!(cached(&dir, "fan", &sha), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_download_that_doesnt_match_is_refused_and_not_kept() {
        let dir = temp_dir("bad");
        let sha = sha256_hex(b"expected");
        assert!(fetch_and_keep(&dir, "fan", &sha, |_| Ok(b"something else".to_vec())).is_err());
        assert!(!dir.join("fan.opus").exists());
        assert!(load(&dir, "white", |_| Ok(Vec::new())).is_err()); // noise is synthesized, never fetched
        let _ = fs::remove_dir_all(&dir);
    }
}
