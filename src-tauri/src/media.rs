//! What's playing, from Windows' media sessions (the source of the system media
//! flyout): Spotify first, otherwise any player that reports itself. Polled
//! once a second for the player widget, which is shown only while there's one.

use serde::Serialize;
use std::{sync::Mutex, thread, time::Duration};
use tauri::{AppHandle, Emitter, Manager};
use windows::{
    core::Result as WinResult,
    Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as SessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    },
    Storage::Streams::{DataReader, IRandomAccessStreamReference},
    Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
};

pub const LABEL: &str = "player";
/// Polls without a session before the widget hides (a track change can drop
/// the session for a moment).
const MISSES_TO_HIDE: u32 = 3;
/// Spotify publishes a new track's cover a little after its title.
const COVER_RETRIES: u32 = 4;
/// With nothing found for this many polls the session manager is asked for
/// again, in case the system's media host restarted under it.
const MISSES_TO_RECONNECT: u32 = 10;

#[derive(Serialize, Clone, PartialEq, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Media {
    pub app: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    /// Position at `updated` (unix ms); the page moves it on while playing.
    pub position: i64,
    /// 0 when the player doesn't report it.
    pub duration: i64,
    pub updated: i64,
    /// `data:` URL of the cover art.
    pub cover: Option<String>,
    pub can_prev: bool,
    pub can_next: bool,
}

/// What the widget shows, and which player it belongs to.
static SHOWN: Mutex<Option<(String, Media)>> = Mutex::new(None);

pub fn active() -> bool {
    SHOWN.lock().unwrap().is_some()
}

/// Friendly name for a session's app id (an AUMID or an exe name).
fn app_name(id: &str) -> String {
    let l = id.to_lowercase();
    let known = [
        ("spotify", "Spotify"),
        ("yandex.music", "Яндекс Музыка"),
        ("yandexmusic", "Яндекс Музыка"),
        ("yandex", "Яндекс"),
        ("chrome", "Chrome"),
        ("msedge", "Edge"),
        ("firefox", "Firefox"),
        ("zen", "Zen"),
        ("opera", "Opera"),
        ("vlc", "VLC"),
        ("zunemusic", "Медиаплеер"),
        ("telegram", "Telegram"),
    ];
    if let Some((_, name)) = known.iter().find(|(k, _)| l.contains(k)) {
        return (*name).to_string();
    }
    let tail = id.rsplit(['\\', '!', '/']).next().unwrap_or(id);
    tail.trim_end_matches(".exe").trim_end_matches(".EXE").to_string()
}

fn is_spotify(id: &str) -> bool {
    id.to_lowercase().contains("spotify")
}

fn playing(s: &Session) -> bool {
    s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).is_ok_and(|st| st == Status::Playing)
}

/// The session to show. The one already shown stays while it plays; otherwise
/// a playing one wins (Spotify first); with nothing playing, the shown one
/// stays, then Spotify, then whatever Windows considers current. Sessions in
/// `skip` (nothing to show) are passed over.
fn pick(manager: &SessionManager, shown: Option<&str>, skip: &[String]) -> Option<(Session, String)> {
    let mut all: Vec<(Session, String, bool)> = Vec::new();
    if let Ok(sessions) = manager.GetSessions() {
        for s in &sessions {
            let Ok(id) = s.SourceAppUserModelId().map(|h| h.to_string()) else { continue };
            if skip.contains(&id) {
                continue;
            }
            let on = playing(&s);
            all.push((s, id, on));
        }
    }
    let take = |i: usize, all: &mut Vec<(Session, String, bool)>| {
        let (s, id, _) = all.swap_remove(i);
        Some((s, id))
    };
    let find = |all: &Vec<(Session, String, bool)>, f: &dyn Fn(&str, bool) -> bool| {
        all.iter().position(|(_, id, on)| f(id, *on))
    };
    if let Some(i) = find(&all, &|id, on| on && Some(id) == shown) {
        return take(i, &mut all);
    }
    if let Some(i) = find(&all, &|id, on| on && is_spotify(id)).or_else(|| find(&all, &|_, on| on)) {
        return take(i, &mut all);
    }
    if let Some(i) = find(&all, &|id, _| Some(id) == shown).or_else(|| find(&all, &|id, _| is_spotify(id))) {
        return take(i, &mut all);
    }
    let current = manager.GetCurrentSession().ok()?;
    let id = current.SourceAppUserModelId().ok()?.to_string();
    (!skip.contains(&id)).then_some((current, id))
}

/// The session to show and what it plays; one without a title is passed over.
fn find(manager: &SessionManager, shown: Option<&str>, cover: &mut Cover) -> Option<(String, Media)> {
    let mut skip = Vec::new();
    for _ in 0..4 {
        let (session, id) = pick(manager, shown, &skip)?;
        match read(&session, &id, cover) {
            Ok(Some(media)) => return Some((id, media)),
            _ => skip.push(id),
        }
    }
    None
}

/// Windows file time (100 ns since 1601) to unix ms.
fn unix_ms(file_time: i64) -> i64 {
    (file_time - 116_444_736_000_000_000) / 10_000
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn read_cover(thumb: &IRandomAccessStreamReference) -> Option<String> {
    let stream = thumb.OpenReadAsync().ok()?.get().ok()?;
    let size = u32::try_from(stream.Size().ok()?).ok().filter(|s| *s > 0 && *s < 4_000_000)?;
    let kind = stream.ContentType().map(|s| s.to_string()).unwrap_or_default();
    let kind = if kind.starts_with("image/") { kind } else { "image/jpeg".to_string() };
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    reader.LoadAsync(size).ok()?.get().ok()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).ok()?;
    Some(format!("data:{kind};base64,{}", base64(&buf)))
}

#[derive(Default)]
struct Cover {
    track: String,
    art: Option<String>,
    retries: u32,
}

/// `None` when the session has nothing to show (no title).
fn read(session: &Session, id: &str, cover: &mut Cover) -> WinResult<Option<Media>> {
    let props = session.TryGetMediaPropertiesAsync()?.get()?;
    let title = props.Title()?.to_string();
    if title.is_empty() {
        return Ok(None);
    }
    let artist = props.Artist()?.to_string();
    let info = session.GetPlaybackInfo()?;
    let controls = info.Controls()?;
    let timeline = session.GetTimelineProperties()?;
    let start = timeline.StartTime()?.Duration;
    let end = timeline.EndTime()?.Duration;
    let track = format!("{id}\n{title}\n{artist}");
    if track != cover.track {
        *cover = Cover { track, art: None, retries: COVER_RETRIES };
    }
    if cover.retries > 0 {
        cover.retries -= 1;
        if let Some(art) = props.Thumbnail().ok().and_then(|t| read_cover(&t)) {
            cover.art = Some(art);
        }
    }
    Ok(Some(Media {
        app: app_name(id),
        title,
        artist,
        playing: info.PlaybackStatus()? == Status::Playing,
        position: (timeline.Position()?.Duration - start).max(0) / 10_000,
        duration: (end - start).max(0) / 10_000,
        updated: unix_ms(timeline.LastUpdatedTime()?.UniversalTime),
        cover: cover.art.clone(),
        can_prev: controls.IsPreviousEnabled().unwrap_or(false),
        can_next: controls.IsNextEnabled().unwrap_or(false),
    }))
}

/// Stores what to show and tells the widget.
fn publish(app: &AppHandle, next: Option<(String, Media)>) {
    let mut shown = SHOWN.lock().unwrap();
    if *shown == next {
        return;
    }
    *shown = next.clone();
    drop(shown);
    let _ = app.emit_to(LABEL, "media", next.as_ref().map(|(_, m)| m));
}

/// Shows the widget while there's something to show, hides it otherwise.
/// Checked every poll, so a stray show (e.g. «Показать виджеты») is undone.
fn sync_window(app: &AppHandle) {
    let want = active();
    if let Some(win) = app.get_webview_window(LABEL) {
        if win.is_visible().is_ok_and(|v| v != want) {
            let _ = if want { win.show() } else { win.hide() };
        }
    }
}

pub fn spawn(app: AppHandle) {
    thread::spawn(move || {
        // SAFETY: plain call; WinRT needs the thread initialized before use.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        let mut manager: Option<SessionManager> = None;
        let mut cover = Cover::default();
        let mut misses = 0;
        loop {
            if manager.is_none() {
                manager = SessionManager::RequestAsync().and_then(|op| op.get()).ok();
            }
            let shown_id = SHOWN.lock().unwrap().as_ref().map(|(id, _)| id.clone());
            match manager.as_ref().and_then(|m| find(m, shown_id.as_deref(), &mut cover)) {
                Some(now) => {
                    misses = 0;
                    publish(&app, Some(now));
                }
                None => {
                    misses += 1;
                    if misses >= MISSES_TO_HIDE {
                        publish(&app, None);
                    }
                    if misses % MISSES_TO_RECONNECT == 0 {
                        manager = None;
                    }
                }
            }
            sync_window(&app);
            thread::sleep(Duration::from_secs(1));
        }
    });
}

#[tauri::command]
pub fn media_state() -> Option<Media> {
    SHOWN.lock().unwrap().as_ref().map(|(_, m)| m.clone())
}

/// `toggle`, `next` or `prev` for the player the widget shows.
#[tauri::command]
pub async fn media_control(action: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        // SAFETY: as in `spawn`; this blocking-pool thread may not be initialized yet.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        let manager = SessionManager::RequestAsync().and_then(|op| op.get()).map_err(|e| e.to_string())?;
        let shown_id = SHOWN.lock().unwrap().as_ref().map(|(id, _)| id.clone());
        let (session, _) = pick(&manager, shown_id.as_deref(), &[]).ok_or("Плеер не найден")?;
        let op = match action.as_str() {
            "toggle" => session.TryTogglePlayPauseAsync(),
            "next" => session.TrySkipNextAsync(),
            "prev" => session.TrySkipPreviousAsync(),
            _ => return Err(format!("unknown action {action}")),
        };
        op.and_then(|op| op.get()).map(|_| ()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }

    #[test]
    fn app_names() {
        assert_eq!(app_name("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(app_name("Spotify.exe"), "Spotify");
        assert_eq!(app_name("Chrome"), "Chrome");
        assert_eq!(app_name("MSEdge"), "Edge");
        assert_eq!(app_name(r"C:\Apps\Foobar2000.exe"), "Foobar2000");
    }

    #[test]
    fn file_time_to_unix() {
        assert_eq!(unix_ms(116_444_736_000_000_000), 0);
        assert_eq!(unix_ms(116_444_736_000_000_000 + 15_000_000), 1_500);
    }
}
