//! Meetings from a Mail.ru calendar over CalDAV, read-only, for the calendar
//! widget. The app password lives in the Windows Credential Manager; the
//! account and the chosen calendars in the settings. Synced every few minutes,
//! with a reminder shortly before each meeting.

use crate::{
    desktop::{finish, APP_ID},
    ical::{self, Event},
    settings::{self, CalendarRef},
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Condvar, Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{AppHandle, Emitter};
use tauri_winrt_notification::Toast;
use windows::{
    core::{IInspectable, HSTRING, PCWSTR, PWSTR},
    Data::Xml::Dom::XmlDocument,
    Foundation::{PropertyValue, Uri},
    Storage::Streams::UnicodeEncoding,
    Web::Http::{Filters::HttpBaseProtocolFilter, HttpClient, HttpMethod, HttpRequestMessage, HttpStringContent},
    Win32::{
        Foundation::FILETIME,
        Security::Credentials::{CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC},
        System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
    },
};

pub const LABEL: &str = "calendar";
const SERVER: &str = "https://calendar.mail.ru";
const CREDENTIAL: &str = "SimpleDeskTasks/calendar.mail.ru";
const SYNC_EVERY: Duration = Duration::from_secs(5 * 60);
/// A request that takes longer is given up on.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const NOT_FOUND: &str = "calendar.mail.ru ответил 404";
/// A reminder this long before a meeting.
const REMIND_MS: i64 = 5 * 60_000;
const NS: &str = "xmlns:d='DAV:' xmlns:c='urn:ietf:params:xml:ns:caldav' xmlns:a='http://apple.com/ns/ical/'";

/// A meeting in one of the user's calendars.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Meeting {
    #[serde(flatten)]
    pub event: Event,
    pub calendar: String,
    pub color: String,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub configured: bool,
    pub meetings: Vec<Meeting>,
    /// When the meetings were last fetched (unix ms), 0 = never.
    pub updated: i64,
    pub error: Option<String>,
    pub syncing: bool,
    /// Meetings by calendar URL: a calendar that fails keeps its last ones.
    #[serde(skip)]
    by_calendar: HashMap<String, Vec<Meeting>>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static WAKE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
/// Bumped on connect / disconnect: a sync begun before then publishes nothing.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn state() -> State {
    STATE.lock().unwrap().clone().unwrap_or_default()
}

/// Changes the state and shows it, unless the account changed since
/// `generation` (then the change is stale).
fn publish(app: &AppHandle, generation: u64, f: impl FnOnce(&mut State)) {
    let mut guard = STATE.lock().unwrap();
    if GENERATION.load(Ordering::SeqCst) != generation {
        return;
    }
    let s = guard.get_or_insert_with(State::default);
    f(s);
    // The widget and the settings window (its status line) both show it;
    // sent under the lock so two publishes can't arrive swapped.
    let _ = app.emit("calendar", &*s);
}

/// A new account (or none): earlier syncs are void.
fn new_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

/// Syncs soon (settings changed, connected, or asked for).
pub fn refresh() {
    let (flag, cv) = &WAKE;
    *flag.lock().unwrap() = true;
    cv.notify_all();
}

// ---------- credentials ----------

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn save_password(email: &str, password: &str) -> Result<(), String> {
    let (target, user) = (wide(CREDENTIAL), wide(email));
    let mut blob = password.as_bytes().to_vec();
    let cred = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_ptr() as *mut _),
        UserName: PWSTR(user.as_ptr() as *mut _),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        LastWritten: FILETIME::default(),
        ..Default::default()
    };
    // SAFETY: every pointer in `cred` outlives the call.
    unsafe { CredWriteW(&cred, 0) }.map_err(|e| e.message().to_string())
}

fn load_password() -> Option<String> {
    let target = wide(CREDENTIAL);
    let mut found: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: the returned block is read, then freed with CredFree.
    unsafe {
        CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut found).ok()?;
        let cred = &*found;
        let bytes = std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize).to_vec();
        CredFree(found as *const _);
        String::from_utf8(bytes).ok()
    }
}

fn forget_password() {
    let target = wide(CREDENTIAL);
    // SAFETY: plain call with a live string.
    let _ = unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) };
}

// ---------- CalDAV ----------

fn base64(bytes: &[u8]) -> String {
    crate::media::base64(bytes)
}

struct Reply {
    status: u16,
    body: String,
    /// Where a redirect points.
    location: Option<String>,
    /// The URL that answered (after redirects): hrefs in the body are relative to it.
    url: String,
}

/// Redirects followed by hand: the system won't follow one for PROPFIND (it
/// wants a user's say-so) and fails the request, and Mail.ru's
/// /.well-known/caldav is a 301 to /principals/.
const MAX_REDIRECTS: usize = 5;

fn dav(method: &str, url: &str, depth: &str, body: &str, auth: &str) -> Result<Reply, String> {
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let reply = send(method, &url, depth, body, auth).map_err(|e| format!("Нет связи с calendar.mail.ru: {}", e.message()))?;
        match (reply.status, &reply.location) {
            // The password goes along, so only within the calendar's own site.
            (301 | 302 | 303 | 307 | 308, Some(to)) if same_site(&absolute(&url, to)) => url = absolute(&url, to),
            (200..=299, _) => return Ok(Reply { url, ..reply }),
            (401 | 403, _) => return Err("Mail.ru не принял почту или пароль приложения".into()),
            (s, _) => return Err(format!("calendar.mail.ru ответил {s}")),
        }
    }
    Err("calendar.mail.ru: слишком много перенаправлений".into())
}

fn send(method: &str, url: &str, depth: &str, body: &str, auth: &str) -> windows::core::Result<Reply> {
    let filter = HttpBaseProtocolFilter::new()?;
    filter.SetAllowUI(false)?; // never pop up a Windows sign-in dialog
    filter.SetAllowAutoRedirect(false)?;
    let client = HttpClient::Create(&filter)?;
    let request = HttpRequestMessage::Create(&HttpMethod::Create(&HSTRING::from(method))?, &Uri::CreateUri(&HSTRING::from(url))?)?;
    let headers = request.Headers()?;
    headers.TryAppendWithoutValidation(&HSTRING::from("Authorization"), &HSTRING::from(auth))?;
    headers.TryAppendWithoutValidation(&HSTRING::from("Depth"), &HSTRING::from(depth))?;
    headers.TryAppendWithoutValidation(&HSTRING::from("User-Agent"), &HSTRING::from("SimpleDeskTasks"))?;
    let content = HttpStringContent::CreateFromStringWithEncodingAndMediaType(
        &HSTRING::from(body),
        UnicodeEncoding::Utf8,
        &HSTRING::from("application/xml"),
    )?;
    request.SetContent(&content)?;
    let response = finish(client.SendRequestAsync(&request)?, HTTP_TIMEOUT)?;
    let status = response.StatusCode()?.0 as u16;
    let location = response.Headers()?.Location().ok().and_then(|u| u.RawUri().ok()).map(|s| s.to_string());
    let body = finish(response.Content()?.ReadAsStringAsync()?, HTTP_TIMEOUT)?.to_string();
    Ok(Reply { status, body, location, url: url.to_string() })
}

fn auth(email: &str, password: &str) -> String {
    format!("Basic {}", base64(format!("{email}:{password}").as_bytes()))
}

fn same_site(url: &str) -> bool {
    url.strip_prefix(SERVER).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// `href` relative to `base`.
fn absolute(base: &str, href: &str) -> String {
    Uri::CreateUri(&HSTRING::from(base))
        .and_then(|b| b.CombineUri(&HSTRING::from(href.trim())))
        .and_then(|u| u.AbsoluteUri())
        .map(|s| s.to_string())
        .unwrap_or_else(|_| href.trim().to_string())
}

fn xml(body: &str) -> Result<XmlDocument, String> {
    let doc = XmlDocument::new().map_err(|e| e.message().to_string())?;
    doc.LoadXml(&HSTRING::from(body)).map_err(|_| "непонятный ответ календаря".to_string())?;
    Ok(doc)
}

/// The d:/c:/a: prefixes for XPath queries.
fn namespaces() -> Result<IInspectable, String> {
    PropertyValue::CreateString(&HSTRING::from(NS)).map_err(|e| e.message().to_string())
}

/// Text of the first node matching `path`.
fn first_text(doc: &XmlDocument, path: &str) -> Option<String> {
    let node = doc.SelectSingleNodeNS(&HSTRING::from(path), &namespaces().ok()?).ok()?;
    node.InnerText().ok().map(|s| s.to_string().trim().to_string()).filter(|s| !s.is_empty())
}

/// The answer, and the URL that gave it (hrefs in it are relative to that).
fn prop_find(url: &str, depth: &str, props: &str, auth: &str) -> Result<(XmlDocument, String), String> {
    let body = format!("<?xml version='1.0' encoding='utf-8'?><d:propfind {NS}><d:prop>{props}</d:prop></d:propfind>");
    let reply = dav("PROPFIND", url, depth, &body, auth)?;
    Ok((xml(&reply.body)?, reply.url))
}

/// `#RRGGBB[AA]` → `#rrggbb`.
fn color(raw: &str) -> String {
    let hex = raw.trim().trim_start_matches('#');
    match hex.get(..6) {
        Some(rgb) if rgb.bytes().all(|b| b.is_ascii_hexdigit()) => format!("#{}", rgb.to_ascii_lowercase()),
        _ => String::new(),
    }
}

/// The account's calendars that hold events.
fn discover(email: &str, password: &str) -> Result<Vec<CalendarRef>, String> {
    let auth = auth(email, password);
    // Mail.ru answers CalDAV at /principals/ (/.well-known/caldav redirects
    // there; the site's root is the web app and says 400 to PROPFIND).
    let mut principal = None;
    let mut answered = None;
    let mut last_err = String::new();
    for start in [format!("{SERVER}/principals/"), format!("{SERVER}/.well-known/caldav")] {
        match prop_find(&start, "0", "<d:current-user-principal/>", &auth) {
            Ok((doc, at)) => {
                principal = first_text(&doc, "//d:current-user-principal/d:href").map(|href| absolute(&at, &href));
                answered = Some(at);
                break; // the other way in leads to the same place
            }
            Err(e) if e.contains("не принял") => return Err(e),
            Err(e) => last_err = e,
        }
    }
    // No current-user-principal: the collection that answered may be the principal itself.
    let principal = principal.or(answered).ok_or(if last_err.is_empty() { "календарь не найден".into() } else { last_err })?;
    let (doc, at) = prop_find(&principal, "0", "<c:calendar-home-set/>", &auth)?;
    let home = absolute(&at, &first_text(&doc, "//c:calendar-home-set/d:href").ok_or("не найден список календарей")?);
    let (doc, home) = prop_find(&home, "1", "<d:resourcetype/><d:displayname/><a:calendar-color/><c:supported-calendar-component-set/>", &auth)?;
    let ns = namespaces()?;
    let responses = doc.SelectNodesNS(&HSTRING::from("//d:response"), &ns).map_err(|e| e.message().to_string())?;
    let mut out = Vec::new();
    for i in 0..responses.Length().unwrap_or(0) {
        let Ok(r) = responses.Item(i) else { continue };
        let pick = |path: &str| -> Option<String> {
            r.SelectSingleNodeNS(&HSTRING::from(path), &ns).ok().and_then(|n| n.InnerText().ok()).map(|s| s.to_string().trim().to_string())
        };
        let is_calendar = r.SelectSingleNodeNS(&HSTRING::from(".//d:resourcetype/c:calendar"), &ns).is_ok();
        let comps = r.SelectNodesNS(&HSTRING::from(".//c:supported-calendar-component-set/c:comp"), &ns).ok();
        let events = match comps {
            Some(list) if list.Length().unwrap_or(0) > 0 => (0..list.Length().unwrap_or(0)).any(|j| {
                list.Item(j)
                    .ok()
                    .and_then(|n| n.Attributes().ok()?.GetNamedItem(&HSTRING::from("name")).ok()?.InnerText().ok())
                    .is_some_and(|name| name.to_string().eq_ignore_ascii_case("VEVENT"))
            }),
            _ => true,
        };
        let Some(href) = pick(".//d:href") else { continue };
        if !is_calendar || !events {
            continue;
        }
        out.push(CalendarRef {
            url: absolute(&home, &href),
            name: pick(".//d:displayname").filter(|n| !n.is_empty()).unwrap_or_else(|| "Календарь".into()),
            color: pick(".//a:calendar-color").map(|c| color(&c)).unwrap_or_default(),
            enabled: true,
        });
    }
    out.retain(|c| !settings::hidden_calendar(&c.name));
    if out.is_empty() {
        return Err("в аккаунте нет календарей со встречами".into());
    }
    Ok(out)
}

fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%Y%m%dT%H%M%SZ").to_string()).unwrap_or_default()
}

/// Meetings of one calendar between `from` and `to` (unix ms).
fn fetch(cal: &CalendarRef, auth: &str, from: i64, to: i64) -> Result<Vec<Meeting>, String> {
    let (s, e) = (stamp(from), stamp(to));
    let body = format!(
        "<?xml version='1.0' encoding='utf-8'?><c:calendar-query {NS}><d:prop><c:calendar-data>\
         <c:expand start='{s}' end='{e}'/></c:calendar-data></d:prop><c:filter><c:comp-filter name='VCALENDAR'>\
         <c:comp-filter name='VEVENT'><c:time-range start='{s}' end='{e}'/></c:comp-filter></c:comp-filter></c:filter>\
         </c:calendar-query>"
    );
    let reply = dav("REPORT", &cal.url, "1", &body, auth).map_err(|e| {
        if e == NOT_FOUND {
            format!("Календарь «{}» не найден — подключите календарь заново", cal.name)
        } else {
            e
        }
    })?;
    let doc = xml(&reply.body)?;
    let ns = namespaces()?;
    let nodes = doc.SelectNodesNS(&HSTRING::from("//c:calendar-data"), &ns).map_err(|e| e.message().to_string())?;
    let mut out = Vec::new();
    for i in 0..nodes.Length().unwrap_or(0) {
        let Some(text) = nodes.Item(i).ok().and_then(|n| n.InnerText().ok()) else { continue };
        for event in ical::parse(&text.to_string(), from, to) {
            out.push(Meeting { event, calendar: cal.name.clone(), color: cal.color.clone() });
        }
    }
    Ok(out)
}

/// The window the widget shows: from the Monday before this month's first day
/// to six weeks ahead.
fn window() -> (i64, i64) {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let first = today.with_day(1).unwrap_or(today);
    let back = (today - first).num_days() + first.weekday().num_days_from_monday() as i64;
    (ical::day_start(-back), ical::day_start(42))
}

fn sync(app: &AppHandle) {
    let generation = GENERATION.load(Ordering::SeqCst);
    let s = settings::get();
    if s.calendar_email.is_empty() {
        publish(app, generation, |st| *st = State::default());
        return;
    }
    let Some(password) = load_password() else {
        publish(app, generation, |st| {
            *st = State { configured: true, error: Some("Пароль не найден — подключите календарь заново".into()), ..Default::default() };
        });
        return;
    };
    publish(app, generation, |st| {
        st.configured = true;
        st.syncing = true;
    });
    let auth = auth(&s.calendar_email, &password);
    let (from, to) = window();
    let enabled: Vec<&CalendarRef> = s.calendars.iter().filter(|c| c.enabled).collect();
    let fetched: Vec<(String, Result<Vec<Meeting>, String>)> = enabled.iter().map(|cal| (cal.url.clone(), fetch(cal, &auth, from, to))).collect();
    publish(app, generation, |st| {
        st.syncing = false;
        st.by_calendar.retain(|url, _| enabled.iter().any(|c| &c.url == url));
        let mut errors: Vec<String> = Vec::new();
        for (url, result) in fetched {
            match result {
                Ok(meetings) => {
                    st.by_calendar.insert(url, meetings);
                }
                // A calendar that failed keeps what it had (shown with its time).
                Err(e) if !errors.contains(&e) => errors.push(e),
                Err(_) => {}
            }
        }
        st.error = (!errors.is_empty()).then(|| errors.join("; "));
        if errors.is_empty() {
            st.updated = crate::store::now_ms();
        }
        let mut meetings: Vec<Meeting> = st.by_calendar.values().flatten().cloned().collect();
        meetings.sort_by(|a, b| (a.event.start, a.event.end, &a.event.id).cmp(&(b.event.start, b.event.end, &b.event.id)));
        st.meetings = meetings;
    });
}

fn remind(due: &Meeting) {
    let at = chrono::DateTime::from_timestamp_millis(due.event.start)
        .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default();
    let place = if due.event.link.is_some() { " · созвон" } else if due.event.location.is_empty() { "" } else { " · место указано" };
    let _ = Toast::new(APP_ID)
        .title(&due.event.title)
        .text1(&format!("Через 5 минут, в {at}{place}"))
        .sound(Some(tauri_winrt_notification::Sound::Reminder))
        .show();
}

pub fn spawn(app: AppHandle) {
    let sync_app = app.clone();
    thread::spawn(move || {
        // SAFETY: plain call; WinRT needs the thread initialized before use.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        loop {
            sync(&sync_app);
            let (flag, cv) = &WAKE;
            let guard = flag.lock().unwrap();
            let (mut woken, _) = cv.wait_timeout_while(guard, SYNC_EVERY, |w| !*w).unwrap();
            *woken = false;
        }
    });
    thread::spawn(move || {
        // By id and start: a meeting moved to later is reminded of again.
        let mut reminded: HashSet<(String, i64)> = HashSet::new();
        loop {
            thread::sleep(Duration::from_secs(20));
            let now = crate::store::now_ms();
            let meetings = state().meetings;
            for m in &meetings {
                let lead = m.event.start - now;
                if !m.event.all_day && lead > 0 && lead <= REMIND_MS && reminded.insert((m.event.id.clone(), m.event.start)) {
                    remind(m);
                }
            }
            // Forget reminders of meetings that are gone or long started.
            reminded.retain(|(id, start)| *start > now - 3_600_000 && meetings.iter().any(|m| &m.event.id == id && m.event.start == *start));
        }
    });
}

#[tauri::command]
pub fn calendar_state() -> State {
    let mut s = state();
    s.configured = !settings::get().calendar_email.is_empty();
    s
}

/// Checks the account, saves the password and the calendars found.
#[tauri::command]
pub async fn calendar_connect(app: AppHandle, email: String, password: String) -> Result<Vec<CalendarRef>, String> {
    let email = email.trim().to_string();
    if !email.contains('@') || password.trim().is_empty() {
        return Err("Нужны почта и пароль для внешних приложений".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        // SAFETY: as in `spawn`.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        let password = password.trim().to_string();
        let calendars = discover(&email, &password)?;
        save_password(&email, &password)?;
        let kept = calendars.clone();
        settings::update(&app, move |s| {
            // Keep the user's on/off choice for calendars seen before.
            let before = std::mem::take(&mut s.calendars);
            s.calendar_email = email;
            s.calendars = kept
                .into_iter()
                .map(|mut c| {
                    if let Some(old) = before.iter().find(|o| o.url == c.url) {
                        c.enabled = old.enabled;
                    }
                    c
                })
                .collect();
        })?;
        new_generation();
        refresh();
        Ok(calendars)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn calendar_disconnect(app: AppHandle) -> Result<(), String> {
    forget_password();
    settings::update(&app, |s| {
        s.calendar_email.clear();
        s.calendars.clear();
    })?;
    // A sync still running for the old account must not bring it back.
    publish(&app, new_generation(), |st| *st = State::default());
    refresh();
    Ok(())
}

#[tauri::command]
pub fn calendar_refresh() {
    refresh();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_normalize() {
        assert_eq!(color("#FF8800FF"), "#ff8800");
        assert_eq!(color("#abc"), "");
        assert_eq!(color(""), "");
        assert_eq!(color("abcdeé"), "");
    }

    #[test]
    fn only_the_calendar_site_gets_redirects_followed() {
        assert!(same_site("https://calendar.mail.ru/principals/"));
        assert!(!same_site("https://calendar.mail.ru.evil.example/x"));
        assert!(!same_site("http://calendar.mail.ru/principals/"));
        assert!(!same_site("https://calendar.mail.ru@evil.example/"));
    }

    /// Against the real server (network): both ways in reach CalDAV, which
    /// turns a made-up account down instead of answering 400.
    #[test]
    #[ignore]
    fn mail_ru_answers_caldav() {
        // SAFETY: as in `spawn`.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        let auth = auth("nobody-sdt-test@mail.ru", "wrong");
        for url in [format!("{SERVER}/principals/"), format!("{SERVER}/.well-known/caldav")] {
            let err = prop_find(&url, "0", "<d:current-user-principal/>", &auth).err().unwrap_or_default();
            assert!(err.contains("не принял"), "{url}: {err}");
        }
    }
}
